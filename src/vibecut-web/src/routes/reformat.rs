//! `POST /api/reformat` — wrap `vibecut_ops::reformat`: a platform
//! version (vertical Reel, square, WhatsApp) of a finished render in one
//! fast ffmpeg pass, written next to it as `<stem>.<preset>.mp4` and
//! served by `GET /api/output` like any render.
//!
//! Default: a job. The response carries `job_id` and the version's
//! `output_path` up front; poll `GET /api/render/{job_id}` (or the WS
//! feed) for progress and the result. `"wait": true` blocks instead and
//! returns the result directly.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use vibecut_ops::error::OpsError;

use crate::error::ApiError;
use crate::routes::output::is_safe_projects_path;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ReformatBody {
    /// The finished render (`output_path` or `version_path` of a render).
    pub output_path: String,
    /// `youtube`, `reels`, `tiktok`, `shorts`, `square`, `whatsapp`, `master`.
    pub preset: String,
    /// `blur` (default), `crop` or `pad`.
    #[serde(default)]
    pub fit: Option<String>,
    #[serde(default)]
    pub wait: bool,
}

pub async fn reformat(
    State(state): State<AppState>,
    Json(body): Json<ReformatBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !is_safe_projects_path(&body.output_path) {
        return Err(OpsError::invalid(format!(
            "path escapes the /projects mount: {:?}",
            body.output_path
        ))
        .into());
    }
    if body.wait {
        let result =
            vibecut_ops::reformat::reformat_sync(&body.output_path, &body.preset, body.fit.as_deref()).await?;
        return Ok(Json(result));
    }
    let (job_id, target) =
        vibecut_ops::reformat::submit_async(&state.jobs, &body.output_path, &body.preset, body.fit.as_deref())
            .await?;
    Ok(Json(serde_json::json!({
        "job_id": job_id,
        "state": "queued",
        "preset": target.preset.name(),
        "output_path": target.target.to_string_lossy(),
    })))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::{router, AppState};

    async fn post(body: serde_json::Value) -> StatusCode {
        router(AppState::default())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/reformat")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn paths_outside_projects_are_rejected() {
        let s = post(serde_json::json!({ "output_path": "../../etc/passwd.mp4", "preset": "reels" })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let s = post(serde_json::json!({ "output_path": "/etc/x.mp4", "preset": "reels" })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        // A reformat writes next to its source: a sibling of the mount
        // that merely shares its prefix must not pass.
        let s = post(serde_json::json!({ "output_path": "/projects_backup/clip.mp4", "preset": "reels" })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn unknown_preset_is_400_and_missing_video_is_404() {
        let s = post(serde_json::json!({ "output_path": "out/x.mp4", "preset": "myspace" })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let s = post(serde_json::json!({ "output_path": "out/x.mp4", "preset": "reels", "fit": "zoom" })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let s = post(serde_json::json!({ "output_path": "out/no_such_render.mp4", "preset": "reels" })).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }
}
