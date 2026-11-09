//! `OpsError` → HTTP translation.
//!
//! This is the REST-specific half of the same boundary
//! `vibecut-mcp` sits on the other side of. Where MCP's
//! string-returning tools surface `NotFound`/`InvalidInput` as a soft
//! `200 OK` body (because the LLM reasons over JSON either way), REST
//! callers expect real status codes — so this mapping is deliberately
//! different from `vibecut-mcp`'s `ops_error_to_string_result`, not
//! a copy of it: `NotFound` → 404, `InvalidInput` → 400, `Internal` →
//! 500, each with a small JSON body `{"error": "...", "hint": "..."}`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use vibecut_ops::error::OpsError;

pub struct ApiError(OpsError);

impl From<OpsError> for ApiError {
    fn from(e: OpsError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message, hint) = match self.0 {
            OpsError::NotFound { what, detail } => {
                (StatusCode::NOT_FOUND, format!("{what} not found"), detail)
            }
            OpsError::InvalidInput { message, hint } => (StatusCode::BAD_REQUEST, message, hint),
            OpsError::Internal(msg) => {
                // The caller only sees the JSON body; a 500 is a server
                // fault, so it goes in the server log too.
                tracing::error!(error = %msg, "request failed with 500");
                (StatusCode::INTERNAL_SERVER_ERROR, msg, None)
            }
        };
        let mut body = serde_json::json!({ "error": message });
        if let Some(h) = hint {
            body["hint"] = serde_json::Value::String(h);
        }
        (status, Json(body)).into_response()
    }
}
