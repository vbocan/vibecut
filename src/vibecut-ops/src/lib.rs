//! Transport-free operations layer for Vibecut.
//!
//! Every operation the MCP
//! server exposes as a tool — discover, list/propose vibes, apply
//! feedback, render (sync + async), inspect frames — lives here as a
//! plain async function that knows nothing about MCP, HTTP, or any
//! other transport. `vibecut-mcp` calls into this crate and
//! translates [`error::OpsError`] into `rmcp::model::ErrorData`; a
//! future `vibecut-web` calls the exact same functions
//! and translates the same error into an HTTP response.
//!
//! This is a pure extraction — the logic is unchanged from what used
//! to live directly in `vibecut-mcp`'s tool implementations, only
//! the transport-specific packaging (MCP `Content`, `ErrorData`,
//! `ToolRouter`) was left behind in that crate.

pub mod config_io;
pub mod discover;
pub mod error;
pub mod feedback;
pub mod footage;
pub mod frames;
pub mod jobs;
pub mod logger;
pub mod music;
pub mod paths;
pub mod plan;
pub mod propose;
pub mod recipe;
pub mod reformat;
pub mod render;
pub mod vibes;
