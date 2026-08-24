use openflow_desktop::local_server::{parse_bootstrap_token, parse_health_response};

#[test]
fn accepts_only_the_expected_bootstrap_envelope() {
    let token = "a".repeat(32);
    assert_eq!(
        parse_bootstrap_token(&format!(
            r#"{{"event":"bootstrap","admin_token":"{token}"}}"#
        )),
        Some(token)
    );
    assert!(parse_bootstrap_token(r#"{"event":"log","admin_token":"not-a-secret"}"#).is_none());
}

#[test]
fn accepts_only_a_compatible_openflow_health_response() {
    assert!(parse_health_response(
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"status\":\"ok\",\"version\":\"0.1.0\",\"protocol_version\":1}"
    ));
    assert!(!parse_health_response(
        b"HTTP/1.1 200 OK\r\n\r\n{\"status\":\"ok\",\"protocol_version\":2}"
    ));
    assert!(!parse_health_response(b"HTTP/1.1 404 Not Found\r\n\r\n{}"));
}
