//! `GET /api/frames/output`, `GET /api/frames/at` — wrap
//! `vibecut_ops::frames`. MCP returns extracted frames as inline
//! `Content::image`; REST has no equivalent, so each frame comes back
//! as a `data:image/jpeg;base64,...` URI a browser `<img>` can point
//! at directly — the packaging this endpoint owns, same role base64
//! plays on the MCP side.

use axum::extract::Query;
use axum::Json;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde::Deserialize;
use vibecut_ops::frames::FrameExtractionResult;

use crate::error::ApiError;

#[derive(Debug, Deserialize)]
pub struct OutputQuery {
    pub output_path: String,
    pub max_frames: Option<u32>,
}

pub async fn inspect_output(
    Query(q): Query<OutputQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let result = vibecut_ops::frames::inspect_output(&q.output_path, q.max_frames).await?;
    Ok(Json(frames_to_json(result)))
}

#[derive(Debug, Deserialize)]
pub struct AtQuery {
    pub output_path: String,
    /// Repeated query key: `?output_path=...&t=1.0&t=4.2&t=9.0`.
    #[serde(default)]
    pub t: Vec<f32>,
}

pub async fn inspect_at(Query(q): Query<AtQuery>) -> Result<Json<serde_json::Value>, ApiError> {
    let result = vibecut_ops::frames::inspect_at(&q.output_path, &q.t).await?;
    Ok(Json(frames_to_json(result)))
}

fn frames_to_json(result: FrameExtractionResult) -> serde_json::Value {
    let mut summary = result.summary_json();
    let inline: Vec<serde_json::Value> = result
        .frames
        .iter()
        .map(|f| {
            serde_json::json!({
                "t_secs": f.t_secs,
                "data_uri": format!("data:image/jpeg;base64,{}", B64.encode(&f.jpeg_bytes)),
            })
        })
        .collect();
    if let serde_json::Value::Object(map) = &mut summary {
        map.insert("frames_inline".into(), serde_json::Value::Array(inline));
    }
    summary
}
