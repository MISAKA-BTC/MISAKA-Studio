//! **Who may call the Studio's own API** — the checks in front of `/api/v1`.
//!
//! The runtime's CORS layer lets any origin through, on purpose: `/v1` is the OpenAI-compatible
//! endpoint, and a page or an app on this machine calling the local model is a use it exists for.
//! `/api/v1` is something else. It rewrites the settings file, starts downloads, loads engines,
//! starts the node and the mining stack, and reads the record log — and a browser will send a
//! request to `http://127.0.0.1:1338` for any website the person happens to have open, asking
//! nobody. Under a CORS layer that allows every origin, that page could also read every answer. So
//! `/api/v1` is guarded, at two strengths.
//!
//! **All of `/api/v1`** refuses a request unless
//!
//! 1. its `Host` header, when the runtime is bound to loopback, names this machine. A DNS-rebinding
//!    page reaches 127.0.0.1 under its own hostname, and the Host header is where that shows. (On a
//!    network bind every request already needs the API key, which no foreign page holds.)
//! 2. its `Origin` header, if it has one, is the Studio's own window — a loopback `http(s)` origin
//!    on any port (the Vite dev server proxies from `localhost:5173`), `tauri://localhost`, or
//!    `http(s)://tauri.localhost` — or, on a network bind, the origin the window was served from;
//!    or an origin the person listed in `server.cors_origins`, which is how a web app they trust
//!    is let in (`"*"` there restores "any origin" for this API, deliberately). A browser sends
//!    `Origin` on every cross-origin request and on every POST, so a foreign page cannot leave it
//!    out; a request with none is a same-origin GET, or a program like `curl` that already runs as
//!    the user.
//! 3. a request without an `Origin` does not say it came from another site (`Sec-Fetch-Site:
//!    cross-site` — a `no-cors` GET, an `<img>` pointed at the API).
//!
//! **`/api/v1/workspace`** — which reads and writes files and runs commands — is stricter still: it
//! is served only on a loopback bind (an API key does not make "run this command" safe to offer
//! the network), only to a peer on this machine, and only to the Studio's own window. Neither a
//! network bind's own origin nor `server.cors_origins` reaches it.
//!
//! `/v1` is not guarded. The bearer-key check, when a key is set, runs before either guard.

use crate::Error;
use crate::state::AppState;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

/// The guard in front of all of `/api/v1`. See the module docs for what it enforces and why.
pub async fn studio(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let (bind, listed) = {
        let settings = state.settings.read().await;
        (settings.server.host.clone(), settings.server.cors_origins.clone())
    };
    match check_studio(&bind, &listed, request.headers()) {
        Ok(()) => next.run(request).await,
        Err(message) => refuse(&request, message),
    }
}

/// The stricter guard in front of `/api/v1/workspace`.
pub async fn workspace(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let bind = state.settings.read().await.server.host.clone();
    // Present when the server was started with connect info (the binary does); a test harness that
    // serves the router bare simply has no peer to check.
    let peer = request.extensions().get::<ConnectInfo<SocketAddr>>().map(|info| info.0.ip());
    match check_workspace(&bind, request.headers(), peer) {
        Ok(()) => next.run(request).await,
        Err(message) => refuse(&request, message),
    }
}

fn refuse(request: &Request, message: String) -> Response {
    tracing::warn!(path = %request.uri().path(), "API request refused: {message}");
    Error::Forbidden { message }.into_response()
}

/// The decision for all of `/api/v1`, as a pure function: `Err` carries the sentence the caller
/// gets. `listed` is `server.cors_origins`.
pub fn check_studio(bind_host: &str, listed: &[String], headers: &HeaderMap) -> std::result::Result<(), String> {
    let loopback = is_loopback_bind(bind_host);
    if loopback {
        check_host(headers)?;
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    let origins = origins(headers);
    for origin in &origins {
        // On a network bind the window is served from the bind's own address, so its origin is
        // the Host it reaches — the one same-origin case a loopback list cannot name.
        let served_here = !loopback && host.is_some_and(|host| same_authority(origin, host));
        if !(is_studio_origin(origin) || served_here || is_listed(origin, listed)) {
            return Err(format!(
                "requests from {origin:?} may not use the Studio's API: it changes settings, downloads and loads models and \
                 reads the record log, so only the Studio's own window may call it — or an origin listed in server.cors_origins \
                 in the settings file"
            ));
        }
    }
    check_cross_site(headers, &origins)
}

/// The decision for `/api/v1/workspace`.
pub fn check_workspace(bind_host: &str, headers: &HeaderMap, peer: Option<IpAddr>) -> std::result::Result<(), String> {
    if !is_loopback_bind(bind_host) {
        return Err(format!(
            "the workspace API is only served when the runtime is bound to a loopback address, and it is bound to {bind_host}. \
             It reads and writes files and runs commands, which an API key does not make safe to offer the network"
        ));
    }
    if let Some(ip) = peer
        && !is_loopback_ip(ip)
    {
        return Err(format!("the workspace API answers only this machine; this request came from {ip}"));
    }
    check_host(headers)?;
    let origins = origins(headers);
    for origin in &origins {
        if !is_studio_origin(origin) {
            return Err(format!(
                "requests from {origin:?} may not use the workspace API: it reads and writes files and runs commands, so only \
                 the Studio's own window (a loopback origin or tauri://localhost) may call it"
            ));
        }
    }
    check_cross_site(headers, &origins)
}

/// Every `Host` header names this machine.
fn check_host(headers: &HeaderMap) -> std::result::Result<(), String> {
    for value in headers.get_all(header::HOST) {
        let host = value.to_str().unwrap_or("");
        if !host_is_loopback(host) {
            return Err(format!(
                "Host {host:?} is not this machine (localhost, 127.0.0.1 or [::1]); the Studio's API refuses it, \
                 which is what keeps a DNS-rebinding page out"
            ));
        }
    }
    Ok(())
}

fn origins(headers: &HeaderMap) -> Vec<&str> {
    headers.get_all(header::ORIGIN).iter().map(|v| v.to_str().unwrap_or("")).collect()
}

/// A request that carries no `Origin` but says it came from another site.
fn check_cross_site(headers: &HeaderMap, origins: &[&str]) -> std::result::Result<(), String> {
    let cross_site =
        headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()).is_some_and(|site| site.trim().eq_ignore_ascii_case("cross-site"));
    if origins.is_empty() && cross_site {
        return Err("a cross-site request without an Origin may not use the Studio's API".to_string());
    }
    Ok(())
}

