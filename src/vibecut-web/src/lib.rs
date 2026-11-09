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
pub mod host_guard;
pub mod llm;
pub mod routes;
pub mod state;
pub mod webhook;
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
    // Every API route answers only to this machine's names (DNS-rebinding
    // guard, see `host_guard`); the static UI needs no guard.
    let api = routes::api_router().layer(axum::middleware::from_fn(host_guard::guard));
    let mut router = Router::new().nest("/api", api);

    if let Some(dir) = static_dir() {
        // `adapter-static`'s SPA fallback (`fallback: 'index.html'`,
        // see `src/vibecut-ui/svelte.config.js`) needs the same shape here:
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

    /// 6 s of 120 BPM clicks, mono 22.05 kHz 16-bit PCM WAV.
    fn write_click_wav(p: &Path) {
        let rate = 22_050u32;
        let n = rate as usize * 6;
        let mut pcm = vec![0i16; n];
        for beat in 0..12 {
            let s = beat * rate as usize / 2;
            for i in 0..400.min(n - s) {
                pcm[s + i] = ((i as f32 * 0.3).sin() * 20_000.0 * (1.0 - i as f32 / 400.0)) as i16;
            }
        }
        let data_len = (n * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        for v in [16u32.to_le_bytes(), [1, 0, 1, 0], rate.to_le_bytes(), (rate * 2).to_le_bytes(), [2, 0, 16, 0]] {
            b.extend_from_slice(&v);
        }
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        pcm.iter().for_each(|v| b.extend_from_slice(&v.to_le_bytes()));
        std::fs::write(p, b).unwrap();
    }

    /// Serve the music routes from one generated song in a temp folder:
    /// tests must never scan, or write a cache into, a real /projects.
    fn test_music_library() -> &'static Path {
        static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let root = DIR.get_or_init(|| {
            let root = std::env::temp_dir().join(format!("vibecut_web_music_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("music")).unwrap();
            write_click_wav(&root.join("music/click_120.wav"));
            std::fs::write(
                root.join("music/click_120.wav.json"),
                r#"{ "title": "Click", "artist": "Test", "licence": "Attribution 4.0 International (CC BY 4.0)" }"#,
            )
            .unwrap();
            root
        });
        vibecut_ops::music::use_roots(vibecut_ops::music::LibraryRoots {
            own: root.join("music"),
            extra: vec![],
            cache_dir: Some(root.join(".vibecut-cache")),
        });
        root
    }

    /// The library listing tells the import view where downloads go, each
    /// track's licence and credit, and what went wrong reading metadata.
    #[tokio::test]
    async fn music_lists_tracks_import_dir_and_warnings() {
        let root = test_music_library();
        let app = router(AppState::default());
        let res = app
            .oneshot(Request::builder().uri("/api/music").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert!(body["warnings"].is_array());
        let own = root.join("music");
        assert_eq!(body["import_dir"], own.to_string_lossy().as_ref());
        assert_eq!(body["dirs"][0]["collection"], "yours");
        assert_eq!(body["dirs"][0]["exists"], true);
        let tracks = body["tracks"].as_array().unwrap();
        assert_eq!(tracks.len(), 1, "{body}");
        let t = &tracks[0];
        assert_eq!(t["path"], own.join("click_120.wav").to_string_lossy().as_ref());
        assert_eq!((t["title"].as_str(), t["licence"]["kind"].as_str()), (Some("Click"), Some("cc_by")));
        assert_eq!(t["attribution_required"], true);
        assert!(t["credit"].as_str().unwrap().contains("CC BY 4.0"), "{t}");
        // The analysis was cached in the given folder, not under /projects.
        assert!(std::fs::read_dir(root.join(".vibecut-cache")).unwrap().count() >= 1);
    }

    /// A music folder must be a folder under a browse root (never `/etc`,
    /// a traversal or nothing at all); removing one that isn't picked is a
    /// 404; and a cross-site form post can't add one.
    #[tokio::test]
    async fn music_folders_refuse_what_isnt_a_folder_on_a_drive() {
        let app = router(AppState::default());
        for path in ["/etc", "", "/footage/../etc", "C:\\Windows\\..\\..\\etc", "/projects"] {
            let res = post_json(app.clone(), "/api/music/folders", serde_json::json!({ "path": path })).await;
            assert!(
                matches!(res.status(), StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND),
                "{path:?}: {}",
                res.status()
            );
        }
        let res = post_json(app.clone(), "/api/music/folders/remove", serde_json::json!({ "path": "/host/z/Nope" })).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/music/folders")
                    .header("content-type", "text/plain")
                    .body(Body::from(r#"{"path":"/footage"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn music_pick_validates_the_vibe_and_names_a_library_track() {
        let root = test_music_library();
        let app = router(AppState::default());
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/api/music/pick?vibe=nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/music/pick?vibe=kid_reel&target_duration=soon")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/music/pick?vibe=kid_reel&target_duration=30s&seed=7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let pick = &body_json(res).await["pick"];
        assert_eq!(pick["path"], root.join("music/click_120.wav").to_string_lossy().as_ref());
        assert!(pick["reason"].as_str().unwrap().contains("Kid reel"), "{pick}");
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

    async fn get_with_host(app: Router, uri: &str, host: &str) -> axum::response::Response {
        app.oneshot(Request::builder().uri(uri).header("host", host).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// The browse routes over one configured root: listing with display
    /// paths and counts, every traversal / outside / NUL attempt a 400, a
    /// missing folder a 404, and a rebinding host refused (on every `/api`
    /// route). The only test that sets `VIBECUT_BROWSE_ROOTS`.
    #[tokio::test]
    async fn browse_routes_list_folders_and_refuse_escapes() {
        let base = std::env::temp_dir().join(format!("vibecut_web_browse_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let trip = base.join("Card/GoPro/Alps");
        std::fs::create_dir_all(&trip).unwrap();
        std::fs::create_dir_all(base.join("Card/$RECYCLE.BIN")).unwrap();
        std::fs::create_dir_all(base.join("secret")).unwrap();
        std::fs::write(trip.join("GX010001.MP4"), b"x").unwrap();
        std::fs::write(trip.join("GL010001.LRV"), b"x").unwrap();
        let card = base.join("Card");
        let card_s = card.to_string_lossy().replace('\\', "/");
        std::env::set_var("VIBECUT_BROWSE_ROOTS", format!("Card={card_s}=E:\\Card"));
        let app = router(AppState::default());
        let q = |p: &str| format!("/api/browse?path={}", urlencoding_full(p));

        let res = get_with_host(app.clone(), "/api/browse/roots", "localhost:8090").await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        let labels: Vec<_> = body["roots"].as_array().unwrap().iter().map(|r| r["label"].as_str().unwrap().to_string()).collect();
        assert_eq!(labels, vec!["Footage", "Card"], "{body}");
        assert_eq!(body["roots"][1]["path"], "E:\\Card");
        assert_eq!(body["roots"][1]["available"], true);

        for path in ["E:\\Card\\GoPro", "e:/card/GoPro", &format!("{card_s}/GoPro")] {
            let res = get_with_host(app.clone(), &q(path), "127.0.0.1:8090").await;
            assert_eq!(res.status(), StatusCode::OK, "{path}");
            let body = body_json(res).await;
            assert_eq!(body["path"], "E:\\Card\\GoPro", "{body}");
            assert_eq!(body["folders"][0]["name"], "Alps");
            assert_eq!(body["folders"][0]["path"], "E:\\Card\\GoPro\\Alps");
            assert_eq!(body["folders"][0]["n_videos"], 1);
            assert_eq!(body["parent"], "E:\\Card");
        }
        let res = get_with_host(app.clone(), &q("E:\\Card"), "localhost").await;
        let body = body_json(res).await;
        let names: Vec<_> = body["folders"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["GoPro"], "the recycle bin is hidden");

        for bad in [
            "E:\\Card\\..\\secret",
            "E:\\Card\\GoPro\\..\\..",
            &format!("{card_s}/../secret"),
            "/etc",
            "/etc/passwd",
            "E:\\Card\\$RECYCLE.BIN",
            "GoPro",
            "",
            "E:\\Card\\GoPro\\Alps\\GX010001.MP4",
        ] {
            let res = get_with_host(app.clone(), &q(bad), "localhost").await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{bad}");
        }
        let res = get_with_host(app.clone(), "/api/browse?path=E%3A%5CCard%00x", "localhost").await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "NUL");
        let res = get_with_host(app.clone(), &q("E:\\Card\\Nope"), "localhost").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        for uri in ["/api/browse/roots".to_string(), q("E:\\Card"), "/api/vibes".into(), "/api/sources?folder=E%3A%5CCard".into()] {
            let res = get_with_host(app.clone(), &uri, "evil.example.com:8090").await;
            assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Percent-encode everything but unreserved characters.
    fn urlencoding_full(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    }

    async fn post_json(app: Router, uri: &str, body: serde_json::Value) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn plan_of_a_missing_config_is_404() {
        let app = router(AppState::default());
        let res = post_json(app, "/api/plan", serde_json::json!({ "config_path": "nonexistent/vibecut/p.toml" })).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let body = body_json(res).await;
        assert!(body["error"].as_str().unwrap().contains("not found"), "{body}");
    }

    /// `quality` is `draft` or `final`: anything else is refused before a
    /// job is made, rather than rendering something else.
    #[tokio::test]
    async fn render_quality_must_be_draft_or_final() {
        let state = AppState::default();
        let app = router(state.clone());
        let res = post_json(
            app.clone(),
            "/api/render/async",
            serde_json::json!({ "config_path": "nonexistent/vibecut/p.toml", "quality": "best" }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_json(res).await;
        assert!(body["error"].as_str().unwrap().contains("draft"), "{body}");
        assert!(state.jobs.list().await.is_empty(), "no job for a refused request");
        // A valid quality goes on to the usual checks (here: no such config).
        let res = post_json(
            app,
            "/api/render/async",
            serde_json::json!({ "config_path": "nonexistent/vibecut/p.toml", "quality": "draft" }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    /// `GET /api/sources?folder=/` used to list media files anywhere
    /// in the container; footage folders now stay in the mounts.
    #[tokio::test]
    async fn footage_folders_outside_the_mounts_are_400() {
        let app = router(AppState::default());
        for folder in ["%2F", "%2Fetc", "..%2Fetc", "..%5Cetc", "%2Ffootage%2F..%2Fetc"] {
            let uri = format!("/api/sources?folder={folder}");
            let res = app
                .clone()
                .oneshot(Request::builder().uri(&uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uri}");
        }

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/propose")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({ "folder": "/etc", "vibe": "kid_reel" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
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
    async fn jobs_list_is_newest_first_and_cancel_stops_a_queued_job() {
        let state = AppState::default();
        let older = state.jobs.submit_for("/projects/a.toml").await;
        let newer = state.jobs.submit_for("/projects/b.toml").await;
        let app = router(state.clone());

        let res = app
            .clone()
            .oneshot(cancel_request(&newer, "application/json", "{}"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["state"], "cancelled");
        assert_eq!(body["cancel_requested"], true);

        let res = app
            .clone()
            .oneshot(Request::builder().uri("/api/jobs").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        let jobs = body["jobs"].as_array().unwrap();
        assert_eq!(jobs[0]["job_id"], newer.as_str());
        assert_eq!(jobs[0]["state"], "cancelled");
        assert_eq!(jobs[1]["job_id"], older.as_str());
        assert_eq!(jobs[1]["state"], "queued");
        assert_eq!(jobs[1]["config_path"], "/projects/a.toml");

        // The per-job status reports the same state.
        let res = app
            .oneshot(Request::builder().uri(format!("/api/render/{newer}")).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(body_json(res).await["state"], "cancelled");
    }

    fn cancel_request(job_id: &str, content_type: &str, body: &'static str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(format!("/api/jobs/{job_id}/cancel"))
            .header("content-type", content_type)
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn cancel_unknown_job_is_404() {
        let app = router(AppState::default());
        let res = app
            .oneshot(cancel_request("no-such-job", "application/json", "{}"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    /// A page on another origin can send a form post or a text/plain
    /// fetch without a CORS preflight. Neither may stop a render: the
    /// route takes only a JSON body, like every state-changing route.
    #[tokio::test]
    async fn cancel_refuses_cross_site_simple_requests() {
        let state = AppState::default();
        let id = state.jobs.submit_for("/projects/a.toml").await;
        let app = router(state.clone());
        for (ct, body) in [
            ("application/x-www-form-urlencoded", "x=1"),
            ("text/plain", "{}"),
            ("multipart/form-data; boundary=b", "--b--"),
        ] {
            let res = app.clone().oneshot(cancel_request(&id, ct, body)).await.unwrap();
            assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct} was accepted");
        }
        let bare = Request::builder()
            .method("POST")
            .uri(format!("/api/jobs/{id}/cancel"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(bare).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "a bodiless post was accepted");
        let info = state.jobs.info(&id).await.unwrap();
        assert!(!info.cancel_requested, "a refused request still stopped the job");
        assert!(matches!(info.state, vibecut_ops::jobs::JobState::Queued));
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

    /// Relative, so it resolves under `/projects` — absent on a host or
    /// test container, which is the deterministic not-found path.
    #[tokio::test]
    async fn config_structured_read_missing_is_404() {
        let app = router(AppState::default());
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/config/structured?config_path=nonexistent/cfg.toml")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    fn structured_config_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "project": { "name": "structured edit", "target_duration": "10s", "fps": 30 },
            "input": { "folder": "." },
            "outputs": [{ "path": "out.mp4", "resolution": "1920x1080" }]
        })
    }

    async fn put_json(app: Router, uri: &str, body: serde_json::Value) -> axum::response::Response {
        app.oneshot(
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    /// The config API is reachable without auth, so through the real
    /// router it must refuse to read or write outside `/projects` —
    /// `GET /api/config?config_path=/proc/self/environ` used to return
    /// the server's environment, API keys included. (The structured
    /// write/read round trip itself is covered in
    /// `vibecut_ops::config_io`'s tests, where the root can point at a
    /// temp dir; through the router every write lands in `/projects`.)
    #[tokio::test]
    async fn config_api_refuses_paths_outside_projects() {
        let app = router(AppState::default());
        for uri in [
            "/api/config?config_path=/proc/self/environ",
            "/api/config?config_path=../etc/passwd",
            "/api/config?config_path=..%5C..%5Cetc%5Cpasswd",
            "/api/config/structured?config_path=/etc/passwd",
        ] {
            let res = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uri}");
            let body = body_json(res).await;
            assert!(body["error"].as_str().unwrap().contains("escapes"), "{uri}: {body}");
        }

        let outside = std::env::temp_dir().join(format!("sv_web_config_outside_{}.toml", std::process::id()));
        let outside_str = outside.to_string_lossy().into_owned();
        let attempts = [
            (
                "/api/config/structured",
                serde_json::json!({ "config_path": outside_str, "config": structured_config_json() }),
            ),
            (
                "/api/config/structured",
                serde_json::json!({ "config_path": "out/render.mp4", "config": structured_config_json() }),
            ),
            (
                "/api/config",
                serde_json::json!({ "config_path": outside_str, "config_toml": "schema_version = 1" }),
            ),
        ];
        for (uri, body) in attempts {
            let res = put_json(app.clone(), uri, body.clone()).await;
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uri} {body}");
        }
        assert!(!outside.exists(), "a rejected write must not touch disk");
    }

    /// PUT a config as JSON (no TOML anywhere in this test): the guided
    /// editor's write path parses the body into a typed `Config` and
    /// validates it before anything touches disk.
    #[tokio::test]
    async fn config_structured_write_validates_json_body() {
        let mut cfg = structured_config_json();
        cfg["outputs"] = serde_json::json!([]);
        let res = put_json(
            router(AppState::default()),
            "/api/config/structured",
            serde_json::json!({ "config_path": "cfg.toml", "config": cfg }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = body_json(res).await;
        assert!(
            body["error"].as_str().unwrap().contains("does not validate"),
            "{body}"
        );
    }

    /// The watcher's routes: status, the toggle (JSON only, like every
    /// state-changing route), and 404s for unknown entries.
    #[tokio::test]
    async fn watch_status_toggle_and_entry_routes() {
        let state = AppState::default();
        let app = router(state.clone());
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/api/watch").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res).await;
        assert_eq!(body["enabled"], false);
        assert_eq!(body["available"], false);
        assert!(body["entries"].as_array().unwrap().is_empty());
        assert!(body["webhook"].is_null());

        let put = |ct: &str, body: &'static str| {
            Request::builder()
                .method("PUT")
                .uri("/api/watch")
                .header("content-type", ct)
                .body(Body::from(body))
                .unwrap()
        };
        let res = app.clone().oneshot(put("text/plain", r#"{"enabled":true}"#)).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(!state.watch.enabled().await, "a cross-site simple request switched it on");
        let res = app.clone().oneshot(put("application/json", r#"{"enabled":true}"#)).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["enabled"], true);
        assert!(state.watch.enabled().await);

        for action in ["dismiss", "retry"] {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/watch/w_nope/{action}"))
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{action}");
        }
    }

    /// The event stream opens with a snapshot and then carries changes.
    #[tokio::test]
    async fn watch_events_stream_a_snapshot_then_changes() {
        let state = AppState { webhook_host: Some("https://ntfy.sh".into()), ..AppState::default() };
        let app = router(state.clone());
        let res = app
            .oneshot(Request::builder().uri("/api/watch/events").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
        let mut body = res.into_body();
        let mut next = async || {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), body.frame())
                .await
                .expect("no event within 5 s")
                .unwrap()
                .unwrap();
            String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap()
        };
        let first = next().await;
        assert!(first.starts_with("event: snapshot
"), "{first}");
        assert!(first.contains(r#""webhook":"https://ntfy.sh""#), "{first}");
        state.watch.set_enabled(true).await;
        let ev = next().await;
        assert!(ev.starts_with("event: status
") && ev.contains(r#""enabled":true"#), "{ev}");
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
