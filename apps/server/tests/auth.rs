use openflow_server::{
    auth::{AuthManager, Principal},
    error::ServerError,
};
use std::time::Duration;

#[tokio::test]
async fn pairing_codes_are_single_use_and_tokens_authenticate() {
    let directory = tempfile::tempdir().unwrap();
    let auth = AuthManager::load(
        directory.path().join("auth.json"),
        Some("a".repeat(32)),
        false,
    )
    .await
    .unwrap();
    let (code, _) = auth
        .create_pairing_code(Duration::from_secs(60))
        .await
        .unwrap();
    let paired = auth.pair_device(&code, "Laptop").await.unwrap();
    assert!(matches!(
        auth.authenticate(&paired.device_token).await.unwrap(),
        Principal::Device(_)
    ));
    assert!(matches!(
        auth.pair_device(&code, "Other").await,
        Err(ServerError::Unauthorized)
    ));

    let file = tokio::fs::read_to_string(directory.path().join("auth.json"))
        .await
        .unwrap();
    assert!(!file.contains(&paired.device_token));
    assert!(!file.contains(&code));
}

#[tokio::test]
async fn generated_admin_token_survives_restart_without_storing_plaintext() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth.json");
    let first = AuthManager::load(path.clone(), None, false).await.unwrap();
    let token = first.take_bootstrap_admin_token().unwrap();
    assert!(matches!(
        first.authenticate(&token).await.unwrap(),
        Principal::Admin
    ));
    drop(first);

    let restarted = AuthManager::load(path.clone(), None, false).await.unwrap();
    assert!(restarted.take_bootstrap_admin_token().is_none());
    assert!(matches!(
        restarted.authenticate(&token).await.unwrap(),
        Principal::Admin
    ));
    let stored = tokio::fs::read_to_string(path).await.unwrap();
    assert!(!stored.contains(&token));
}

#[tokio::test]
async fn desktop_managed_admin_token_rotates_and_is_emitted_again() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("auth.json");
    let first = AuthManager::load(path.clone(), None, true).await.unwrap();
    let first_token = first.take_bootstrap_admin_token().unwrap();

    let restarted = AuthManager::load(path, None, true).await.unwrap();
    let next_token = restarted.take_bootstrap_admin_token().unwrap();
    assert_ne!(first_token, next_token);
    assert!(restarted.authenticate(&next_token).await.is_ok());
    assert!(restarted.authenticate(&first_token).await.is_err());
}
