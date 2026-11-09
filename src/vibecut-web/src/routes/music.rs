//! `GET /api/music` — the analysed music library with each track's
//! licence and credit (wraps `vibecut_ops::music::list_library`);
//! `GET /api/music/pick?vibe=&target_duration=&seed=` — the song a
//! proposal would auto-pick ("Picked for you"); `GET /api/music/file?path=`
//! — stream one library track for previewing in the browser;
//! `POST /api/music/folders` `{path}` — add a folder on a drive to the
//! library; `POST /api/music/folders/remove` `{path}` — take it out again.

use axum::extract::{Query, Request};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use vibecut_ops::error::OpsError;

use crate::error::ApiError;
use crate::routes::output::serve_file;

pub async fn list_music() -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(library_json().await?))
}

/// The library as `GET /api/music` returns it. Each folder says whether
/// the user picked it on a drive (`picked`: it can be removed) and how
/// the user knows it (`display`: `D:\Music`).
async fn library_json() -> Result<serde_json::Value, ApiError> {
    let lib = vibecut_ops::music::list_library().await?;
    let picked = vibecut_ops::music::picked_folders();
    let host = vibecut_ops::timeline_export::path_map_from_env();
    let import_dir = vibecut_ops::music::own_music_dir().to_string_lossy().into_owned();
    Ok(serde_json::json!({
        "tracks": lib.tracks,
        "warnings": lib.warnings,
        // Where to put downloads: the user's own folder, as the server sees
        // it and (when the host folder is known) as the user does.
        "import_dir_display": host_display(&import_dir, &host),
        "import_dir": import_dir,
        "dirs": vibecut_ops::music::music_dirs()
            .into_iter()
            .map(|(d, c)| {
                let path = d.to_string_lossy().into_owned();
                serde_json::json!({
                    "display": host_display(&path, &host),
                    "path": path,
                    "collection": c,
                    "exists": d.is_dir(),
                    "picked": picked.contains(&d),
                })
            })
            .collect::<Vec<_>>(),
    }))
}

/// `path` as the user knows it, from the container → host folder map
/// (`/projects/music` → `D:\Vibecut\projects\music`, `/host/d/Music` →
/// `D:\Music`); unchanged when no mapped folder holds it.
fn host_display(path: &str, map: &[(String, String)]) -> String {
    for (container, host) in map {
        let Some(rest) = path.strip_prefix(container.as_str()) else { continue };
        if !(rest.is_empty() || rest.starts_with('/')) {
            continue;
        }
        let host = host.trim_end_matches(['/', '\\']);
        let windows = host.as_bytes().get(1) == Some(&b':') || host.starts_with('\\');
        return if windows {
            let joined = format!("{host}{rest}").replace('/', "\\");
            if joined.len() == 2 { format!("{joined}\\") } else { joined }
        } else {
            format!("{host}{rest}")
        };
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::host_display;

    #[test]
    fn container_paths_show_as_host_folders() {
        let map = vec![
            ("/projects".to_string(), "D:/Vibecut/projects".to_string()),
            ("/host/d".to_string(), "D:".to_string()),
            ("/footage".to_string(), "/Users/me/Movies".to_string()),
        ];
        assert_eq!(host_display("/projects/music", &map), r"D:\Vibecut\projects\music");
        assert_eq!(host_display("/host/d/Music/Road trip", &map), r"D:\Music\Road trip");
        assert_eq!(host_display("/host/d", &map), r"D:\");
        assert_eq!(host_display("/footage/x", &map), "/Users/me/Movies/x");
        assert_eq!(host_display("/projects-old/music", &map), "/projects-old/music");
        assert_eq!(host_display("/app/music", &map), "/app/music");
    }
}

#[derive(Debug, Deserialize)]
pub struct FolderBody {
    /// A display path (`D:\Music`) or a container path.
    pub path: String,
}

/// Add a folder on a drive to the library; answers with the folder and
/// the library as it is now (its songs analysed).
pub async fn add_music_folder(Json(body): Json<FolderBody>) -> Result<Json<serde_json::Value>, ApiError> {
    let added = vibecut_ops::music::add_folder(&body.path).await?;
    let mut lib = library_json().await?;
    lib["added"] = serde_json::to_value(&added).unwrap_or_default();
    Ok(Json(lib))
}

/// Take a picked folder out of the library (nothing on the drive changes).
pub async fn remove_music_folder(Json(body): Json<FolderBody>) -> Result<Json<serde_json::Value>, ApiError> {
    if !vibecut_ops::music::remove_folder(&body.path)? {
        return Err(OpsError::not_found_with("picked music folder", body.path).into());
    }
    Ok(Json(library_json().await?))
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
