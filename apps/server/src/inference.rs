use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use openflow_protocol::{
    AudioEncoding, ComputeBackend, ModelKind, TokenEvidence, TranscriptHypothesis,
    TranscriptionRequest, TranscriptionResult,
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

    /// Ranks a finite set of ASR-supported alternatives with the optional
    /// language model. Implementations without a language model return `None`.
    async fn rank_candidates(
        &self,
        _request: CandidateRankingRequest,
    ) -> Result<Option<CandidateRankingResult>, ServerError> {
        Ok(None)
    }

    /// Releases backend state belonging to a session that ended without a
    /// final segment (for example, because its WebSocket disconnected).
    async fn cancel_session(&self, _session_id: Uuid) {}
}

#[derive(Clone, Debug)]
pub struct CandidateRankingRequest {
    pub session_id: Uuid,
    pub left_context: String,
    pub right_context: String,
    pub candidates: Vec<LanguageCandidate>,
    pub propose_normalizations: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct LanguageCandidate {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LanguageCandidateScore {
    pub id: String,
    pub log_probability: f64,
    pub token_count: usize,
    pub mean_log_probability: f64,
    pub candidate_log_probability: f64,
    pub right_context_log_probability_delta: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct NormalizationProposal {
    pub start_byte: usize,
    pub end_byte: usize,
    pub source: String,
    pub replacement: String,
    pub kind: String,
    pub grounding: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CandidateNormalization {
    candidate_id: String,
    #[serde(default)]
    proposals: Vec<NormalizationProposal>,
}

#[derive(Clone, Debug, Deserialize)]
struct WorkerCandidateRankingResult {
    rankings: Vec<LanguageCandidateScore>,
    normalization: Option<CandidateNormalization>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CandidateRankingResult {
    pub rankings: Vec<LanguageCandidateScore>,
    pub normalization_candidate_id: Option<String>,
    pub normalization_proposals: Vec<NormalizationProposal>,
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
    #[serde(default)]
    hypotheses: Vec<AsrHypothesis>,
}

#[derive(Deserialize)]
struct AsrHypothesis {
    text: String,
    #[serde(default)]
    score: f32,
    #[serde(default)]
    mean_log_probability: f32,
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

fn asr_token_evidence(flat_tokens: Vec<AsrToken>, segments: Vec<AsrSegment>) -> Vec<TokenEvidence> {
    if segments.is_empty() {
        return flat_tokens
            .into_iter()
            .map(|token| TokenEvidence {
                text: token.text,
                start_ms: 0,
                end_ms: 0,
                probability: token.probability.clamp(0.0, 1.0),
            })
            .collect();
    }
    segments
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

    async fn rank_candidates_once(
        &self,
        request: &CandidateRankingRequest,
        state: &mut WorkerEngineState,
    ) -> Result<Option<CandidateRankingResult>, ServerError> {
        let Some(llm) = &self.llm else {
            return Ok(None);
        };
        if state.loaded_llm.is_none() {
            return Ok(None);
        }
        let worker_session = state.sessions.entry(request.session_id).or_default();
        if !worker_session.llm_started {
            llm.call("start_session", json!({"session_id": request.session_id}))
                .await?;
            worker_session.llm_started = true;
        }
        let response = llm
            .call(
                "rank_candidates",
                json!({
                    "session_id": request.session_id,
                    "left_context": request.left_context,
                    "right_context": request.right_context,
                    "candidates": request.candidates,
                    "propose_normalizations": request.propose_normalizations,
                }),
            )
            .await?;
        let result: WorkerCandidateRankingResult = serde_json::from_value(response)?;
        let (normalization_candidate_id, normalization_proposals) =
            result.normalization.map_or_else(
                || (None, Vec::new()),
                |normalization| (Some(normalization.candidate_id), normalization.proposals),
            );
        Ok(Some(CandidateRankingResult {
            rankings: result.rankings,
            normalization_candidate_id,
            normalization_proposals,
        }))
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

        // Native ASR calls are intentionally stateless decodes. The route sends
        // the complete bounded rolling window plus a short committed prompt.
        let samples_s16le_base64 = pcm_s16le_base64(request.config.audio_encoding, &request.audio)?;
        let response = self
            .asr
            .call(
                "transcribe",
                json!({
                    "session_id": request.config.session_id,
                    "final": request.final_segment,
                    "samples_s16le_base64": samples_s16le_base64,
                    "initial_prompt": asr_prompt(request),
                }),
            )
            .await?;
        let asr: AsrResult = serde_json::from_value(response)?;
        if request.final_segment && self.print_transcripts {
            print_raw_transcript(request.config.session_id, request.segment_id, &asr.text);
        }
        let tokens = asr_token_evidence(asr.tokens, asr.segments);
        let hypotheses = asr
            .hypotheses
            .into_iter()
            .map(|hypothesis| TranscriptHypothesis {
                text: hypothesis.text,
                score: hypothesis.score,
                mean_log_probability: hypothesis.mean_log_probability,
                tokens: asr_token_evidence(hypothesis.tokens, hypothesis.segments),
            })
            .collect();

        // Freeform transcript cleanup is intentionally not performed here.
        // The route layer may ask the LLM to rank only ASR-supported
        // alternatives and to return exact-span surface normalizations.
        let formatted_text = asr.text.clone();
        let edits = Vec::new();

        if request.final_segment && self.print_transcripts {
            print_cleanup_result(
                request.config.session_id,
                request.segment_id,
                &asr.text,
                &formatted_text,
            );
        }

        Ok(TranscriptionResult {
            raw_text: asr.text,
            formatted_text,
            tokens,
            edits,
            hypotheses,
        })
    }
}

fn asr_prompt(request: &TranscriptionRequest) -> String {
    let mut parts = Vec::new();
    if let Some(context) = request
        .prompt_context
        .as_deref()
        .map(str::trim)
        .filter(|context| !context.is_empty())
    {
        parts.push(context);
    }
    parts.extend(
        request
            .config
            .glossary
            .iter()
            .map(String::as_str)
            .filter(|entry| !entry.trim().is_empty()),
    );
    parts.join(", ")
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

    async fn rank_candidates(
        &self,
        request: CandidateRankingRequest,
    ) -> Result<Option<CandidateRankingResult>, ServerError> {
        let mut state = self.state.lock().await;
        let result = self.rank_candidates_once(&request, &mut state).await;
        if result.as_ref().is_err_and(is_worker_transport_failure) {
            // The next transcription call reloads the configured model pair.
            // Candidate ranking is optional and must not manufacture a result
            // after its language worker has lost state.
            state.loaded_llm = None;
            state.sessions.remove(&request.session_id);
        }
        result
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
    for bytes in bytes.as_chunks::<2>().0 {
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
