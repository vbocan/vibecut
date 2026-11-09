//! `GET /api/music` — the analysed music library with each track's
//! licence and credit (wraps `vibecut_ops::music::list_library`);
//! `GET /api/music/pick?vibe=&target_duration=&seed=` — the song a
//! proposal would auto-pick ("Picked for you"); `GET /api/music/file?path=`
//! — stream one library track for previewing in the browser.

use axum::extract::{Query, Request};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use vibecut_ops::error::OpsError;

use crate::error::ApiError;
use crate::routes::output::serve_file;

pub async fn list_music() -> Result<Json<serde_json::Value>, ApiError> {
    let lib = vibecut_ops::music::list_library().await?;
    Ok(Json(serde_json::json!({
        "tracks": lib.tracks,
        "warnings": lib.warnings,
        // Where to put downloads: the user's own folder, as the server sees it.
        "import_dir": vibecut_ops::music::own_music_dir().to_string_lossy(),
        "dirs": vibecut_ops::music::music_dirs()
            .into_iter()
            .map(|(d, c)| serde_json::json!({ "path": d.to_string_lossy(), "collection": c, "exists": d.is_dir() }))
            .collect::<Vec<_>>(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct PickQuery {
    pub vibe: String,
    /// Same as `POST /api/propose`: omitted or empty = the vibe's default.
    pub target_duration: Option<String>,
    #[serde(default = "default_seed")]
    pub seed: u64,
}

fn default_seed() -> u64 {
    42
}

pub async fn pick_music(Query(q): Query<PickQuery>) -> Result<Json<serde_json::Value>, ApiError> {
    let target = q.target_duration.as_deref().filter(|t| !t.trim().is_empty());
    let pick = vibecut_ops::propose::preview_pick(&q.vibe, target, q.seed).await?;
    Ok(Json(serde_json::json!({ "pick": pick })))
}

#[derive(Debug, Deserialize)]
pub struct MusicFileQuery {
    pub path: String,
}

pub async fn serve_music_file(
    Query(q): Query<MusicFileQuery>,
    req: Request,
) -> Result<Response, ApiError> {
    let path = std::path::PathBuf::from(&q.path);
    if !vibecut_ops::music::is_library_file(&path) {
        return Err(OpsError::invalid(format!(
            "not an audio file in a music directory: {:?}",
            q.path
        ))
        .into());
    }
    serve_file(path, req).await
}
