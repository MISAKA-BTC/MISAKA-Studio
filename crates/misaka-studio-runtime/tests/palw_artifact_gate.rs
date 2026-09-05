//! **A PALW artifact must never reach an inference engine.**
//!
//! The models directory holds both kinds of file on purpose — a `.gguf` an engine loads, and a
//! `.palwart` a class is mined with — and the model list shows both, because the MISAKA runtime
//! backend loads artifacts and nothing else can. The backend, though, is one global setting, so
//! nothing above the load knows whether the pairing works.
//!
//! On 2026-09-04 it did not: `llama-server` was handed `qwen25-1.5b-a16.palwart`, read `PALW` where
//! `GGUF` should be, aborted, and its fifteen lines of stderr came back as the load error. Error
//! notifications stay until they are dismissed, and that message was taller than the window, so the
//! dismiss button sat above the top edge — a log the user could not close.
//!
//! No engine is needed to run this, and that is the point: the refusal happens before the Studio
//! goes looking for one.

use misaka_studio_core::settings::{BackendKind, BackendSettings, Settings};
use misaka_studio_runtime::{AppState, Error};
use std::path::{Path, PathBuf};

/// A models directory holding one artifact and one file that is not one.
fn models_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    // Real magic, token length: nothing here parses the file, and a scan that had to read 1.7 GiB
    // to list a directory would be the wrong design to test against.
    std::fs::write(dir.path().join("qwen25-1.5b-a16.palwart"), b"PALW\0\0\0\x01").expect("artifact");
    std::fs::write(dir.path().join("qwen36.palwq36.part"), b"PALW\0\0\0\x01").expect("partial");
    dir
}

async fn studio(models: &Path, kind: BackendKind) -> (std::sync::Arc<AppState>, tempfile::TempDir) {
    let data = tempfile::tempdir().expect("tempdir");
    let settings = Settings {
        models_dir: models.to_path_buf(),
        backend: BackendSettings {
            kind,
            // Deliberately absent. The gate must fire before the Studio asks whether an engine is
            // installed, so this path is never opened.
            llama_server_path: Some(PathBuf::from("/nonexistent/llama-server")),
            ..Default::default()
        },
        ..Default::default()
    };
    let state = AppState::new(settings, data.path().join("settings.json"), data.path().to_path_buf()).await;
    (state, data)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_artifact_is_listed_as_a_model_but_refused_by_an_engine_that_cannot_read_it() {
    let models = models_dir();
    let (state, _data) = studio(models.path(), BackendKind::LlamaCpp).await;

    // Listed: the Studio does not hide a file that is sitting in the models directory, and the
    // MISAKA backend can load this one.
    let listed = state.store.list().await;
    let ids: Vec<&str> = listed.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"qwen25-1.5b-a16"), "the artifact should be listed: {ids:?}");
    // A half-finished download is not an artifact yet; offering it would offer a failure.
    assert!(!ids.iter().any(|id| id.starts_with("qwen36")), "a .part must not be listed: {ids:?}");

    // Since the engine is chosen by the file (`backend_for`), an artifact under a llama.cpp
    // configuration is never handed to llama.cpp: the integer runtime is chosen for it instead.
    // So the load either succeeds (the runtime is installed on this machine) or fails as THAT
    // engine being unavailable — named, with a remedy — and never as llama.cpp's stderr. The
    // pairing refusal itself is unit-tested where it lives; this test pins the outcome a person
    // sees, on any machine.
    match state.load("qwen25-1.5b-a16", None).await {
        Ok(status) => {
            assert_ne!(status.backend, "llamacpp", "a PALW artifact must not be loaded by llama.cpp: {status:?}");
        }
        Err(error) => {
            let message = error.to_string();
            // The failure this replaces: another program's stderr, verbatim, in a notification.
            assert!(!message.contains("gguf_init_from_reader"), "{message}");
            assert!(message.contains("misaka"), "the message must name the engine that can read it: {message}");
            assert!(message.lines().count() == 1, "one line, so a notification can hold it: {message}");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_backend_that_can_read_an_artifact_is_not_gated() {
    let models = models_dir();
    let (state, _data) = studio(models.path(), BackendKind::Misaka).await;

    // A gate that turned into a blanket ban would leave the one backend that CAN run an artifact
    // unable to. What is asserted is only that the gate did not fire — not what happens next, which
    // depends on whether this machine has the MISAKA runtime installed. Asserting the failure would
    // make this test pass for the wrong reason on a machine that has one.
    match state.load("qwen25-1.5b-a16", None).await {
        // An installed runtime loaded it, which is the claim.
        Ok(status) => assert_eq!(status.backend, "misaka"),
        // No runtime here: it must fail on the engine, not on the file.
        Err(error) => {
            assert!(
                matches!(error, Error::BackendUnavailable { .. }),
                "the artifact must reach the misaka backend and fail there, if at all: {error}"
            );
            assert!(!error.to_string().contains("PALW class artifact"), "{error}");
        }
    }
}