/// An origin as written in a header or a settings file, compared: case-blind, no trailing slash.
fn normalize_origin(origin: &str) -> String {
    let origin = origin.trim().to_ascii_lowercase();
    origin.strip_suffix('/').map(str::to_string).unwrap_or(origin)
}

/// Listed in `server.cors_origins` — exactly, or by a `"*"` entry.
fn is_listed(origin: &str, listed: &[String]) -> bool {
    let origin = normalize_origin(origin);
    listed.iter().any(|entry| {
        let entry = normalize_origin(entry);
        entry == "*" || entry == origin
    })
}

/// Whether `origin` (`http(s)://authority`) names the same authority as a `Host` header.
fn same_authority(origin: &str, host: &str) -> bool {
    let origin = normalize_origin(origin);
    let Some(authority) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    !authority.is_empty() && authority == host.trim().to_ascii_lowercase()
}

/// `server.host` names a loopback interface.
fn is_loopback_bind(host: &str) -> bool {
    let host = host.trim();
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(is_loopback_ip)
}

fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    }
}

/// An authority — `host[:port]` — that names this machine: `localhost`, `127.0.0.1`, `[::1]`, or
/// a `*.localhost` name (which resolves to loopback by definition, RFC 6761), on any port.
fn host_is_loopback(authority: &str) -> bool {
    let authority = authority.trim();
    let port_ok = |port: &str| !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((inside, after)) = rest.split_once(']') else { return false };
        let after_ok = after.is_empty() || after.strip_prefix(':').is_some_and(port_ok);
        return after_ok && inside.parse::<std::net::Ipv6Addr>().is_ok_and(|ip| ip.is_loopback());
    }
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port_ok(port) => host,
        Some(_) => return false,
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host == "127.0.0.1" {
        return true;
    }
    host.strip_suffix(".localhost").is_some_and(|label| {
        !label.is_empty()
            && !label.starts_with('.')
            && label.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    })
}

