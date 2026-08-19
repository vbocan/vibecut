//! Shared state every handler reaches through axum's `State` extractor.

use vibecut_ops::jobs::JobRegistry;
use vibecut_ops::logger::SessionLogger;

/// Mirrors what `VibecutMcpServer` holds — the job registry and the
/// same `SessionLogger` substrate, so a history panel can show web-UI
/// actions alongside MCP-conversation ones instead of the JSONL log
/// only ever reflecting Claude Desktop use.
#[derive(Clone, Default)]
pub struct AppState {
    pub jobs: JobRegistry,
    pub logger: SessionLogger,
}
