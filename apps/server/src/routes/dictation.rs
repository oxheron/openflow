use crate::{
    error::ServerError,
    inference::{CandidateRankingRequest, LanguageCandidate, NormalizationProposal},
    rolling_consensus::{
        ConsensusConfig, Hypothesis as ConsensusHypothesis, Pass as ConsensusPass,
        RollingConsensus, TimedWord,
    },
    state::{AppState, SessionLease},
};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, header},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use openflow_protocol::{
    AudioEncoding, CleanupEdit, ClientMessage, CorrectionPatch, ModelKind, PROTOCOL_VERSION,
    PartialTranscript, SegmentFinal, ServerMessage, SessionConfig, TokenEvidence,
    TranscriptionRequest, TranscriptionResult,
};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    time::timeout,
};
use uuid::Uuid;

const MAX_WEBSOCKET_MESSAGE_BYTES: usize = 64 * 1024;
// Audio bytes have their own strict semaphore budget below. This event count
// accommodates a complete byte-bounded session even when a WebView emits small
// PCM messages while a comparatively slow partial Whisper decode is running.
const INBOUND_QUEUE_CAPACITY: usize = 4_096;
const OUTBOUND_QUEUE_CAPACITY: usize = 32;
const MAX_QUEUED_AUDIO_BYTES: usize = 2 * 1024 * 1024;
const SESSION_CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Authenticates and upgrades a request to the dictation WebSocket protocol.
///
/// # Errors
///
/// Returns an authorization error when the bearer subprotocol is absent or
/// invalid.
pub async fn upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, ServerError> {
    let token = websocket_bearer(&headers).ok_or(ServerError::Unauthorized)?;
    // Authenticate before switching protocols and intentionally never attach the
    // credential to tracing spans or query strings.
    state.auth.authenticate(token).await?;
    Ok(ws
        .max_message_size(MAX_WEBSOCKET_MESSAGE_BYTES)
        // The credential-bearing offered protocol is never echoed back.
        .protocols(["openflow.v1"])
        .on_upgrade(move |socket| async move {
            run_socket(socket, state).await;
        }))
}

/// Extracts the credential-bearing OpenFlow WebSocket subprotocol.
#[doc(hidden)]
pub fn websocket_bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)?
        .to_str()
        .ok()?
        .split(',')
        .map(str::trim)
        .find_map(|protocol| protocol.strip_prefix("openflow.bearer."))
        .filter(|token| !token.is_empty() && token.len() <= 512)
}

#[derive(Debug)]
enum InboundMessage {
    Client(ClientMessage),
    Audio {
        bytes: Vec<u8>,
        _permit: OwnedSemaphorePermit,
    },
    Ping(Vec<u8>),
}

#[derive(Default)]
struct SocketSession {
    config: Option<SessionConfig>,
    audio: Vec<u8>,
    segment_id: u64,
    revision: u64,
    sequence: u64,
    bytes_at_last_partial: usize,
    previous_partial: String,
    consensus: Option<RollingConsensus>,
    lease: Option<SessionLease>,
}

#[allow(clippy::too_many_lines)]
async fn run_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (outbound, mut outbound_rx) = mpsc::channel::<Message>(OUTBOUND_QUEUE_CAPACITY);
    let (inbound, inbound_rx) = mpsc::channel(INBOUND_QUEUE_CAPACITY);
    let audio_budget = Arc::new(Semaphore::new(MAX_QUEUED_AUDIO_BYTES));
    let active_session = Arc::new(StdMutex::new(None));

    let writer = tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            if sink.send(message).await.is_err() {
                break;
            }
        }
    });
    if send(
        &outbound,
        &ServerMessage::Ready {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await
    .is_err()
    {
        drop(outbound);
        let _ = writer.await;
        return;
    }

    let processor_state = state.clone();
    let processor_outbound = outbound.clone();
    let processor_session = Arc::clone(&active_session);
    let mut processor = tokio::spawn(async move {
        process_messages(
            inbound_rx,
            processor_outbound,
            processor_state,
            processor_session,
        )
        .await;
    });
    let mut processor_finished = false;

    loop {
        tokio::select! {
            result = &mut processor => {
                if let Err(error) = result
                    && !error.is_cancelled()
                {
                    tracing::warn!(%error, "dictation message processor failed");
                }
                processor_finished = true;
                break;
            }
            message = stream.next() => {
                let Some(Ok(message)) = message else {
                    break;
                };
                let event = match message {
                    Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(message) => Some(InboundMessage::Client(message)),
                        Err(error) => {
                            if send_error(
                                &outbound,
                                ServerError::BadRequest(format!("invalid client message: {error}")),
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            None
                        }
                    },
                    Message::Binary(chunk) => {
                        let permit_count = u32::try_from(chunk.len())
                            .expect("WebSocket message limit is smaller than u32::MAX");
                        let Ok(permit) =
                            Arc::clone(&audio_budget).try_acquire_many_owned(permit_count)
                        else {
                            try_send_error(
                                &outbound,
                                &ServerError::Conflict(
                                    "queued audio exceeded 2 MiB; inference is not keeping up"
                                        .into(),
                                ),
                            );
                            break;
                        };
                        Some(InboundMessage::Audio {
                            bytes: chunk.clone(),
                            _permit: permit,
                        })
                    }
                    Message::Ping(payload) => Some(InboundMessage::Ping(payload.clone())),
                    Message::Close(_) => break,
                    Message::Pong(_) => None,
                };
                if let Some(event) = event {
                    match inbound.try_send(event) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            try_send_error(
                                &outbound,
                                &ServerError::Conflict(
                                    "dictation input queue is full; audio producer exceeded inference capacity"
                                        .into(),
                                ),
                            );
                            break;
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => break,
                    }
                }
            }
        }
    }

    drop(inbound);
    if !processor_finished {
        processor.abort();
        let _ = processor.await;
    }
    let session_id = active_session
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(session_id) = session_id {
        let cleanup = state.inference.cancel_session(session_id);
        if timeout(SESSION_CLEANUP_TIMEOUT, cleanup).await.is_err() {
            tracing::warn!(%session_id, "timed out waiting for disconnected inference session cleanup");
        }
    }
    drop(outbound);
    let _ = writer.await;
}

