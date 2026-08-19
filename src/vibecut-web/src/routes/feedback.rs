//! `POST /api/feedback` — wraps
//! `vibecut_ops::feedback::apply_feedback_to_file`.
//!
//! The Preview + Timeline wireframe's quick-feedback chips (Calmer,
//! Punchier, Drop segment, …) and right-click segment actions all
//! resolve to one `FeedbackAction` variant and hit this one endpoint —
//! no LLM required, same as the MCP `apply_feedback` tool.

use std::time::Instant;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use vibecut_ops::feedback::FeedbackAction;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct FeedbackBody {
    pub config_path: String,
    pub action: FeedbackAction,
}

pub async fn apply_feedback(
    State(state): State<AppState>,
    Json(body): Json<FeedbackBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let started = Instant::now();
    let args_json = serde_json::json!({ "config_path": body.config_path, "action": &body.action });
    let result = vibecut_ops::feedback::apply_feedback_to_file(&body.config_path, &body.action).await;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    let (excerpt, ok) = match &result {
        Ok(r) => (
            serde_json::json!({ "config_path": r.config_path, "n_changes": r.changes.len(), "changes": r.changes }),
            true,
        ),
        Err(e) => (serde_json::json!({ "error": e.to_string() }), false),
    };
    state.logger.record("feedback", args_json, excerpt, elapsed_ms, ok);

    let result = result?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "config_path": result.config_path,
        "changes": result.changes,
        "n_changes": result.changes.len(),
    })))
}
