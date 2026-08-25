use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use openflow_protocol::{
    AudioEncoding, CleanupEdit, ComputeBackend, ModelKind, TokenEvidence, TranscriptionRequest,
    TranscriptionResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, mpsc, oneshot},
    time::timeout,
};
use uuid::Uuid;

use crate::error::ServerError;

const MAX_WORKER_FRAME_BYTES: usize = 16 * 1024 * 1024;
const WORKER_QUEUE_CAPACITY: usize = 8;
const WORKER_CALL_TIMEOUT: Duration = Duration::from_mins(5);
const WORKER_RESPONSE_TIMEOUT: Duration = Duration::from_mins(6);
const WORKER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_TRANSPORT_PREFIX: &str = "native worker transport failure";

#[async_trait]
pub trait InferenceEngine: Send + Sync + 'static {
    /// Returns compute backends compiled into the configured native worker.
    fn compute_backends(&self) -> Vec<ComputeBackend> {
        vec![ComputeBackend::Cpu]
    }

    /// Loads the selected model pair before microphone audio begins flowing.
    async fn prepare_models(
        &self,
        _asr_model_id: &str,
        _cleanup_model_id: Option<&str>,
    ) -> Result<(), ServerError> {
        Ok(())
    }

    /// Unloads a model kind when it is no longer selected. Implementations
    /// that do not retain models may keep the default no-op behavior.
    async fn unload_model(&self, _kind: ModelKind) -> Result<(), ServerError> {
        Ok(())
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResult, ServerError>;

    /// Releases backend state belonging to a session that ended without a
    /// final segment (for example, because its WebSocket disconnected).
    async fn cancel_session(&self, _session_id: Uuid) {}
}

#[derive(Debug)]
pub struct UnavailableInferenceEngine;

#[async_trait]
impl InferenceEngine for UnavailableInferenceEngine {
    async fn prepare_models(
        &self,
        _asr_model_id: &str,
        _cleanup_model_id: Option<&str>,
    ) -> Result<(), ServerError> {
        Err(ServerError::Inference(
            "no inference worker has been configured".into(),
        ))
    }

    async fn transcribe(
        &self,
        _request: TranscriptionRequest,
    ) -> Result<TranscriptionResult, ServerError> {
        Err(ServerError::Inference(
            "no inference worker has been configured".into(),
        ))
    }
}

/// Persistent, serialized client for the native worker's framed JSON protocol.
/// Keeping stderr inherited makes diagnostics visible without ever mixing them
/// into machine-readable stdout.
#[derive(Debug)]
pub struct WorkerClient {
    commands: mpsc::Sender<WorkerCommand>,
    next_id: AtomicU64,
    capabilities: WorkerCapabilities,
}

#[derive(Debug)]
struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Debug)]
struct WorkerCommand {
    id: u64,
    command: String,
    params: Value,
    response: oneshot::Sender<Result<Value, ServerError>>,
}

#[derive(Clone, Debug, Deserialize)]
struct WorkerCapabilities {
    worker: String,
    version: String,
    backends: Vec<String>,
    compute_backends: Vec<String>,
}

#[derive(Deserialize)]
struct WorkerPing {
    worker: String,
    version: String,
    protocol_version: u64,
}

#[derive(Deserialize)]
struct WorkerBackends {
    backends: Vec<String>,
    #[serde(default)]
    compute_backends: Vec<String>,
}

#[derive(Serialize)]
struct WorkerRequest<'a> {
    id: u64,
    command: &'a str,
    params: Value,
}

#[derive(Deserialize)]
struct WorkerResponse {
    id: u64,
    ok: bool,
    #[serde(default)]
    result: Value,
    error: Option<WorkerResponseError>,
}

#[derive(Deserialize)]
struct WorkerResponseError {
    code: String,
    message: String,
}

impl WorkerClient {
    /// Starts a persistent framed worker and verifies it with `ping`.
    ///
    /// # Errors
    ///
    /// Returns an error when the process or its pipes cannot be opened or the
    /// worker handshake fails.
    pub async fn spawn(path: &Path) -> Result<Self, ServerError> {
        Self::spawn_checked(path, None).await
    }