async fn process_messages(
    mut inbound: mpsc::Receiver<InboundMessage>,
    outbound: mpsc::Sender<Message>,
    state: AppState,
    active_session: Arc<StdMutex<Option<Uuid>>>,
) {
    let mut session = SocketSession::default();
    let mut pending = None;
    loop {
        let message = if let Some(message) = pending.take() {
            message
        } else {
            let Some(message) = inbound.recv().await else {
                break;
            };
            message
        };
        let outcome = match message {
            InboundMessage::Client(message) => {
                handle_command(&outbound, &state, &mut session, &active_session, message).await
            }
            InboundMessage::Audio {
                bytes,
                _permit: _audio_permit,
            } => {
                let (outcome, next) =
                    handle_audio_burst(&outbound, &state, &mut session, &mut inbound, &bytes).await;
                pending = next;
                outcome
            }
            InboundMessage::Ping(payload) => {
                outbound.send(Message::Pong(payload)).await.map_err(|_| ())
            }
        };
        if outcome.is_err() {
            break;
        }
    }
}

async fn handle_audio_burst(
    outbound: &mpsc::Sender<Message>,
    state: &AppState,
    session: &mut SocketSession,
    inbound: &mut mpsc::Receiver<InboundMessage>,
    first: &[u8],
) -> (Result<(), ()>, Option<InboundMessage>) {
    let mut outcome = append_audio(outbound, state, session, first).await;
    let mut pending = None;
    while outcome.is_ok() {
        match inbound.try_recv() {
            Ok(InboundMessage::Audio {
                bytes,
                _permit: _audio_permit,
            }) => outcome = append_audio(outbound, state, session, &bytes).await,
            Ok(message) => {
                pending = Some(message);
                break;
            }
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                break;
            }
        }
    }
    let finalization_is_next = matches!(
        pending.as_ref(),
        Some(InboundMessage::Client(
            ClientMessage::Commit | ClientMessage::Stop
        ))
    );
    if outcome.is_ok() && !finalization_is_next {
        outcome = maybe_transcribe_partial(outbound, state, session).await;
    }
    (outcome, pending)
}

