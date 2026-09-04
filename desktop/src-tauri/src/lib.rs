//! The decisions the desktop shell makes before it opens a window.
//!
//! Separated from `main.rs` because they are the only part of the shell worth testing: where the
//! runtime binary is, whether one is already running, and which port to use. The window itself is
//! Tauri's.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The runtime's default port, matching `ServerSettings::default`.
pub const DEFAULT_PORT: u16 = 1338;

/// The name `/api/v1/health` reports. Used to tell *our* runtime from whatever else happens to be
/// listening on the port — a stale process from another app answering 200 would otherwise be
/// adopted as if it were ours.
pub const RUNTIME_NAME: &str = "misaka-studio-runtime";

/// How the window got its runtime.
#[derive(Debug, PartialEq, Eq)]
pub enum RuntimeSource {
    /// One was already running and answered as ours. Nothing was spawned, and nothing will be
    /// killed on exit — a headless runtime someone started deliberately must survive the window
    /// closing.
    Attached(u16),
    /// The shell should start one on this port.
    Spawn(u16),
}

impl RuntimeSource {
    pub fn port(&self) -> u16 {
        match *self {
            RuntimeSource::Attached(port) | RuntimeSource::Spawn(port) => port,
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port())
    }
}

/// Ask `127.0.0.1:port` whether it is a MISAKA runtime.
///
/// A hand-rolled HTTP/1.0 GET rather than an HTTP client: the shell's whole network surface is
/// this one request against loopback, and a TLS stack and connection pool for it would be the
/// largest dependency in the crate.
pub fn health_check(port: u16, timeout: Duration) -> bool {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else { return false };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream.write_all(b"GET /api/v1/health HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").is_err() {
        return false;
    }
    let mut response = String::new();
    // Bounded read: a misbehaving server on this port must not be able to hold the window's
    // startup open by streaming forever.
    let mut buffer = [0u8; 4096];
    let mut total = 0;
    while let Ok(read) = stream.read(&mut buffer) {
        if read == 0 {
            break;
        }
        response.push_str(&String::from_utf8_lossy(&buffer[..read]));
        total += read;
        if total > 64 * 1024 {
            break;
        }
    }
    // **The status line answers in the version we ASKED in.** The request above is HTTP/1.0, and a
    // conforming server replies `HTTP/1.0 200 OK` — so demanding "HTTP/1.1 200" here made this
    // check false against our own healthy runtime, every time. The shell then took the other
    // branch and spawned a rival runtime on a random port: two Studios, two model loads, and a
    // window pointed at the one that had just started rather than the one already working.
    // Measured on 2026-09-04 against a runtime that answered `HTTP/1.0 200 OK` in 3 ms.
    let status_ok = response
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("HTTP/") && line.split_whitespace().nth(1) == Some("200"));
    status_ok && response.contains(RUNTIME_NAME)
}

/// True when nothing holds the port.
pub fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Ask the OS for an unused port.
pub fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok().map(|address| address.port())
}

/// Decide where this window's runtime comes from.
///
/// The order matters and is the whole behaviour:
///
/// 1. **Already running and ours** — attach. Opening the app while a headless `misaka-studiod` is
///    serving, or opening a second window, joins the running one instead of starting a rival that
///    would load the same model into the same GPU a second time.
/// 2. **Port free** — spawn there, so the endpoint other applications point at is the configured
///    one.
/// 3. **Port taken by something else** — spawn on an ephemeral port. The window works; the
///    configured port stays with whatever owns it.
pub fn resolve_runtime(preferred_port: u16) -> RuntimeSource {
    if health_check(preferred_port, Duration::from_millis(400)) {
        return RuntimeSource::Attached(preferred_port);
    }
    if port_is_free(preferred_port) {
        return RuntimeSource::Spawn(preferred_port);
    }
    RuntimeSource::Spawn(free_port().unwrap_or(preferred_port))
}

