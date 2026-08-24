use crate::error::ServerError;
use futures_util::StreamExt;
use openflow_protocol::{
    ActiveModel, ComputeBackend, ModelInfo, ModelKind, ModelRegistry, ModelSpec, ModelState,
};
use reqwest::{Client, StatusCode, header};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    sync::RwLock,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct ModelManager {
    inner: Arc<ModelManagerInner>,
}

#[derive(Debug)]
struct ModelManagerInner {
    cache_dir: PathBuf,
    records: RwLock<HashMap<String, ModelRecord>>,
    active: RwLock<HashMap<ModelKind, String>>,
    downloads: RwLock<HashMap<String, Arc<CancellationToken>>>,
    client: Client,
}

#[derive(Clone, Debug)]
struct ModelRecord {
    spec: ModelSpec,
    state: ModelState,
}

impl ModelManager {
    /// Loads and validates the model registry, then verifies the on-disk cache.
    ///
    /// # Errors
    ///
    /// Returns an error when registry/cache data is invalid or inaccessible.
    pub async fn load(
        cache_dir: PathBuf,
        registry_path: Option<&Path>,
    ) -> Result<Self, ServerError> {
        let registry = match registry_path {
            Some(path) => serde_json::from_slice::<ModelRegistry>(&tokio::fs::read(path).await?)?,
            None => bundled_registry(),
        };
        validate_registry(&registry)?;
        tokio::fs::create_dir_all(&cache_dir).await?;

        let manager = Self {
            inner: Arc::new(ModelManagerInner {
                cache_dir,
                records: RwLock::new(
                    registry
                        .models
                        .into_iter()
                        .map(|spec| {
                            (
                                spec.id.clone(),
                                ModelRecord {
                                    spec,
                                    state: ModelState::NotDownloaded,
                                },
                            )
                        })
                        .collect(),
                ),
                active: RwLock::new(HashMap::new()),
                downloads: RwLock::new(HashMap::new()),
                client: Client::builder()
                    .user_agent(concat!("openflow-server/", env!("CARGO_PKG_VERSION")))
                    .redirect(reqwest::redirect::Policy::custom(|attempt| {
                        if attempt.previous().len() >= 5 {
                            attempt.error("too many model download redirects")
                        } else if attempt.url().scheme() == "https" {
                            attempt.follow()
                        } else {
                            attempt.error("model download redirect must remain HTTPS")
                        }
                    }))
                    .connect_timeout(Duration::from_secs(30))
                    .read_timeout(Duration::from_secs(60))
                    .build()
                    .map_err(|error| ServerError::Download(error.to_string()))?,
            }),
        };
        manager.discover_cached_models().await?;
        Ok(manager)
    }

    pub async fn list(&self) -> Vec<ModelInfo> {
        self.list_for_memory(None).await
    }

    /// Marks one compatible ASR/cleanup pair as recommended. Cleanup quality is
    /// prioritized, then ASR quality, while preserving a 20% memory reserve.
    pub async fn list_for_memory(&self, available_memory_bytes: Option<u64>) -> Vec<ModelInfo> {
        let records = self.inner.records.read().await;
        let active = self.inner.active.read().await;
        let recommended = available_memory_bytes.and_then(|bytes| {
            let budget = bytes.saturating_mul(80) / 100;
            let mut asr: Vec<_> = records
                .values()
                .filter(|record| record.spec.kind == ModelKind::SpeechToText)
                .collect();
            let mut cleanup: Vec<_> = records
                .values()
                .filter(|record| record.spec.kind == ModelKind::TextCleanup)
                .collect();
            asr.sort_by_key(|record| std::cmp::Reverse(record.spec.estimated_memory_bytes));
            cleanup.sort_by_key(|record| std::cmp::Reverse(record.spec.estimated_memory_bytes));
            cleanup.into_iter().find_map(|cleanup| {
                asr.iter()
                    .find(|asr| {
                        asr.spec
                            .estimated_memory_bytes
                            .saturating_add(cleanup.spec.estimated_memory_bytes)
                            <= budget
                    })
                    .map(|asr| (asr.spec.id.clone(), cleanup.spec.id.clone()))
            })
        });
        let mut models: Vec<_> = records
            .values()
            .map(|record| {
                let mut spec = record.spec.clone();
                let is_recommended = recommended
                    .as_ref()
                    .is_some_and(|(asr, cleanup)| spec.id == *asr || spec.id == *cleanup);
                spec.metadata
                    .insert("recommended".into(), is_recommended.to_string());
                ModelInfo {
                    spec,
                    state: record.state.clone(),
                    active: active.get(&record.spec.kind) == Some(&record.spec.id),
                }
            })
            .collect();
        models.sort_by(|left, right| left.spec.id.cmp(&right.spec.id));
        models
    }

