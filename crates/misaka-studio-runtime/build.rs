//! Make the UI bundle's directory exist, and rebuild when it changes.
//!
//! `src/api/ui.rs` embeds `ui/dist` into the binary with `rust-embed`, which fails to compile if
//! the folder is missing. On a fresh clone it *is* missing — `npm run build` has not run yet — and
//! a Rust crate that cannot be compiled without first running a JavaScript build is a crate that
//! breaks `cargo check`, CI, and every contributor who only wants to touch the server. So the
//! directory is created empty when absent: the embed is then empty, `Ui::is_empty()` is true, and
//! the runtime serves its headless page exactly as it did before this file existed.

use std::path::PathBuf;

fn main() {
    let dist = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    if !dist.is_dir() {
        // Failure is not fatal: a read-only source tree still compiles, the embed is just empty.
        let _ = std::fs::create_dir_all(&dist);
    }
    // The embed's contents are compiled in, so a rebuilt UI must trigger a recompile. Cargo only
    // watches what it is told to watch, and the default (the whole crate directory) does not
    // reach a sibling directory two levels up.
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed=build.rs");

    // The target triple this binary is built for, as the components manifest spells a row's
    // `platform` (`aarch64-apple-darwin`, `x86_64-unknown-linux-musl`, ADR-0096 Decision 10).
    // Only the build knows it exactly — `std::env::consts` has the OS and the arch but not the
    // vendor or the libc — and a runtime that guessed the triple would compare manifest rows
    // against a string nobody published.
    println!("cargo:rustc-env=MISAKA_STUDIO_TARGET={}", std::env::var("TARGET").unwrap_or_else(|_| "unknown".into()));
    println!("cargo:rerun-if-env-changed=TARGET");
}