    async fn spawn_checked(
        path: &Path,
        expected_worker: Option<&str>,
    ) -> Result<Self, ServerError> {
        let (process, capabilities) = start_worker(path, expected_worker).await?;
        let (commands, receiver) = mpsc::channel(WORKER_QUEUE_CAPACITY);
        tokio::spawn(worker_actor(
            path.to_path_buf(),
            expected_worker.map(str::to_owned),
            process,
            receiver,
        ));
        Ok(Self {
            commands,
            next_id: AtomicU64::new(1),
            capabilities,
        })
    }

    /// Returns the worker identity and the backends advertised at startup.
    #[must_use]
    pub fn capability_summary(&self) -> (&str, &str, &[String]) {
        (
            &self.capabilities.worker,
            &self.capabilities.version,
            &self.capabilities.backends,
        )
    }

    fn supports(&self, backend: &str) -> bool {
        self.capabilities
            .backends
            .iter()
            .any(|item| item == backend)
    }

    /// Sends one framed JSON command through a bounded, cancellation-safe
    /// worker queue and waits for its response.
    ///
    /// Dropping this future does not abandon a response on stdout: the worker
    /// actor completes the exchange before accepting another command.
    ///
    /// # Errors
    ///
    /// Returns an error for overload, timeout, transport, serialization, or a
    /// worker-reported failure.
    pub async fn call(&self, command: &str, params: Value) -> Result<Value, ServerError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (response, receiver) = oneshot::channel();
        self.commands
            .try_send(WorkerCommand {
                id,
                command: command.to_owned(),
                params,
                response,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => ServerError::Inference(
                    "native worker queue is full; retry after the active decode".into(),
                ),
                mpsc::error::TrySendError::Closed(_) => {
                    ServerError::Inference("native worker command channel has closed".into())
                }
            })?;
        timeout(WORKER_RESPONSE_TIMEOUT, receiver)
            .await
            .map_err(|_| ServerError::Inference("native worker call timed out".into()))?
            .map_err(|_| ServerError::Inference("native worker stopped unexpectedly".into()))?
    }
}

fn spawn_process(path: &Path) -> Result<WorkerProcess, ServerError> {
    let mut child = Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            ServerError::Inference(format!("failed to start {}: {error}", path.display()))
        })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| ServerError::Inference("worker stdin was unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ServerError::Inference("worker stdout was unavailable".into()))?;
    Ok(WorkerProcess {
        child,
        stdin,
        stdout: BufReader::new(stdout),
    })
}

async fn exchange(
    process: &mut WorkerProcess,
    id: u64,
    command: &str,
    params: Value,
) -> Result<Value, ServerError> {
    let body = serde_json::to_vec(&WorkerRequest {
        id,
        command,
        params,
    })?;
    if body.len() > MAX_WORKER_FRAME_BYTES {
        return Err(ServerError::PayloadTooLarge(
            "native worker request exceeds 16 MiB".into(),
        ));
    }
    let frame_length = u32::try_from(body.len()).map_err(|_| {
        ServerError::PayloadTooLarge("native worker request exceeds u32 framing".into())
    })?;
    process.stdin.write_all(&frame_length.to_be_bytes()).await?;
    process.stdin.write_all(&body).await?;
    process.stdin.flush().await?;

    let mut length = [0_u8; 4];
    process.stdout.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_WORKER_FRAME_BYTES {
        return Err(ServerError::Inference(
            "native worker response exceeds 16 MiB".into(),
        ));
    }
    let mut response_bytes = vec![0; length];
    process.stdout.read_exact(&mut response_bytes).await?;
    let response: WorkerResponse = serde_json::from_slice(&response_bytes)?;
    if response.id != id {
        return Err(ServerError::Inference(format!(
            "native worker response id mismatch: expected {id}, received {}",
            response.id
        )));
    }
    if response.ok {
        Ok(response.result)
    } else {
        let error = response.error.unwrap_or(WorkerResponseError {
            code: "unknown".into(),
            message: "native worker returned no error details".into(),
        });
        Err(ServerError::Inference(format!(
            "worker {}: {}",
            error.code, error.message
        )))
    }
}

