use openflow_protocol::{
    AudioEncoding, ClientMessage, ComputeBackend, ModelInfo, ModelKind, ModelSpec, ModelState,
    SessionConfig,
};
use std::collections::BTreeMap;
use uuid::Uuid;

#[test]
fn client_message_round_trips() {
    let message = ClientMessage::Start(SessionConfig {
        session_id: Uuid::new_v4(),
        audio_encoding: AudioEncoding::PcmS16Le,
        sample_rate_hz: 16_000,
        channels: 1,
        language: Some("en".into()),
        asr_model_id: None,
        cleanup_model_id: None,
        glossary: vec!["OpenFlow".into()],
        options: BTreeMap::new(),
    });

    let encoded = serde_json::to_string(&message).unwrap();
    let decoded: ClientMessage = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, message);
}

#[test]
fn model_state_is_flattened_for_non_rust_clients() {
    let model = ModelInfo {
        spec: ModelSpec {
            id: "test-model".into(),
            display_name: "Test model".into(),
            kind: ModelKind::TextCleanup,
            family: "llama.cpp".into(),
            source_url: "https://example.invalid/model.gguf".into(),
            revision: "abc".into(),
            sha256: "0".repeat(64),
            size_bytes: 100,
            estimated_memory_bytes: 200,
            license: "Apache-2.0".into(),
            quantization: "Q4_K_M".into(),
            languages: vec!["multilingual".into()],
            backends: vec![ComputeBackend::Cpu],
            metadata: BTreeMap::new(),
        },
        state: ModelState::Downloading {
            downloaded_bytes: 25,
            total_bytes: 100,
        },
        active: false,
    };
    let encoded = serde_json::to_value(&model).unwrap();
    assert_eq!(encoded["state"], "downloading");
    assert_eq!(encoded["downloaded_bytes"], 25);
    assert_eq!(encoded["total_bytes"], 100);
    let decoded: ModelInfo = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, model);
}

#[test]
fn rocm_compute_backend_uses_the_stable_wire_name() {
    let encoded = serde_json::to_string(&ComputeBackend::Rocm).unwrap();
    assert_eq!(encoded, r#""rocm""#);
    assert_eq!(
        serde_json::from_str::<ComputeBackend>(&encoded).unwrap(),
        ComputeBackend::Rocm
    );
}
