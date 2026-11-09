//! What a finished watched reel tells the outside world: the event the
//! web UI shows as a toast, and the JSON a user-configured webhook gets
//! (`VIBECUT_WATCH_WEBHOOK` — wire it to ntfy, Home Assistant, a Telegram
//! relay…). Built here, transport-free; the web server does the POST.

use serde::Serialize;
use serde_json::{json, Value};

use super::state::WatchEntry;

/// Something the watcher wants listeners (the UI's event stream, the
/// webhook) to know.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchEvent {
    /// The watcher was switched on or off.
    Status { enabled: bool },
    /// An entry appeared or changed (status, progress).
    Entry { entry: WatchEntry },
    /// An entry left the list (a copy that vanished, a dismissed one).
    Removed { id: String },
    /// A watched reel is ready: toast, browser notification, webhook.
    ReelReady { entry: WatchEntry },
}

impl WatchEvent {
    /// The event's name on the wire (`event:` in the SSE stream).
    pub fn name(&self) -> &'static str {
        match self {
            WatchEvent::Status { .. } => "status",
            WatchEvent::Entry { .. } => "entry",
            WatchEvent::Removed { .. } => "removed",
            WatchEvent::ReelReady { .. } => "reel_ready",
        }
    }
}

fn str_at<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a str> {
    v.and_then(|v| v.get(key)).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The reel's title: the intro-card title the proposal used, else the
/// folder's name.
pub fn reel_title(entry: &WatchEntry) -> String {
    str_at(entry.proposal.as_ref(), "title").map(str::to_string).unwrap_or_else(|| entry.name.clone())
}

/// `"12/12 cuts on the beat"` from a render's manifest summary.
pub fn beat_line(render: &Value) -> Option<String> {
    let b = render.get("manifest_summary")?.get("beat_sync")?;
    let n = b.get("n_cuts")?.as_u64().filter(|n| *n > 0)?;
    let ok = b.get("within_tolerance").and_then(Value::as_u64).unwrap_or(0);
    Some(format!("{ok}/{n} cuts on the beat"))
}

/// The finished reel's length in seconds.
pub fn reel_duration(render: &Value) -> Option<f64> {
    render.get("manifest_summary")?.get("actual_duration_secs")?.as_f64()
}

/// `https://host/api/output?path=…` for a file under /projects, when the
/// server's public address is known.
fn file_link(base: Option<&str>, path: Option<&str>) -> Option<String> {
    Some(format!("{}/api/output?path={}", base?, encode_query(path?)))
}

/// Percent-encode a query value (everything but unreserved characters and
/// `/`, which reads better in a path and is legal in a query).
pub fn encode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The webhook body for a finished reel. `public_url` (`VIBECUT_PUBLIC_URL`,
/// e.g. `http://nas.local:8090`) turns paths into links a phone can open;
/// without it the `*_url` fields are null and only paths are given.
///
/// ```json
/// { "event": "reel_ready", "id": "w…", "title": "Coastline Bike Ride · 2026-11-09",
///   "message": "Your reel “Coastline Bike Ride · 2026-11-09” is ready: 30 s, 12/12 cuts on the beat.",
///   "folder": "Coastline Bike Ride", "output": "/projects/out/coastline_bike_ride.mp4",
///   "output_url": "http://nas.local:8090/api/output?path=…", "duration_secs": 30.0,
///   "beat_sync": { "tempo_bpm": 132.0, "n_cuts": 12, "within_tolerance": 12,
///                  "mean_abs_error_ms": 8.1, "summary": "12/12 cuts on the beat" },
///   "poster_url": "http://nas.local:8090/api/output?path=…poster.jpg",
///   "poster_path": "/projects/out/versions/…poster.jpg",
///   "app_url": "http://nas.local:8090/?watch=w…", "finished_at": "2026-11-09T10:15:00+00:00" }
/// ```
pub fn reel_payload(entry: &WatchEntry, public_url: Option<&str>) -> Value {
    let base = public_url.map(|u| u.trim_end_matches('/')).filter(|u| !u.is_empty());
    let render = entry.render.as_ref();
    let title = reel_title(entry);
    let output = str_at(render, "output_path");
    let kept = str_at(render, "version_path").or(output);
    let duration = render.and_then(reel_duration);
    let beat_summary = render.and_then(beat_line);
    let beat_sync = render
        .and_then(|r| r.get("manifest_summary"))
        .and_then(|m| m.get("beat_sync"))
        .filter(|b| b.is_object())
        .map(|b| {
            let mut b = b.clone();
            b["summary"] = beat_summary.clone().map_or(Value::Null, Value::from);
            b
        });
    let mut facts = Vec::new();
    if let Some(d) = duration {
        facts.push(format!("{} s", d.round() as i64));
    }
    if let Some(b) = &beat_summary {
        facts.push(b.clone());
    }
    let message = if facts.is_empty() {
        format!("Your reel “{title}” is ready.")
    } else {
        format!("Your reel “{title}” is ready: {}.", facts.join(", "))
    };
    json!({
        "event": "reel_ready",
        "id": entry.id,
        "title": title,
        "message": message,
        "folder": entry.name,
        "output": output,
        "output_url": file_link(base, kept),
        "duration_secs": duration,
        "beat_sync": beat_sync,
        "poster_url": file_link(base, entry.poster_path.as_deref()),
        "poster_path": entry.poster_path,
        "app_url": base.map(|b| format!("{b}/?watch={}", encode_query(&entry.id))),
        "finished_at": entry.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::scan::{Unit, UnitKind};
    use crate::watch::state::EntryStatus;
    use std::path::{Path, PathBuf};

    fn done_entry() -> WatchEntry {
        let unit = Unit {
            path: PathBuf::from("/footage/Coastline Bike Ride"),
            footage: PathBuf::from("/footage/Coastline Bike Ride"),
            kind: UnitKind::Folder,
            name: "Coastline Bike Ride".into(),
        };
        let mut e = WatchEntry::new(&unit, "c", Path::new("/footage"), EntryStatus::Done);
        e.proposal = Some(json!({ "title": "Coastline Bike Ride · 2026-11-09", "vibe": "action_highlight" }));
        e.render = Some(json!({
            "ok": true,
            "output_path": "/projects/out/coastline_bike_ride.mp4",
            "version_path": "/projects/out/versions/coastline_bike_ride__20261109T101500_job_3.mp4",
            "manifest_summary": {
                "actual_duration_secs": 30.03,
                "beat_sync": { "tempo_bpm": 132.0, "n_cuts": 12, "within_tolerance": 12, "mean_abs_error_ms": 8.1 }
            }
        }));
        e.poster_path = Some("/projects/out/versions/coastline_bike_ride__20261109T101500_job_3.poster.jpg".into());
        e
    }

    #[test]
    fn webhook_payload_has_the_promised_shape() {
        let e = done_entry();
        let p = reel_payload(&e, Some("http://nas.local:8090/"));
        assert_eq!(p["event"], "reel_ready");
        assert_eq!(p["id"], e.id.as_str());
        assert_eq!(p["title"], "Coastline Bike Ride · 2026-11-09");
        assert_eq!(p["folder"], "Coastline Bike Ride");
        assert_eq!(p["output"], "/projects/out/coastline_bike_ride.mp4");
        assert_eq!(p["duration_secs"], 30.03);
        assert_eq!(p["beat_sync"]["n_cuts"], 12);
        assert_eq!(p["beat_sync"]["within_tolerance"], 12);
        assert_eq!(p["beat_sync"]["mean_abs_error_ms"], 8.1);
        assert_eq!(p["beat_sync"]["summary"], "12/12 cuts on the beat");
        assert_eq!(
            p["message"],
            "Your reel “Coastline Bike Ride · 2026-11-09” is ready: 30 s, 12/12 cuts on the beat."
        );
        // Links point at this reel's own kept version and poster.
        assert_eq!(
            p["output_url"],
            "http://nas.local:8090/api/output?path=/projects/out/versions/coastline_bike_ride__20261109T101500_job_3.mp4"
        );
        assert!(p["poster_url"].as_str().unwrap().ends_with("job_3.poster.jpg"));
        assert_eq!(p["poster_path"], e.poster_path.as_deref().unwrap());
        assert_eq!(p["app_url"], format!("http://nas.local:8090/?watch={}", e.id));
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        for k in ["title", "output", "duration_secs", "beat_sync", "poster_url"] {
            assert!(keys.contains(&k), "missing {k}: {keys:?}");
        }
    }

    #[test]
    fn without_a_public_url_links_are_null_and_paths_remain() {
        let mut e = done_entry();
        let p = reel_payload(&e, None);
        assert!(p["output_url"].is_null() && p["poster_url"].is_null() && p["app_url"].is_null());
        assert_eq!(p["output"], "/projects/out/coastline_bike_ride.mp4");
        // No manifest summary: no beat line, a plainer message.
        e.render = Some(json!({ "output_path": "/projects/out/x.mp4" }));
        e.proposal = None;
        let p = reel_payload(&e, Some(""));
        assert!(p["beat_sync"].is_null() && p["duration_secs"].is_null());
        assert_eq!(p["title"], "Coastline Bike Ride");
        assert_eq!(p["message"], "Your reel “Coastline Bike Ride” is ready.");
        assert!(p["output_url"].is_null());
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode_query("/projects/out/a b&c.mp4"), "/projects/out/a%20b%26c.mp4");
        assert_eq!(encode_query("Zi·1"), "Zi%C2%B71");
    }

    #[test]
    fn events_serialise_with_their_type() {
        let e = done_entry();
        let v = serde_json::to_value(WatchEvent::ReelReady { entry: e.clone() }).unwrap();
        assert_eq!(v["type"], "reel_ready");
        assert_eq!(v["entry"]["id"], e.id.as_str());
        assert_eq!(v["entry"]["status"], "done");
        assert_eq!(WatchEvent::Removed { id: "x".into() }.name(), "removed");
        assert_eq!(serde_json::to_value(WatchEvent::Status { enabled: true }).unwrap()["enabled"], true);
    }
}
