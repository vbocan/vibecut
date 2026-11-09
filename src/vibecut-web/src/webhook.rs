//! The watched-reel webhook: when the folder watcher finishes a reel,
//! POST its JSON summary ([`vibecut_ops::watch::reel_payload`]) to the
//! one URL the user configured (`VIBECUT_WATCH_WEBHOOK`) — ntfy, Home
//! Assistant, a Telegram relay.
//!
//! Careful by design: only that URL is ever called (redirects are not
//! followed); each POST times out; a failure is retried once, later, and
//! only when retrying can help (no answer, or a 5xx) — never a storm;
//! reels are delivered one at a time. Webhook URLs often carry a secret
//! (a topic, a token), so logs and the UI only ever show the scheme and
//! host.

use std::sync::Arc;
use std::time::Duration;

use reqwest::{redirect, Url};
use tokio::sync::broadcast::{self, error::RecvError};
use vibecut_ops::watch::{reel_payload, WatchEntry, WatchEvent};

/// Whole-request timeout.
const TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Wait before the single retry.
const RETRY_AFTER: Duration = Duration::from_secs(30);

/// Why a delivery didn't land.
#[derive(Debug, PartialEq)]
pub enum Failure {
    /// No answer: connection refused, DNS, timeout.
    Unreachable(String),
    /// The server answered with this status (a redirect included).
    Status(u16),
}

impl Failure {
    /// Retrying can help: the server wasn't there, or had a moment (5xx).
    /// A 4xx or a redirect will answer the same way next time.
    pub fn worth_retrying(&self) -> bool {
        match self {
            Failure::Unreachable(_) => true,
            Failure::Status(s) => *s >= 500,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Unreachable(why) => write!(f, "no answer ({why})"),
            Failure::Status(s) if (300..400).contains(s) => write!(f, "HTTP {s} (redirects are not followed)"),
            Failure::Status(s) => write!(f, "HTTP {s}"),
        }
    }
}

pub struct Webhook {
    url: Url,
    /// `VIBECUT_PUBLIC_URL`: makes the payload's paths into links.
    public_url: Option<String>,
    client: reqwest::Client,
    retry_after: Duration,
}

impl Webhook {
    /// From `VIBECUT_WATCH_WEBHOOK` and `VIBECUT_PUBLIC_URL`; `None` when
    /// no webhook is set (or it isn't an http(s) URL — logged without it).
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("VIBECUT_WATCH_WEBHOOK").ok().filter(|v| !v.trim().is_empty())?;
        let public = std::env::var("VIBECUT_PUBLIC_URL").ok().filter(|v| !v.trim().is_empty());
        match Webhook::new(raw.trim(), public) {
            Ok(w) => Some(w),
            Err(why) => {
                tracing::warn!("VIBECUT_WATCH_WEBHOOK ignored: {why}");
                None
            }
        }
    }

    pub fn new(url: &str, public_url: Option<String>) -> Result<Self, &'static str> {
        let url = Url::parse(url).map_err(|_| "not a URL")?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("not an http(s) URL");
        }
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(redirect::Policy::none())
            .user_agent(concat!("vibecut/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| "could not set up an HTTP client")?;
        Ok(Webhook { url, public_url, client, retry_after: RETRY_AFTER })
    }

    /// Shorter retry wait, for tests.
    pub fn with_retry_after(mut self, d: Duration) -> Self {
        self.retry_after = d;
        self
    }

    /// Where it goes, safe to show: scheme and host only.
    pub fn host(&self) -> String {
        redact(&self.url)
    }

    /// One POST. Never mentions the URL in what it returns.
    pub async fn deliver(&self, payload: &serde_json::Value) -> Result<u16, Failure> {
        match self.client.post(self.url.clone()).json(payload).send().await {
            Ok(res) if res.status().is_success() => Ok(res.status().as_u16()),
            Ok(res) => Err(Failure::Status(res.status().as_u16())),
            Err(e) => Err(Failure::Unreachable(describe(e))),
        }
    }

    /// Tell the webhook about a finished reel: one POST, and one retry
    /// after a while if that can help. Returns the outcome of the last try.
    pub async fn send_reel(&self, entry: &WatchEntry) -> Result<u16, Failure> {
        let payload = reel_payload(entry, self.public_url.as_deref());
        let mut outcome = self.deliver(&payload).await;
        if let Err(f) = &outcome {
            if f.worth_retrying() {
                tracing::info!(to = %self.host(), "webhook: {f}; trying once more in {}s", self.retry_after.as_secs());
                tokio::time::sleep(self.retry_after).await;
                outcome = self.deliver(&payload).await;
            }
        }
        match &outcome {
            Ok(status) => tracing::info!(to = %self.host(), status, "webhook: reel delivered"),
            Err(f) => tracing::warn!(to = %self.host(), "webhook: reel not delivered: {f}"),
        }
        outcome
    }

    /// Deliver every finished reel the watcher announces, one at a time.
    pub fn spawn(self, mut rx: broadcast::Receiver<WatchEvent>) {
        let hook = Arc::new(self);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(WatchEvent::ReelReady { entry }) => {
                        let _ = hook.send_reel(&entry).await;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(n)) => tracing::warn!(skipped = n, "webhook: fell behind the watcher's events"),
                    Err(RecvError::Closed) => return,
                }
            }
        });
    }
}

