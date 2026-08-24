use super::{authenticate, require_admin};
use crate::{error::ServerError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use openflow_protocol::{
    CapabilitiesResponse, CreatePairingCodeResponse, DeviceInfo, HealthResponse,
    InteractivePairDeviceRequest, PROTOCOL_VERSION, PairDeviceRequest, PairDeviceResponse,
};
use uuid::Uuid;

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
    })
}

pub async fn capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CapabilitiesResponse>, ServerError> {
    authenticate(&state, &headers).await?;
    Ok(Json(CapabilitiesResponse {
        protocol_version: PROTOCOL_VERSION,
        server_version: env!("CARGO_PKG_VERSION").into(),
        hardware: state.hardware.clone(),
        max_audio_bytes_per_session: state.config.max_audio_bytes_per_session,
        active_session: state.sessions.is_active(),
        active_models: state.models.active_models().await,
    }))
}

pub async fn pair_device(
    State(state): State<AppState>,
    Json(request): Json<PairDeviceRequest>,
) -> Result<Json<PairDeviceResponse>, ServerError> {
    Ok(Json(
        state
            .auth
            .pair_device(&request.pairing_code, &request.device_name)
            .await?,
    ))
}

pub async fn pair_device_interactively(
    State(state): State<AppState>,
    Json(request): Json<InteractivePairDeviceRequest>,
) -> Result<Json<PairDeviceResponse>, ServerError> {
    let device_name = request.device_name.trim();
    if device_name.is_empty()
        || device_name.len() > 128
        || device_name.chars().any(char::is_control)
    {
        return Err(ServerError::BadRequest(
            "device_name must contain 1-128 printable characters".into(),
        ));
    }
    if request.verification_code.len() != 6
        || !request
            .verification_code
            .bytes()
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(ServerError::BadRequest(
            "verification_code must contain exactly six digits".into(),
        ));
    }
    if !state
        .pairing_prompt
        .confirm(device_name, &request.verification_code)
        .await?
    {
        return Err(ServerError::Forbidden(
            "the server operator denied the pairing request".into(),
        ));
    }

    // Reuse the existing single-use enrollment path so interactive approval
    // and administrator-created codes have identical persistence semantics.
    let (pairing_code, _) = state
        .auth
        .create_pairing_code(state.config.pairing_ttl)
        .await?;
    Ok(Json(
        state.auth.pair_device(&pairing_code, device_name).await?,
    ))
}

pub async fn create_pairing_code(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<CreatePairingCodeResponse>, ServerError> {
    require_admin(&state, &headers).await?;
    let (pairing_code, expires_at_unix_seconds) = state
        .auth
        .create_pairing_code(state.config.pairing_ttl)
        .await?;
    Ok(Json(CreatePairingCodeResponse {
        pairing_code,
        expires_at_unix_seconds,
    }))
}

pub async fn list_devices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeviceInfo>>, ServerError> {
    require_admin(&state, &headers).await?;
    Ok(Json(state.auth.list_devices().await))
}

pub async fn revoke_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(), ServerError> {
    require_admin(&state, &headers).await?;
    state.auth.revoke_device(id).await
}
