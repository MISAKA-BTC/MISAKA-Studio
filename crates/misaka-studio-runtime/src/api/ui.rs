//! The UI bundle, compiled into the binary.
//!
//! # Why the runtime carries its own UI
//!
//! The desktop window is a client of `http://127.0.0.1:<port>` — deliberately, so the window and a
//! browser see the same app through the same public API. That makes the runtime's `/` the app, and
//! it used to be found by *looking around on disk*: `--ui-dir`, then `MISAKA_STUDIO_UI_DIR`, then a
//! few paths beside the executable, then `ui/dist` **relative to the process's working directory**.
//!
//! That last one is what made it work in development and fail everywhere else. The shell spawns
//! `misaka-studiod` with its own working directory inherited, so running the app out of the source
//! tree found `ui/dist`, and launching the packaged `.app` from Finder — working directory `/`,
//! executable in `Contents/Resources/resources/` — found nothing. The window then rendered the
//! runtime's headless "no UI bundle" page: the whole product, replaced by a note about a flag.
//!
//! A packaging fix (ship `ui/dist` as a bundle resource, pass `--ui-dir`) would repair the `.app`
//! and leave `misaka-studiod` still unable to serve its own UI when started from any other
//! directory — which is what the README tells people to do. Embedding removes the search entirely:
//! the bytes travel with the binary, so there is no arrangement of working directory, install
//! prefix or bundle layout that can separate them.
//!
//! # What is still on disk
//!
//! `--ui-dir` (and the paths [`crate::locate_ui`] looks at) still win. A directory is a live
//! bundle: `npm run build` and reload, no recompile. The embed is the floor under it, not a
//! replacement for it.
//!
//! Source maps are excluded — 2.9 MB of the 3.5 MB bundle, useful only next to the sources they
//! map, and available in any disk-served build.

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../ui/dist"]
#[exclude = "*.map"]
struct Ui;

/// Whether this binary carries a UI. False for a build made before `npm run build` ever ran, which
/// is a legitimate headless runtime.
pub fn embedded_is_empty() -> bool {
    Ui::iter().next().is_none()
}

/// How many files the embed holds — for `--check`, so "the UI is inside the binary" is a claim the
/// operator can see a number behind.
pub fn embedded_len() -> usize {
    Ui::iter().count()
}

/// Serve a path out of the embedded bundle, with the SPA fallback to `index.html`.
///
/// Unknown paths return the document rather than 404 because the UI routes on the client: a reload
/// on `/#/network` (or any future path route) must hand back the app, not an error.
pub fn serve(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Ui::get(path) {
        return respond(path, file.data.into_owned());
    }
    // A request that looks like an asset must 404 rather than receive HTML. Handing index.html to
    // a `<script src>` that 404s is how a blank window with a MIME-type console error happens —
    // the browser reports "unexpected token '<'" and nothing points at the missing file.
    if path.contains('.') && !path.ends_with(".html") {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match Ui::get("index.html") {
        Some(index) => respond("index.html", index.data.into_owned()),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn respond(path: &str, bytes: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // `index.html` names the hashed asset files, so it must be revalidated; the hashed files
    // themselves never change under their name and can be held. Getting this backwards pins a
    // stale app in the window across an upgrade.
    let cache = if path.ends_with(".html") { "no-cache" } else { "public, max-age=31536000, immutable" };
    ([(header::CONTENT_TYPE, mime.as_ref()), (header::CACHE_CONTROL, cache)], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The release binary this ships as must contain the app. A build whose embed is empty serves
    /// the headless page — correct for a bare API build, catastrophic for the desktop one, and
    /// indistinguishable from the outside until a window opens on it.
    ///
    /// Skipped rather than failed when `ui/dist` has no `index.html`: a checkout that has never run
    /// `npm run build` is a legitimate state for `cargo test`, and build.rs creates the empty
    /// directory for exactly that case.
    #[test]
    fn the_binary_carries_the_ui_when_one_was_built() {
        let built = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist/index.html").is_file();
        if !built {
            eprintln!("skipped: ui/dist/index.html does not exist — run `npm --prefix ui run build`");
            return;
        }
        assert!(!embedded_is_empty(), "ui/dist has an index.html but the embed is empty");
        assert!(Ui::get("index.html").is_some(), "the embed has files but not index.html");
    }

    #[test]
    fn an_unknown_route_gets_the_app_and_a_missing_asset_gets_a_404() {
        if embedded_is_empty() {
            return;
        }
        assert_eq!(serve("/network").status(), StatusCode::OK, "a client route must return the app");
        assert_eq!(serve("/assets/does-not-exist.js").status(), StatusCode::NOT_FOUND, "a missing asset must not be HTML");
    }

    #[test]
    fn index_is_revalidated_and_hashed_assets_are_held() {
        if embedded_is_empty() {
            return;
        }
        let index = serve("/");
        assert_eq!(index.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
        let asset = Ui::iter().find(|f| f.ends_with(".js")).map(|f| f.to_string());
        if let Some(asset) = asset {
            let response = serve(&asset);
            assert_eq!(response.headers().get(header::CACHE_CONTROL).unwrap(), "public, max-age=31536000, immutable");
            assert!(response.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap().contains("javascript"));
        }
    }
}
