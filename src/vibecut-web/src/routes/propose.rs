//! `POST /api/propose` — wraps `vibecut_ops::propose::propose_vibe`.

use std::time::Instant;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ProposeBody {
    #[serde(default)]
    pub folder: String,
    #[serde(default = "default_vibe")]
    pub vibe: String,
    pub title: Option<String>,
    pub target_duration: Option<String>,
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_vibe() -> String {
    "kid_reel".to_string()
}
fn default_seed() -> u64 {
    42
}

pub async fn propose_vibe(
    State(state): State<AppState>,
    Json(body): Json<ProposeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let started = Instant::now();
    let args_json = serde_json::json!({ "folder": body.folder, "vibe": body.vibe });
    let result = vibecut_ops::propose::propose_vibe(vibecut_ops::propose::ProposeArgs {
        folder: &body.folder,
        vibe: &body.vibe,
        title: body.title.as_deref(),
        target_duration: body.target_duration.as_deref(),
        seed: body.seed,
    })
    .await;

    let elapsed_ms = started.elapsed().as_millis() as u64;
    let (excerpt, ok) = match &result {
        Ok(r) => (
            serde_json::json!({ "vibe": r.vibe, "config_path": r.config_path, "output_path": r.output_path }),
            true,
        ),
        Err(e) => (serde_json::json!({ "error": e.to_string() }), false),
    };
    state.logger.record("propose", args_json, excerpt, elapsed_ms, ok);

    let result = result?;
    Ok(Json(serde_json::json!({
        "vibe": result.vibe,
        "vibe_name": result.vibe_name,
        "rationale": result.rationale,
        "config_path": result.config_path,
        "config_toml": result.config_toml,
        "output_path": result.output_path,
    })))
}
