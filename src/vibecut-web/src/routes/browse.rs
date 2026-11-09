//! `GET /api/browse/roots`, `GET /api/browse?path=…` — wrap
//! `vibecut_ops::browse`: the drives and folders a footage folder can be
//! picked from, one level at a time. Read-only. (Like every `/api` route
//! they sit behind [`crate::host_guard`], which matters most here: they
//! list folder names on every mounted drive.)

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    #[serde(default)]
    pub path: String,
}

pub async fn roots() -> Result<Json<serde_json::Value>, ApiError> {
    let roots = vibecut_ops::browse::list_roots().await?;
    Ok(Json(serde_json::json!({ "roots": roots })))
}

pub async fn browse(Query(q): Query<BrowseQuery>) -> Result<Json<serde_json::Value>, ApiError> {
    let listing = vibecut_ops::browse::browse(&q.path).await?;
    Ok(Json(serde_json::to_value(listing).unwrap_or(serde_json::Value::Null)))
}
