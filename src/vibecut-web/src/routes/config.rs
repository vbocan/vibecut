//! `GET /api/config`, `PUT /api/config` — wrap
//! `vibecut_ops::config_io`. The Config editor screen's textarea
//! reads through the GET (to refresh after a structured
//! `apply_feedback` mutation changed the file) and writes through the
//! PUT (hand-edited TOML, validated the same way any other mutation
//! is before it touches disk).
//!
//! `GET /api/config/structured`, `PUT /api/config/structured` — the
//! same file, as a typed `Config` instead of a TOML string. This is
//! what the guided (wizard) editor talks to; no TOML syntax crosses
//! the wire on this path in either direction.

use axum::extract::Query;
use axum::Json;
use serde::Deserialize;
use vibecut_ops::config_io::Config;

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct ConfigPathQuery {
    pub config_path: String,
}

pub async fn read_config(
    Query(q): Query<ConfigPathQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = vibecut_ops::config_io::read_config_toml(&q.config_path).await?;
    Ok(Json(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)))
}

#[derive(Debug, Deserialize)]
pub struct WriteConfigBody {
    pub config_path: String,
    pub config_toml: String,
}

pub async fn write_config(
    Json(body): Json<WriteConfigBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result =
        vibecut_ops::config_io::write_config_toml(&body.config_path, &body.config_toml).await?;
    Ok(Json(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)))
}

pub async fn read_config_structured(
    Query(q): Query<ConfigPathQuery>,
) -> Result<Json<Config>, ApiError> {
    let cfg = vibecut_ops::config_io::read_config_structured(&q.config_path).await?;
    Ok(Json(cfg))
}

#[derive(Debug, Deserialize)]
pub struct WriteConfigStructuredBody {
    pub config_path: String,
    pub config: Config,
}

pub async fn write_config_structured(
    Json(body): Json<WriteConfigStructuredBody>,
) -> Result<Json<Config>, ApiError> {
    let cfg =
        vibecut_ops::config_io::write_config_structured(&body.config_path, &body.config).await?;
    Ok(Json(cfg))
}
