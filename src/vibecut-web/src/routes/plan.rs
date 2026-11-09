//! `POST /api/plan` — wraps `vibecut_ops::plan::plan`: the edit before the
//! encode. Returns in seconds on analysed footage: the timeline (every
//! clip's source window, output start/end, speed, crossfades), the beats,
//! the song, the length, and a frame time per clip for `GET
//! /api/frames/at`. While a render of the same config runs, this is that
//! render's own plan (the request waits for it rather than planning twice).

use axum::Json;
use serde::Deserialize;

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct PlanBody {
    pub config_path: String,
}

pub async fn plan(Json(body): Json<PlanBody>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(vibecut_ops::plan::plan(&body.config_path).await?))
}
