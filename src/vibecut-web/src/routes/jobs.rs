//! `GET /api/jobs` and `POST /api/jobs/{job_id}/cancel` — the render
//! queue: list recent jobs, stop a wrong one. Thin wrappers over
//! `vibecut_ops::render::{list_jobs, cancel}`; the per-job
//! `GET /api/render/{job_id}` and the progress WebSocket report the
//! same states, `cancelled` included.

use axum::extract::{Path, State};
use axum::Json;
use vibecut_ops::error::OpsError;

use crate::error::ApiError;
use crate::state::AppState;

/// Recent jobs, newest first: id, config, state, submitted / started /
/// finished times, progress while running, output paths once done.
pub async fn list_jobs(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(vibecut_ops::render::list_jobs(&state.jobs).await)
}

/// Stop a job. Answers at once with the job's state: `cancelled` for a
/// job that was still queued, `running` with `cancel_requested: true`
/// for one that is being stopped (it turns `cancelled` within about a
/// second), or its final state if it had already finished.
pub async fn cancel_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match vibecut_ops::render::cancel(&state.jobs, &job_id).await {
        Some(v) => Ok(Json(v)),
        None => Err(OpsError::not_found_with("job", job_id).into()),
    }
}
