use openflow_protocol::{ModelRegistry, ModelState};
use openflow_server::models::ModelManager;
use sha2::{Digest, Sha256};

#[tokio::test]
async fn bundled_registry_is_valid() {
    let directory = tempfile::tempdir().unwrap();
    ModelManager::load(directory.path().into(), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn registry_model_ids_cannot_escape_the_cache_directory() {
    let directory = tempfile::tempdir().unwrap();
    let seed = ModelManager::load(directory.path().join("seed"), None)
        .await
        .unwrap();
    let mut spec = seed.list().await.remove(0).spec;
    spec.id = "..".into();
    let registry = ModelRegistry {
        schema_version: 1,
        generated_at: "2026-08-18T00:00:00Z".into(),
        models: vec![spec],
    };
    let registry_path = directory.path().join("registry.json");
    tokio::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap())
        .await
        .unwrap();

    let error = ModelManager::load(directory.path().join("cache"), Some(&registry_path))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid or duplicate model id"));
}

#[tokio::test]
async fn recommendations_reserve_memory_and_prefer_cleanup_quality() {
    let directory = tempfile::tempdir().unwrap();
    let manager = ModelManager::load(directory.path().into(), None)
        .await
        .unwrap();
    let models = manager.list_for_memory(Some(10_000_000_000)).await;
    let recommended: Vec<_> = models
        .iter()
        .filter(|model| model.spec.metadata.get("recommended").map(String::as_str) == Some("true"))
        .map(|model| model.spec.id.as_str())
        .collect();
    assert!(recommended.contains(&"whisper-large-v3-turbo-q8_0"));
    assert!(recommended.contains(&"qwen3-4b-q4_k_m"));

    let larger = manager.list_for_memory(Some(20_000_000_000)).await;
    let larger_recommended: Vec<_> = larger
        .iter()
        .filter(|model| model.spec.metadata.get("recommended").map(String::as_str) == Some("true"))
        .map(|model| model.spec.id.as_str())
        .collect();
    assert!(larger_recommended.contains(&"whisper-large-v3-q5_0"));
    assert!(larger_recommended.contains(&"qwen3-8b-q4_k_m"));

    let constrained = manager.list_for_memory(Some(6_000_000_000)).await;
    assert!(constrained.iter().all(|model| {
        model.spec.metadata.get("recommended").map(String::as_str) == Some("false")
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_is_signalled_and_exposed_without_deleting_the_partial() {
    let directory = tempfile::tempdir().unwrap();
    let manager = ModelManager::load(directory.path().into(), None)
        .await
        .unwrap();
    let id = "qwen3-1.7b-q8_0";
    let partial = directory.path().join(id).join("model.part");
    tokio::fs::create_dir_all(partial.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&partial, b"resumable bytes")
        .await
        .unwrap();

    // A current-thread runtime cannot poll the spawned transfer while this
    // test future is still running. Cancel before the first explicit yield;
    // download_and_verify observes its cancellation guard before any HTTP I/O.
    assert!(manager.start_download(id).await.unwrap());
    manager.cancel_download(id).await.unwrap();

    let models = manager.list().await;
    assert!(matches!(
        models
            .iter()
            .find(|model| model.spec.id == id)
            .unwrap()
            .state,
        ModelState::Cancelling
    ));

    // Let the worker observe cancellation and clean its bookkeeping. Every
    // worker poll happens after the flag is set, so no HTTPS request is sent.
    for _ in 0..10 {
        tokio::task::yield_now().await;
        let state = manager
            .list()
            .await
            .into_iter()
            .find(|model| model.spec.id == id)
            .unwrap()
            .state;
        if matches!(state, ModelState::NotDownloaded) {
            break;
        }
    }
    let state = manager
        .list()
        .await
        .into_iter()
        .find(|model| model.spec.id == id)
        .unwrap()
        .state;
    assert!(matches!(state, ModelState::NotDownloaded));
    assert_eq!(tokio::fs::read(partial).await.unwrap(), b"resumable bytes");
}

#[tokio::test(flavor = "current_thread")]
async fn complete_partial_is_verified_and_promoted_without_an_http_request() {
    let directory = tempfile::tempdir().unwrap();
    let seed = ModelManager::load(directory.path().join("seed"), None)
        .await
        .unwrap();
    let mut spec = seed.list().await.remove(0).spec;
    let bytes = b"complete resumable model";
    spec.size_bytes = bytes.len() as u64;
    spec.sha256 = hex::encode(Sha256::digest(bytes));
    // If the implementation tries to resume a complete partial, this endpoint
    // is deliberately unreachable and the test fails instead of using network.
    spec.source_url = "https://127.0.0.1:1/should-not-be-requested".into();
    let registry = ModelRegistry {
        schema_version: 1,
        generated_at: "2026-08-18T00:00:00Z".into(),
        models: vec![spec.clone()],
    };
    let registry_path = directory.path().join("registry.json");
    tokio::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap())
        .await
        .unwrap();

    let cache = directory.path().join("cache");
    let partial = cache.join(&spec.id).join("model.part");
    tokio::fs::create_dir_all(partial.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&partial, bytes).await.unwrap();
    let manager = ModelManager::load(cache.clone(), Some(&registry_path))
        .await
        .unwrap();
    assert!(manager.start_download(&spec.id).await.unwrap());

    for _ in 0..100 {
        tokio::task::yield_now().await;
        if matches!(manager.list().await[0].state, ModelState::Ready) {
            break;
        }
    }
    assert!(matches!(manager.list().await[0].state, ModelState::Ready));
    assert_eq!(
        tokio::fs::read(cache.join(&spec.id).join("model.bin"))
            .await
            .unwrap(),
        bytes
    );
    assert!(!partial.exists());

    manager.activate(&spec.id).await.unwrap();
    assert!(manager.list().await[0].active);
    assert!(manager.deactivate(&spec.id).await.unwrap());
    assert!(!manager.list().await[0].active);
    assert!(!manager.deactivate(&spec.id).await.unwrap());
}