#[allow(clippy::too_many_lines)]
async fn handle_command(
    outbound: &mpsc::Sender<Message>,
    state: &AppState,
    session: &mut SocketSession,
    active_session: &StdMutex<Option<Uuid>>,
    message: ClientMessage,
) -> Result<(), ()> {
    match message {
        ClientMessage::Start(config) => {
            if session.config.is_some() {
                return send_error(
                    outbound,
                    ServerError::Conflict("this socket already has an active session".into()),
                )
                .await;
            }
            if config.sample_rate_hz != 16_000 || config.channels != 1 {
                return send_error(
                    outbound,
                    ServerError::BadRequest(
                        "dictation audio must be 16 kHz and single-channel".into(),
                    ),
                )
                .await;
            }
            if config.audio_encoding != AudioEncoding::PcmS16Le {
                return send_error(
                    outbound,
                    ServerError::BadRequest(
                        "the native streaming protocol currently requires PCM S16LE audio".into(),
                    ),
                )
                .await;
            }
            let Some(lease) = state.sessions.try_acquire() else {
                return send_error(
                    outbound,
                    ServerError::Conflict("another dictation session is already active".into()),
                )
                .await;
            };
            if state.config.worker_backend != "mock" {
                let _lifecycle = state.model_lifecycle.lock().await;
                if let Err(error) = validate_requested_models(state, &config).await {
                    return send_error(outbound, error).await;
                }
                if let Some(asr_model_id) = config.asr_model_id.as_deref()
                    && let Err(error) = state
                        .inference
                        .prepare_models(asr_model_id, config.cleanup_model_id.as_deref())
                        .await
                {
                    return send_error(outbound, error).await;
                }
            }
            let session_id = config.session_id;
            let consensus = RollingConsensus::new(ConsensusConfig {
                history_size: 3,
                agreement_passes: state.config.consensus_passes,
                unstable_tail_ms: state.config.unstable_tail_ms,
                ..ConsensusConfig::default()
            })
            .map_err(|error| {
                tracing::error!(%error, "invalid rolling transcript consensus configuration");
            })
            .ok();
            if consensus.is_none() {
                return send_error(
                    outbound,
                    ServerError::Configuration(
                        "invalid rolling transcript consensus configuration".into(),
                    ),
                )
                .await;
            }
            session.config = Some(config);
            session.consensus = consensus;
            session.lease = Some(lease);
            *active_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id);
            send(outbound, &ServerMessage::SessionStarted { session_id }).await
        }
        ClientMessage::Commit => finalize_segment(outbound, state, session).await,
        ClientMessage::Stop => {
            if !session.audio.is_empty()
                && finalize_segment(outbound, state, session).await.is_err()
            {
                return Err(());
            }
            let Some(config) = session.config.take() else {
                return send_error(
                    outbound,
                    ServerError::BadRequest("session has not been started".into()),
                )
                .await;
            };
            if timeout(
                SESSION_CLEANUP_TIMEOUT,
                state.inference.cancel_session(config.session_id),
            )
            .await
            .is_err()
            {
                tracing::warn!(
                    session_id = %config.session_id,
                    "timed out waiting for inference session cleanup"
                );
            }
            session.lease.take();
            *active_session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
            send(
                outbound,
                &ServerMessage::SessionStopped {
                    session_id: config.session_id,
                },
            )
            .await
        }
        ClientMessage::Ping { nonce } => send(outbound, &ServerMessage::Pong { nonce }).await,
    }
}

async fn validate_requested_models(
    state: &AppState,
    config: &SessionConfig,
) -> Result<(), ServerError> {
    let asr_id = config.asr_model_id.as_deref().ok_or_else(|| {
        ServerError::BadRequest("an ASR model must be selected for native inference".into())
    })?;
    state.models.model_path(asr_id).await?;
    if let Some(cleanup_id) = config.cleanup_model_id.as_deref() {
        state.models.model_path(cleanup_id).await?;
    }

    let models = state.models.list().await;
    let asr = models
        .iter()
        .find(|model| model.spec.id == asr_id)
        .ok_or_else(|| ServerError::NotFound(format!("model {asr_id}")))?;
    if asr.spec.kind != ModelKind::SpeechToText {
        return Err(ServerError::BadRequest(format!(
            "model {asr_id} is not a speech-to-text model"
        )));
    }
    let mut required = asr.spec.estimated_memory_bytes;
    if let Some(cleanup_id) = config.cleanup_model_id.as_deref() {
        let cleanup = models
            .iter()
            .find(|model| model.spec.id == cleanup_id)
            .ok_or_else(|| ServerError::NotFound(format!("model {cleanup_id}")))?;
        if cleanup.spec.kind != ModelKind::TextCleanup {
            return Err(ServerError::BadRequest(format!(
                "model {cleanup_id} is not a text-cleanup model"
            )));
        }
        required = required.saturating_add(cleanup.spec.estimated_memory_bytes);
    }
    if let Some(available) = state
        .hardware
        .accelerator_memory_bytes
        .or(state.hardware.system_memory_bytes)
    {
        enforce_memory_budget(required, available)?;
    }
    Ok(())
}

/// Applies the server's twenty-percent model memory reserve.
#[doc(hidden)]
pub fn enforce_memory_budget(required: u64, available: u64) -> Result<(), ServerError> {
    let budget = available.saturating_mul(80) / 100;
    if required > budget {
        return Err(ServerError::Conflict(format!(
            "selected models require an estimated {required} bytes, exceeding the safe 80% memory budget of {budget} bytes"
        )));
    }
    Ok(())
}

