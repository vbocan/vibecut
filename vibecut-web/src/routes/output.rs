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
use vibecut_ops::paths::{resolve_projects, PROJECTS_ROOT};

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct OutputFileQuery {
    pub path: String,
}

pub async fn serve_output(Query(q): Query<OutputFileQuery>) -> Result<Response, ApiError> {
    if !is_safe_projects_path(&q.path) {
        return Err(OpsError::invalid(format!(
            "path escapes the /projects mount: {:?}",
            q.path
        ))
        .into());
    }

    let resolved = resolve_projects(&q.path);
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
fn is_safe_projects_path(user_path: &str) -> bool {
    use std::path::{Component, Path};

    if Path::new(user_path)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return false;
    }

    let normalized = user_path.replace('\\', "/");
    // Rooted = POSIX-absolute ("/...") or a Windows drive prefix
    // ("C:...", "D:...", …) — either way, must already live under
    // PROJECTS_ROOT to be allowed through.
    let looks_rooted = normalized.starts_with('/') || normalized.as_bytes().get(1) == Some(&b':');
    if looks_rooted {
        return normalized.starts_with(PROJECTS_ROOT);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn accepts_relative_and_projects_rooted_paths() {
        assert!(is_safe_projects_path("out/render.mp4"));
        assert!(is_safe_projects_path("render.mp4"));
        assert!(is_safe_projects_path("/projects/out/render.mp4"));
    }
}
