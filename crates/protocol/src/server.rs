use crate::{ActiveModel, ComputeBackend};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub protocol_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub os: String,
    pub architecture: String,
    pub logical_cpus: usize,
    pub system_memory_bytes: Option<u64>,
    pub accelerator_memory_bytes: Option<u64>,
    #[serde(default)]
    pub backends: Vec<ComputeBackend>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    pub protocol_version: u16,
    pub server_version: String,
    pub hardware: HardwareProfile,
    pub max_audio_bytes_per_session: usize,
    pub active_session: bool,
    #[serde(default)]
    pub active_models: Vec<ActiveModel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairDeviceRequest {
    pub pairing_code: String,
    pub device_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractivePairDeviceRequest {
    pub device_name: String,
    /// Six digits shown on both the requesting client and server terminal.
    pub verification_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairDeviceResponse {
    pub device_id: Uuid,
    /// Returned once. The server persists only its SHA-256 hash.
    pub device_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePairingCodeResponse {
    pub pairing_code: String,
    pub expires_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: Uuid,
    pub name: String,
    pub created_at_unix_seconds: u64,
}
