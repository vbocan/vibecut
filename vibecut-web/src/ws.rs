//! `GET /api/ws/render/{job_id}` — push-based render progress.
//!
//! Not a new capability: [`vibecut_ops::render::render_status`]
//! already exposes this same state, polled by the MCP `render_status`
//! tool and by `GET /api/render/{job_id}`. This handler just saves a
//! browser client from having to poll itself — the server polls the
//! [`vibecut_ops::jobs::JobRegistry`] every 500 ms and pushes only
//! on change, closing the socket once the job reaches `done` or
//! `failed`. If this polling interval ever needs to shrink, the right
//! fix is a push-capable registry (a `tokio::sync::watch` per job)
//! inside `vibecut-ops` itself, so every transport benefits — not
//! a tighter loop here.

use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use vibecut_ops::jobs::JobState;

use crate::state::AppState;

pub async fn render_progress_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| stream_job(socket, state, job_id))
}

async fn stream_job(mut socket: WebSocket, state: AppState, job_id: String) {
    let mut last_sent: Option<String> = None;
    loop {
        let Some(st) = vibecut_ops::render::render_status(&state.jobs, &job_id).await else {
            let _ = socket
                .send(Message::Text(
                    serde_json::json!({ "error": format!("unknown job_id: {job_id}") })
                        .to_string()
                        .into(),
                ))
                .await;
            return;
        };

        let is_terminal = matches!(&st, JobState::Done { .. } | JobState::Failed { .. });
        let text = vibecut_ops::render::job_state_json(&job_id, st).to_string();

        if last_sent.as_deref() != Some(text.as_str()) {
            if socket.send(Message::Text(text.clone().into())).await.is_err() {
                return; // client disconnected
            }
            last_sent = Some(text);
        }

        if is_terminal {
            return; // job finished; nothing more to stream
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