async fn start_worker(
    path: &Path,
    expected_worker: Option<&str>,
) -> Result<(WorkerProcess, WorkerCapabilities), ServerError> {
    let mut process = spawn_process(path)?;
    let ping: WorkerPing = serde_json::from_value(
        timeout(
            WORKER_HANDSHAKE_TIMEOUT,
            exchange(&mut process, 0, "ping", json!({})),
        )
        .await
        .map_err(|_| ServerError::Inference("native worker handshake timed out".into()))??,
    )?;
    if ping.protocol_version != 1 {
        return Err(ServerError::Inference(format!(
            "unsupported native worker protocol {}; expected 1",
            ping.protocol_version
        )));
    }
    if let Some(expected) = expected_worker
        && ping.worker != expected
    {
        return Err(ServerError::Inference(format!(
            "native worker identity mismatch: expected {expected}, received {}",
            ping.worker
        )));
    }
    let listed: WorkerBackends = serde_json::from_value(
        timeout(
            WORKER_HANDSHAKE_TIMEOUT,
            exchange(&mut process, 0, "list_backends", json!({})),
        )
        .await
        .map_err(|_| ServerError::Inference("native worker capability query timed out".into()))??,
    )?;
    if listed.backends.is_empty() {
        return Err(ServerError::Inference(
            "native worker advertised no compiled backends".into(),
        ));
    }
    let capabilities = WorkerCapabilities {
        worker: ping.worker,
        version: ping.version,
        backends: listed.backends,
        compute_backends: listed.compute_backends,
    };
    Ok((process, capabilities))
}

async fn worker_actor(
    path: PathBuf,
    expected_worker: Option<String>,
    initial_process: WorkerProcess,
    mut receiver: mpsc::Receiver<WorkerCommand>,
) {
    let mut process = Some(initial_process);
    while let Some(command) = receiver.recv().await {
        if process.is_none() {
            match start_worker(&path, expected_worker.as_deref()).await {
                Ok((next, _)) => process = Some(next),
                Err(error) => {
                    let _ = command.response.send(Err(transport_error(&error)));
                    continue;
                }
            }
        }
        let result = timeout(
            WORKER_CALL_TIMEOUT,
            exchange(
                process.as_mut().expect("worker process was initialized"),
                command.id,
                &command.command,
                command.params,
            ),
        )
        .await
        .map_err(|_| ServerError::Inference("native worker command timed out".into()))
        .and_then(std::convert::identity);
        let result = match result {
            Ok(value) => Ok(value),
            Err(error) if is_command_error(&error) => Err(error),
            Err(error) => {
                if let Some(mut broken) = process.take() {
                    let _ = broken.child.start_kill();
                    let _ = broken.child.wait().await;
                }
                Err(transport_error(&error))
            }
        };
        let _ = command.response.send(result);
    }
}

fn transport_error(error: &ServerError) -> ServerError {
    ServerError::Inference(format!("{WORKER_TRANSPORT_PREFIX}: {error}"))
}

fn is_command_error(error: &ServerError) -> bool {
    matches!(error, ServerError::PayloadTooLarge(_))
        || matches!(error, ServerError::Inference(message) if message.starts_with("worker "))
}

/// Adapter composing the native ASR worker with an optional cleanup worker.
#[derive(Debug)]
pub struct WorkerInferenceEngine {
    asr: Arc<WorkerClient>,
    llm: Option<Arc<WorkerClient>>,
    model_cache_dir: PathBuf,
    backend: String,
    compute_backends: Vec<ComputeBackend>,
    print_transcripts: bool,
    state: Mutex<WorkerEngineState>,
}

#[derive(Debug, Default)]
struct WorkerEngineState {
    loaded_asr: Option<String>,
    loaded_llm: Option<String>,
    sessions: HashMap<Uuid, WorkerSession>,
}