async fn append_audio(
    outbound: &mpsc::Sender<Message>,
    state: &AppState,
    session: &mut SocketSession,
    chunk: &[u8],
) -> Result<(), ()> {
    if session.config.is_none() {
        return send_error(
            outbound,
            ServerError::BadRequest("start a session before sending audio".into()),
        )
        .await;
    }
    if session.audio.len().saturating_add(chunk.len()) > state.config.max_audio_bytes_per_session {
        return send_error(
            outbound,
            ServerError::PayloadTooLarge("session audio limit exceeded".into()),
        )
        .await;
    }
    session.audio.extend_from_slice(chunk);
    Ok(())
}

async fn maybe_transcribe_partial(
    outbound: &mpsc::Sender<Message>,
    state: &AppState,
    session: &mut SocketSession,
) -> Result<(), ()> {
    if session
        .audio
        .len()
        .saturating_sub(session.bytes_at_last_partial)
        >= state.config.partial_decode_bytes
    {
        session.bytes_at_last_partial = session.audio.len();
        transcribe_partial(outbound, state, session).await?;
    }
    Ok(())
}

fn pcm_duration_ms(byte_count: usize) -> u64 {
    (byte_count as u64 / 2).saturating_mul(1_000) / 16_000
}

#[doc(hidden)]
#[must_use]
pub fn rolling_audio_window(audio: &[u8], maximum_bytes: usize) -> (&[u8], u64, u64) {
    let end = audio.len() & !1;
    let retained = end.min(maximum_bytes & !1);
    let start = end.saturating_sub(retained);
    (
        &audio[start..end],
        pcm_duration_ms(start),
        pcm_duration_ms(end),
    )
}

fn finish_word(
    words: &mut Vec<TimedWord>,
    text: &mut String,
    start_ms: &mut u64,
    end_ms: &mut u64,
    probability_sum: &mut f32,
    probability_count: &mut usize,
) {
    if text.is_empty() {
        return;
    }
    words.push(TimedWord {
        text: std::mem::take(text),
        start_ms: *start_ms,
        end_ms: (*end_ms).max(*start_ms),
        probability: u16::try_from(*probability_count)
            .ok()
            .filter(|count| *count > 0)
            .map(|count| *probability_sum / f32::from(count)),
    });
    *probability_sum = 0.0;
    *probability_count = 0;
}

fn timed_words(
    transcript: &str,
    tokens: &[TokenEvidence],
    window_start_ms: u64,
    window_end_ms: u64,
) -> Vec<TimedWord> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut current_start = window_start_ms;
    let mut current_end = window_start_ms;
    let mut probability_sum = 0.0_f32;
    let mut probability_count = 0_usize;
    for token in tokens {
        let token_start = window_start_ms
            .saturating_add(token.start_ms)
            .min(window_end_ms);
        let token_end = window_start_ms
            .saturating_add(token.end_ms)
            .min(window_end_ms)
            .max(token_start);
        let mut probability_added = false;
        for character in token.text.chars() {
            if character.is_whitespace() {
                finish_word(
                    &mut words,
                    &mut current,
                    &mut current_start,
                    &mut current_end,
                    &mut probability_sum,
                    &mut probability_count,
                );
                probability_added = false;
                continue;
            }
            if current.is_empty() {
                current_start = token_start;
                current_end = token_end;
            } else {
                current_end = current_end.max(token_end);
            }
            current.push(character);
            if !probability_added {
                probability_sum += token.probability.clamp(0.0, 1.0);
                probability_count += 1;
                probability_added = true;
            }
        }
    }
    finish_word(
        &mut words,
        &mut current,
        &mut current_start,
        &mut current_end,
        &mut probability_sum,
        &mut probability_count,
    );
    if !words.is_empty() {
        return words;
    }

    let fallback = transcript.split_whitespace().collect::<Vec<_>>();
    let duration = window_end_ms.saturating_sub(window_start_ms);
    let count = fallback.len().max(1) as u64;
    fallback
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let index = index as u64;
            TimedWord {
                text: text.to_owned(),
                start_ms: window_start_ms + duration * index / count,
                end_ms: window_start_ms + duration * (index + 1) / count,
                probability: None,
            }
        })
        .collect()
}

fn consensus_pass(
    result: &TranscriptionResult,
    window_start_ms: u64,
    window_end_ms: u64,
) -> ConsensusPass {
    let hypotheses = if result.hypotheses.is_empty() {
        vec![ConsensusHypothesis {
            words: timed_words(
                &result.raw_text,
                &result.tokens,
                window_start_ms,
                window_end_ms,
            ),
            normalized_log_probability: None,
        }]
    } else {
        result
            .hypotheses
            .iter()
            .map(|hypothesis| ConsensusHypothesis {
                words: timed_words(
                    &hypothesis.text,
                    &hypothesis.tokens,
                    window_start_ms,
                    window_end_ms,
                ),
                normalized_log_probability: Some(hypothesis.mean_log_probability),
            })
            .collect()
    };
    ConsensusPass {
        window_start_ms,
        window_end_ms,
        hypotheses,
    }
}

