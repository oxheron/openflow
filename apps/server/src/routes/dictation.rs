use crate::{
    error::ServerError,
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
    AudioEncoding, ClientMessage, CorrectionPatch, ModelKind, PROTOCOL_VERSION, PartialTranscript,
    SegmentFinal, ServerMessage, SessionConfig, TranscriptionRequest,
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
            session.config = Some(config);
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

async fn transcribe_partial(
    outbound: &mpsc::Sender<Message>,
    state: &AppState,
    session: &mut SocketSession,
) -> Result<(), ()> {
    let Some(config) = session.config.clone() else {
        return Ok(());
    };
    let result = match state
        .inference
        .transcribe(TranscriptionRequest {
            config: config.clone(),
            segment_id: session.segment_id,
            audio: session.audio.clone(),
            final_segment: false,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => return send_error(outbound, error).await,
    };
    session.revision += 1;
    session.sequence += 1;
    let stable_prefix_bytes = common_utf8_prefix(&session.previous_partial, &result.raw_text);
    let partial = PartialTranscript {
        session_id: config.session_id,
        segment_id: session.segment_id,
        revision: session.revision,
        sequence: session.sequence,
        text: result.raw_text.clone(),
        stable_prefix_bytes,
        tokens: result.tokens,
    };
    session.previous_partial = result.raw_text;
    send(outbound, &ServerMessage::Partial(partial)).await
}

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
    let result = match state
        .inference
        .transcribe(TranscriptionRequest {
            config: config.clone(),
            segment_id: session.segment_id,
            audio: audio.clone(),
            final_segment: true,
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
            raw_text: result.raw_text.clone(),
            formatted_text: result.formatted_text.clone(),
            tokens: result.tokens,
            edits: result.edits.clone(),
        }),
    )
    .await?;
    if result.formatted_text != result.raw_text {
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
                raw_text_sha256: hex::encode(Sha256::digest(result.raw_text.as_bytes())),
                replacement: result.formatted_text,
                edits: result.edits,
            }),
        )
        .await?;
    }
    session.segment_id += 1;
    session.bytes_at_last_partial = 0;
    session.previous_partial.clear();
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