/// The Studio's own window: a loopback `http(s)` origin on any port, or Tauri's.
fn is_studio_origin(origin: &str) -> bool {
    let origin = origin.trim().to_ascii_lowercase();
    if origin == "tauri://localhost" {
        return true;
    }
    let Some(authority) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    // An origin has no path; tolerate the trailing slash some clients add, and nothing else.
    let authority = authority.strip_suffix('/').unwrap_or(authority);
    !authority.contains(['/', '@', '?', '#']) && host_is_loopback(authority)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(), HeaderValue::from_str(value).unwrap());
        }
        map
    }

    /// The workspace guard on a loopback bind with no peer known — and, because everything it
    /// refuses the Studio guard refuses too, the cases both share.
    fn allowed(pairs: &[(&str, &str)]) -> bool {
        check_workspace("127.0.0.1", &headers(pairs), None).is_ok()
    }

    fn studio_allowed(bind: &str, listed: &[&str], pairs: &[(&str, &str)]) -> bool {
        let listed: Vec<String> = listed.iter().map(|s| s.to_string()).collect();
        check_studio(bind, &listed, &headers(pairs)).is_ok()
    }

    /// The three the Studio actually sends: `curl`/the desktop shell (no Origin), the Vite dev
    /// server, and the packaged Tauri window.
    const OWN_CALLERS: &[&[(&str, &str)]] = &[
        &[("host", "127.0.0.1:1338")],
        &[],
        // The desktop shell's health probe writes the Host without a port.
        &[("host", "127.0.0.1")],
        &[("host", "localhost:5174"), ("origin", "http://localhost:5174")],
        &[("host", "127.0.0.1:1338"), ("origin", "tauri://localhost")],
        &[("origin", "http://tauri.localhost")],
        &[("origin", "https://tauri.localhost")],
        &[("origin", "http://127.0.0.1:1338")],
        &[("host", "[::1]:1338"), ("origin", "http://[::1]:1338")],
        &[("host", "app.localhost:5173")],
        // A navigation typed into the address bar is `none`, and a same-origin fetch is `same-origin`.
        &[("sec-fetch-site", "none")],
        &[("origin", "http://localhost:5173"), ("sec-fetch-site", "cross-site")],
    ];

    const FOREIGN: &[&[(&str, &str)]] = &[
        &[("origin", "https://evil.example")],
        &[("host", "evil.example:1338")],
        &[("host", "127.0.0.1:1338"), ("origin", "null")],
        &[("origin", "http://localhost.evil.example")],
        &[("origin", "http://evil.example/.localhost")],
        &[("origin", "http://user@localhost")],
        &[("origin", "file://")],
        &[("origin", "tauri://evil")],
        &[("host", "127.0.0.2:1338")],
        &[("host", "localhost:notaport")],
        &[("host", "[::2]:1338")],
        // Two Origins, one of them foreign.
        &[("origin", "http://localhost:5173"), ("origin", "https://evil.example")],
        // A no-cors GET from another site carries no Origin, but says where it came from.
        &[("host", "127.0.0.1:1338"), ("sec-fetch-site", "cross-site")],
    ];

    #[test]
    fn the_studios_own_callers_are_allowed() {
        for pairs in OWN_CALLERS {
            assert!(allowed(pairs), "workspace: {pairs:?}");
            assert!(studio_allowed("127.0.0.1", &[], pairs), "studio: {pairs:?}");
        }
    }

    #[test]
    fn a_foreign_page_or_a_rebound_host_is_refused() {
        for pairs in FOREIGN {
            assert!(!allowed(pairs), "workspace: {pairs:?}");
            assert!(!studio_allowed("127.0.0.1", &[], pairs), "studio: {pairs:?}");
        }
    }

    #[test]
    fn a_listed_origin_reaches_the_studio_api_and_never_the_workspace() {
        let app = [("host", "127.0.0.1:1338"), ("origin", "https://app.example")];
        assert!(studio_allowed("127.0.0.1", &["https://app.example"], &app));
        // Written the way people write them: a trailing slash, another case.
        assert!(studio_allowed("127.0.0.1", &["HTTPS://App.Example/"], &app));
        assert!(!studio_allowed("127.0.0.1", &["https://other.example"], &app));
        assert!(!studio_allowed("127.0.0.1", &["http://app.example"], &app), "the scheme is part of the origin");
        // "*" is the deliberate way back to "any origin" — for the Studio API, not for the folder API.
        assert!(studio_allowed("127.0.0.1", &["*"], &[("origin", "https://evil.example")]));
        assert!(!allowed(&app));
        // A listed origin is still no way around the Host check: rebinding is not an origin.
        assert!(!studio_allowed("127.0.0.1", &["*"], &[("host", "evil.example:1338"), ("origin", "http://evil.example:1338")]));
    }

    #[test]
    fn a_network_bind_serves_its_own_window_and_refuses_other_pages() {
        // The window served from the LAN address calls back to it: same origin as the Host.
        let window = [("host", "192.168.1.10:1338"), ("origin", "http://192.168.1.10:1338")];
        assert!(studio_allowed("0.0.0.0", &[], &window));
        assert!(studio_allowed("0.0.0.0", &[], &[("host", "192.168.1.10:1338")]));
        assert!(!studio_allowed("0.0.0.0", &[], &[("host", "192.168.1.10:1338"), ("origin", "https://evil.example")]));
        assert!(studio_allowed(
            "0.0.0.0",
            &["https://evil.example"],
            &[("host", "192.168.1.10:1338"), ("origin", "https://evil.example")]
        ));
        // On loopback the same pair is a rebinding page, and is refused by its Host.
        assert!(!studio_allowed("127.0.0.1", &[], &window));
    }

    #[test]
    fn a_network_bind_refuses_the_whole_workspace_api() {
        let error = check_workspace("0.0.0.0", &headers(&[]), None).unwrap_err();
        assert!(error.contains("loopback"), "{error}");
        assert!(check_workspace("192.168.1.5", &headers(&[]), None).is_err());
        for local in ["127.0.0.1", "localhost", "::1", "[::1]", "127.0.0.5"] {
            assert!(check_workspace(local, &headers(&[]), None).is_ok(), "{local}");
        }
    }

    #[test]
    fn a_peer_off_this_machine_is_refused_by_the_workspace_api() {
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(check_workspace("127.0.0.1", &headers(&[]), Some(lan)).is_err());
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(check_workspace("127.0.0.1", &headers(&[]), Some(mapped)).is_ok());
        assert!(check_workspace("127.0.0.1", &headers(&[]), Some("127.0.0.1".parse().unwrap())).is_ok());
    }
}