fn trailing_context(text: &str, maximum_bytes: usize) -> String {
    let mut start = text.len().saturating_sub(maximum_bytes);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

#[doc(hidden)]
#[must_use]
pub fn language_left_context(text: &str, maximum_bytes: usize) -> String {
    let mut context = trailing_context(text, maximum_bytes);
    if !context.is_empty() && !context.chars().next_back().is_some_and(char::is_whitespace) {
        context.push(' ');
    }
    context
}

#[doc(hidden)]
#[must_use]
pub fn language_right_context(
    update: &crate::rolling_consensus::ConsensusUpdate,
    ambiguity: &crate::rolling_consensus::AmbiguousSpan,
    maximum_bytes: usize,
) -> String {
    let unstable = update.best_unstable_text.trim_start();
    let Some(mut context) = ambiguity
        .candidates
        .iter()
        .filter_map(|candidate| unstable.strip_prefix(&candidate.text))
        .min_by_key(|context| context.len())
    else {
        return String::new();
    };
    if context.len() > maximum_bytes {
        let mut end = maximum_bytes;
        while end > 0 && !context.is_char_boundary(end) {
            end -= 1;
        }
        context = &context[..end];
    }
    context.to_owned()
}

fn lexical_skeleton(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_character) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(left_index + 1);
        for (right_index, right_character) in right.iter().enumerate() {
            current.push(
                (previous[right_index + 1] + 1)
                    .min(current[right_index] + 1)
                    .min(previous[right_index] + usize::from(left_character != *right_character)),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

fn phonetic_skeleton(text: &str) -> String {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| match word.to_ascii_lowercase().as_str() {
            "are" => "r".to_owned(),
            "bee" | "be" => "b".to_owned(),
            "cue" | "queue" => "q".to_owned(),
            "dee" => "d".to_owned(),
            "eye" => "i".to_owned(),
            "ex" => "x".to_owned(),
            "gee" => "g".to_owned(),
            "jay" => "j".to_owned(),
            "oh" | "owe" => "o".to_owned(),
            "sea" | "see" => "c".to_owned(),
            "ess" => "s".to_owned(),
            "tea" | "tee" => "t".to_owned(),
            "you" => "u".to_owned(),
            "vee" => "v".to_owned(),
            "why" => "y".to_owned(),
            "zed" | "zee" => "z".to_owned(),
            other => other.to_owned(),
        })
        .collect()
}

fn known_surface_alias(source: &str, replacement: &str) -> bool {
    matches!(
        (lexical_skeleton(source).as_str(), replacement.trim()),
        ("pietorch" | "pytorch", "PyTorch")
            | ("gethub" | "github", "GitHub")
            | ("getlab" | "gitlab", "GitLab")
            | ("cplusplus" | "seeplusplus", "C++")
    )
}

fn glossary_supported_alias(source: &str, replacement: &str, glossary: &[String]) -> bool {
    let replacement_key = lexical_skeleton(replacement);
    if replacement_key.is_empty() || !glossary.iter().any(|entry| entry == replacement) {
        return false;
    }
    let source_key = phonetic_skeleton(source);
    let replacement_key = phonetic_skeleton(replacement);
    let key_length = source_key
        .chars()
        .count()
        .max(replacement_key.chars().count());
    if key_length < 6 {
        return source_key == replacement_key;
    }
    let maximum_distance = (key_length / 4).clamp(1, 3);
    edit_distance(&source_key, &replacement_key) <= maximum_distance
}

fn normalization_is_grounded(proposal: &NormalizationProposal, glossary: &[String]) -> bool {
    let same_skeleton =
        lexical_skeleton(&proposal.source) == lexical_skeleton(&proposal.replacement);
    match (proposal.kind.as_str(), proposal.grounding.as_str()) {
        ("formatting", "lexical_skeleton") => {
            if proposal.source.is_empty() {
                matches!(
                    proposal.replacement.as_str(),
                    "." | "," | "!" | "?" | ";" | ":" | "\n" | "…" | "。" | "！" | "？" | "、"
                )
            } else {
                same_skeleton
            }
        }
        ("word_boundary" | "orthographic_normalization", "lexical_skeleton") => same_skeleton,
        ("orthographic_normalization" | "canonical_name", "phonetic_equivalence")
        | ("canonical_name", "canonical_alias") => {
            known_surface_alias(&proposal.source, &proposal.replacement)
                || glossary_supported_alias(&proposal.source, &proposal.replacement, glossary)
        }
        ("spoken_symbol", "spoken_symbol") => {
            known_surface_alias(&proposal.source, &proposal.replacement)
        }
        _ => false,
    }
}

#[doc(hidden)]
#[must_use]
pub fn apply_normalizations(
    text: &str,
    proposals: &[NormalizationProposal],
    glossary: &[String],
) -> Option<String> {
    if proposals.len() > 8 {
        return None;
    }
    let mut proposals = proposals.to_vec();
    proposals.sort_by_key(|proposal| (proposal.start_byte, proposal.end_byte));
    let mut previous_end = 0;
    for proposal in &proposals {
        if proposal.start_byte > proposal.end_byte
            || proposal.end_byte > text.len()
            || proposal.start_byte < previous_end
            || !text.is_char_boundary(proposal.start_byte)
            || !text.is_char_boundary(proposal.end_byte)
            || text[proposal.start_byte..proposal.end_byte] != proposal.source
            || proposal.source.len() > 128
            || proposal.replacement.len() > 128
            || proposal.source.split_whitespace().count() > 4
            || proposal.replacement.split_whitespace().count() > 4
            || proposal
                .replacement
                .chars()
                .any(|character| character.is_control() && character != '\n' && character != '\t')
            || proposal.source == proposal.replacement
            || !normalization_is_grounded(proposal, glossary)
        {
            return None;
        }
        previous_end = proposal.end_byte;
    }
    let mut normalized = text.to_owned();
    for proposal in proposals.iter().rev() {
        normalized.replace_range(
            proposal.start_byte..proposal.end_byte,
            &proposal.replacement,
        );
    }
    Some(normalized)
}

async fn arbitrate_mature_ambiguity(
    state: &AppState,
    config: &SessionConfig,
    consensus: &mut RollingConsensus,
    update: crate::rolling_consensus::ConsensusUpdate,
) -> crate::rolling_consensus::ConsensusUpdate {
    let Some(ambiguity) = update.ambiguities.first() else {
        return update;
    };
    if config.cleanup_model_id.is_none() || ambiguity.candidates.len() < 2 {
        return update;
    }
    let right_context = language_right_context(&update, ambiguity, 1_024);
    let candidates = ambiguity
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| !candidate.text.trim().is_empty())
        .map(|(index, candidate)| LanguageCandidate {
            id: index.to_string(),
            text: candidate.text.clone(),
        })
        .collect::<Vec<_>>();
    if candidates.len() < 2 {
        return update;
    }
    let ranking = match state
        .inference
        .rank_candidates(CandidateRankingRequest {
            session_id: config.session_id,
            left_context: language_left_context(consensus.committed_text(), 1_024),
            right_context,
            candidates,
            propose_normalizations: false,
        })
        .await
    {
        Ok(Some(ranking)) => ranking,
        Ok(None) => return update,
        Err(error) => {
            tracing::warn!(%error, "language-model ambiguity ranking failed; retaining ASR hypotheses");
            return update;
        }
    };

    let language_min = ranking
        .rankings
        .iter()
        .map(|score| score.mean_log_probability)
        .fold(f64::INFINITY, f64::min);
    let language_max = ranking
        .rankings
        .iter()
        .map(|score| score.mean_log_probability)
        .fold(f64::NEG_INFINITY, f64::max);
    let language_range = (language_max - language_min).max(f64::EPSILON);
    let mut scored = ambiguity
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| !candidate.text.trim().is_empty())
        .map(|(index, candidate)| {
            let pass_support = f64::from(u32::try_from(candidate.pass_support).unwrap_or(u32::MAX));
            let hypothesis_support =
                f64::from(u32::try_from(candidate.hypothesis_support).unwrap_or(u32::MAX));
            let best_rank = f64::from(u32::try_from(candidate.best_rank).unwrap_or(u32::MAX));
            let acoustic = pass_support * 10.0 + hypothesis_support * 2.0 - best_rank * 0.5
                + candidate
                    .best_normalized_log_probability
                    .map_or(0.0, |score| f64::from(score.clamp(-5.0, 0.0)) * 0.1)
                + candidate
                    .best_mean_word_probability
                    .map_or(0.0, |probability| f64::from(probability) * 0.1);
            let language = ranking
                .rankings
                .iter()
                .find(|score| score.id == index.to_string())
                .map_or(0.0, |score| {
                    ((score.mean_log_probability - language_min) / language_range) * 0.75
                });
            (index, acoustic + language)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.1.total_cmp(&left.1));
    let Some(&(selected_index, selected_score)) = scored.first() else {
        return update;
    };
    if ambiguity.candidates[selected_index].pass_support < 2
        || scored
            .get(1)
            .is_some_and(|(_, runner_up)| selected_score - runner_up < 0.25)
    {
        return update;
    }

    let selected = &ambiguity.candidates[selected_index].text;
    // Keep the immutable committed prefix as the acoustic wording. Surface
    // normalizations are applied to the complete raw segment at the versioned
    // final/correction boundary, where their byte offsets remain auditable.
    match consensus.resolve_ambiguity(selected, selected) {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(%error, "language-model selection no longer matches rolling ASR state");
            update
        }
    }
}

