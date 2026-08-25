use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use openflow_protocol::{
    CreatePairingCodeResponse, InteractivePairDeviceRequest, PairDeviceRequest, PairDeviceResponse,
};
use openflow_server::{
    UnavailableInferenceEngine,
    config::ServerConfig,
    error::ServerError,
    pairing::PairingPrompt,
    routes::router,
    state::{AppState, SessionLease},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tower::ServiceExt;

#[derive(Debug)]
struct FixedPairingPrompt(bool);

#[async_trait]
impl PairingPrompt for FixedPairingPrompt {
    async fn confirm(
        &self,
        _device_name: &str,
        _verification_code: &str,
    ) -> Result<bool, ServerError> {
        Ok(self.0)
    }
}

async fn test_app() -> (Router, AppState, String, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let config = Arc::new(ServerConfig {
        bind_address: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        tls: None,
        model_registry_path: None,
        model_cache_dir: directory.path().join("models"),
        auth_store_path: directory.path().join("auth.json"),
        admin_token: None,
        rotate_bootstrap_admin_token: false,
        pairing_ttl: Duration::from_secs(60),
        max_audio_bytes_per_session: 1_920_000,
        partial_decode_bytes: 16_000,
        rolling_window_bytes: 800_000,
        unstable_tail_ms: 6_000,
        consensus_passes: 3,
        asr_worker_path: None,
        llm_worker_path: None,
        worker_backend: "mock".into(),
        interactive_pairing: false,
        print_transcripts: false,
    });
    let state = AppState::new(config, Arc::new(UnavailableInferenceEngine))
        .await
        .unwrap();
    let token = state.auth.take_bootstrap_admin_token().unwrap();
    (router(state.clone()), state, token, directory)
}

#[tokio::test]
async fn health_is_public_but_capabilities_require_authentication() {
    let (app, _state, token, _directory) = test_app().await;
    let response = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::get("/v1/capabilities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .oneshot(
            Request::get("/v1/capabilities")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_can_issue_a_code_that_enrolls_one_device() {
    let (app, _state, token, _directory) = test_app().await;
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/pairing-codes")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let pairing: CreatePairingCodeResponse = serde_json::from_slice(&body).unwrap();

    let request = PairDeviceRequest {
        pairing_code: pairing.pairing_code,
        device_name: "Test laptop".into(),
    };
    let response = app
        .oneshot(
            Request::post("/v1/pair")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let paired: PairDeviceResponse = serde_json::from_slice(&body).unwrap();
    assert!(paired.device_token.len() >= 32);
}

#[tokio::test]
async fn foreground_approval_enrolls_a_persistent_device() {
    let (_app, state, _token, _directory) = test_app().await;
    let mut interactive_state = state.clone();
    interactive_state.pairing_prompt = Arc::new(FixedPairingPrompt(true));
    let app = router(interactive_state);
    let request = InteractivePairDeviceRequest {
        device_name: "MacBook Pro".into(),
        verification_code: "041923".into(),
    };
    let response = app
        .oneshot(
            Request::post("/v1/pair/interactive")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let paired: PairDeviceResponse = serde_json::from_slice(&body).unwrap();
    let principal = state.auth.authenticate(&paired.device_token).await.unwrap();
    assert!(matches!(
        principal,
        openflow_server::auth::Principal::Device(_)
    ));
}

#[tokio::test]
async fn foreground_denial_and_malformed_requests_fail_closed() {
    let (_app, state, _token, _directory) = test_app().await;
    let mut interactive_state = state;
    interactive_state.pairing_prompt = Arc::new(FixedPairingPrompt(false));
    let app = router(interactive_state);

    let denied = InteractivePairDeviceRequest {
        device_name: "Unknown laptop".into(),
        verification_code: "123456".into(),
    };
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/pair/interactive")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&denied).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let invalid = InteractivePairDeviceRequest {
        device_name: "terminal\nspoof".into(),
        verification_code: "12345x".into(),
    };
    let response = app
        .oneshot(
            Request::post("/v1/pair/interactive")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&invalid).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn model_lifecycle_mutations_are_rejected_during_dictation() {
    let (app, state, token, _directory) = test_app().await;
    let _lease: SessionLease = state.sessions.try_acquire().unwrap();
    for (method, uri, body) in [
        (
            "POST",
            "/v1/models/activate",
            Body::from(r#"{"model_id":"qwen3-1.7b-q8_0"}"#),
        ),
        (
            "POST",
            "/v1/models/deactivate",
            Body::from(r#"{"model_id":"qwen3-1.7b-q8_0"}"#),
        ),
        ("DELETE", "/v1/models/qwen3-1.7b-q8_0", Body::empty()),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{method} {uri}");
    }
}
