use openflow_server::{
    error::ServerError,
    pairing::{PairingPrompt, TerminalPairingPrompt},
};

#[tokio::test]
async fn terminal_pairing_is_disabled_unless_explicitly_enabled() {
    let error = TerminalPairingPrompt::new(false)
        .confirm("Laptop", "123456")
        .await
        .unwrap_err();
    assert!(matches!(error, ServerError::ServiceUnavailable(_)));
}
