//! Transport-free error type.
//!
//! Every op in this crate returns `Result<T, OpsError>`. Callers own
//! the translation into their transport's error shape — the MCP layer
//! maps this to `rmcp::model::ErrorData`, a future REST layer would
//! map it to an HTTP status + JSON body. This crate has no opinion on
//! either.

use std::fmt;

#[derive(Debug)]
pub enum OpsError {
    /// The caller pointed at something that doesn't exist: a footage
    /// folder, a config file, a manifest, a rendered mp4, a job id, an
    /// unknown vibe key.
    NotFound {
        what: String,
        detail: Option<String>,
    },
    /// The input was malformed independent of whether the referenced
    /// resource exists — a non-UTF-8 path, a NaN timestamp, an
    /// out-of-range scale factor.
    InvalidInput {
        message: String,
        hint: Option<String>,
    },
    /// Something failed inside the pipeline, the filesystem, or ffmpeg
    /// that isn't diagnosable from the caller's arguments alone.
    Internal(String),
}

impl OpsError {
    pub fn not_found(what: impl Into<String>) -> Self {
        Self::NotFound {
            what: what.into(),
            detail: None,
        }
    }

    pub fn not_found_with(what: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::NotFound {
            what: what.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
            hint: None,
        }
    }

    pub fn invalid_with_hint(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::InvalidInput {
            message: message.into(),
            hint: Some(hint.into()),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

impl fmt::Display for OpsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { what, detail } => {
                write!(f, "not found: {what}")?;
                if let Some(d) = detail {
                    write!(f, " ({d})")?;
                }
                Ok(())
            }
            Self::InvalidInput { message, hint } => {
                write!(f, "invalid input: {message}")?;
                if let Some(h) = hint {
                    write!(f, " — {h}")?;
                }
                Ok(())
            }
            Self::Internal(m) => write!(f, "internal error: {m}"),
        }
    }
}

impl std::error::Error for OpsError {}