#[derive(Debug, Default)]
struct WorkerSession {
    asr_started: bool,
    llm_started: bool,
}

#[derive(Deserialize)]
struct AsrResult {
    text: String,
    #[serde(default)]
    tokens: Vec<AsrToken>,
    #[serde(default)]
    segments: Vec<AsrSegment>,
}

#[derive(Deserialize)]
struct AsrToken {
    text: String,
    probability: f32,
}

#[derive(Deserialize)]
struct AsrSegment {
    start_ms: u64,
    end_ms: u64,
    #[serde(default)]
    tokens: Vec<AsrToken>,
}

impl WorkerInferenceEngine {
    /// Starts the ASR worker and, when configured, the cleanup worker.
    ///
    /// # Errors
    ///
    /// Returns an error when either worker cannot be started or does not answer
    /// its initial handshake.
    pub async fn spawn(
        asr_path: &Path,
        llm_path: Option<&Path>,
        model_cache_dir: PathBuf,
        backend: String,
        print_transcripts: bool,
    ) -> Result<Self, ServerError> {
        let asr =
            Arc::new(WorkerClient::spawn_checked(asr_path, Some("openflow-asr-worker")).await?);
        let llm = match llm_path {
            Some(path) => Some(Arc::new(
                WorkerClient::spawn_checked(path, Some("openflow-llm-worker")).await?,
            )),
            None => None,
        };
        let required_asr_backend = if backend == "mock" {
            "mock"
        } else {
            "whisper.cpp"
        };
        if !asr.supports(required_asr_backend) {
            return Err(ServerError::Configuration(format!(
                "ASR worker does not provide required backend {required_asr_backend}; available: {}",
                asr.capabilities.backends.join(", ")
            )));
        }
        if let Some(llm) = &llm {
            let required_llm_backend = if backend == "mock" {
                "mock"
            } else {
                "llama.cpp"
            };
            if !llm.supports(required_llm_backend) {
                return Err(ServerError::Configuration(format!(
                    "LLM worker does not provide required backend {required_llm_backend}; available: {}",
                    llm.capabilities.backends.join(", ")
                )));
            }
        }
        let mut compute_backends = decode_compute_backends(&asr.capabilities.compute_backends);
        if let Some(llm) = &llm {
            let llm_compute = decode_compute_backends(&llm.capabilities.compute_backends);
            compute_backends.retain(|backend| llm_compute.contains(backend));
        }
        if !compute_backends.contains(&ComputeBackend::Cpu) {
            compute_backends.push(ComputeBackend::Cpu);
        }
        Ok(Self {
            asr,
            llm,
            model_cache_dir,
            backend,
            compute_backends,
            print_transcripts,
            state: Mutex::new(WorkerEngineState::default()),
        })
    }

    async fn load_models(
        &self,
        state: &mut WorkerEngineState,
        asr_model: &str,
        llm_model: Option<&str>,
    ) -> Result<(), ServerError> {
        if state.loaded_asr.as_deref() != Some(asr_model) {
            if state.loaded_asr.is_some() {
                self.asr.call("unload_model", json!({})).await?;
                state.loaded_asr = None;
            }
            self.asr
                .call(
                    "load_model",
                    json!({
                        "backend": self.backend_for_asr(),
                        "model_path": self.model_path(asr_model),
                    }),
                )
                .await?;
            state.loaded_asr = Some(asr_model.into());
        }
        if let Some(llm) = &self.llm {
            match llm_model {
                Some(model) if state.loaded_llm.as_deref() != Some(model) => {
                    if state.loaded_llm.is_some() {
                        llm.call("unload_model", json!({})).await?;
                        state.loaded_llm = None;
                    }
                    llm.call(
                        "load_model",
                        json!({
                            "backend": self.backend_for_llm(),
                            "model_path": self.model_path(model),
                        }),
                    )
                    .await?;
                    state.loaded_llm = Some(model.into());
                }
                None if state.loaded_llm.is_some() => {
                    llm.call("unload_model", json!({})).await?;
                    state.loaded_llm = None;
                }
                Some(_) | None => {}
            }
        }
        Ok(())
    }

