use crate::ModelKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioEncoding {
    PcmS16Le,
    Opus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub session_id: Uuid,
    pub audio_encoding: AudioEncoding,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub language: Option<String>,
    pub asr_model_id: Option<String>,
    pub cleanup_model_id: Option<String>,
    #[serde(default)]
    pub glossary: Vec<String>,
    #[serde(default)]
    pub options: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenEvidence {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Probability in the inclusive range 0..=1.
    pub probability: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartialTranscript {
    pub session_id: Uuid,
    pub segment_id: u64,
    pub revision: u64,
    /// Monotonically increasing for every event in a WebSocket session.
    pub sequence: u64,
    pub text: String,
    pub stable_prefix_bytes: usize,
    #[serde(default)]
    pub tokens: Vec<TokenEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CleanupEdit {
    pub start_byte: usize,
    pub end_byte: usize,
    pub replacement: String,
    pub reason: String,
    pub source_confidence: f32,
    pub score_delta_per_token: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentFinal {
    pub session_id: Uuid,
    pub segment_id: u64,
    pub revision: u64,
    pub sequence: u64,
    pub raw_text: String,
    pub formatted_text: String,
    #[serde(default)]
    pub tokens: Vec<TokenEvidence>,
    #[serde(default)]
    pub edits: Vec<CleanupEdit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionPatch {
    pub session_id: Uuid,
    pub segment_id: u64,
    pub base_revision: u64,
    pub revision: u64,
    pub sequence: u64,
    pub raw_text_sha256: String,
    pub replacement: String,
    #[serde(default)]
    pub edits: Vec<CleanupEdit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ClientMessage {
    Start(SessionConfig),
    Commit,
    Stop,
    Ping { nonce: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready { protocol_version: u16 },
    SessionStarted { session_id: Uuid },
    Partial(PartialTranscript),
    Final(SegmentFinal),
    Correction(CorrectionPatch),
    SessionStopped { session_id: Uuid },
    Pong { nonce: u64 },
    Error(ProtocolError),
}

/// Preferred name for messages sent from the server. Kept as an alias so the
/// direction is immediately clear to client implementations.
pub type ServerEvent = ServerMessage;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

/// A backend-neutral request used by the server's inference adapter.
#[derive(Debug, Clone)]
pub struct TranscriptionRequest {
    pub config: SessionConfig,
    pub segment_id: u64,
    pub audio: Vec<u8>,
    pub final_segment: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub raw_text: String,
    pub formatted_text: String,
    #[serde(default)]
    pub tokens: Vec<TokenEvidence>,
    #[serde(default)]
    pub edits: Vec<CleanupEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveModel {
    pub kind: ModelKind,
    pub model_id: String,
}
