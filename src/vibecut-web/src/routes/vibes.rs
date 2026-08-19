//! `GET /api/vibes` — wraps `vibecut_ops::propose::list_vibes`.

use axum::Json;

pub async fn list_vibes() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "vibes": vibecut_ops::propose::list_vibes() }))
}
