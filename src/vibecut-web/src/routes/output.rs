//! `GET /api/output` — stream a file from under `/projects` (a
//! rendered mp4, typically) to the browser so a `<video>` element can
//! play it directly.
//!
//! Web-only: MCP clients get pixels via `inspect_output`/`inspect_at`
//! instead, which is why this lives here and not in `vibecut-ops`
//! — serving raw bytes with HTTP semantics (content-type) is a
//! REST-transport concern, not a shared operation.

use axum::body::Body;
use axum::extract::Query;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use vibecut_ops::error::OpsError;
use vibecut_ops::paths::resolve_projects_checked;

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct OutputFileQuery {
    pub path: String,
}

pub async fn serve_output(Query(q): Query<OutputFileQuery>) -> Result<Response, ApiError> {
    // Confined to /projects (traversal, absolute paths elsewhere, and
    // symlinks out of the mount are all a 400) by the shared resolver.
    let resolved = resolve_projects_checked(&q.path)?;
    if !resolved.exists() {
        return Err(
            OpsError::not_found_with("output file", resolved.to_string_lossy().into_owned())
                .into(),
        );
    }

    let bytes = tokio::fs::read(&resolved).await.map_err(|e| {
        ApiError::from(OpsError::internal(format!(
            "could not read {}: {e}",
            resolved.display()
        )))
    })?;

    let content_type = match resolved.extension().and_then(|e| e.to_str()) {
        Some("mp4") => "video/mp4",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("json") => "application/json",
        Some("toml") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };

    Ok((StatusCode::OK, [(header::CONTENT_TYPE, content_type)], Body::from(bytes)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn status_for(path: &str) -> StatusCode {
        match serve_output(Query(OutputFileQuery { path: path.into() })).await {
            Ok(resp) => resp.status(),
            Err(e) => e.into_response().status(),
        }
    }

    #[tokio::test]
    async fn rejects_parent_dir_traversal() {
        for p in ["../../etc/passwd", "out/../../etc/passwd", "..\\..\\Windows\\System32"] {
            assert_eq!(status_for(p).await, StatusCode::BAD_REQUEST, "{p:?}");
        }
    }

    #[tokio::test]
    async fn rejects_absolute_paths_outside_projects() {
        for p in [
            "/etc/passwd",
            "/proc/self/environ",
            "/projects-evil/render.mp4",
            "C:/Windows/System32",
            "C:\\Windows\\System32",
            "D:/anything",
        ] {
            assert_eq!(status_for(p).await, StatusCode::BAD_REQUEST, "{p:?}");
        }
    }

    #[tokio::test]
    async fn projects_paths_get_past_the_guard() {
        // Not on disk in a test environment, so a 404 — not a 400.
        for p in ["out/render.mp4", "render.mp4", "/projects/out/render.mp4"] {
            assert_eq!(status_for(p).await, StatusCode::NOT_FOUND, "{p:?}");
        }
    }
}
