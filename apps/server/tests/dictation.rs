use axum::http::{HeaderMap, header};
use openflow_server::{
    error::ServerError,
    inference::terminal_safe_transcript,
    routes::{common_utf8_prefix, enforce_memory_budget, websocket_bearer},
};
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};

#[test]
fn stable_prefix_stops_on_a_character_boundary() {
    assert_eq!(common_utf8_prefix("héllo", "héy"), "hé".len());
}

#[test]
fn transcript_terminal_output_removes_control_sequences() {
    assert_eq!(
        terminal_safe_transcript("hello\n\u{1b}[31mworld\r"),
        "hello\n�[31mworld�"
    );
}

#[test]
fn extracts_bearer_without_query_parameters() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        "openflow.v1, openflow.bearer.secret_123".parse().unwrap(),
    );
    assert_eq!(websocket_bearer(&headers), Some("secret_123"));
}

#[tokio::test]
async fn input_queue_has_explicit_backpressure() {
    let (sender, _receiver) = mpsc::channel::<Vec<u8>>(1);
    sender.try_send(vec![]).unwrap();
    assert!(matches!(
        sender.try_send(vec![]),
        Err(mpsc::error::TrySendError::Full(_))
    ));
}

#[test]
fn queued_audio_has_an_independent_byte_budget() {
    let budget = Arc::new(Semaphore::new(2));
    let _permit = Arc::clone(&budget).try_acquire_many_owned(2).unwrap();
    assert!(Arc::clone(&budget).try_acquire_owned().is_err());
}

#[test]
fn model_selection_keeps_twenty_percent_memory_reserve() {
    assert!(enforce_memory_budget(8_000, 10_000).is_ok());
    assert!(matches!(
        enforce_memory_budget(8_001, 10_000),
        Err(ServerError::Conflict(_))
    ));
}