    fn model_path(&self, model_id: &str) -> String {
        self.model_cache_dir
            .join(model_id)
            .join("model.bin")
            .to_string_lossy()
            .into_owned()
    }

    fn backend_for_asr(&self) -> &str {
        match self.backend.as_str() {
            "llama.cpp" => "auto",
            value => value,
        }
    }

    fn backend_for_llm(&self) -> &str {
        match self.backend.as_str() {
            "whisper.cpp" => "auto",
            value => value,
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn transcribe_once(
        &self,
        request: &TranscriptionRequest,
        state: &mut WorkerEngineState,
    ) -> Result<TranscriptionResult, ServerError> {
        let asr_model = request
            .config
            .asr_model_id
            .as_deref()
            .or_else(|| (self.backend == "mock").then_some("mock"))
            .ok_or_else(|| {
                ServerError::BadRequest("an asr_model_id is required for native inference".into())
            })?;
        let llm_model = request.config.cleanup_model_id.as_deref();
        if request.final_segment && self.print_transcripts {
            print_audio_diagnostics(
                request.config.session_id,
                request.segment_id,
                &pcm_s16le_diagnostics(&request.audio),
            );
        }
        if llm_model.is_some() && self.llm.is_none() {
            return Err(ServerError::Configuration(
                "a cleanup model was selected, but no LLM worker is configured".into(),
            ));
        }
        self.load_models(state, asr_model, llm_model).await?;
        let worker_session = state.sessions.entry(request.config.session_id).or_default();
        if !worker_session.asr_started {
            let language = request.config.language.as_deref().unwrap_or("auto");
            self.asr
                .call(
                    "start_session",
                    json!({
                        "session_id": request.config.session_id,
                        "language": language,
                        "initial_prompt": request.config.glossary.join(", "),
                    }),
                )
                .await?;
            worker_session.asr_started = true;
        }

        // Native ASR calls are intentionally stateless decodes: every rolling
        // request contains the complete current segment so revisions are stable.
        let samples_s16le_base64 = pcm_s16le_base64(request.config.audio_encoding, &request.audio)?;
        let response = self
            .asr
            .call(
                "transcribe",
                json!({
                    "session_id": request.config.session_id,
                    "final": request.final_segment,
                    "samples_s16le_base64": samples_s16le_base64,
                }),
            )
            .await?;
        let asr: AsrResult = serde_json::from_value(response)?;
        if request.final_segment && self.print_transcripts {
            print_raw_transcript(request.config.session_id, request.segment_id, &asr.text);
        }
        let tokens: Vec<TokenEvidence> = if asr.segments.is_empty() {
            asr.tokens
                .into_iter()
                .map(|token| TokenEvidence {
                    text: token.text,
                    start_ms: 0,
                    end_ms: 0,
                    probability: token.probability.clamp(0.0, 1.0),
                })
                .collect()
        } else {
            asr.segments
                .into_iter()
                .flat_map(|segment| {
                    let token_count = segment.tokens.len().max(1) as u64;
                    let duration = segment.end_ms.saturating_sub(segment.start_ms);
                    segment
                        .tokens
                        .into_iter()
                        .enumerate()
                        .map(move |(index, token)| {
                            let index = index as u64;
                            TokenEvidence {
                                text: token.text,
                                start_ms: segment.start_ms + duration * index / token_count,
                                end_ms: segment.start_ms + duration * (index + 1) / token_count,
                                probability: token.probability.clamp(0.0, 1.0),
                            }
                        })
                })
                .collect()
        };

        let (formatted_text, edits) = if request.final_segment {
            if let (Some(llm), Some(_)) = (&self.llm, llm_model) {
                let cleanup: Result<Value, ServerError> = async {
                    if !worker_session.llm_started {
                        llm.call(
                            "start_session",
                            json!({"session_id": request.config.session_id}),
                        )
                        .await?;
                        worker_session.llm_started = true;
                    }
                    let mut cleanup_params = json!({
                        "session_id": request.config.session_id,
                        "text": asr.text,
                    });
                    let reconstructed: String =
                        tokens.iter().map(|token| token.text.as_str()).collect();
                    if reconstructed == asr.text {
                        cleanup_params.as_object_mut().expect("JSON object").insert(
                            "tokens".into(),
                            json!(
                                tokens
                                    .iter()
                                    .map(|token| json!({
                                        "text": token.text,
                                        "probability": token.probability,
                                    }))
                                    .collect::<Vec<_>>()
                            ),
                        );
                    } else {
                        // A backend with token text that does not exactly reconstruct
                        // the transcript cannot safely associate probability evidence
                        // with byte ranges. Omitting evidence makes lexical edits fail
                        // closed while still allowing independently safe formatting.
                        tracing::warn!(
                            session_id = %request.config.session_id,
                            "ASR token evidence did not reconstruct the transcript; lexical cleanup is disabled for this segment"
                        );
                    }
                    llm.call("cleanup", cleanup_params).await
                }
                .await;
                match cleanup {
                    Ok(value) => parse_cleanup(&value, &asr.text),
                    Err(error) if is_worker_transport_failure(&error) => return Err(error),
                    Err(error) => {
                        // Cleanup must never make an otherwise valid dictation
                        // unavailable. Raw ASR remains the safe fallback.
                        tracing::warn!(
                            session_id = %request.config.session_id,
                            %error,
                            "text cleanup failed; returning the raw transcript"
                        );
                        (asr.text.clone(), Vec::new())
                    }
                }
            } else {
                (asr.text.clone(), Vec::new())
            }
        } else {
            (asr.text.clone(), Vec::new())
        };

        if request.final_segment && self.print_transcripts {
            print_cleanup_result(
                request.config.session_id,
                request.segment_id,
                &asr.text,
                &formatted_text,
            );
        }

        if request.final_segment {
            let llm_started = worker_session.llm_started;
            if let Err(error) = self
                .asr
                .call(
                    "end_session",
                    json!({"session_id": request.config.session_id}),
                )
                .await
            {
                tracing::warn!(
                    session_id = %request.config.session_id,
                    %error,
                    "ASR session teardown failed after a completed transcript"
                );
                if is_worker_transport_failure(&error) {
                    state.loaded_asr = None;
                }
            }
            if llm_started
                && let Some(llm) = &self.llm
                && let Err(error) = llm
                    .call(
                        "end_session",
                        json!({"session_id": request.config.session_id}),
                    )
                    .await
            {
                tracing::warn!(
                    session_id = %request.config.session_id,
                    %error,
                    "LLM session teardown failed after a completed transcript"
                );
                if is_worker_transport_failure(&error) {
                    state.loaded_llm = None;
                }
            }
            state.sessions.remove(&request.config.session_id);
        }

        Ok(TranscriptionResult {
            raw_text: asr.text,
            formatted_text,
            tokens,
            edits,
        })
    }
}

#[async_trait]
impl InferenceEngine for WorkerInferenceEngine {
    fn compute_backends(&self) -> Vec<ComputeBackend> {
        self.compute_backends.clone()
    }

    async fn prepare_models(
        &self,
        asr_model_id: &str,
        cleanup_model_id: Option<&str>,
    ) -> Result<(), ServerError> {
        if cleanup_model_id.is_some() && self.llm.is_none() {
            return Err(ServerError::Configuration(
                "a cleanup model was selected, but no LLM worker is configured".into(),
            ));
        }
        let mut state = self.state.lock().await;
        self.load_models(&mut state, asr_model_id, cleanup_model_id)
            .await
    }

    async fn unload_model(&self, kind: ModelKind) -> Result<(), ServerError> {
        let mut state = self.state.lock().await;
        if !state.sessions.is_empty() {
            return Err(ServerError::Conflict(
                "models cannot be unloaded during an active dictation session".into(),
            ));
        }
        match kind {
            ModelKind::SpeechToText if state.loaded_asr.is_some() => {
                self.asr.call("unload_model", json!({})).await?;
                state.loaded_asr = None;
            }
            ModelKind::TextCleanup if state.loaded_llm.is_some() => {
                if let Some(llm) = &self.llm {
                    llm.call("unload_model", json!({})).await?;
                }
                state.loaded_llm = None;
            }
            ModelKind::SpeechToText | ModelKind::TextCleanup => {}
        }
        Ok(())
    }

    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResult, ServerError> {
        let mut state = self.state.lock().await;
        let first = self.transcribe_once(&request, &mut state).await;
        if first.as_ref().is_err_and(is_worker_transport_failure) {
            tracing::warn!(
                session_id = %request.config.session_id,
                "native worker transport failed; rebuilding worker state and retrying once"
            );
            *state = WorkerEngineState::default();
            return self.transcribe_once(&request, &mut state).await;
        }
        first
    }

    async fn cancel_session(&self, session_id: Uuid) {
        let mut state = self.state.lock().await;
        let Some(session) = state.sessions.remove(&session_id) else {
            return;
        };
        // These calls are best effort. The actor drains an in-flight decode
        // before them, preserving framing even when the request future was
        // cancelled because its WebSocket disappeared.
        if session.asr_started {
            let result = self
                .asr
                .call("end_session", json!({"session_id": session_id}))
                .await;
            if result.as_ref().is_err_and(is_worker_transport_failure) {
                state.loaded_asr = None;
            }
        }
        if session.llm_started
            && let Some(llm) = &self.llm
        {
            let result = llm
                .call("end_session", json!({"session_id": session_id}))
                .await;
            if result.as_ref().is_err_and(is_worker_transport_failure) {
                state.loaded_llm = None;
            }
        }
    }
}

fn decode_compute_backends(values: &[String]) -> Vec<ComputeBackend> {
    let mut decoded = Vec::new();
    for value in values {
        let backend = match value.as_str() {
            "cuda" => Some(ComputeBackend::Cuda),
            "rocm" => Some(ComputeBackend::Rocm),
            "metal" => Some(ComputeBackend::Metal),
            "vulkan" => Some(ComputeBackend::Vulkan),
            "cpu" => Some(ComputeBackend::Cpu),
            _ => None,
        };
        if let Some(backend) = backend
            && !decoded.contains(&backend)
        {
            decoded.push(backend);
        }
    }
    if decoded.is_empty() {
        decoded.push(ComputeBackend::Cpu);
    }
    decoded
}

fn is_worker_transport_failure(error: &ServerError) -> bool {
    matches!(error, ServerError::Inference(message) if message.starts_with(WORKER_TRANSPORT_PREFIX))
}

fn print_raw_transcript(session_id: Uuid, segment_id: u64, raw: &str) {
    eprintln!();
    eprintln!(
        "========== OpenFlow raw transcript · session {session_id} · segment {segment_id} =========="
    );
    eprintln!("{}", terminal_safe_transcript(raw));
    eprintln!("================================================================================");
    eprintln!();
}

fn print_audio_diagnostics(session_id: Uuid, segment_id: u64, audio: &PcmDiagnostics) {
    eprintln!(
        "OpenFlow audio · session {session_id} · segment {segment_id}: {} samples, {} ms, RMS {:.5}, peak {:.5}, {:.1}% nonzero",
        audio.sample_count, audio.duration_ms, audio.rms, audio.peak, audio.nonzero_percent,
    );
}

fn print_cleanup_result(session_id: Uuid, segment_id: u64, raw: &str, formatted: &str) {
    if formatted == raw {
        eprintln!(
            "OpenFlow cleanup · session {session_id} · segment {segment_id}: unchanged or unavailable"
        );
        return;
    }
    eprintln!();
    eprintln!(
        "========== OpenFlow cleaned transcript · session {session_id} · segment {segment_id} =========="
    );
    eprintln!("{}", terminal_safe_transcript(formatted));
    eprintln!(
        "===================================================================================="
    );
    eprintln!();
}

/// Removes terminal control characters from model text before foreground display.
#[doc(hidden)]
pub fn terminal_safe_transcript(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character == '\n' || character == '\t' || !character.is_control() {
                character
            } else {
                '\u{fffd}'
            }
        })
        .collect()
}

