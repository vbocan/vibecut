//! Style codes ("recipes") — wraps `vibecut_ops::recipe`:
//!
//! - `POST /api/recipes/code` — a share code for a project's style;
//! - `GET /api/recipes/preview?code=` — what a code (or a saved style's
//!   name) would make, and which song it would get from this library;
//! - `POST /api/recipes/apply` — a proposal for a footage folder in that
//!   style, shaped like `POST /api/propose`'s, ready for the normal render;
//! - `GET /api/recipes` / `POST /api/recipes` — list / save named styles
//!   (`/projects/recipes/<name>.json`).

use std::time::Instant;

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use vibecut_ops::error::OpsError;
use vibecut_ops::recipe::{self, ApplyArgs, PreviewResult, SavedStyle, SavedStyles, ShareArgs, ShareResult};

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CodeBody {
    /// The project config to share the style of.
    pub config_path: String,
    /// The shown version's manifest: its song credit and tempo fill in a
    /// song the music library doesn't know.
    pub manifest_path: Option<String>,
    /// The vibe the project was made from; omitted = the closest one.
    pub vibe: Option<String>,
    /// Share "let Vibecut pick" instead of the song itself.
    #[serde(default)]
    pub let_vibecut_pick: bool,
}

fn share_args(b: &CodeBody) -> ShareArgs<'_> {
    ShareArgs {
        config_path: &b.config_path,
        manifest_path: b.manifest_path.as_deref(),
        vibe: b.vibe.as_deref(),
        let_vibecut_pick: b.let_vibecut_pick,
    }
}

pub async fn make_code(Json(body): Json<CodeBody>) -> Result<Json<ShareResult>, ApiError> {
    Ok(Json(recipe::share_style(share_args(&body)).await?))
}

#[derive(Debug, Deserialize)]
pub struct PreviewQuery {
    pub code: String,
}

pub async fn preview(Query(q): Query<PreviewQuery>) -> Result<Json<PreviewResult>, ApiError> {
    Ok(Json(recipe::preview_style(&q.code).await?))
}

#[derive(Debug, Deserialize)]
pub struct ApplyBody {
    /// A style code, or a saved style's name.
    pub code: String,
    #[serde(default)]
    pub folder: String,
    pub title: Option<String>,
}

pub async fn apply(
    State(state): State<AppState>,
    Json(body): Json<ApplyBody>,
) -> Result<Json<recipe::ApplyResult>, ApiError> {
    let started = Instant::now();
    let args_json = serde_json::json!({ "code": body.code, "folder": body.folder });
    let result = recipe::apply_style(ApplyArgs { code: &body.code, folder: &body.folder, title: body.title.as_deref() }).await;
    let (excerpt, ok) = match &result {
        Ok(r) => (
            serde_json::json!({
                "vibe": r.proposal.vibe,
                "config_path": r.proposal.config_path,
                "output_path": r.proposal.output_path,
                "music": r.music_match.how,
            }),
            true,
        ),
        Err(e) => (serde_json::json!({ "error": e.to_string() }), false),
    };
    state.logger.record("apply_style", args_json, excerpt, started.elapsed().as_millis() as u64, ok);
    Ok(Json(result?))
}

pub async fn list() -> Json<SavedStyles> {
    Json(recipe::list_styles().await)
}

#[derive(Debug, Deserialize)]
pub struct SaveBody {
    pub name: String,
    /// The code to save; or, without one, the style of `config_path`.
    pub code: Option<String>,
    #[serde(flatten)]
    pub from: Option<CodeBody>,
}

pub async fn save(Json(body): Json<SaveBody>) -> Result<Json<SavedStyle>, ApiError> {
    let code = match (&body.code, &body.from) {
        (Some(c), _) if !c.trim().is_empty() => c.clone(),
        (_, Some(from)) => recipe::share_style(share_args(from)).await?.code,
        _ => return Err(OpsError::invalid("give a `code`, or a `config_path` to save the style of").into()),
    };
    Ok(Json(recipe::save_style(&body.name, &code).await?))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::{router, AppState};

    async fn call(req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let res = router(AppState::default()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    /// Share a project's style, then preview the code: the same style in
    /// words, and a clear 400 for a code from a newer Vibecut.
    #[tokio::test]
    async fn share_then_preview_a_style() {
        let dir = std::env::temp_dir().join(format!("vibecut_web_recipes_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.toml");
        std::fs::write(
            &path,
            r##"
schema_version = 1
[project]
name = "Coastline · 2026-11-08"
target_duration = "45s"
fps = 30
seed = 42
fade_in = "200ms"
fade_out = "1s 500ms"
[input]
folder = "/footage/Coastline"
[intro]
duration = "3s"
fade_in = "300ms"
fade_out = "300ms"
bg_color = "#0a0a0a"
[[intro.layers]]
kind = "text"
content = "Coastline · 2026-11-08"
size = 110
[selection]
min_clip = "1s 500ms"
max_clip = "4s"
order = "energy_arc"
variety = 0.1
[transition]
duration = "300ms"
speed_ramp_probability = 0.7
speed_ramp_duration = "2s"
speed_ramp_factor = 3.0
[[outputs]]
path = "/projects/out/coastline.mp4"
resolution = "1920x1080"
crf = 20
audio_bitrate = "192k"
"##,
        )
        .unwrap();

        let (status, shared) =
            call(post("/api/recipes/code", serde_json::json!({ "config_path": path.to_string_lossy() }))).await;
        assert_eq!(status, StatusCode::OK, "{shared}");
        let code = shared["code"].as_str().unwrap();
        assert!(code.starts_with("VC1-") && shared["chars"].as_u64().unwrap() < 40, "{shared}");
        assert_eq!(shared["style"]["vibe"], "action_highlight");
        assert_eq!(shared["style"]["music"]["kind"], "original");

        let (status, preview) = call(get(&format!("/api/recipes/preview?code={code}"))).await;
        assert_eq!(status, StatusCode::OK, "{preview}");
        assert_eq!(preview["code"], code);
        assert_eq!(preview["style"]["target_duration"], "45s");
        assert_eq!(preview["music_match"]["how"], "original_sound");
        let chips = preview["style"]["chips"].as_array().unwrap();
        assert!(chips.iter().any(|c| c["label"] == "Length" && c["value"] == "45 s"), "{chips:?}");

        let newer = code.replacen("VC1-", "VC2-", 1);
        let (status, err) = call(get(&format!("/api/recipes/preview?code={newer}"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(err["error"].as_str().unwrap().contains("newer Vibecut"), "{err}");
        assert!(err["hint"].as_str().unwrap().contains("update Vibecut"), "{err}");

        let tampered = format!("{}{}", &code[..code.len() - 2], if code.ends_with("AA") { "BB" } else { "AA" });
        let (status, err) = call(get(&format!("/api/recipes/preview?code={tampered}"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn apply_and_save_report_bad_input() {
        // No such footage folder.
        let (status, _) = call(post(
            "/api/recipes/apply",
            serde_json::json!({ "code": "VC1-Q1VCGg", "folder": "/no/such/footage" }),
        ))
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // A name that isn't a saved style.
        let (status, _) = call(post("/api/recipes/apply", serde_json::json!({ "code": "Winter", "folder": "/x" }))).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // Saving needs a code or a config.
        let (status, err) = call(post("/api/recipes", serde_json::json!({ "name": "x" }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{err}");
        // The list works without a recipes folder.
        let (status, list) = call(get("/api/recipes")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(list["styles"].is_array() && list["warnings"].is_array(), "{list}");
    }
}
