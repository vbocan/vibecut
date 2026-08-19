//! vibecut-web binary.
//!
//! Standalone REST + WebSocket server over `vibecut-ops`, for
//! direct browser control instead of an LLM conversation. Runs on its
//! own port for now; a later phase folds this into the same container
//! as `vibecut-mcp` alongside the built SvelteKit static assets once
//! those exist.
//!
//! The free-text brief feature needs `ANTHROPIC_API_KEY` set in the
//! environment — see `src/llm.rs`. Without it, `/api/brief` returns a
//! clear error rather than the server failing to start; the rest of
//! the app works fine unconfigured.

use std::net::SocketAddr;

use vibecut_ops::logger::SessionLogger;
use vibecut_web::AppState;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // vibecut_ops included explicitly — SessionLogger and
                // every op's own tracing calls live in that crate, and
                // an EnvFilter with only one target directive silently
                // drops events from every other target by default.
                .unwrap_or_else(|_| "vibecut_web=info,vibecut_ops=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let bind_addr: SocketAddr = std::env::var("WEB_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_string())
        .parse()?;

    let state = AppState {
        logger: SessionLogger::open("vibecut-web", env!("CARGO_PKG_VERSION")),
        ..Default::default()
    };
    let app = vibecut_web::router(state);

    tracing::info!("vibecut-web starting on {bind_addr}");
    tracing::info!("  GET/POST http://{bind_addr}/api/*");
    tracing::info!("  WS       ws://{bind_addr}/api/ws/render/{{job_id}}");

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
