mod admin;
mod dictation;
mod models;

#[doc(hidden)]
pub use dictation::{
    apply_normalizations, common_utf8_prefix, enforce_memory_budget, language_left_context,
    language_right_context, rolling_audio_window, websocket_bearer,
};

use crate::{auth::Principal, error::ServerError, state::AppState};
use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderMap, header},
    routing::{delete, get, post},
};
use tower_http::trace::TraceLayer;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(admin::health))
        .route("/v1/capabilities", get(admin::capabilities))
        .route("/v1/pair", post(admin::pair_device))
        .route(
            "/v1/pair/interactive",
            post(admin::pair_device_interactively),
        )
        .route("/v1/pairing-codes", post(admin::create_pairing_code))
        .route("/v1/devices", get(admin::list_devices))
        .route("/v1/devices/:id", delete(admin::revoke_device))
        .route("/v1/models", get(models::list_models))
        .route("/v1/models/download", post(models::download_model))
        .route("/v1/models/cancel", post(models::cancel_model_download))
        .route("/v1/models/activate", post(models::activate_model))
        .route("/v1/models/deactivate", post(models::deactivate_model))
        .route("/v1/models/:id", delete(models::delete_model))
        .route("/v1/dictation", get(dictation::upgrade))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub(super) async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Principal, ServerError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or(ServerError::Unauthorized)?;
    state.auth.authenticate(value).await
}

pub(super) async fn require_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), ServerError> {
    match authenticate(state, headers).await? {
        Principal::Admin => Ok(()),
        Principal::Device(_) => Err(ServerError::AdminRequired),
    }
}
