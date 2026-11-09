//! `GET /api/output` — stream a file from under `/projects` (a
//! rendered mp4, typically) to the browser so a `<video>` element can
//! play it directly.
//!
//! Web-only: MCP clients get pixels via `inspect_output`/`inspect_at`
//! instead, which is why this lives here and not in `vibecut-ops`
//! — serving raw bytes with HTTP semantics (content-type) is a
//! REST-transport concern, not a shared operation.

use axum::extract::{Query, Request};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use vibecut_ops::error::OpsError;
use vibecut_ops::paths::{resolve_projects_checked, PROJECTS_ROOT};

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct OutputFileQuery {
    pub path: String,
}

pub async fn serve_output(
    Query(q): Query<OutputFileQuery>,
    req: Request,
) -> Result<Response, ApiError> {
    if !is_safe_projects_path(&q.path) {
        return Err(OpsError::invalid(format!(
            "path escapes the /projects mount: {:?}",
            q.path
        ))
        .into());
    }
    // Then the shared resolver: absolute paths elsewhere and symlinks
    // out of the mount are a 400 too.
    serve_file(resolve_projects_checked(&q.path)?, req).await
}

/// Stream `path` with full HTTP file semantics via tower-http's
/// `ServeFile`: `Range` requests (so `<video>`/`<audio>` can seek
/// without downloading the whole file) and conditional requests.
/// Callers check the path is allowed first.
///
/// The content type comes from a fixed list of media and data types,
/// never from guessing: anything else (an `.html` or `.svg` someone
/// dropped under /projects) is sent as an attachment, so it can't run
/// as a page on this origin.
pub async fn serve_file(path: std::path::PathBuf, req: Request) -> Result<Response, ApiError> {
    use axum::http::{header, HeaderValue};
    if !path.is_file() {
        return Err(OpsError::not_found_with("file", path.to_string_lossy().into_owned()).into());
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let known = content_type_for(&ext);
    match ServeFile::new(&path).oneshot(req).await {
        Ok(res) => {
            let mut res = res.into_response();
            let h = res.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(known.unwrap_or("application/octet-stream")),
            );
            h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            if known.is_none() {
                h.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment"));
            }
            Ok(res)
        }
        Err(e) => Err(OpsError::internal(format!("could not read {}: {e}", path.display())).into()),
    }
}

/// Media and data types `serve_file` will label; everything else
/// downloads as an opaque file.
fn content_type_for(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "aac" => "audio/aac",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "json" => "application/json",
        "toml" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// Pure, filesystem-free traversal guard: reject any `..` path
/// component outright, and require a rooted input to already sit
/// under [`PROJECTS_ROOT`] rather than resolving it as-is.
///
/// Deliberately does its own string-level "is this rooted" check
/// instead of `Path::is_absolute()` — that method is
/// platform-dependent, and the mismatch is exactly the kind of bug
/// that hides until it matters: on Windows, `Path::new("/etc/passwd")
/// .is_absolute()` is **false** (no drive prefix), which would have
/// let a POSIX-rooted path fall through to the "relative, get
/// anchored under PROJECTS_ROOT" branch as if it were harmless — on
/// Linux (where this actually runs, inside the container) the same
/// string IS absolute and IS the traversal this guard exists to
/// catch. `PROJECTS_ROOT` is always the container's POSIX path
/// regardless of dev host, so the check has to be POSIX-aware
/// unconditionally, not deferred to a platform-native primitive. A
/// dev-host unit test running on Windows caught this; see
/// `rejects_absolute_paths_outside_projects` below.
pub(crate) fn is_safe_projects_path(user_path: &str) -> bool {
    use std::path::{Component, Path};

    if Path::new(user_path)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return false;
    }
    // Also treat '\' as a separator, so a Windows-style "..\.." is
    // rejected on Linux too, where it would otherwise be one component.
    if user_path.split(['/', '\\']).any(|seg| seg == "..") {
        return false;
    }

    let normalized = user_path.replace('\\', "/");
    // Rooted = POSIX-absolute ("/...") or a Windows drive prefix
    // ("C:...", "D:...", …) — either way, must already live under
    // PROJECTS_ROOT to be allowed through.
    let looks_rooted = normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':');
    if looks_rooted {
        // Whole path components: "/projects-old/x.mp4" is a sibling of
        // the mount, not inside it.
        return normalized == PROJECTS_ROOT
            || normalized
                .strip_prefix(PROJECTS_ROOT)
                .is_some_and(|rest| rest.starts_with('/'));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_media_and_data_types_are_labelled() {
        assert_eq!(content_type_for("mp4"), Some("video/mp4"));
        assert_eq!(content_type_for("m4a"), Some("audio/mp4"));
        assert_eq!(content_type_for("html"), None);
        assert_eq!(content_type_for("svg"), None);
        assert_eq!(content_type_for("xml"), None);
    }

    #[test]
    fn rejects_parent_dir_traversal() {
        assert!(!is_safe_projects_path("../../etc/passwd"));
        assert!(!is_safe_projects_path("out/../../etc/passwd"));
        assert!(!is_safe_projects_path("..\\..\\Windows\\System32"));
    }

    #[test]
    fn rejects_absolute_paths_outside_projects() {
        assert!(!is_safe_projects_path("/etc/passwd"));
        assert!(!is_safe_projects_path("C:/Windows/System32"));
        assert!(!is_safe_projects_path("C:\\Windows\\System32"));
        assert!(!is_safe_projects_path("D:/anything"));
    }

    #[test]
    fn rejects_siblings_of_the_projects_mount() {
        // A string prefix isn't a path prefix: these sit next to /projects.
        assert!(!is_safe_projects_path("/projects-old/x.mp4"));
        assert!(!is_safe_projects_path("/projects2/x.mp4"));
        assert!(!is_safe_projects_path("/projectsX/a.mp4"));
        assert!(!is_safe_projects_path("/projects_backup/clip.mp4"));
        assert!(is_safe_projects_path("/projects"));
        assert!(is_safe_projects_path("/projects/"));
        assert!(is_safe_projects_path("\\projects\\out\\render.mp4"));
    }

    #[test]
    fn accepts_relative_and_projects_rooted_paths_lexically() {
        assert!(is_safe_projects_path("out/render.mp4"));
        assert!(is_safe_projects_path("render.mp4"));
        assert!(is_safe_projects_path("/projects/out/render.mp4"));
    }

    async fn status_for(path: &str) -> axum::http::StatusCode {
        let req = Request::new(axum::body::Body::empty());
        match serve_output(Query(OutputFileQuery { path: path.into() }), req).await {
            Ok(resp) => resp.status(),
            Err(e) => e.into_response().status(),
        }
    }

    #[tokio::test]
    async fn traversal_and_outside_paths_are_400() {
        for p in [
            "../../etc/passwd",
            "out/../../etc/passwd",
            r"..\..\Windows\System32",
            "/etc/passwd",
            "/proc/self/environ",
            "/projects-evil/render.mp4",
            "C:/Windows/System32",
            "D:/anything",
        ] {
            assert_eq!(status_for(p).await, axum::http::StatusCode::BAD_REQUEST, "{p:?}");
        }
    }

    #[tokio::test]
    async fn projects_paths_get_past_the_guard() {
        // Not on disk in a test environment, so a 404, not a 400.
        for p in ["out/render.mp4", "render.mp4", "/projects/out/render.mp4"] {
            assert_eq!(status_for(p).await, axum::http::StatusCode::NOT_FOUND, "{p:?}");
        }
    }
}