async fn normalize_final_text(
    state: &AppState,
    config: &SessionConfig,
    raw_text: &str,
) -> (String, Vec<CleanupEdit>) {
    if config.cleanup_model_id.is_none() || raw_text.trim().is_empty() || raw_text.len() > 1_024 {
        return (raw_text.to_owned(), Vec::new());
    }
    let ranking = match state
        .inference
        .rank_candidates(CandidateRankingRequest {
            session_id: config.session_id,
            left_context: String::new(),
            right_context: String::new(),
            candidates: vec![LanguageCandidate {
                id: "final".into(),
                text: raw_text.to_owned(),
            }],
            propose_normalizations: true,
        })
        .await
    {
        Ok(Some(ranking)) => ranking,
        Ok(None) => return (raw_text.to_owned(), Vec::new()),
        Err(error) => {
            tracing::warn!(%error, "final transcript normalization failed; retaining ASR wording");
            return (raw_text.to_owned(), Vec::new());
        }
    };
    if ranking.normalization_candidate_id.as_deref() != Some("final") {
        return (raw_text.to_owned(), Vec::new());
    }
    let Some(normalized) =
        apply_normalizations(raw_text, &ranking.normalization_proposals, &config.glossary)
    else {
        tracing::warn!("language worker returned invalid final normalization proposals");
        return (raw_text.to_owned(), Vec::new());
    };
    let edits = ranking
        .normalization_proposals
        .into_iter()
        .map(|proposal| CleanupEdit {
            start_byte: proposal.start_byte,
            end_byte: proposal.end_byte,
            replacement: proposal.replacement,
            reason: format!("{}:{}", proposal.kind, proposal.grounding),
            source_confidence: 0.0,
            score_delta_per_token: None,
        })
        .collect();
    (normalized, edits)
}