/// Summary of a final PCM segment printed by the foreground diagnostic host.
#[derive(Debug, Clone, Copy, PartialEq)]
#[doc(hidden)]
pub struct PcmDiagnostics {
    pub sample_count: usize,
    pub duration_ms: u64,
    pub rms: f64,
    pub peak: f64,
    pub nonzero_percent: f64,
}

/// Calculates signal diagnostics for 16 kHz mono PCM S16LE without retaining audio.
#[doc(hidden)]
pub fn pcm_s16le_diagnostics(bytes: &[u8]) -> PcmDiagnostics {
    let mut square_sum = 0.0;
    let mut peak = 0.0_f64;
    let mut sample_count = 0_usize;
    let mut nonzero_count_f64 = 0.0_f64;
    let mut sample_count_f64 = 0.0_f64;
    for bytes in bytes.chunks_exact(2) {
        let sample = f64::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0;
        square_sum += sample * sample;
        peak = peak.max(sample.abs());
        if sample != 0.0 {
            nonzero_count_f64 += 1.0;
        }
        sample_count += 1;
        sample_count_f64 += 1.0;
    }
    let rms = if sample_count == 0 {
        0.0
    } else {
        (square_sum / sample_count_f64).sqrt()
    };
    let nonzero_percent = if sample_count == 0 {
        0.0
    } else {
        nonzero_count_f64 * 100.0 / sample_count_f64
    };
    PcmDiagnostics {
        sample_count,
        duration_ms: u64::try_from(sample_count).unwrap_or(u64::MAX) * 1000 / 16_000,
        rms,
        peak,
        nonzero_percent,
    }
}