/// Where `misaka-studiod` is.
///
/// The layouts the app actually runs in, which is more than the three this comment used to
/// claim: an installed bundle (`resources/` beside the executable on Windows and Linux,
/// `Contents/Resources/resources/` in a macOS `.app`), the portable layout and a `cargo build`
/// tree (bare beside the executable), a `cargo tauri dev` build (the workspace target directory),
/// and a developer's PATH.
///
/// The bundle cases were missing until v0.1.0 shipped without them: the sidecar was present in
/// every installer and unreachable from all three, because the search fell through to the
/// relative developer paths below, which resolve against the process CWD and exist only on a
/// machine that built the thing. `the_sidecar_is_found_in_a_*` covers each layout now, built
/// from the directory trees the v0.1.0 artifacts were found to contain.
pub fn locate_runtime_binary(exe_dir: Option<&Path>) -> Option<PathBuf> {
    let name = if cfg!(windows) { "misaka-studiod.exe" } else { "misaka-studiod" };

    if let Ok(explicit) = std::env::var("MISAKA_STUDIOD") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(dir) = exe_dir {
        let candidates = [
            // A `cargo build` tree, and the portable layout: bare beside the executable.
            dir.join(name),
            // A Windows or Linux bundle. `resources/*` keeps its directory name when tauri copies
            // it in, so the sidecar lands one level DOWN from the executable, not beside it.
            dir.join("resources").join(name),
            // `cargo tauri dev` puts the shell in target/debug next to the sidecar.
            dir.join("..").join(name),
            // A macOS bundle: Contents/MacOS/<app> and Contents/Resources/resources/<sidecar>.
            dir.join("../Resources/resources").join(name),
            dir.join("../Resources").join(name),
        ];
        for candidate in candidates {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // Development, run from the desktop crate: the studio workspace's own target directory.
    for relative in ["target/debug", "target/release", "../../target/debug", "../../target/release"] {
        let candidate = PathBuf::from(relative).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    which(name)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|candidate| candidate.is_file())
}

/// The port the user's settings ask for.
///
/// Read from the settings file rather than hardcoded, so the window and the API agree about where
/// the runtime lives. A missing or unreadable file is the default, not an error: the first run has
/// no settings file at all.
pub fn configured_port(settings_path: &Path) -> u16 {
    if let Ok(port) = std::env::var("MISAKA_STUDIO_PORT")
        && let Ok(port) = port.parse()
    {
        return port;
    }
    let Ok(text) = std::fs::read_to_string(settings_path) else { return DEFAULT_PORT };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|value| value.get("server")?.get("port")?.as_u64())
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port > 0)
        .unwrap_or(DEFAULT_PORT)
}

