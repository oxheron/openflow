//! Headless `OpenFlow` server.
//!
//! This crate owns networking, authentication, model lifecycle, and dictation
//! session orchestration. Inference is supplied through [`InferenceEngine`], so
//! CPU/GPU worker processes can be swapped without changing the public API.

pub mod auth;
pub mod config;
pub mod error;
pub mod inference;
pub mod models;
pub mod pairing;
pub mod routes;
pub mod state;

use crate::{config::ServerConfig, error::ServerError, state::AppState};
use axum::Router;
use std::{net::SocketAddr, sync::Arc};

pub use inference::{
    InferenceEngine, UnavailableInferenceEngine, WorkerClient, WorkerInferenceEngine,
};
pub use state::SessionGate;

pub fn app(state: AppState) -> Router {
    routes::router(state)
}

/// Runs the server until the process is terminated.
///
/// # Errors
///
/// Returns an error for invalid configuration, TLS setup failure, listener I/O,
/// or a serving failure.
pub async fn serve(config: ServerConfig, state: AppState) -> Result<(), ServerError> {
    config.validate()?;
    let address: SocketAddr = config.bind_address;
    let app = app(state);

    if let Some(tls) = &config.tls {
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &tls.certificate_path,
            &tls.private_key_path,
        )
        .await
        .map_err(|error| ServerError::Configuration(error.to_string()))?;
        tracing::info!(%address, "serving OpenFlow with TLS");
        axum_server::bind_rustls(address, tls_config)
            .serve(app.into_make_service())
            .await
            .map_err(ServerError::Io)
    } else {
        tracing::info!(%address, "serving OpenFlow on loopback");
        let listener = tokio::net::TcpListener::bind(address).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(ServerError::Io)
    }
}

async fn shutdown_signal() {
    let control_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }
}

/// Convenience constructor used by embedders and the binary.
///
/// # Errors
///
/// Returns an error when server state cannot be initialized.
pub async fn build_state(
    config: Arc<ServerConfig>,
    engine: Arc<dyn InferenceEngine>,
) -> Result<AppState, ServerError> {
    AppState::new(config, engine).await
}
