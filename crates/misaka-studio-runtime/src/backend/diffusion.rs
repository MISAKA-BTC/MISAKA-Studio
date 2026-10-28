//! The diffusion backend — a `diffusers` sidecar, supervised as a child process.
//!
//! Same shape as `llamacpp.rs`/`mlx.rs`: a model's forward pass is not implemented here, or in
//! any Rust ML crate — `sidecars/diffusion/server.py` runs `diffusers.DiffusionPipeline`, which
//! resolves whichever pipeline class a checkpoint's own `model_index.json` names. This module's
//! job is only to supervise that process and speak its small JSON API.
//!
//! It does not reuse [`super::openai_child::ChildEngine`]: that supervisor's `load`/`generate`
//! are typed against [`super::LoadRequest`]/[`super::GenerationRequest`]/[`super::StreamEvent`]
//! — the chat shape — and an image request/response is a different shape (a resolution and a
//! reference-image list in, a batch of PNGs back, no token stream). Generalizing `ChildEngine`
//! over both would touch the three backends that already depend on its chat-typed methods for no
//! benefit to them, so this is a second, smaller supervisor with the same process lifecycle
//! (spawn, health poll, log capture, kill-on-drop) instead.

use super::{Availability, GeneratedImage, ImageBackend, ImageLoadRequest, LoadedImageModel, Txt2ImgRequest};
use crate::{Error, Result};
use base64::Engine as _;
use futures_util::future::BoxFuture;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;

const LOG_LINES: usize = 200;
const HEALTH_POLL: Duration = Duration::from_millis(250);

struct Running {
    child: tokio::process::Child,
    port: u16,
    model: LoadedImageModel,
}

pub struct DiffusionBackend {
    python: PathBuf,
    script: PathBuf,
    device: Option<String>,
    startup_timeout: Duration,
    running: Arc<RwLock<Option<Running>>>,
    log: Arc<Mutex<VecDeque<String>>>,
    http: reqwest::Client,
}

impl DiffusionBackend {
    pub fn new(python: Option<PathBuf>, script: Option<PathBuf>, device: Option<String>, startup_timeout: Duration) -> Self {
        DiffusionBackend {
            python: python.unwrap_or_else(|| PathBuf::from("python3")),
            script: script.unwrap_or_else(|| PathBuf::from("sidecars/diffusion/server.py")),
            device,
            startup_timeout,
            running: Arc::new(RwLock::new(None)),
            log: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_LINES))),
            // Image generation legitimately runs for minutes; only the connect leg is bounded.
            http: reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build().expect("http client builds"),
        }
    }

    fn push_log(log: &Arc<Mutex<VecDeque<String>>>, line: String) {
        let mut guard = log.lock().expect("log lock");
        if guard.len() == LOG_LINES {
            guard.pop_front();
        }
        guard.push_back(line);
    }

    fn log_tail(&self, lines: usize) -> String {
        let guard = self.log.lock().expect("log lock");
        let skip = guard.len().saturating_sub(lines);
        guard.iter().skip(skip).cloned().collect::<Vec<_>>().join("\n")
    }

    async fn base_url(&self) -> Result<String> {
        let guard = self.running.read().await;
        let running = guard.as_ref().ok_or(Error::NoModelLoaded)?;
        Ok(format!("http://127.0.0.1:{}", running.port))
    }
}

fn free_port() -> Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| Error::io("127.0.0.1:0", e))?;
    let port = listener.local_addr().map_err(|e| Error::io("127.0.0.1:0", e))?.port();
    Ok(port)
}

enum Pipe {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

async fn drain<R: tokio::io::AsyncRead + Unpin>(stream: R, log: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "engine", "{line}");
        DiffusionBackend::push_log(&log, line);
    }
}

