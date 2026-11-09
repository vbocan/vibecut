//! Route table. One module per `vibecut-ops` module it wraps, same
//! grouping the crate-level docs describe.

pub mod brief;
pub mod config;
pub mod discover;
pub mod feedback;
pub mod frames;
pub mod jobs;
pub mod music;
pub mod output;
pub mod propose;
pub mod reformat;
pub mod render;
pub mod sessions;
pub mod vibes;
pub mod watch;

use axum::routing::{get, post};
use axum::{Json, Router};

use crate::state::AppState;

/// The `/api/*` route table, without the `/api` prefix itself — that's
/// added once by [`crate::router`] so this stays mountable at any
/// prefix a future caller (e.g. a merged vibecut-mcp container)
/// wants.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/sources", get(discover::discover_sources))
        .route("/footage", get(discover::list_footage))
        .route("/vibes", get(vibes::list_vibes))
        .route("/propose", post(propose::propose_vibe))
        .route(
            "/config",
            get(config::read_config).put(config::write_config),
        )
        .route(
            "/config/structured",
            get(config::read_config_structured).put(config::write_config_structured),
        )
        .route("/feedback", post(feedback::apply_feedback))
        .route("/brief", post(brief::submit_brief))
        .route("/render", post(render::render_sync))
        .route("/render/async", post(render::render_async))
        .route("/render/{job_id}", get(render::render_status))
        .route("/jobs", get(jobs::list_jobs))
        .route("/jobs/{job_id}/cancel", post(jobs::cancel_job))
        .route("/frames/output", get(frames::inspect_output))
        .route("/frames/at", get(frames::inspect_at))
        .route("/output", get(output::serve_output))
        .route("/reformat", post(reformat::reformat))
        .route("/music", get(music::list_music))
        .route("/music/file", get(music::serve_music_file))
        .route("/music/pick", get(music::pick_music))
        .route("/sessions", get(sessions::list_sessions))
        .route("/watch", get(watch::get_watch).put(watch::put_watch))
        .route("/watch/events", get(watch::events))
        .route("/watch/{id}/dismiss", post(watch::dismiss))
        .route("/watch/{id}/retry", post(watch::retry))
        .route("/ws/render/{job_id}", get(crate::ws::render_progress_ws))
}

async fn health() -> Json<serde_json::Value> {
    // `ai`: whether the free-text brief (POST /brief) can work, so the UI
    // can hide or explain the "Ask AI" box instead of failing on submit.
    let ai = std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.trim().is_empty());
    Json(serde_json::json!({ "ok": true, "service": "vibecut-web", "ai": ai }))
}
