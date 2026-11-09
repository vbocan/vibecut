//! `GET /api/export` — the timeline of a finished render as a file
//! another editor opens: `format=fcpxml` (Final Cut Pro, DaVinci Resolve)
//! or `format=otio` (OpenTimelineIO), built by
//! `vibecut_ops::timeline_export` from the render's manifest and pointing
//! at the original footage. `fps` (optional: `29.97`, `25`, `30000/1001`)
//! sets the sequence rate; the render's own by default.
//!
//! Always a download (`Content-Disposition: attachment`, `nosniff`): an
//! XML file served inline could run as a page on this origin.

use axum::extract::Query;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use vibecut_ops::error::OpsError;
use vibecut_ops::timeline_export::ExportedTimeline;

use crate::error::ApiError;
use crate::routes::output::is_safe_projects_path;

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    /// A rendered video (its `output_path` or `version_path`), one of its
    /// platform versions, or its manifest.
    pub output_path: String,
    /// `fcpxml` or `otio`.
    pub format: String,
    #[serde(default)]
    pub fps: Option<String>,
}

pub async fn export_timeline(Query(q): Query<ExportQuery>) -> Result<Response, ApiError> {
    if !is_safe_projects_path(&q.output_path) {
        return Err(OpsError::invalid(format!("path escapes the /projects mount: {:?}", q.output_path)).into());
    }
    let t = vibecut_ops::timeline_export::export_timeline_async(&q.output_path, &q.format, q.fps.as_deref()).await?;
    Ok(download(t))
}

/// The export as a file download.
fn download(t: ExportedTimeline) -> Response {
    let content_type = format!("{}; charset=utf-8", t.format.content_type());
    let mut res = t.body.into_response();
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&content_type) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&t.file_name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// `attachment` with an ASCII `filename` for old clients and the exact
/// UTF-8 name as `filename*` (RFC 6266 / 5987).
fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| if (c.is_ascii_graphic() || c == ' ') && c != '"' && c != '\\' { c } else { '_' })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use vibecut_ops::timeline_export::ExportFormat;

    use crate::{router, AppState};

    async fn get(uri: &str) -> (StatusCode, serde_json::Value) {
        let res = router(AppState::default())
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    #[tokio::test]
    async fn paths_outside_projects_are_rejected() {
        for p in ["../../etc/passwd", "/etc/x.mp4", "/projects_backup/x.mp4", "C:/Windows/x.mp4"] {
            let (s, _) = get(&format!("/api/export?format=otio&output_path={}", p.replace('/', "%2F"))).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{p}");
        }
    }

    #[tokio::test]
    async fn bad_formats_and_rates_are_400_and_a_missing_render_is_404() {
        let (s, body) = get("/api/export?format=edl&output_path=out%2Fx.mp4").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body["hint"].as_str().unwrap().contains("fcpxml"), "{body}");
        let (s, _) = get("/api/export?format=otio&fps=fast&output_path=out%2Fx.mp4").await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = get("/api/export?format=fcpxml&output_path=out%2Fno_such_render.mp4").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = get("/api/export?format=fcpxml").await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "output_path is required");
    }

    #[tokio::test]
    async fn the_download_is_an_attachment_with_its_name() {
        let t = ExportedTimeline {
            format: ExportFormat::Fcpxml,
            file_name: "Été reel \"1\".fcpxml".into(),
            manifest_path: "/projects/out/x.manifest.json".into(),
            body: "<fcpxml/>".into(),
            summary: serde_json::Value::Null,
        };
        let res = download(t);
        let h = res.headers();
        assert_eq!(h[header::CONTENT_TYPE], "application/xml; charset=utf-8");
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(
            h[header::CONTENT_DISPOSITION],
            "attachment; filename=\"_t_ reel _1_.fcpxml\"; filename*=UTF-8''%C3%89t%C3%A9%20reel%20%221%22.fcpxml"
        );
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"<fcpxml/>");
    }
}
