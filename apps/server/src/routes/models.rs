use super::{authenticate, dictation::enforce_memory_budget};
use crate::{error::ServerError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use openflow_protocol::{
    ActivateModelRequest, CancelModelDownloadRequest, DeactivateModelRequest, DownloadModelRequest,
    ModelInfo, ModelKind, ModelMutationResponse, ModelState,
};

pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ModelInfo>>, ServerError> {
    authenticate(&state, &headers).await?;
    let memory_budget = state
        .hardware
        .accelerator_memory_bytes
        .or(state.hardware.system_memory_bytes);
    Ok(Json(state.models.list_for_memory(memory_budget).await))
}

pub async fn download_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DownloadModelRequest>,
) -> Result<(StatusCode, Json<ModelMutationResponse>), ServerError> {
    authenticate(&state, &headers).await?;
    let accepted = state.models.start_download(&request.model_id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(ModelMutationResponse {
            model_id: request.model_id,
            accepted,
        }),
    ))
}

pub async fn cancel_model_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CancelModelDownloadRequest>,
) -> Result<Json<ModelMutationResponse>, ServerError> {
    authenticate(&state, &headers).await?;
    state.models.cancel_download(&request.model_id).await?;
    Ok(Json(ModelMutationResponse {
        model_id: request.model_id,
        accepted: true,
    }))
}

pub async fn activate_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ActivateModelRequest>,
) -> Result<Json<ModelMutationResponse>, ServerError> {
    authenticate(&state, &headers).await?;
    let _lifecycle = state.model_lifecycle.lock().await;
    reject_during_dictation(&state)?;
    let models = state.models.list().await;
    let selected = models
        .iter()
        .find(|model| model.spec.id == request.model_id)
        .ok_or_else(|| ServerError::NotFound(format!("model {}", request.model_id)))?;
    if !matches!(selected.state, ModelState::Ready) {
        return Err(ServerError::Conflict(format!(
            "model {} must finish downloading before activation",
            request.model_id
        )));
    }
    let active = state.models.active_models().await;
    let currently_active = |kind| {
        active
            .iter()
            .find(|model| model.kind == kind)
            .map(|model| model.model_id.as_str())
    };
    let (asr, cleanup) = match selected.spec.kind {
        ModelKind::SpeechToText => (
            Some(request.model_id.as_str()),
            currently_active(ModelKind::TextCleanup),
        ),
        ModelKind::TextCleanup => (
            currently_active(ModelKind::SpeechToText),
            Some(request.model_id.as_str()),
        ),
    };
    let required_memory = [asr, cleanup]
        .into_iter()
        .flatten()
        .filter_map(|id| {
            models
                .iter()
                .find(|model| model.spec.id == id)
                .map(|model| model.spec.estimated_memory_bytes)
        })
        .fold(0_u64, u64::saturating_add);
    if let Some(available) = state
        .hardware
        .accelerator_memory_bytes
        .or(state.hardware.system_memory_bytes)
    {
        enforce_memory_budget(required_memory, available)?;
    }
    if state.config.worker_backend != "mock"
        && let Some(asr) = asr
    {
        state.inference.prepare_models(asr, cleanup).await?;
    }
    state.models.activate(&request.model_id).await?;
    Ok(Json(ModelMutationResponse {
        model_id: request.model_id,
        accepted: true,
    }))
}

pub async fn deactivate_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DeactivateModelRequest>,
) -> Result<Json<ModelMutationResponse>, ServerError> {
    authenticate(&state, &headers).await?;
    let _lifecycle = state.model_lifecycle.lock().await;
    reject_during_dictation(&state)?;
    let model = state
        .models
        .list()
        .await
        .into_iter()
        .find(|model| model.spec.id == request.model_id)
        .ok_or_else(|| ServerError::NotFound(format!("model {}", request.model_id)))?;
    if !model.active {
        return Ok(Json(ModelMutationResponse {
            model_id: request.model_id,
            accepted: false,
        }));
    }
    state.inference.unload_model(model.spec.kind).await?;
    let accepted = state.models.deactivate(&request.model_id).await?;
    Ok(Json(ModelMutationResponse {
        model_id: request.model_id,
        accepted,
    }))
}

pub async fn delete_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_id): Path<String>,
) -> Result<StatusCode, ServerError> {
    authenticate(&state, &headers).await?;
    let _lifecycle = state.model_lifecycle.lock().await;
    reject_during_dictation(&state)?;
    state.models.delete(&model_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn reject_during_dictation(state: &AppState) -> Result<(), ServerError> {
    if state.sessions.is_active() {
        Err(ServerError::Conflict(
            "model activation, deactivation, and deletion are unavailable during dictation".into(),
        ))
    } else {
        Ok(())
    }
}