    pub async fn active_models(&self) -> Vec<ActiveModel> {
        self.inner
            .active
            .read()
            .await
            .iter()
            .map(|(kind, model_id)| ActiveModel {
                kind: *kind,
                model_id: model_id.clone(),
            })
            .collect()
    }

    /// Returns the verified cache path for a ready model.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is unknown or its model is not ready.
    pub async fn model_path(&self, model_id: &str) -> Result<PathBuf, ServerError> {
        let records = self.inner.records.read().await;
        let record = records
            .get(model_id)
            .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
        if !matches!(record.state, ModelState::Ready) {
            return Err(ServerError::Conflict(format!(
                "model {model_id} is not ready"
            )));
        }
        Ok(self.final_path(&record.spec))
    }

    /// Starts a background, resumable download. Repeated requests while the
    /// same model is downloading are idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error when the model identifier is unknown.
    pub async fn start_download(&self, model_id: &str) -> Result<bool, ServerError> {
        let cancellation = Arc::new(CancellationToken::new());
        {
            let mut downloads = self.inner.downloads.write().await;
            let mut records = self.inner.records.write().await;
            let record = records
                .get_mut(model_id)
                .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
            if matches!(
                record.state,
                ModelState::Ready | ModelState::Downloading { .. } | ModelState::Cancelling
            ) {
                return Ok(false);
            }
            record.state = ModelState::Downloading {
                downloaded_bytes: 0,
                total_bytes: record.spec.size_bytes,
            };
            downloads.insert(model_id.to_owned(), Arc::clone(&cancellation));
        }

        let manager = self.clone();
        let id = model_id.to_owned();
        tokio::spawn(async move {
            let result = manager.download_and_verify(&id, &cancellation).await;
            let cancelled = cancellation.is_cancelled();
            {
                let mut downloads = manager.inner.downloads.write().await;
                if downloads
                    .get(&id)
                    .is_some_and(|current| Arc::ptr_eq(current, &cancellation))
                {
                    downloads.remove(&id);
                }
            }
            if let Err(error) = result {
                if !cancelled {
                    tracing::warn!(model_id = %id, error = %error, "model download failed");
                }
                if let Some(record) = manager.inner.records.write().await.get_mut(&id)
                    && !matches!(record.state, ModelState::Ready)
                {
                    record.state = if cancelled {
                        ModelState::NotDownloaded
                    } else {
                        ModelState::Failed {
                            message: error.to_string(),
                        }
                    };
                }
            }
        });
        Ok(true)
    }

    /// Requests cancellation of an in-progress download. Its partial file is
    /// retained so selecting the model later can resume it.
    ///
    /// # Errors
    ///
    /// Returns an error when the model is unknown or has no active download.
    pub async fn cancel_download(&self, model_id: &str) -> Result<(), ServerError> {
        let downloads = self.inner.downloads.write().await;
        let mut records = self.inner.records.write().await;
        let record = records
            .get_mut(model_id)
            .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
        if !matches!(
            record.state,
            ModelState::Downloading { .. } | ModelState::Verifying | ModelState::Cancelling
        ) {
            return Err(ServerError::Conflict(format!(
                "model {model_id} is not being downloaded"
            )));
        }
        let cancellation = downloads.get(model_id).ok_or_else(|| {
            ServerError::Conflict(format!("model {model_id} download is already finishing"))
        })?;
        cancellation.cancel();
        record.state = ModelState::Cancelling;
        Ok(())
    }

    /// Marks a verified model active for its model kind.
    ///
    /// # Errors
    ///
    /// Returns an error when the model is unknown or not ready.
    pub async fn activate(&self, model_id: &str) -> Result<(), ServerError> {
        let kind = {
            let records = self.inner.records.read().await;
            let record = records
                .get(model_id)
                .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
            if !matches!(record.state, ModelState::Ready) {
                return Err(ServerError::Conflict(format!(
                    "model {model_id} must finish downloading before activation"
                )));
            }
            record.spec.kind
        };
        self.inner
            .active
            .write()
            .await
            .insert(kind, model_id.into());
        Ok(())
    }

