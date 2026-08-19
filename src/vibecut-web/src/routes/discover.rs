//! `GET /api/sources` — wraps `vibecut_ops::discover::discover_sources`.

use axum::extract::Query;
use axum::Json;
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
