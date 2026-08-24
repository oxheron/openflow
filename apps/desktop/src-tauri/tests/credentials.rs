use openflow_desktop::credentials::{endpoint_account, valid_token};

#[test]
fn endpoint_accounts_allow_tls_and_loopback_only() {
    assert_eq!(
        endpoint_account("http://127.0.0.1:8765/").unwrap(),
        "server:http://127.0.0.1:8765"
    );
    assert_eq!(
        endpoint_account("https://host.example/base/").unwrap(),
        "server:https://host.example/base"
    );
    assert!(endpoint_account("http://host.example:8765").is_err());
    assert!(endpoint_account("https://user:secret@host.example").is_err());
}

#[test]
fn validates_websocket_safe_tokens() {
    assert!(valid_token("device_token_1234567890"));
    assert!(!valid_token("too-short"));
    assert!(!valid_token("not safe because spaces are present"));
}
