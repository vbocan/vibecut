//! `GET /api/music` — the analysed music library (wraps
//! `vibecut_ops::music::list_tracks`); `GET /api/music/file?path=` —
//! stream one library track for previewing in the browser.

use axum::extract::{Query, Request};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use vibecut_ops::error::OpsError;

use crate::error::ApiError;
use crate::routes::output::serve_file;

pub async fn list_music() -> Result<Json<serde_json::Value>, ApiError> {
    let tracks = vibecut_ops::music::list_tracks().await?;
    Ok(Json(serde_json::json!({
        "tracks": tracks,
        "dirs": vibecut_ops::music::music_dirs()
            .into_iter()
            .map(|(d, c)| serde_json::json!({ "path": d.to_string_lossy(), "collection": c }))
            .collect::<Vec<_>>(),
    })))
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
