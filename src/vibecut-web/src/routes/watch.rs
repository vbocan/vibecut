//! The folder watcher: `GET /api/watch` (on/off, what is watched, the
//! "New footage found" list), `PUT /api/watch` (`{ "enabled": bool }`),
//! `POST /api/watch/{id}/dismiss` and `/retry`, and
//! `GET /api/watch/events` — a server-sent event stream: `snapshot` (the
//! same body as `GET /api/watch`, first and whenever the client fell
//! behind), then `status`, `entry`, `removed` and `reel_ready` as they
//! happen. Thin wrappers over `vibecut_ops::watch::WatchService`.

use std::convert::Infallible;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use futures_util::stream::{self, Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use vibecut_ops::error::OpsError;
use vibecut_ops::watch::WatchEvent;

use crate::error::ApiError;
use crate::state::AppState;

/// The watcher's status plus `webhook`: where finished reels are posted
/// (scheme and host only), or null.
async fn status(state: &AppState) -> serde_json::Value {
    let mut v = state.watch.status_json().await;
    v["webhook"] = state.webhook_host.clone().map_or(serde_json::Value::Null, Into::into);
    v
}

pub async fn get_watch(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(status(&state).await)
}

#[derive(Debug, Deserialize)]
pub struct WatchToggle {
    pub enabled: bool,
}

/// Switch the watcher on or off (remembered across restarts).
pub async fn put_watch(State(state): State<AppState>, Json(body): Json<WatchToggle>) -> Json<serde_json::Value> {
    let started = Instant::now();
    state.watch.set_enabled(body.enabled).await;
    let v = status(&state).await;
    state.logger.record(
        "watch",
        serde_json::json!({ "enabled": body.enabled }),
        serde_json::json!({ "enabled": v["enabled"], "available": v["available"] }),
        started.elapsed().as_millis() as u64,
        true,
    );
    Json(v)
}

type EntryResult = Result<Option<vibecut_ops::watch::WatchEntry>, &'static str>;

fn entry_response(id: String, r: EntryResult) -> Result<Json<serde_json::Value>, ApiError> {
    match r {
        Ok(Some(e)) => Ok(Json(serde_json::to_value(e).unwrap_or_default())),
        Ok(None) => Err(OpsError::not_found_with("watched folder", id).into()),
        Err(why) => Err(OpsError::invalid(why).into()),
    }
}

/// Take an entry off the list (a waiting or queued one is then never
/// rendered). JSON body (`{}`), like every state-changing route.
pub async fn dismiss(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(_body): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let r = state.watch.dismiss(&id).await;
    entry_response(id, r)
}

/// Queue a failed, stopped or skipped folder again.
pub async fn retry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(_body): Json<serde_json::Map<String, serde_json::Value>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let r = state.watch.retry(&id).await;
    entry_response(id, r)
}

fn sse_event(ev: &WatchEvent) -> Event {
    Event::default().event(ev.name()).data(serde_json::to_string(ev).unwrap_or_default())
}

async fn snapshot_event(state: &AppState) -> Event {
    Event::default().event("snapshot").data(status(state).await.to_string())
}

/// The live feed. Subscribes before reading the snapshot, so nothing
/// that happens in between is missed.
pub async fn events(State(state): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.watch.subscribe();
    let first = snapshot_event(&state).await;
    let rest = stream::unfold((rx, state), |(mut rx, state)| async move {
        let ev = match rx.recv().await {
            Ok(ev) => sse_event(&ev),
            Err(RecvError::Lagged(_)) => snapshot_event(&state).await,
            Err(RecvError::Closed) => return None,
        };
        Some((Ok(ev), (rx, state)))
    });
    Sse::new(stream::once(async move { Ok::<_, Infallible>(first) }).chain(rest)).keep_alive(KeepAlive::default())
}