async fn transcribe_partial(
    outbound: &mpsc::Sender<Message>,
    state: &AppState,
    session: &mut SocketSession,
) -> Result<(), ()> {
    let Some(config) = session.config.clone() else {
        return Ok(());
    };
    let (audio, window_start_ms, window_end_ms) = {
        let (audio, start_ms, end_ms) =
            rolling_audio_window(&session.audio, state.config.rolling_window_bytes);
        (audio.to_vec(), start_ms, end_ms)
    };
    let prompt_context = session
        .consensus
        .as_ref()
        .map(|consensus| trailing_context(consensus.committed_text(), 512))
        .filter(|context| !context.is_empty());
    let result = match state
        .inference
        .transcribe(TranscriptionRequest {
            config: config.clone(),
            segment_id: session.segment_id,
            audio,
            final_segment: false,
            prompt_context,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => return send_error(outbound, error).await,
    };
    let pass = consensus_pass(&result, window_start_ms, window_end_ms);
    let Some(consensus) = session.consensus.as_mut() else {
        return send_error(
            outbound,
            ServerError::Inference("rolling transcript consensus was not initialized".into()),
        )
        .await;
    };
    let update = match consensus.observe(pass) {
        Ok(update) => update,
        Err(error) => {
            return send_error(
                outbound,
                ServerError::Inference(format!("rolling transcript consensus failed: {error}")),
            )
            .await;
        }
    };
    let update = arbitrate_mature_ambiguity(state, &config, consensus, update).await;
    session.revision += 1;
    session.sequence += 1;
    let text = update.best_text();
    let stable_prefix_bytes = update.committed_text.len();
    let tokens = result
        .tokens
        .into_iter()
        .map(|mut token| {
            token.start_ms = token.start_ms.saturating_add(window_start_ms);
            token.end_ms = token.end_ms.saturating_add(window_start_ms);
            token
        })
        .collect();
    let partial = PartialTranscript {
        session_id: config.session_id,
        segment_id: session.segment_id,
        revision: session.revision,
        sequence: session.sequence,
        text: text.clone(),
        stable_prefix_bytes,
        tokens,
    };
    session.previous_partial = text;
    send(outbound, &ServerMessage::Partial(partial)).await
}

#[allow(clippy::too_many_lines)]
async fn finalize_segment(
    outbound: &mpsc::Sender<Message>,
    state: &AppState,
    session: &mut SocketSession,
) -> Result<(), ()> {
    let Some(config) = session.config.clone() else {
        return send_error(
            outbound,
            ServerError::BadRequest("session has not been started".into()),
        )
        .await;
    };
    if session.audio.is_empty() {
        return Ok(());
    }
    let audio = std::mem::take(&mut session.audio);
    let (decode_audio, window_start_ms, window_end_ms) = {
        let (window, start_ms, end_ms) =
            rolling_audio_window(&audio, state.config.rolling_window_bytes);
        (window.to_vec(), start_ms, end_ms)
    };
    let prompt_context = session
        .consensus
        .as_ref()
        .map(|consensus| trailing_context(consensus.committed_text(), 512))
        .filter(|context| !context.is_empty());
    let result = match state
        .inference
        .transcribe(TranscriptionRequest {
            config: config.clone(),
            segment_id: session.segment_id,
            audio: decode_audio,
            final_segment: true,
            prompt_context,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => {
            // Keep the segment available for a retryable worker failure. The
            // bounded per-session limit makes this single defensive copy small.
            session.audio = audio;
            return send_error(outbound, error).await;
        }
    };
    let pass = consensus_pass(&result, window_start_ms, window_end_ms);
    let Some(consensus) = session.consensus.as_mut() else {
        session.audio = audio;
        return send_error(
            outbound,
            ServerError::Inference("rolling transcript consensus was not initialized".into()),
        )
        .await;
    };
    match consensus.observe(pass) {
        Ok(update) => {
            let _ = arbitrate_mature_ambiguity(state, &config, consensus, update).await;
        }
        Err(error) => {
            // A commit immediately following an unchanged partial can have the
            // same live edge. Existing consensus remains safe to finalize.
            tracing::warn!(%error, "final ASR pass did not advance rolling consensus");
        }
    }
    let final_update = consensus.finalize();
    let raw_text = if final_update.committed_text.is_empty() {
        result.raw_text.clone()
    } else {
        final_update.committed_text
    };
    let (formatted_text, edits) = normalize_final_text(state, &config, &raw_text).await;
    let tokens = result
        .tokens
        .into_iter()
        .map(|mut token| {
            token.start_ms = token.start_ms.saturating_add(window_start_ms);
            token.end_ms = token.end_ms.saturating_add(window_start_ms);
            token
        })
        .collect();
    session.revision += 1;
    session.sequence += 1;
    let final_revision = session.revision;
    send(
        outbound,
        &ServerMessage::Final(SegmentFinal {
            session_id: config.session_id,
            segment_id: session.segment_id,
            revision: final_revision,
            sequence: session.sequence,
            raw_text: raw_text.clone(),
            formatted_text: formatted_text.clone(),
            tokens,
            edits: edits.clone(),
        }),
    )
    .await?;
    if formatted_text != raw_text {
        session.revision += 1;
        session.sequence += 1;
        send(
            outbound,
            &ServerMessage::Correction(CorrectionPatch {
                session_id: config.session_id,
                segment_id: session.segment_id,
                base_revision: final_revision,
                revision: session.revision,
                sequence: session.sequence,
                raw_text_sha256: hex::encode(Sha256::digest(raw_text.as_bytes())),
                replacement: formatted_text,
                edits,
            }),
        )
        .await?;
    }
    session.segment_id += 1;
    session.bytes_at_last_partial = 0;
    session.previous_partial.clear();
    consensus.reset();
    Ok(())
}

async fn send(outbound: &mpsc::Sender<Message>, message: &ServerMessage) -> Result<(), ()> {
    let encoded = serde_json::to_string(message).map_err(|_| ())?;
    outbound.send(Message::Text(encoded)).await.map_err(|_| ())
}

async fn send_error(outbound: &mpsc::Sender<Message>, error: ServerError) -> Result<(), ()> {
    send(outbound, &ServerMessage::Error(error.protocol_error())).await
}

fn try_send_error(outbound: &mpsc::Sender<Message>, error: &ServerError) {
    if let Ok(encoded) = serde_json::to_string(&ServerMessage::Error(error.protocol_error())) {
        let _ = outbound.try_send(Message::Text(encoded));
    }
}

/// Returns the common byte prefix without splitting a UTF-8 code point.
#[doc(hidden)]
#[must_use]
pub fn common_utf8_prefix(left: &str, right: &str) -> usize {
    let mut count = left
        .as_bytes()
        .iter()
        .zip(right.as_bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while count > 0 && !right.is_char_boundary(count) {
        count -= 1;
    }
    count
}
