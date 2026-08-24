use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    SpeechToText,
    TextCleanup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeBackend {
    Cpu,
    Cuda,
    Rocm,
    Metal,
    Vulkan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Stable registry identifier, such as `whisper-large-v3-turbo-q8_0`.
    pub id: String,
    pub display_name: String,
    pub kind: ModelKind,
    pub family: String,
    pub source_url: String,
    /// Immutable upstream revision or commit.
    pub revision: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub estimated_memory_bytes: u64,
    pub license: String,
    pub quantization: String,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub backends: Vec<ComputeBackend>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModelState {
    NotDownloaded,
    Downloading {
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Cancelling,
    Verifying,
    Ready,
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    #[serde(flatten)]
    pub spec: ModelSpec,
    #[serde(flatten)]
    pub state: ModelState,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRegistry {
    pub schema_version: u16,
    pub generated_at: String,
    pub models: Vec<ModelSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadModelRequest {
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelModelDownloadRequest {
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivateModelRequest {
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeactivateModelRequest {
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMutationResponse {
    pub model_id: String,
    pub accepted: bool,
}