/// The script injected into the webview before the app's own code runs.
///
/// The UI reads `window.__MISAKA_STUDIO_API__` and falls back to same-origin when it is absent —
/// so the same bundle works unchanged whether it is served by the runtime over HTTP or loaded
/// from disk by this shell, which is the point of having one bundle.
pub fn api_base_script(base_url: &str) -> String {
    format!("window.__MISAKA_STUDIO_API__ = {};", serde_json::to_string(base_url).expect("a string serialises"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configured_port_comes_from_the_settings_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");

        // No file yet — first run.
        assert_eq!(configured_port(&path), DEFAULT_PORT);

        std::fs::write(&path, r#"{"server":{"host":"127.0.0.1","port":9099}}"#).expect("write");
        assert_eq!(configured_port(&path), 9099);

        // A corrupt file must not stop the app opening.
        std::fs::write(&path, "{ not json").expect("write");
        assert_eq!(configured_port(&path), DEFAULT_PORT);
    }

    #[test]
    fn an_unused_port_is_spawned_on_rather_than_attached_to() {
        let port = free_port().expect("a port");
        assert_eq!(resolve_runtime(port), RuntimeSource::Spawn(port));
    }

    /// A port held by something that is not our runtime must not be adopted: the window would
    /// then be talking to whatever else is listening there.
    #[test]
    fn a_port_held_by_a_stranger_is_not_attached_to() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
        let port = listener.local_addr().expect("addr").port();
        match resolve_runtime(port) {
            RuntimeSource::Spawn(chosen) => assert_ne!(chosen, port, "it must move to a free port"),
            other => panic!("expected a spawn on another port, got {other:?}"),
        }
    }

    /// The runtime answers in the version the check asks in, and the check asks in 1.0.
    #[test]
    fn health_check_accepts_the_version_it_asked_in() {
        for version in ["HTTP/1.0", "HTTP/1.1"] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
            let port = listener.local_addr().expect("addr").port();
            let body = format!("{{\"status\":\"ok\",\"name\":\"{RUNTIME_NAME}\"}}");
            let response = format!(
                "{version} 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            let server = std::thread::spawn(move || {
                if let Ok((mut stream, _)) = listener.accept() {
                    let mut sink = [0u8; 1024];
                    let _ = stream.read(&mut sink);
                    let _ = stream.write_all(response.as_bytes());
                }
            });
            assert!(health_check(port, Duration::from_millis(500)), "{version} must be accepted");
            let _ = server.join();
        }
    }

    /// A 200 from something that is not the runtime is still not the runtime.
    #[test]
    fn health_check_is_false_for_a_stranger_answering_200() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binds");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut sink = [0u8; 1024];
                let _ = stream.read(&mut sink);
                let _ = stream.write_all(b"HTTP/1.0 200 OK\r\ncontent-length: 2\r\n\r\nhi");
            }
        });
        assert!(!health_check(port, Duration::from_millis(500)));
        let _ = server.join();
    }

    #[test]
    fn health_check_is_false_for_a_closed_port() {
        let port = free_port().expect("a port");
        assert!(!health_check(port, Duration::from_millis(100)));
    }

    #[test]
    fn the_injected_script_is_valid_javascript_with_the_url_quoted() {
        let script = api_base_script("http://127.0.0.1:1338");
        assert_eq!(script, "window.__MISAKA_STUDIO_API__ = \"http://127.0.0.1:1338\";");
        // A URL with a quote in it must not break out of the string literal.
        let hostile = api_base_script("http://127.0.0.1:1\"; alert(1); \"");
        assert!(!hostile.contains("alert(1);\""), "the quote must stay escaped: {hostile}");
    }

    /// The layouts the bundlers actually produce, taken from the v0.1.0 artifacts by extracting
    /// them: `resources/*` keeps its directory name inside the bundle, so the sidecar is never
    /// bare beside the executable the way a `cargo build` leaves it.
    #[test]
    fn the_sidecar_is_found_in_a_windows_or_linux_bundle() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: single-threaded under ENV_LOCK.
        unsafe { std::env::remove_var("MISAKA_STUDIOD") };
        let dir = tempfile::tempdir().expect("tempdir");
        let name = if cfg!(windows) { "misaka-studiod.exe" } else { "misaka-studiod" };
        let resources = dir.path().join("resources");
        std::fs::create_dir_all(&resources).expect("mkdir");
        let sidecar = resources.join(name);
        std::fs::write(&sidecar, b"#!/bin/sh\n").expect("write");

        let found = locate_runtime_binary(Some(dir.path())).expect("the sidecar");
        assert_eq!(found.canonicalize().ok(), sidecar.canonicalize().ok(), "found {found:?}");
    }

    #[test]
    fn the_sidecar_is_found_in_a_macos_app_bundle() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: single-threaded under ENV_LOCK.
        unsafe { std::env::remove_var("MISAKA_STUDIOD") };
        let dir = tempfile::tempdir().expect("tempdir");
        let name = if cfg!(windows) { "misaka-studiod.exe" } else { "misaka-studiod" };
        let macos = dir.path().join("Contents/MacOS");
        let resources = dir.path().join("Contents/Resources/resources");
        std::fs::create_dir_all(&macos).expect("mkdir");
        std::fs::create_dir_all(&resources).expect("mkdir");
        let sidecar = resources.join(name);
        std::fs::write(&sidecar, b"#!/bin/sh\n").expect("write");

        let found = locate_runtime_binary(Some(&macos)).expect("the sidecar");
        assert_eq!(found.canonicalize().ok(), sidecar.canonicalize().ok(), "found {found:?}");
    }

    /// The portable layout and a `cargo build` tree: bare beside the executable.
    #[test]
    fn the_sidecar_is_found_beside_the_executable() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // SAFETY: single-threaded under ENV_LOCK.
        unsafe { std::env::remove_var("MISAKA_STUDIOD") };
        let dir = tempfile::tempdir().expect("tempdir");
        let name = if cfg!(windows) { "misaka-studiod.exe" } else { "misaka-studiod" };
        let sidecar = dir.path().join(name);
        std::fs::write(&sidecar, b"#!/bin/sh\n").expect("write");

        let found = locate_runtime_binary(Some(dir.path())).expect("the sidecar");
        assert_eq!(found.canonicalize().ok(), sidecar.canonicalize().ok(), "found {found:?}");
    }

    /// `MISAKA_STUDIOD` is process-global and `locate_runtime_binary` reads it first, so every
    /// test that depends on it being set — or unset — has to hold this.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn an_explicit_binary_path_is_honoured() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        let name = if cfg!(windows) { "misaka-studiod.exe" } else { "misaka-studiod" };
        let path = dir.path().join(name);
        std::fs::write(&path, b"#!/bin/sh\n").expect("write");
        // SAFETY: single-threaded test process; the variable is read back immediately below.
        unsafe { std::env::set_var("MISAKA_STUDIOD", &path) };
        assert_eq!(locate_runtime_binary(None), Some(path));
        unsafe { std::env::remove_var("MISAKA_STUDIOD") };
    }
}