/// Validates and encodes PCM for the compact native-worker transport.
///
/// # Errors
///
/// Returns an error for unsupported encodings or incomplete S16LE samples.
#[doc(hidden)]
pub fn pcm_s16le_base64(encoding: AudioEncoding, bytes: &[u8]) -> Result<String, ServerError> {
    match encoding {
        AudioEncoding::PcmS16Le => {
            if bytes.len() & 1 != 0 {
                return Err(ServerError::BadRequest(
                    "PCM S16LE audio must contain complete samples".into(),
                ));
            }
            Ok(BASE64_STANDARD.encode(bytes))
        }
        AudioEncoding::Opus => Err(ServerError::BadRequest(
            "the native adapter currently requires PCM S16LE audio".into(),
        )),
    }
}

fn parse_cleanup(value: &Value, original: &str) -> (String, Vec<CleanupEdit>) {
    let text = value
        .get("formatted_text")
        .or_else(|| value.get("text"))
        .and_then(Value::as_str)
        .unwrap_or(original)
        .to_owned();
    let edits = value
        .get("decisions")
        .and_then(Value::as_array)
        .map(|decisions| {
            decisions
                .iter()
                .filter(|decision| {
                    decision
                        .get("accepted")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .filter_map(|decision| {
                    let edit = decision.get("edit")?;
                    let start_byte = usize::try_from(edit.get("start_byte")?.as_u64()?).ok()?;
                    let end_byte = usize::try_from(edit.get("end_byte")?.as_u64()?).ok()?;
                    let source_confidence = finite_f32(
                        decision
                            .get("source_confidence")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0),
                    )?;
                    Some(CleanupEdit {
                        start_byte,
                        end_byte,
                        replacement: edit.get("replacement")?.as_str()?.into(),
                        reason: decision
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("accepted")
                            .into(),
                        source_confidence,
                        score_delta_per_token: decision
                            .get("llm_advantage_nats_per_token")
                            .and_then(Value::as_f64)
                            .and_then(finite_f32),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (text, edits)
}

#[allow(clippy::cast_possible_truncation)]
fn finite_f32(value: f64) -> Option<f32> {
    (value.is_finite() && value >= f64::from(f32::MIN) && value <= f64::from(f32::MAX))
        .then_some(value as f32)
}
