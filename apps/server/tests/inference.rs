use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use openflow_protocol::{
    AudioEncoding, ComputeBackend, ModelKind, SessionConfig, TranscriptionRequest,
};
use openflow_server::{
    InferenceEngine, WorkerClient, WorkerInferenceEngine,
    inference::{pcm_s16le_base64, pcm_s16le_diagnostics},
};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

#[test]
fn encodes_complete_pcm_samples_compactly() {
    let encoded = pcm_s16le_base64(AudioEncoding::PcmS16Le, &[0, 0, 0xff, 0x7f, 0, 0x80]).unwrap();
    assert_eq!(
        BASE64_STANDARD.decode(encoded).unwrap(),
        [0, 0, 0xff, 0x7f, 0, 0x80]
    );
    assert!(pcm_s16le_base64(AudioEncoding::PcmS16Le, &[0]).is_err());
}

#[test]
fn summarizes_pcm_signal_without_retaining_audio() {
    let audio = [0_i16, 16_384, -16_384, i16::MAX]
        .into_iter()
        .flat_map(i16::to_le_bytes)
        .collect::<Vec<_>>();
    let diagnostics = pcm_s16le_diagnostics(&audio);
    assert_eq!(diagnostics.sample_count, 4);
    assert_eq!(diagnostics.duration_ms, 0);
    assert!((diagnostics.rms - 0.61236).abs() < 0.0001);
    assert!((diagnostics.peak - 0.99997).abs() < 0.0001);
    assert!((diagnostics.nonzero_percent - 75.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn framed_worker_handshake_and_restart() {
    let Some(path) = std::env::var_os("OPENFLOW_TEST_ASR_WORKER").map(PathBuf::from) else {
        return;
    };
    let client = WorkerClient::spawn(&path).await.unwrap();
    let (worker, _version, backends) = client.capability_summary();
    assert_eq!(worker, "openflow-asr-worker");
    assert!(backends.iter().any(|backend| backend == "mock"));

    client.call("shutdown", json!({})).await.unwrap();
    let error = client.call("ping", json!({})).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("native worker transport failure")
    );
    let restarted = client.call("ping", json!({})).await.unwrap();
    assert_eq!(restarted["worker"], "openflow-asr-worker");
}

#[tokio::test]
async fn actual_workers_complete_a_mock_session() {
    let Some(asr_path) = std::env::var_os("OPENFLOW_TEST_ASR_WORKER").map(PathBuf::from) else {
        return;
    };
    let Some(llm_path) = std::env::var_os("OPENFLOW_TEST_LLM_WORKER").map(PathBuf::from) else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let engine = WorkerInferenceEngine::spawn(
        &asr_path,
        Some(&llm_path),
        directory.path().to_path_buf(),
        "mock".into(),
        false,
    )
    .await
    .unwrap();
    assert!(engine.compute_backends().contains(&ComputeBackend::Cpu));
    engine.prepare_models("mock", Some("mock")).await.unwrap();
    let session_id = Uuid::new_v4();
    let config = SessionConfig {
        session_id,
        audio_encoding: AudioEncoding::PcmS16Le,
        sample_rate_hz: 16_000,
        channels: 1,
        language: None,
        asr_model_id: None,
        cleanup_model_id: Some("mock".into()),
        glossary: Vec::new(),
        options: BTreeMap::new(),
    };
    let partial = engine
        .transcribe(TranscriptionRequest {
            config: config.clone(),
            segment_id: 0,
            audio: vec![0; 640],
            final_segment: false,
        })
        .await
        .unwrap();
    assert!(partial.raw_text.is_empty());
    let final_result = engine
        .transcribe(TranscriptionRequest {
            config,
            segment_id: 0,
            audio: vec![0; 1_280],
            final_segment: true,
        })
        .await
        .unwrap();
    assert_eq!(final_result.raw_text, final_result.formatted_text);
    engine.unload_model(ModelKind::TextCleanup).await.unwrap();
    engine.prepare_models("mock", None).await.unwrap();
}
