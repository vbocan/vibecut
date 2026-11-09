//! `GET /api/sources` — wraps `vibecut_ops::discover::discover_sources`.

use axum::extract::Query;
use axum::Json;

/// `GET /api/footage` — folders under `/footage` that hold videos.
pub async fn list_footage() -> Result<Json<serde_json::Value>, crate::error::ApiError> {
    let folders = vibecut_ops::footage::list_folders().await?;
    Ok(Json(serde_json::json!({ "folders": folders })))
}
use serde::Deserialize;

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct DiscoverQuery {
    #[serde(default)]
    pub folder: String,
}

pub async fn discover_sources(
    Query(q): Query<DiscoverQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = vibecut_ops::discover::discover_sources(&q.folder).await?;
    Ok(Json(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)))
}
