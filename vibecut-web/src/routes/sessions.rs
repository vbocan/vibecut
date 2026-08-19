//! `GET /api/sessions` — list session JSONL files under
//! `/projects/sessions/`. Reading one file's actual contents reuses
//! the existing `/api/output` file-serving route — a session log is
//! just another file under `/projects` — so this is the only new
//! endpoint the history panel needed.

use axum::Json;

use crate::error::ApiError;

pub async fn list_sessions() -> Result<Json<serde_json::Value>, ApiError> {
    let sessions = vibecut_ops::logger::list_sessions()?;
    Ok(Json(serde_json::json!({ "sessions": sessions })))
}
