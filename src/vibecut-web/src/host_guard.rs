//! DNS-rebinding guard for `/api/*`.
//!
//! The API reads footage from wherever the server can (with the host's
//! drives mounted at `/host`, every drive), and it is meant for the
//! browser on this machine. A web page elsewhere can't read its answers
//! (no CORS), unless it rebinds its own DNS name to 127.0.0.1: then its
//! requests are same-origin, but they carry the page's name in `Host`.
//! So a request whose `Host` is a DNS name is refused with `403` unless
//! that name can't be an attacker's: `localhost` / `*.localhost`, an IP
//! address, a name without a dot (a machine or container name no public
//! DNS answers for), the host of `VIBECUT_PUBLIC_URL`, or one listed in
//! `VIBECUT_ALLOWED_HOSTS` (comma-separated). Requests without a `Host`
//! (HTTP/1.0 tools, in-process tests) pass.

use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;

/// `axum::middleware::from_fn` body.
pub async fn guard(req: Request, next: Next) -> Response {
    if let Some(host) = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()) {
        if !host_allowed(host, &allowed_from_env()) {
            tracing::warn!(host, "refused a request for a host name that isn't allowed (see VIBECUT_ALLOWED_HOSTS)");
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": format!("Vibecut doesn't answer to host {host:?}"),
                    "hint": "open it at localhost or an IP address, or add the name to VIBECUT_ALLOWED_HOSTS",
                })),
            )
                .into_response();
        }
    }
    next.run(req).await
}

/// Names from `VIBECUT_ALLOWED_HOSTS` and the host of `VIBECUT_PUBLIC_URL`.
fn allowed_from_env() -> Vec<String> {
    let mut out: Vec<String> = std::env::var("VIBECUT_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|s| strip_port(s.trim()).to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if let Ok(url) = std::env::var("VIBECUT_PUBLIC_URL") {
        let url = url.trim();
        let rest = url.split_once("://").map_or(url, |(_, r)| r);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = strip_port(host).to_ascii_lowercase();
        if !host.is_empty() {
            out.push(host);
        }
    }
    out
}

fn strip_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    match host.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    }
}

/// See the module docs.
pub fn host_allowed(host: &str, extra: &[String]) -> bool {
    let h = strip_port(host.trim()).trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return false;
    }
    h.parse::<std::net::IpAddr>().is_ok()
        || h == "localhost"
        || h.ends_with(".localhost")
        || !h.contains('.')
        || extra.iter().any(|e| *e == h)
}

#[cfg(test)]
mod tests {
    use super::host_allowed;

    #[test]
    fn hosts_that_can_and_cannot_be_rebound() {
        let extra = vec!["vibecut.lan".to_string()];
        for ok in [
            "localhost:8090",
            "LOCALHOST",
            "127.0.0.1:8090",
            "[::1]:8090",
            "192.168.1.20",
            "vc-browse:8090",
            "app.localhost",
            "vibecut.lan:8090",
            "localhost.:8090",
        ] {
            assert!(host_allowed(ok, &extra), "{ok}");
        }
        for bad in ["evil.example.com", "evil.example.com:8090", "127.0.0.1.nip.io", "localhost.evil.com", "", ":8090"] {
            assert!(!host_allowed(bad, &extra), "{bad}");
        }
    }
}