/// `scheme://host[:port]` — no user, password, path or query, which is
/// where webhook secrets live.
pub fn redact(url: &Url) -> String {
    let host = url.host_str().unwrap_or("?");
    match url.port() {
        Some(p) => format!("{}://{host}:{p}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// A transport error in words, without the URL reqwest puts in it.
fn describe(e: reqwest::Error) -> String {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "could not connect"
    } else {
        "request failed"
    };
    let detail = e.without_url().to_string();
    format!("{kind}: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    use axum::http::{HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Router;
    use vibecut_ops::watch::{EntryStatus, UnitKind};

    /// What the local test server received.
    #[derive(Default)]
    struct Seen {
        hits: Vec<(String, String, serde_json::Value)>,
    }

    /// A webhook receiver on 127.0.0.1: `/hook` takes it, `/down` is
    /// always 500, `/flaky` is 503 once then fine, `/moved` redirects to
    /// `/elsewhere`, `/forbidden` is 403.
    async fn receiver() -> (String, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let record = |name: &'static str, seen: Arc<Mutex<Seen>>| {
            move |headers: HeaderMap, body: String| {
                let seen = seen.clone();
                async move {
                    let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                    let json = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
                    let mut g = seen.lock().unwrap();
                    g.hits.push((name.to_string(), ct, json));
                    let n = g.hits.iter().filter(|h| h.0 == name).count();
                    match name {
                        "down" => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                        "flaky" if n == 1 => StatusCode::SERVICE_UNAVAILABLE.into_response(),
                        "forbidden" => StatusCode::FORBIDDEN.into_response(),
                        "moved" => (StatusCode::FOUND, [("location", "/elsewhere")]).into_response(),
                        _ => StatusCode::OK.into_response(),
                    }
                }
            }
        };
        let app = Router::new()
            .route("/hook/{secret}", post(record("hook", seen.clone())))
            .route("/down", post(record("down", seen.clone())))
            .route("/flaky", post(record("flaky", seen.clone())))
            .route("/forbidden", post(record("forbidden", seen.clone())))
            .route("/moved", post(record("moved", seen.clone())))
            .route("/elsewhere", post(record("elsewhere", seen.clone())).get(record("elsewhere", seen.clone())));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), seen)
    }

    fn reel() -> WatchEntry {
        let unit = vibecut_ops::watch::scan::Unit {
            path: "/footage/Coastline".into(),
            footage: "/footage/Coastline".into(),
            kind: UnitKind::Folder,
            name: "Coastline".into(),
        };
        let mut e = WatchEntry::new(&unit, "", Path::new("/footage"), EntryStatus::Done);
        e.proposal = Some(serde_json::json!({ "title": "Coastline · 2026-11-09" }));
        e.render = Some(serde_json::json!({
            "output_path": "/projects/out/coastline.mp4",
            "manifest_summary": { "actual_duration_secs": 30.0, "beat_sync": { "n_cuts": 12, "within_tolerance": 11 } }
        }));
        e.poster_path = Some("/projects/out/coastline.poster.jpg".into());
        e
    }

    fn hook(url: &str) -> Webhook {
        Webhook::new(url, Some("http://nas.local:8090".into())).unwrap().with_retry_after(Duration::from_millis(20))
    }

    #[tokio::test]
    async fn posts_the_reel_as_json_to_the_configured_url() {
        let (base, seen) = receiver().await;
        let w = hook(&format!("{base}/hook/s3cret-topic"));
        assert_eq!(w.send_reel(&reel()).await, Ok(200));
        let g = seen.lock().unwrap();
        assert_eq!(g.hits.len(), 1);
        let (_, ct, body) = &g.hits[0];
        assert_eq!(ct, "application/json");
        assert_eq!(body["title"], "Coastline · 2026-11-09");
        assert_eq!(body["output"], "/projects/out/coastline.mp4");
        assert_eq!(body["duration_secs"], 30.0);
        assert_eq!(body["beat_sync"]["summary"], "11/12 cuts on the beat");
        assert_eq!(
            body["poster_url"],
            "http://nas.local:8090/api/output?path=/projects/out/coastline.poster.jpg"
        );
        // What may be shown never includes the secret path.
        assert_eq!(w.host(), base);
        assert!(!w.host().contains("s3cret"));
    }

    #[tokio::test]
    async fn retries_once_on_a_5xx_and_never_more() {
        let (base, seen) = receiver().await;
        assert_eq!(hook(&format!("{base}/flaky")).send_reel(&reel()).await, Ok(200));
        assert_eq!(hook(&format!("{base}/down")).send_reel(&reel()).await, Err(Failure::Status(500)));
        // A 4xx won't change by asking again.
        assert_eq!(hook(&format!("{base}/forbidden")).send_reel(&reel()).await, Err(Failure::Status(403)));
        let g = seen.lock().unwrap();
        let count = |n: &str| g.hits.iter().filter(|h| h.0 == n).count();
        assert_eq!((count("flaky"), count("down"), count("forbidden")), (2, 2, 1));
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (base, seen) = receiver().await;
        let out = hook(&format!("{base}/moved")).send_reel(&reel()).await;
        assert_eq!(out, Err(Failure::Status(302)));
        assert!(out.unwrap_err().to_string().contains("not followed"));
        let g = seen.lock().unwrap();
        assert_eq!(g.hits.iter().filter(|h| h.0 == "elsewhere").count(), 0, "followed a redirect");
        assert_eq!(g.hits.iter().filter(|h| h.0 == "moved").count(), 1);
    }

    #[tokio::test]
    async fn failures_never_reveal_the_url() {
        // A port nobody listens on: bind, note it, close it.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let w = hook(&format!("http://127.0.0.1:{port}/hook/s3cret-token?key=hunter2"));
        let err = w.deliver(&serde_json::json!({})).await.unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, Failure::Unreachable(_)), "{msg}");
        assert!(!msg.contains("s3cret") && !msg.contains("hunter2"), "{msg}");
        assert!(err.worth_retrying());
    }

    #[test]
    fn only_http_urls_are_accepted_and_shown_redacted() {
        assert!(Webhook::new("ftp://example.com/x", None).is_err());
        assert!(Webhook::new("not a url", None).is_err());
        assert!(Webhook::new("file:///etc/passwd", None).is_err());
        let w = Webhook::new("https://user:pass@ntfy.sh/my-topic?auth=abc", None).unwrap();
        assert_eq!(w.host(), "https://ntfy.sh");
        let w = Webhook::new("http://homeassistant.local:8123/api/webhook/abc123", None).unwrap();
        assert_eq!(w.host(), "http://homeassistant.local:8123");
    }

    /// Wired to the watcher's events: a finished reel is posted; other
    /// events are not.
    #[tokio::test]
    async fn follows_the_watchers_events() {
        let (base, seen) = receiver().await;
        let (tx, rx) = broadcast::channel(8);
        hook(&format!("{base}/hook/t")).spawn(rx);
        tx.send(WatchEvent::Status { enabled: true }).unwrap();
        tx.send(WatchEvent::Entry { entry: reel() }).unwrap();
        tx.send(WatchEvent::ReelReady { entry: reel() }).unwrap();
        for _ in 0..100 {
            if !seen.lock().unwrap().hits.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let g = seen.lock().unwrap();
        assert_eq!(g.hits.len(), 1);
        assert_eq!(g.hits[0].2["event"], "reel_ready");
    }
}