    /// Removes a model from the active selection for its kind.
    ///
    /// Returns `true` when the requested model was active. The verified cache
    /// remains available for later activation.
    ///
    /// # Errors
    ///
    /// Returns an error when the model identifier is unknown.
    pub async fn deactivate(&self, model_id: &str) -> Result<bool, ServerError> {
        let kind = self
            .inner
            .records
            .read()
            .await
            .get(model_id)
            .map(|record| record.spec.kind)
            .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
        let mut active = self.inner.active.write().await;
        if active.get(&kind).is_some_and(|active| active == model_id) {
            active.remove(&kind);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Deletes an inactive, completed model from the cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the model is unknown, active, downloading, or the
    /// cache file cannot be removed.
    pub async fn delete(&self, model_id: &str) -> Result<(), ServerError> {
        let spec = {
            let records = self.inner.records.read().await;
            let record = records
                .get(model_id)
                .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
            if matches!(
                record.state,
                ModelState::Downloading { .. } | ModelState::Cancelling | ModelState::Verifying
            ) {
                return Err(ServerError::Conflict(format!(
                    "model {model_id} is currently being downloaded"
                )));
            }
            if self
                .inner
                .active
                .read()
                .await
                .values()
                .any(|active| active == model_id)
            {
                return Err(ServerError::Conflict(format!(
                    "deactivate model {model_id} before deleting it"
                )));
            }
            record.spec.clone()
        };
        let final_path = self.final_path(&spec);
        remove_file_if_present(&final_path).await?;
        remove_file_if_present(&final_path.with_extension("part")).await?;
        if let Some(record) = self.inner.records.write().await.get_mut(model_id) {
            record.state = ModelState::NotDownloaded;
        }
        Ok(())
    }

    async fn discover_cached_models(&self) -> Result<(), ServerError> {
        let snapshots: Vec<_> = self
            .inner
            .records
            .read()
            .await
            .values()
            .map(|record| record.spec.clone())
            .collect();
        for spec in snapshots {
            let path = self.final_path(&spec);
            let valid_size = tokio::fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.len() == spec.size_bytes);
            let valid_digest = valid_size
                && sha256_file(&path, None)
                    .await
                    .is_ok_and(|digest| digest.eq_ignore_ascii_case(&spec.sha256));
            if valid_digest {
                if let Some(record) = self.inner.records.write().await.get_mut(&spec.id) {
                    record.state = ModelState::Ready;
                }
            } else if valid_size {
                tracing::warn!(model_id = %spec.id, "removing cached model with invalid checksum");
                tokio::fs::remove_file(path).await?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn download_and_verify(
        &self,
        model_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), ServerError> {
        ensure_not_cancelled(cancellation)?;
        let spec = self
            .inner
            .records
            .read()
            .await
            .get(model_id)
            .map(|record| record.spec.clone())
            .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
        let final_path = self.final_path(&spec);
        let part_path = final_path.with_extension("part");
        if let Some(parent) = final_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let existing = tokio::fs::metadata(&part_path)
            .await
            .map_or(0, |metadata| metadata.len());
        if existing > spec.size_bytes {
            tokio::fs::remove_file(&part_path).await?;
        }
        let offset = if existing <= spec.size_bytes {
            existing
        } else {
            0
        };
        let mut downloaded = offset;
        if offset < spec.size_bytes {
            let mut request = self.inner.client.get(&spec.source_url);
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            let response = tokio::select! {
                () = cancellation.cancelled() => return Err(cancelled_download()),
                response = request.send() => response
                    .map_err(|error| ServerError::Download(error.to_string()))?,
            };
            ensure_not_cancelled(cancellation)?;
            if !response.status().is_success() {
                return Err(ServerError::Download(format!(
                    "upstream returned {}",
                    response.status()
                )));
            }
            let resumed = offset > 0 && response.status() == StatusCode::PARTIAL_CONTENT;
            downloaded = if resumed { offset } else { 0 };
            let mut file = if resumed {
                OpenOptions::new().append(true).open(&part_path).await?
            } else {
                File::create(&part_path).await?
            };
            let mut stream = response.bytes_stream();
            loop {
                let next = tokio::select! {
                    () = cancellation.cancelled() => return Err(cancelled_download()),
                    next = stream.next() => next,
                };
                let Some(chunk) = next else { break };
                let chunk = chunk.map_err(|error| ServerError::Download(error.to_string()))?;
                downloaded = downloaded.saturating_add(chunk.len() as u64);
                if downloaded > spec.size_bytes {
                    return Err(ServerError::Download(
                        "download exceeded registry size".into(),
                    ));
                }
                file.write_all(&chunk).await?;
                if let Some(record) = self.inner.records.write().await.get_mut(model_id)
                    && !matches!(record.state, ModelState::Cancelling)
                {
                    record.state = ModelState::Downloading {
                        downloaded_bytes: downloaded,
                        total_bytes: spec.size_bytes,
                    };
                }
            }
            file.flush().await?;
            file.sync_all().await?;
            drop(file);
        }
        ensure_not_cancelled(cancellation)?;
        if downloaded != spec.size_bytes {
            return Err(ServerError::Download(format!(
                "expected {} bytes, received {downloaded}",
                spec.size_bytes
            )));
        }
        if let Some(record) = self.inner.records.write().await.get_mut(model_id)
            && !cancellation.is_cancelled()
            && !matches!(record.state, ModelState::Cancelling)
        {
            record.state = ModelState::Verifying;
        }
        ensure_not_cancelled(cancellation)?;
        let digest = sha256_file(&part_path, Some(cancellation)).await?;
        if !digest.eq_ignore_ascii_case(&spec.sha256) {
            tokio::fs::remove_file(&part_path).await?;
            return Err(ServerError::Download(format!(
                "SHA-256 mismatch: expected {}, received {digest}",
                spec.sha256
            )));
        }
        // Keep the record locked across the atomic promotion. A cancellation
        // request takes this same lock before it marks its flag, so it either
        // wins before this block or observes Ready and returns a conflict. The
        // API can never acknowledge cancellation while this task publishes the
        // model at the same time.
        let mut records = self.inner.records.write().await;
        let record = records
            .get_mut(model_id)
            .ok_or_else(|| ServerError::NotFound(format!("model {model_id}")))?;
        ensure_not_cancelled(cancellation)?;
        if matches!(record.state, ModelState::Cancelling) {
            return Err(ServerError::Download("model download cancelled".into()));
        }
        tokio::fs::rename(&part_path, &final_path).await?;
        record.state = ModelState::Ready;
        Ok(())
    }

    fn final_path(&self, spec: &ModelSpec) -> PathBuf {
        self.inner.cache_dir.join(&spec.id).join("model.bin")
    }
}

async fn sha256_file(
    path: &Path,
    cancellation: Option<&CancellationToken>,
) -> Result<String, ServerError> {
    let mut file = File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        if let Some(cancellation) = cancellation {
            ensure_not_cancelled(cancellation)?;
        }
        let count = file.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), ServerError> {
    if cancellation.is_cancelled() {
        Err(cancelled_download())
    } else {
        Ok(())
    }
}

fn cancelled_download() -> ServerError {
    ServerError::Download("model download cancelled".into())
}

async fn remove_file_if_present(path: &Path) -> Result<(), ServerError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_registry(registry: &ModelRegistry) -> Result<(), ServerError> {
    if registry.schema_version != 1 {
        return Err(ServerError::Configuration(format!(
            "unsupported model registry schema {}",
            registry.schema_version
        )));
    }
    let mut ids = std::collections::HashSet::new();
    for model in &registry.models {
        if model.id.is_empty()
            || !model
                .id
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !model
                .id
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !model
                .id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
            || !ids.insert(&model.id)
        {
            return Err(ServerError::Configuration(format!(
                "invalid or duplicate model id: {}",
                model.id
            )));
        }
        if model.sha256.len() != 64 || hex::decode(&model.sha256).is_err() {
            return Err(ServerError::Configuration(format!(
                "model {} has an invalid SHA-256",
                model.id
            )));
        }
        if model.revision.is_empty()
            || model.license.is_empty()
            || model.size_bytes == 0
            || model.estimated_memory_bytes == 0
        {
            return Err(ServerError::Configuration(format!(
                "model {} has incomplete immutable metadata",
                model.id
            )));
        }
        let url = reqwest::Url::parse(&model.source_url).map_err(|error| {
            ServerError::Configuration(format!("model {} URL: {error}", model.id))
        })?;
        if url.scheme() != "https" {
            return Err(ServerError::Configuration(format!(
                "model {} must use an HTTPS source URL",
                model.id
            )));
        }
    }
    Ok(())
}

fn bundled_registry() -> ModelRegistry {
    ModelRegistry {
        schema_version: 1,
        generated_at: "2026-08-18T00:00:00Z".into(),
        models: vec![
            ModelSpec {
                id: "whisper-large-v3-turbo-q8_0".into(),
                display_name: "Whisper Large v3 Turbo Q8".into(),
                kind: ModelKind::SpeechToText,
                family: "whisper.cpp".into(),
                source_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/0b364b566045a405be7225ee1e415a073e04da77/ggml-large-v3-turbo-q8_0.bin".into(),
                revision: "0b364b566045a405be7225ee1e415a073e04da77".into(),
                sha256: "317eb69c11673c9de1e1f0d459b253999804ec71ac4c23c17ecf5fbe24e259a1".into(),
                size_bytes: 874_188_075,
                estimated_memory_bytes: 2_500_000_000,
                license: "MIT".into(),
                quantization: "Q8_0".into(),
                languages: vec!["multilingual".into()],
                backends: vec![ComputeBackend::Cpu, ComputeBackend::Cuda, ComputeBackend::Rocm, ComputeBackend::Metal, ComputeBackend::Vulkan],
                metadata: BTreeMap::new(),
            },
            ModelSpec {
                id: "whisper-large-v3-q5_0".into(),
                display_name: "Whisper Large v3 Q5".into(),
                kind: ModelKind::SpeechToText,
                family: "whisper.cpp".into(),
                source_url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/c521a4b02f422512d734391fdf08bb08c0862f68/ggml-large-v3-q5_0.bin".into(),
                revision: "c521a4b02f422512d734391fdf08bb08c0862f68".into(),
                sha256: "d75795ecff3f83b5faa89d1900604ad8c780abd5739fae406de19f23ecd98ad1".into(),
                size_bytes: 1_081_140_203,
                estimated_memory_bytes: 4_500_000_000,
                license: "MIT".into(),
                quantization: "Q5_0".into(),
                languages: vec!["multilingual".into()],
                backends: vec![ComputeBackend::Cpu, ComputeBackend::Cuda, ComputeBackend::Rocm, ComputeBackend::Metal, ComputeBackend::Vulkan],
                metadata: [("tier".into(), "maximum_quality".into())].into_iter().collect(),
            },
            ModelSpec {
                id: "qwen3-1.7b-q8_0".into(),
                display_name: "Qwen3 1.7B Q8".into(),
                kind: ModelKind::TextCleanup,
                family: "llama.cpp".into(),
                source_url: "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/90862c4b9d2787eaed51d12237eafdfe7c5f6077/Qwen3-1.7B-Q8_0.gguf".into(),
                revision: "90862c4b9d2787eaed51d12237eafdfe7c5f6077".into(),
                sha256: "061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a".into(),
                size_bytes: 1_834_426_016,
                estimated_memory_bytes: 3_000_000_000,
                license: "Apache-2.0".into(),
                quantization: "Q8_0".into(),
                languages: vec!["multilingual".into()],
                backends: vec![ComputeBackend::Cpu, ComputeBackend::Cuda, ComputeBackend::Rocm, ComputeBackend::Metal, ComputeBackend::Vulkan],
                metadata: BTreeMap::new(),
            },
            ModelSpec {
                id: "qwen3-4b-q4_k_m".into(),
                display_name: "Qwen3 4B Q4 K M".into(),
                kind: ModelKind::TextCleanup,
                family: "llama.cpp".into(),
                source_url: "https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/a9a60d009fa7ff9606305047c2bf77ac25dbec49/Qwen3-4B-Q4_K_M.gguf".into(),
                revision: "a9a60d009fa7ff9606305047c2bf77ac25dbec49".into(),
                sha256: "7485fe6f11af29433bc51cab58009521f205840f5b4ae3a32fa7f92e8534fdf5".into(),
                size_bytes: 2_497_280_256,
                estimated_memory_bytes: 4_500_000_000,
                license: "Apache-2.0".into(),
                quantization: "Q4_K_M".into(),
                languages: vec!["multilingual".into()],
                backends: vec![ComputeBackend::Cpu, ComputeBackend::Cuda, ComputeBackend::Rocm, ComputeBackend::Metal, ComputeBackend::Vulkan],
                metadata: [("parameters".into(), "4B".into())].into_iter().collect(),
            },
            ModelSpec {
                id: "qwen3-8b-q4_k_m".into(),
                display_name: "Qwen3 8B Q4 K M".into(),
                kind: ModelKind::TextCleanup,
                family: "llama.cpp".into(),
                source_url: "https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/6a569868d07d3bd59e8b97fb001bf8c0b254bb20/Qwen3-8B-Q4_K_M.gguf".into(),
                revision: "6a569868d07d3bd59e8b97fb001bf8c0b254bb20".into(),
                sha256: "d98cdcbd03e17ce47681435b5150e34c1417f50b5c0019dd560e4882c5745785".into(),
                size_bytes: 5_027_783_488,
                estimated_memory_bytes: 7_000_000_000,
                license: "Apache-2.0".into(),
                quantization: "Q4_K_M".into(),
                languages: vec!["multilingual".into()],
                backends: vec![ComputeBackend::Cpu, ComputeBackend::Cuda, ComputeBackend::Rocm, ComputeBackend::Metal, ComputeBackend::Vulkan],
                metadata: [("parameters".into(), "8B".into()), ("tier".into(), "maximum_quality".into())].into_iter().collect(),
            },
        ],
    }
}
