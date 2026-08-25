use openflow_server::{
    InferenceEngine, UnavailableInferenceEngine, WorkerInferenceEngine, build_state,
    config::ServerConfig, serve,
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "openflow_server=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = Arc::new(ServerConfig::from_env()?);
    let asr_worker = config
        .asr_worker_path
        .clone()
        .or_else(|| sibling_binary("openflow-asr-worker"));
    let llm_worker = config
        .llm_worker_path
        .clone()
        .or_else(|| sibling_binary("openflow-llm-worker"));
    let inference: Arc<dyn InferenceEngine> = if let Some(asr_path) = asr_worker {
        Arc::new(
            WorkerInferenceEngine::spawn(
                &asr_path,
                llm_worker.as_deref(),
                config.model_cache_dir.clone(),
                config.worker_backend.clone(),
                config.print_transcripts,
            )
            .await?,
        )
    } else {
        tracing::warn!(
            "openflow-asr-worker was not found; model management is available but dictation is disabled"
        );
        Arc::new(UnavailableInferenceEngine)
    };
    let state = build_state(Arc::clone(&config), inference).await?;

    if let Some(token) = state.auth.take_bootstrap_admin_token() {
        // This single JSON line is a protected bootstrap channel for a spawning
        // local client. Logging uses stderr and never receives this credential.
        println!(
            "{}",
            json!({
                "event": "bootstrap",
                "address": config.bind_address.to_string(),
                "admin_token": token,
            })
        );
    }
    serve((*config).clone(), state).await?;
    Ok(())
}

fn sibling_binary(name: &str) -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join(name);
    path.is_file().then_some(path)
}
