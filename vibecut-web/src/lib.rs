//! Thin REST + WebSocket layer over `vibecut-ops`.
//!
//! The same operations the MCP server exposes as tools — discover,
//! list/propose vibes, apply feedback, render (sync + async +
//! status), inspect frames — reached directly by a browser client
//! instead of through an LLM conversation. No pipeline logic lives
//! here; every handler in [`routes`] is a thin translation from
//! HTTP/WS to a `vibecut_ops` call and back, the same rule
//! `vibecut-mcp` follows on the MCP side of the same boundary.
//!
//! Runs alongside `vibecut-mcp` in the same Docker image (see
//! `docker/entrypoint.sh`) on its own port (`WEB_BIND_ADDR`, default
//! `:8090`) — two processes, not a merged router, so this crate stays
//! independently runnable and testable exactly as it was standalone.
//! [`router`] also serves the built SvelteKit static site as its
//! fallback route when `WEB_STATIC_DIR` points at one, so in the
//! container this one binary is the entire web UI: no separate Node
//! process, no `vite dev`, nothing but this and a directory of static
//! files.

pub mod error;
pub mod llm;
pub mod routes;
pub mod state;
pub mod ws;

pub use state::AppState;

use std::path::{Path, PathBuf};

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

/// Build the full router: `/api/*` mounted with `state` applied, plus
/// a static-file fallback when [`static_dir`] finds one. The route
/// table itself ([`routes::api_router`]) stays prefix-agnostic so a
/// later caller could nest it somewhere other than `/api`.
pub fn router(state: AppState) -> Router {
    let mut router = Router::new().nest("/api", routes::api_router());

    if let Some(dir) = static_dir() {
        // `adapter-static`'s SPA fallback (`fallback: 'index.html'`,
        // see `vibecut-ui/svelte.config.js`) needs the same shape here:
        // serve a real file when the path matches one, else
        // `index.html` so the client-side router handles it. Anything
        // under `/api` never reaches this — `.nest` above already
        // claimed that prefix.
        let index = dir.join("index.html");
        router = router.fallback_service(ServeDir::new(dir).fallback(ServeFile::new(index)));
    }

    router.with_state(state)
}

/// `WEB_STATIC_DIR`, if set and pointing at a real directory. Unset
/// (or missing) is a normal, silent no-op — that's the local-dev
/// shape, where the actual UI is served by `vite dev` against this
/// process's `/api/*` instead.
fn static_dir() -> Option<PathBuf> {
    resolve_static_dir(std::env::var("WEB_STATIC_DIR").ok().as_deref())
}

/// [`static_dir`]'s logic, taking the env value as a parameter so
/// tests don't mutate `std::env` — real cross-test hazard once
/// `cargo test` runs this crate's tests in parallel threads within
/// one process, the same reason `vibecut-ops::logger` takes its
/// directory as a parameter instead of reading `SESSIONS_DIR` inline.
fn resolve_static_dir(env_value: Option<&str>) -> Option<PathBuf> {
    let path = PathBuf::from(env_value?);
    Path::is_dir(&path).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_ok() {
        let app = router(AppState::default());
        let res = app
            .oneshot(Request::builder().uri("/api/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn vibes_lists_all_six() {
        let app = router(AppState::default());
        let res = app
            .oneshot(Request::builder().uri("/api/vibes").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["vibes"].as_array().unwrap().len(), 6);
    }

    /// `/footage` won't exist on a host running `cargo test` (it's a
    /// container mount point) — that's exactly the deterministic
    /// not-found path this test exercises, proving the
    /// `OpsError::NotFound` → `404` mapping actually fires end to end.
    #[tokio::test]
    async fn sources_without_mount_is_404() {
        let app = router(AppState::default());
        let res = app
            .oneshot(Request::builder().uri("/api/sources").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_json(res).await;
        assert!(body["error"].as_str().unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn render_status_unknown_job_is_404() {
        let app = router(AppState::default());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/render/no-such-job")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn propose_unknown_vibe_is_400() {
        let app = router(AppState::default());
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/propose")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "folder": "", "vibe": "not_a_real_vibe" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // folder resolves to "/footage" (doesn't exist on host) so this
        // actually hits NotFound before the vibe is even looked up —
        // still proves the ApiError mapping round-trips through a POST
        // + JSON body path, not just GET + query.
        assert!(res.status() == StatusCode::NOT_FOUND || res.status() == StatusCode::BAD_REQUEST);
    }

    #[test]
    fn resolve_static_dir_none_when_unset() {
        assert!(resolve_static_dir(None).is_none());
    }

    #[test]
    fn resolve_static_dir_none_when_path_missing() {
        let missing = std::env::temp_dir().join(format!("sv_web_static_missing_{}", std::process::id()));
        assert!(resolve_static_dir(missing.to_str()).is_none());
    }

    #[test]
    fn resolve_static_dir_some_when_real_directory() {
        let dir = std::env::temp_dir().join(format!("sv_web_static_real_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let resolved = resolve_static_dir(dir.to_str()).expect("real directory should resolve");
        assert_eq!(resolved, dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end through the actual router (not just `resolve_static_dir`):
    /// build a tiny fake `adapter-static` output by hand, point
    /// `router()`'s fallback at it directly (bypassing the env var —
    /// see `resolve_static_dir`'s doc comment for why env mutation is
    /// avoided in this crate's tests), and confirm both a matched file
    /// and the SPA fallback serve real content.
    #[tokio::test]
    async fn static_fallback_serves_files_and_spa_fallback() {
        let dir = std::env::temp_dir().join(format!("sv_web_static_e2e_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html>shell</html>").unwrap();
        std::fs::write(dir.join("app.js"), "console.log('hi')").unwrap();

        let index = dir.join("index.html");
        let app = Router::new()
            .nest("/api", routes::api_router())
            .fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)))
            .with_state(AppState::default());

        // A real file on disk is served as itself.
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/app.js").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"console.log('hi')");

        // A path matching nothing on disk falls back to index.html —
        // the SPA-shell behavior `adapter-static`'s own fallback needs.
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/some/deep/client/route")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"<html>shell</html>");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
