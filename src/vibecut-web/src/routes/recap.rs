//! `GET /api/recap` and `POST /api/recap` — the "Your year" recap
//! (wraps `vibecut_ops::recap`).
//!
//! `GET /api/recap?year=2026` (or `?from=2026-03&to=2026-06`, `&length=60`)
//! answers what a recap would play — its months or trips, moments and
//! length — without writing anything, plus every year's summary
//! (`years`: months, trips, whether there's enough for a recap). With no
//! period it plans the latest year that has saved versions.
//!
//! `POST /api/recap` `{ "year": 2026 }` (or `from` / `to`; optional
//! `length` in seconds or `"1m30s"`, `title`, `vibe`, `soundtrack`, `seed`,
//! `exclude` — windows to leave out — `shuffle`, a seed for another draw
//! of each month's strong moments, and `transition`, a `[transition]` to
//! keep)
//! writes the recap's project config to `/projects/recaps/` and, unless
//! `"render": false`, submits its render as a normal job: the response
//! carries `config_path`, `output_path`, the `plan`, the song, and
//! `job_id`. Poll `GET /api/render/{job_id}` or the WebSocket feed as for
//! any render.

use std::time::Instant;

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use vibecut_ops::recap::{self, RecapArgs, SelectionExclude, Transition};

use crate::error::ApiError;
use crate::state::AppState;

/// A length as seconds (`60`) or text (`"90s"`, `"1m30s"`).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Length {
    Secs(f64),
    Text(String),
}

impl Length {
    fn secs(&self) -> Result<f64, ApiError> {
        match self {
            Length::Secs(s) => recap::parse_length(&s.to_string()),
            Length::Text(t) => recap::parse_length(t),
        }
        .map_err(ApiError::from)
    }
}

fn length_secs(l: Option<&Length>) -> Result<Option<f64>, ApiError> {
    l.map(Length::secs).transpose()
}

#[derive(Debug, Deserialize)]
pub struct RecapQuery {
    pub year: Option<i32>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub length: Option<String>,
}

pub async fn preview_recap(Query(q): Query<RecapQuery>) -> Result<Json<serde_json::Value>, ApiError> {
    let length = length_secs(q.length.map(Length::Text).as_ref())?;
    let preview = recap::preview(RecapArgs {
        year: q.year,
        from: q.from,
        to: q.to,
        length_secs: length,
        ..RecapArgs::default()
    })
    .await?;
    Ok(Json(serde_json::to_value(preview).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
pub struct RecapBody {
    pub year: Option<i32>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub length: Option<Length>,
    pub title: Option<String>,
    /// Vibe whose music profile picks the song (default `family_chronicle`).
    pub vibe: Option<String>,
    /// Song file; omit to let Vibecut pick, `"none"` for no music.
    pub soundtrack: Option<String>,
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// Submit the render job too (default). `false` only writes the config.
    #[serde(default = "default_render")]
    pub render: bool,
    /// Windows dropped from the recap on screen (its `[[selection.exclude]]`,
    /// as `{ file, start, end }`): kept out of the new one.
    #[serde(default)]
    pub exclude: Vec<SelectionExclude>,
    /// Another draw of each month's strong moments (the Studio's Shuffle).
    pub shuffle: Option<u64>,
    /// The recap on screen's `[transition]`, for the new one to keep.
    pub transition: Option<Transition>,
}

fn default_seed() -> u64 {
    42
}
fn default_render() -> bool {
    true
}

pub async fn make_recap(
    State(state): State<AppState>,
    Json(body): Json<RecapBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let started = Instant::now();
    let args_json = serde_json::json!({
        "year": body.year, "from": body.from, "to": body.to, "soundtrack": body.soundtrack, "render": body.render,
    });
    let length = length_secs(body.length.as_ref())?;
    let result = recap::make(RecapArgs {
        year: body.year,
        from: body.from,
        to: body.to,
        length_secs: length,
        title: body.title,
        vibe: body.vibe,
        soundtrack: body.soundtrack,
        seed: body.seed,
        exclude: body.exclude,
        shuffle: body.shuffle,
        transition: body.transition,
    })
    .await;

    let (excerpt, ok) = match &result {
        Ok(r) => (
            serde_json::json!({
                "config_path": r.config_path,
                "output_path": r.output_path,
                "moments": r.plan.moments.len(),
                "grouping": r.plan.grouping,
                "soundtrack": r.soundtrack,
            }),
            true,
        ),
        Err(e) => (serde_json::json!({ "error": e.to_string() }), false),
    };
    state.logger.record("recap", args_json, excerpt, started.elapsed().as_millis() as u64, ok);

    let result = result?;
    let mut out = serde_json::to_value(&result).unwrap_or_default();
    if body.render {
        let job_id = vibecut_ops::render::submit_async(&state.jobs, &result.config_path).await?;
        out["job_id"] = job_id.into();
        out["state"] = "queued".into();
    }
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::{router, AppState};

    async fn send(req: Request<Body>) -> StatusCode {
        router(AppState::default()).oneshot(req).await.unwrap().status()
    }

    fn post(body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/recap")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(query: &str) -> Request<Body> {
        Request::builder().uri(format!("/api/recap?{query}")).body(Body::empty()).unwrap()
    }

    /// Bad periods and lengths are refused before anything is written.
    #[tokio::test]
    async fn bad_periods_and_lengths_are_400() {
        for body in [
            serde_json::json!({ "year": 2026, "from": "2026-03" }),
            serde_json::json!({ "from": "March" }),
            serde_json::json!({ "from": "2026-06", "to": "2026-02" }),
            serde_json::json!({ "year": 2026, "length": "soon" }),
            serde_json::json!({ "year": 2026, "length": -5 }),
            serde_json::json!({ "year": 2026, "vibe": "polka" }),
            serde_json::json!({ "year": 2026, "soundtrack": "/etc/passwd.mp3" }),
        ] {
            assert_eq!(send(post(body.clone())).await, StatusCode::BAD_REQUEST, "{body}");
        }
        assert_eq!(send(get("year=2026&from=2026-03")).await, StatusCode::BAD_REQUEST);
        assert_eq!(send(get("year=2026&length=1h")).await, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_body_must_be_json() {
        let req = Request::builder().method("POST").uri("/api/recap").body(Body::from("year=2026")).unwrap();
        assert_eq!(send(req).await, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
}
