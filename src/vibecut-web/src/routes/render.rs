//! `POST /api/render`, `POST /api/render/async`, `GET
//! /api/render/{job_id}` — wrap `vibecut_ops::render`. Same
//! sync/async pair the MCP `render` / `render_async` / `render_status`
//! tools expose; the WebSocket progress feed in [`crate::ws`] is a
//! push-based alternative to polling the last one.

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use vibecut_ops::error::OpsError;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct RenderBody {
    pub config_path: String,
    /// `draft` (a fast 540p preview beside the final, `<stem>.draft.mp4`)
    /// or `final`; absent: the config's `[project].quality`, else final.
    #[serde(default)]
    pub quality: Option<String>,
}

impl RenderBody {
    fn request(&self) -> Result<vibecut_ops::render::RenderRequest, ApiError> {
        Ok(vibecut_ops::render::RenderRequest::from_quality_field(self.quality.as_deref())?)
    }
}

/// Blocking: takes 1–10 minutes. Fine for a first REST cut — the
/// browser client is expected to prefer `render_async` + either
/// polling `/api/render/{job_id}` or the WS feed, same trade-off the
/// MCP tool surface already documents.
pub async fn render_sync(
    Json(body): Json<RenderBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = vibecut_ops::render::render_sync_with(&body.config_path, body.request()?).await?;
    Ok(Json(result))
}

pub async fn render_async(
    State(state): State<AppState>,
    Json(body): Json<RenderBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let req = body.request()?;
    let job_id = vibecut_ops::render::submit_async_with(&state.jobs, &body.config_path, req).await?;
    Ok(Json(serde_json::json!({
        "job_id": job_id,
        "state": "queued",
        "quality": req.quality.map(|q| q.as_str()),
    })))
}

pub async fn render_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match vibecut_ops::render::render_status(&state.jobs, &job_id).await {
        Some(st) => Ok(Json(vibecut_ops::render::job_state_json(&job_id, st))),
        None => Err(OpsError::not_found_with("job", job_id).into()),
    }
}
