//! Vibecut MCP server binary.
//!
//! Speaks Streamable HTTP (MCP 2025-06-18 spec) on port 8080 by default.
//! Long-lived: run as `docker run -d` and point Claude Desktop / LM Studio
//! at the URL. The container preserves the visual-analysis cache hot
//! across conversations, so re-renders skip the 22-min cold pass.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{routing::{delete, get, post}, Router};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager,
    tower::{StreamableHttpServerConfig, StreamableHttpService},
};
use vibecut_mcp::VibecutMcpServer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // vibecut_ops included explicitly since the
                // SessionLogger move — its tracing calls (and every
                // other op's) live in that crate now, not this one.
                .unwrap_or_else(|_| "vibecut_mcp=info,vibecut_ops=info,rmcp=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let bind_addr: SocketAddr = std::env::var("MCP_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()?;

    tracing::info!("vibecut-mcp starting on {bind_addr}");
    vibecut_ops::music::warm_up();

    // The factory is invoked once per session by the tower service.
    let service_factory = move || Ok::<_, std::io::Error>(VibecutMcpServer::new());

    let session_manager = Arc::new(LocalSessionManager::default());
    let streamable_config = StreamableHttpServerConfig {
        sse_keep_alive: Some(std::time::Duration::from_secs(30)),
        stateful_mode: true,
    };
    let streamable_service = StreamableHttpService::new(
        service_factory,
        session_manager,
        streamable_config,
    );

    // Streamable HTTP exposes one URL accepting POST/GET/DELETE — same
    // shape qrng-mcp uses, same shape Claude Desktop and LM Studio
    // expect for a 2025-06-18 server.
    let app = Router::new()
        .route("/", post({
            let svc = streamable_service.clone();
            move |req| async move {
                let mut svc = svc.clone();
                tower::Service::call(&mut svc, req).await
            }
        }))
        .route("/", get({
            let svc = streamable_service.clone();
            move |req| async move {
                let mut svc = svc.clone();
                tower::Service::call(&mut svc, req).await
            }
        }))
        .route("/", delete({
            let svc = streamable_service.clone();
            move |req| async move {
                let mut svc = svc.clone();
                tower::Service::call(&mut svc, req).await
            }
        }));

    tracing::info!("Streamable HTTP endpoints:");
    tracing::info!("  POST   http://{bind_addr}/");
    tracing::info!("  GET    http://{bind_addr}/");
    tracing::info!("  DELETE http://{bind_addr}/");

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
