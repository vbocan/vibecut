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
use vibecut_ops::error::OpsError;

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

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;

    fn status<T>(r: Result<T, ApiError>) -> Option<StatusCode> {
        r.err().map(|e| e.into_response().status())
    }

    /// `GET /api/config?config_path=/proc/self/environ` used to hand back
    /// the server's environment, `ANTHROPIC_API_KEY` included.
    #[tokio::test]
    async fn reads_outside_projects_are_a_400() {
        for p in ["/proc/self/environ", "../etc/passwd", "..\\..\\etc\\passwd"] {
            let q = || Query(ConfigPathQuery { config_path: p.into() });
            assert_eq!(status(read_config(q()).await), Some(StatusCode::BAD_REQUEST), "{p:?}");
            assert_eq!(
                status(read_config_structured(q()).await),
                Some(StatusCode::BAD_REQUEST),
                "{p:?}"
            );
        }
    }

    /// A config that parses and validates, so only the path can be at fault.
    const VALID_TOML: &str = r#"
schema_version = 1
[project]
name = "x"
target_duration = "10s"
fps = 30
[input]
folder = "."
[[outputs]]
path = "out.mp4"
resolution = "1920x1080"
"#;

    #[tokio::test]
    async fn writes_outside_projects_or_to_non_toml_are_a_400() {
        for p in ["/etc/cron.d/x.toml", "../x.toml", "out/render.mp4"] {
            let body = WriteConfigBody {
                config_path: p.into(),
                config_toml: VALID_TOML.into(),
            };
            assert_eq!(
                status(write_config(Json(body)).await),
                Some(StatusCode::BAD_REQUEST),
                "{p:?}"
            );
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PatchBody {
    pub config_path: String,
    pub changes: Vec<vibecut_ops::config_io::ConfigChange>,
}

/// `POST /api/config/patch` — change settings in place (the Fine-tune
/// page): comments, order and unknown keys are kept; a change that would
/// make the config invalid is a 400 and nothing is written.
pub async fn patch_config(Json(body): Json<PatchBody>) -> Result<Json<vibecut_ops::config_io::ConfigText>, ApiError> {
    if body.changes.is_empty() || body.changes.len() > 64 {
        return Err(OpsError::invalid("send between 1 and 64 changes").into());
    }
    Ok(Json(vibecut_ops::config_io::patch_config(&body.config_path, &body.changes).await?))
}

#[derive(Debug, Deserialize)]
pub struct DefaultsQuery {
    pub config_path: String,
    pub vibe: Option<String>,
}

/// `GET /api/config/defaults?config_path=&vibe=` — the project's vibe
/// defaults and what the project changed from them.
pub async fn config_defaults(Query(q): Query<DefaultsQuery>) -> Result<Json<vibecut_ops::recipe::VibeDefaults>, ApiError> {
    let cfg = vibecut_ops::config_io::read_config_structured(&q.config_path).await?;
    let vibe = q.vibe.as_deref().filter(|v| !v.trim().is_empty());
    Ok(Json(vibecut_ops::recipe::vibe_defaults(&cfg, vibe)?))
}