impl ImageBackend for DiffusionBackend {
    fn name(&self) -> &'static str {
        "diffusion"
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async move {
            let output = tokio::process::Command::new(&self.python)
                .arg("--version")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await;
            match output {
                Ok(out) if out.status.success() => {
                    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&out.stderr));
                    Availability::Available { detail: text.lines().next().unwrap_or("present").trim().to_string() }
                }
                _ => Availability::Unavailable {
                    reason: format!("{} could not be run", self.python.display()),
                    remedy: "Install Python 3 and `pip install -r sidecars/diffusion/requirements.txt`, or set \
                             image_backend.python_path."
                        .into(),
                },
            }
        })
    }

    fn load(&self, request: ImageLoadRequest) -> BoxFuture<'_, Result<LoadedImageModel>> {
        Box::pin(async move {
            // A load replaces whatever was loaded — two sidecars would otherwise fight over the
            // same GPU memory, the same failure `ChildEngine::load` avoids for chat models.
            self.unload().await?;

            let started = Instant::now();
            let port = free_port()?;

            let mut command = tokio::process::Command::new(&self.python);
            command
                .arg(&self.script)
                .arg("--port")
                .arg(port.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            tracing::info!(program = %self.python.display(), script = %self.script.display(), port, "starting diffusion sidecar");
            let mut child = command.spawn().map_err(|e| Error::Engine {
                backend: "diffusion",
                message: format!("could not start {}: {e}", self.python.display()),
            })?;

            for stream in [child.stdout.take().map(Pipe::Out), child.stderr.take().map(Pipe::Err)].into_iter().flatten() {
                let log = self.log.clone();
                tokio::spawn(async move {
                    match stream {
                        Pipe::Out(s) => drain(s, log).await,
                        Pipe::Err(s) => drain(s, log).await,
                    }
                });
            }

            let base = format!("http://127.0.0.1:{port}");
            let deadline = Instant::now() + self.startup_timeout;
            loop {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(Error::Engine {
                        backend: "diffusion",
                        message: format!("sidecar exited with {status} before it was ready:\n{}", self.log_tail(15)),
                    });
                }
                if let Ok(resp) = self.http.get(format!("{base}/health")).send().await
                    && resp.status().is_success()
                {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill().await;
                    return Err(Error::Engine {
                        backend: "diffusion",
                        message: format!(
                            "sidecar did not become ready within {}s. Check that its dependencies are installed \
                             (`pip install -r sidecars/diffusion/requirements.txt`):\n{}",
                            self.startup_timeout.as_secs(),
                            self.log_tail(10)
                        ),
                    });
                }
                tokio::time::sleep(HEALTH_POLL).await;
            }

            let device = self.device.clone().unwrap_or_else(|| "cpu".into());
            let load_body = serde_json::json!({
                "model_id": request.model_id,
                "repo_id_or_path": request.model_path.display().to_string(),
                "dtype": request.dtype.as_str(),
                "device": device,
            });
            let response = self.http.post(format!("{base}/load")).json(&load_body).send().await.map_err(|e| Error::Engine {
                backend: "diffusion",
                message: format!("the sidecar did not accept the load request: {e}"),
            })?;
            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                let _ = child.kill().await;
                return Err(Error::Engine {
                    backend: "diffusion",
                    message: format!("sidecar returned {status} on load: {}", text.trim()),
                });
            }

            let model = LoadedImageModel { model_id: request.model_id.clone(), load_ms: started.elapsed().as_millis() as u64 };
            *self.running.write().await = Some(Running { child, port, model: model.clone() });
            tracing::info!(model = %model.model_id, ms = model.load_ms, "diffusion sidecar ready");
            Ok(model)
        })
    }

    fn unload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            if let Some(mut running) = self.running.write().await.take() {
                tracing::info!(model = %running.model.model_id, "stopping diffusion sidecar");
                let _ = running.child.kill().await;
                let _ = running.child.wait().await;
            }
            Ok(())
        })
    }

    fn loaded(&self) -> BoxFuture<'_, Option<LoadedImageModel>> {
        Box::pin(async move {
            let mut guard = self.running.write().await;
            if let Some(running) = guard.as_mut()
                && let Ok(Some(status)) = running.child.try_wait()
            {
                tracing::warn!(model = %running.model.model_id, %status, "the diffusion sidecar exited on its own");
                *guard = None;
                return None;
            }
            guard.as_ref().map(|r| r.model.clone())
        })
    }

    fn generate(&self, request: Txt2ImgRequest) -> BoxFuture<'_, Result<Vec<GeneratedImage>>> {
        Box::pin(async move {
            let base = self.base_url().await?;
            let body = serde_json::json!({
                "prompt": request.prompt,
                "negative_prompt": request.negative_prompt,
                "width": request.width,
                "height": request.height,
                "steps": request.steps,
                "guidance_scale": request.guidance_scale,
                "seed": request.seed,
                "reference_images": request.reference_images.iter().map(|b| base64::engine::general_purpose::STANDARD.encode(b)).collect::<Vec<_>>(),
                "n": request.n,
            });
            let response = self.http.post(format!("{base}/generate")).json(&body).send().await.map_err(|e| Error::Engine {
                backend: "diffusion",
                message: format!("the sidecar did not accept the request: {e}"),
            })?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                return Err(Error::Engine { backend: "diffusion", message: format!("sidecar returned {status}: {}", text.trim()) });
            }

            let parsed: serde_json::Value = response.json().await.map_err(|e| Error::Engine {
                backend: "diffusion",
                message: format!("could not parse the sidecar's response: {e}"),
            })?;
            let images = parsed.get("images").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            images
                .iter()
                .map(|v| {
                    let text = v
                        .as_str()
                        .ok_or_else(|| Error::Engine { backend: "diffusion", message: "image entry was not a string".into() })?;
                    let png_bytes = base64::engine::general_purpose::STANDARD
                        .decode(text)
                        .map_err(|e| Error::Engine { backend: "diffusion", message: format!("could not decode an image: {e}") })?;
                    Ok(GeneratedImage { png_bytes })
                })
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_python_reports_unavailable_with_a_remedy() {
        let backend = DiffusionBackend::new(Some(PathBuf::from("/no/such/python-binary")), None, None, Duration::from_secs(1));
        match backend.availability().await {
            Availability::Unavailable { reason, remedy } => {
                assert!(reason.contains("no/such/python-binary"));
                assert!(remedy.contains("pip install"));
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }
}
