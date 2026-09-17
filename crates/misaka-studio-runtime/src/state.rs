//! Everything the server holds, and the two decisions it makes on the user's behalf:
//! **which backend** and **how many layers on the GPU**.
//!
//! Both are "Auto" by default, and both are the sort of automatic that has to be explainable —
//! the app reports what it chose and why, because a person whose model is unexpectedly slow
//! needs to see "23 of 33 layers offloaded, VRAM was the limit" rather than a spinner.

use crate::backend::devices::{EngineDevice, Offload, OffloadEvidence, first_gpu};
use crate::backend::llamacpp::{LlamaCppBackend, accelerator_tag};
use crate::backend::misaka::MisakaBackend;
use crate::backend::mlx::MlxBackend;
use crate::backend::mock::MockBackend;
use crate::backend::{ChatMessage, GenerationRequest, LoadRequest, LoadedModel, SharedBackend, StreamEvent, Usage};
use crate::catalog::Catalog;
use crate::context::tokens::TokenCounter;
use crate::context::{ContextPlan, ContextReport, PlanInputs, SuppliedMemory};
use crate::download::DownloadManager;
use crate::metrics::MetricsHub;
use crate::records::{RecordStore, StoredRecord};
use crate::store::ModelStore;
use crate::{Error, Result};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use misaka_studio_core::HardwareSnapshot;
use misaka_studio_core::model::LocalModel;
use misaka_studio_core::palw;
use misaka_studio_core::provenance::{
    InferenceInputs, InferenceRecord, ModelIdentity, RuntimeIdentity, SamplingCommitment, canonical_prompt_bytes,
    canonical_raw_prompt_bytes,
};
use misaka_studio_core::settings::{BackendKind, GpuLayers, Settings};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// A model that is loaded, with everything provenance needs about it.
#[derive(Clone)]
pub struct LoadedState {
    pub model: LocalModel,
    pub loaded: LoadedModel,
    pub runtime: RuntimeIdentity,
    /// `None` until the file has been hashed.
    pub identity: Option<ModelIdentity>,
    pub backend: String,
    /// A sentence for the user when the offload that happened is not the one that was asked for.
    pub offload_note: Option<String>,
}

/// What the UI shows about the current runtime.
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeStatus {
    pub backend: String,
    pub backend_available: bool,
    pub model_id: Option<String>,
    pub context_size: Option<u32>,
    pub gpu_layers: Option<u32>,
    /// What became of the offload request, with its evidence. See [`crate::backend::devices`].
    pub offload: Option<Offload>,
    /// Why `gpu_layers` is not what the settings asked for, when it is not.
    pub offload_note: Option<String>,
    pub load_ms: Option<u64>,
    pub runtime_hash: Option<String>,
    pub runtime_class_id: Option<String>,
    pub model_hash: Option<String>,
    pub descriptor: Option<misaka_studio_core::provenance::RuntimeDescriptor>,
}

pub struct AppState {
    pub settings: RwLock<Settings>,
    pub settings_path: PathBuf,
    pub data_dir: PathBuf,
    pub hardware: HardwareSnapshot,
    pub store: Arc<ModelStore>,
    pub downloads: Arc<DownloadManager>,
    pub metrics: Arc<MetricsHub>,
    pub node: Arc<crate::node::NodeManager>,
    pub records: RwLock<Arc<RecordStore>>,
    /// Prompts waiting to be — or already — mined behind the chat. See `crate::mining_queue`.
    pub mining: Arc<crate::mining_queue::MiningQueue>,
    /// One load at a time. A startup load and a chat that asked for a model before it finished
    /// used to start two engines side by side, and the one that finished last was the one loaded.
    load_lock: tokio::sync::Mutex<()>,
    /// Installs a `llama-server` built for this machine's GPU. See `crate::engines`.
    pub engines: Arc<crate::engines::EngineInstaller>,
    /// Summarises older turns on a local model, when `context.summarizer_model` names one.
    pub summarizer: Arc<crate::context::summarizer::Summarizer>,
    /// Tokenizers by file: loaded, loading, or failed. A `tokenizer.json` is 7 MB of JSON; it is
    /// read once, in the background.
    counters: Arc<std::sync::Mutex<std::collections::HashMap<PathBuf, CounterSlot>>>,
    catalog: RwLock<Arc<Catalog>>,
    backend: RwLock<SharedBackend>,
    loaded: RwLock<Option<LoadedState>>,
}

impl AppState {
    pub async fn new(settings: Settings, settings_path: PathBuf, data_dir: PathBuf) -> Arc<Self> {
        let hardware = HardwareSnapshot::probe();
        let store = Arc::new(ModelStore::new(vec![settings.models_dir.clone()]));
        if let Err(e) = store.refresh().await {
            tracing::warn!("initial model scan failed: {e}");
        }
        let records = RecordStore::open(
            data_dir.join("inference-records.jsonl"),
            settings.provenance.max_records,
            settings.provenance.record_inferences,
        )
        .await;
        if let Err(e) = records.trim().await {
            tracing::warn!("could not trim the record log: {e}");
        }
        let catalog = Arc::new(Catalog::new(settings.huggingface.endpoint.clone(), settings.huggingface.token.clone()));
        let backend = build_backend(&settings, &hardware);
        let metrics = MetricsHub::new(&hardware);

        let node = Arc::new(crate::node::NodeManager::with_journal(Some(data_dir.join("produced-blocks.jsonl"))));
        let mining = crate::mining_queue::MiningQueue::open(data_dir.join("mining-queue.json")).await;
        let engines = Arc::new(crate::engines::EngineInstaller::new(data_dir.join("engines").join("llama.cpp")));
        let app = Arc::new(AppState {
            settings: RwLock::new(settings),
            settings_path,
            data_dir,
            hardware,
            store,
            downloads: Arc::new(DownloadManager::new()),
            metrics,
            node,
            records: RwLock::new(records),
            mining,
            load_lock: tokio::sync::Mutex::new(()),
            engines,
            summarizer: Arc::new(crate::context::summarizer::Summarizer::new()),
            counters: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            catalog: RwLock::new(catalog),
            backend: RwLock::new(backend),
            loaded: RwLock::new(None),
        });
        // The queue's worker lives as long as the app: prompts queued in an earlier run are still
        // owed a claim, and the person may have closed the window on them on purpose.
        app.mining.spawn_worker(app.clone());
        app
    }

    /// **Whether something other than the pool's gateway can answer the loaded model.**
    ///
    /// `Background` mining only makes sense if the chat has somewhere else to go: the local
    /// integer runtime for a class artifact (`misaka-palw-serve`, when it is installed) or
    /// llama.cpp/MLX for a GGUF (when their servers are). With nothing else installed the chat
    /// must keep mining inline, and the queue must not be fed on top of it — that would mine one
    /// prompt twice. `Err` carries the reason, for the panel.
    pub async fn local_engine_for_loaded_model(&self) -> std::result::Result<(), String> {
        let settings = self.settings.read().await.clone();
        let Some(loaded) = self.loaded().await else {
            return Err("no model is loaded".to_string());
        };
        let is_artifact = loaded.model.path.file_name().and_then(|n| n.to_str()).is_some_and(palw::is_artifact_filename);
        if is_artifact {
            let serve = settings.backend.misaka_serve_path.clone();
            match serve {
                Some(path) if path.is_file() => Ok(()),
                Some(path) => Err(format!("the local integer runtime is not at {}", path.display())),
                None => Err("the local integer runtime (misaka-palw-serve) is not installed; the chat can only be answered by the pool's gateway".to_string()),
            }
        } else {
            let backend = build_backend_kind(BackendKind::Auto, &settings, &self.hardware);
            if backend.availability().await.is_available() {
                Ok(())
            } else {
                Err(format!("no local engine can run this file ({} is not installed)", backend.name()))
            }
        }
    }

    pub async fn catalog(&self) -> Arc<Catalog> {
        self.catalog.read().await.clone()
    }

    /// Install the default class's artifact in the background, if it is not already here.
    ///
    /// "Download the Studio and it can already mine a model class" is the intent, and the reason
    /// it is a first-run download rather than a file in the repository is arithmetic: the
    /// artifact is 1.7 GiB, GitHub refuses any file over 100 MB, and LFS's free tier is smaller
    /// than the file. So the bytes arrive the same way pressing Install would deliver them —
    /// same manager, same progress in the UI, same verification against the digest the chain
    /// registered — just without anyone having to ask.
    ///
    /// It never blocks startup and never fails it. No network, a full disk, a mirror that is
    /// down: each of those leaves a working Studio with the Install button exactly where it was.
    /// Called by the binary rather than from `new` so that constructing state in a test does not
    /// reach the network.
    /// **Have the engine up before anyone asks.** Loads [`Settings::load_on_start`], if set.
    ///
    /// In the background and after the listener binds its own task, because a 1.7 GiB artifact is
    /// tens of seconds of mapping and an app that shows nothing until then looks broken. The
    /// outcome is logged either way: a startup load that silently did not happen is
    /// indistinguishable, from the chat box, from an engine that is merely slow.
    pub fn spawn_startup_load(self: &Arc<Self>) {
        let state = self.clone();
        tokio::spawn(async move {
            let Some(model_id) = state.settings.read().await.load_on_start.clone() else { return };
            tracing::info!(model = %model_id, "loading at startup");
            match state.load(&model_id, None).await {
                Ok(status) => tracing::info!(
                    model = %model_id,
                    backend = %status.backend,
                    ms = status.load_ms.unwrap_or(0),
                    "loaded at startup"
                ),
                // Named, not swallowed: the two reasons this fails — the model is gone, or the
                // engine for it is not installed — are both fixed from the Settings page, and the
                // person reading the log is the person who has to fix them.
                Err(e) => tracing::warn!(model = %model_id, "startup load failed: {e}"),
            }
        });
    }

    pub fn spawn_default_class_install(self: &Arc<Self>) {
        let state = self.clone();
        tokio::spawn(async move {
            let settings = state.settings.read().await.clone();
            if !settings.node.install_default_class_artifact {
                return;
            }

            let spec = misaka_studio_core::palw::default_class();
            let misaka_studio_core::palw::PalwArtifactSource::Download { filename, repo_path, sha256, size_bytes, hf_repo, .. } =
                &spec.artifact
            else {
                // A class with no published artifact cannot be preinstalled, and inventing a
                // conversion the user did not ask for is not the fallback.
                return;
            };

            let destination = settings.models_dir.join(filename);
            match tokio::fs::metadata(&destination).await {
                Ok(meta) if meta.len() == *size_bytes => return,
                // Someone else's file under our name — a half-copy, a different conversion. Not
                // ours to delete, and re-downloading beside it is not possible anyway.
                Ok(meta) => {
                    tracing::warn!(
                        "{} exists at {} bytes where {} expects {size_bytes}; leaving it alone",
                        destination.display(),
                        meta.len(),
                        spec.name
                    );
                    return;
                }
                Err(_) => {}
            }

            tracing::info!("installing the default class artifact for {} from {hf_repo} ({size_bytes} bytes)", spec.name);
            let catalog = state.catalog().await;
            let started = state
                .downloads
                .start(
                    &catalog,
                    state.store.clone(),
                    settings.models_dir.clone(),
                    hf_repo.to_string(),
                    // Pinned by content digest, so a branch that moves cannot change what
                    // verifies — the same reason the class download endpoint uses `main`.
                    "main".to_string(),
                    repo_path.to_string(),
                    Some(sha256.to_string()),
                    Some(*size_bytes),
                    None,
                )
                .await;
            match started {
                Ok(progress) => tracing::info!("default class artifact downloading into {}", progress.destination.display()),
                Err(e) => tracing::warn!("could not start the default class artifact download: {e}"),
            }
        });
    }

    pub async fn backend(&self) -> SharedBackend {
        self.backend.read().await.clone()
    }

    pub async fn loaded(&self) -> Option<LoadedState> {
        self.loaded.read().await.clone()
    }

    /// Apply new settings: persist them, then rebuild whatever they changed.
    ///
    /// Changing the backend or the model directory unloads the current model. That is the honest
    /// behaviour — the loaded model may not exist under the new directory, and it certainly is
    /// not loaded in the new engine — and it is stated in the API response rather than left for
    /// the user to discover when generation fails.
    pub async fn apply_settings(&self, new: Settings) -> Result<Settings> {
        let old = self.settings.read().await.clone();
        new.save(&self.settings_path)?;

        // A gateway engine IS its address and its token: `GatewayBackend::new` copies both at
        // construction and never reads settings again. Joining a new pool slot (or forgetting one)
        // rewrites exactly those two fields while the kind stays `Gateway`, so without this the
        // engine kept answering — and mining — for the slot the person had just left, with that
        // slot's token, while every status panel named the new one.
        let backend_changed = new.backend.kind != old.backend.kind
            || new.backend.llama_server_path != old.backend.llama_server_path
            || new.backend.mlx_server_path != old.backend.mlx_server_path
            || new.node.palw_gateway_url != old.node.palw_gateway_url
            || new.node.pool_slot_token != old.node.pool_slot_token
            // Background mining moves the chat off the gateway and onto the local runtime (when
            // it is installed); the engine has to follow the switch, in both directions.
            || new.node.mining_mode != old.node.mining_mode;
        let models_dir_changed = new.models_dir != old.models_dir;
        let hub_changed = new.huggingface.endpoint != old.huggingface.endpoint || new.huggingface.token != old.huggingface.token;
        let recording_changed = new.provenance.record_inferences != old.provenance.record_inferences
            || new.provenance.max_records != old.provenance.max_records;

        if new.context.summarizer_model != old.context.summarizer_model
            || new.backend.llama_server_path != old.backend.llama_server_path
        {
            self.summarizer.shutdown().await;
        }
        if backend_changed {
            self.unload().await?;
            *self.backend.write().await = build_backend(&new, &self.hardware);
        }
        if models_dir_changed {
            self.store.set_roots(vec![new.models_dir.clone()]).await?;
        }
        if hub_changed {
            *self.catalog.write().await = Arc::new(Catalog::new(new.huggingface.endpoint.clone(), new.huggingface.token.clone()));
        }
        if recording_changed {
            *self.records.write().await = RecordStore::open(
                self.data_dir.join("inference-records.jsonl"),
                new.provenance.max_records,
                new.provenance.record_inferences,
            )
            .await;
        }

        *self.settings.write().await = new.clone();
        Ok(new)
    }

    /// **The engine that can read THIS file.**
    ///
    /// The configured engine is used whenever it can run the model. When it cannot — a class
    /// artifact under llama.cpp, or a GGUF under the integer runtime — the other kind is built
    /// instead, because the pairing is a property of the file and not a preference: a person with
    /// a GGUF and a `.palwart` in one directory was otherwise told to go and change a setting
    /// between every message.
    ///
    /// For an artifact the pool's gateway wins when one is configured: it is the engine that also
    /// MINES, and someone who joined a pool slot asked for exactly that.
    async fn backend_for(&self, model: &LocalModel, settings: &Settings) -> SharedBackend {
        let configured = self.backend.read().await.clone();
        let is_artifact = model.path.file_name().and_then(|n| n.to_str()).is_some_and(palw::is_artifact_filename);
        // In `Background` mode the chat is answered here and the gateway mines from the queue —
        // but only when the local integer runtime is actually installed. Otherwise the gateway
        // stays the chat's engine and the queue is told not to double up (see
        // `local_engine_for_loaded_model`).
        let local_serve_installed = settings.backend.misaka_serve_path.as_deref().is_some_and(|p| p.is_file());
        let background = settings.node.mining_mode == misaka_studio_core::settings::MiningMode::Background;
        let prefer_local = is_artifact && background && local_serve_installed;
        if prefer_local && configured.name() == crate::backend::gateway::NAME {
            return build_backend_kind(BackendKind::Misaka, settings, &self.hardware);
        }
        let reads_artifacts = [MisakaBackend::NAME, crate::backend::gateway::NAME].contains(&configured.name());
        if is_artifact == reads_artifacts {
            return configured;
        }
        let kind = if is_artifact {
            if settings.node.palw_gateway_url.is_some() && !prefer_local { BackendKind::Gateway } else { BackendKind::Misaka }
        } else {
            BackendKind::Auto
        };
        build_backend_kind(kind, settings, &self.hardware)
    }

    /// Load a model into the engine that can run it.
    pub async fn load(&self, model_id: &str, context_override: Option<u32>) -> Result<RuntimeStatus> {
        let _one_at_a_time = self.load_lock.lock().await;
        // Asked for what is already loaded — typically a chat that waited here for the startup load
        // of the same model: that load is the answer, not a reason to start the engine again.
        if let Some(current) = self.loaded().await
            && current.model.id == model_id
            && context_override.is_none_or(|n| n == current.loaded.context_size)
            && self.backend().await.loaded().await.is_some()
        {
            return Ok(self.status_from(Some(&current), true).await);
        }
        let model = self.store.require(model_id).await?;
        let settings = self.settings.read().await.clone();
        let backend = self.backend_for(&model, &settings).await;

        // A PALW artifact is not a GGUF. `llama-server` reads its first four bytes, finds `PALW`
        // where `GGUF` should be, and aborts — and what the user got for a file the Studio already
        // knew was unloadable was fifteen lines of another program's stderr, in a notification that
        // stays until it is dismissed. The check is here, above every backend, because the model
        // list deliberately carries artifacts (the MISAKA runtime loads them; nothing else can) and
        // the backend is a global setting, so the pairing can only be judged at the load.
        //
        // Before the availability check on purpose: `llamacpp is not available, install it` is a
        // true sentence that sends someone to build an engine which still could not read this
        // file. The pairing is wrong whether or not the engine is installed.
        //
        // The pairing runs BOTH ways. An engine handed the other kind of file does not decline it:
        // it starts, reads a header it cannot parse, and aborts, and what the user gets for a
        // mismatch the Studio could see coming is fifteen lines of that program's stderr.
        if let Some(message) = engine_pairing_refusal(&model.id, model.path.file_name().and_then(|n| n.to_str()), backend.name()) {
            return Err(Error::BadRequest { message });
        }

        let availability = backend.availability().await;
        if let crate::backend::Availability::Unavailable { reason, remedy } = availability {
            return Err(Error::BackendUnavailable { backend: backend.name().to_string(), reason, remedy });
        }

        let context_size =
            context_override.or(settings.generation.context_size).unwrap_or_else(|| model.recommended_context(&self.hardware) as u32);
        // The engine's own device list, when it has one: a CPU-only build on a machine with a
        // card must plan for the CPU, and a Vulkan build on a card the probe cannot see must plan
        // for the card.
        // Only an engine that offloads is given a layer count. The integer runtime computes on the CPU by
        // construction and the gateway runs somewhere else; a planned count for either was echoed back
        // as "28/28 on GPU" on the model bar, once the artifact's header said how many layers it has.
        let offloads = ![MisakaBackend::NAME, crate::backend::gateway::NAME].contains(&backend.name());
        let devices = if offloads { backend.devices().await } else { None };
        let gpu_layers = if offloads {
            plan_gpu_layers(&model, &self.hardware, devices.as_deref(), context_size as u64, settings.backend.gpu_layers)
        } else {
            None
        };

        let loaded = backend
            .load(LoadRequest {
                model_id: model.id.clone(),
                model_path: model.path.clone(),
                context_size,
                gpu_layers,
                threads: settings.backend.threads,
                flash_attention: settings.backend.flash_attention,
                use_mmap: settings.backend.use_mmap,
                use_mlock: settings.backend.use_mlock,
                // The header already told us; the engine has no way to guess.
                needs_default_chat_template: !model.has_chat_template,
                extra_args: settings.backend.extra_args.clone(),
            })
            .await?;

        let runtime = RuntimeIdentity::derive(backend.descriptor().await);
        // Hashing is deliberately not done here: it would add a minute to every load of a large
        // model. The identity fills in the first time provenance is asked for.
        let identity = model.identity();
        // The engine that answered this load is the one `generate` has to reach, so the choice is
        // recorded rather than recomputed. Unloading does not put the configured one back: what
        // matters is which engine holds the model, and after an unload none does.
        *self.backend.write().await = backend.clone();
        let offload_note = offload_note(settings.backend.gpu_layers, loaded.offload.as_ref(), &self.hardware);
        if let Some(note) = &offload_note {
            tracing::warn!(model = %model.id, "{note}");
        }
        let state = LoadedState { model, loaded, runtime, identity, backend: backend.name().to_string(), offload_note };
        // A class's tokenizer is what its 512 tokens are counted in; have it ready before the first
        // question rather than after it.
        self.prepare_tokenizer(&state.model).await;
        *self.loaded.write().await = Some(state.clone());
        Ok(self.status_from(Some(&state), true).await)
    }

    pub async fn unload(&self) -> Result<()> {
        let backend = self.backend().await;
        backend.unload().await?;
        *self.loaded.write().await = None;
        Ok(())
    }

    /// Compute and cache the model identity for the loaded model, hashing the file if needed.
    pub async fn resolve_identity(&self) -> Result<Option<ModelIdentity>> {
        let Some(state) = self.loaded().await else { return Ok(None) };
        if let Some(identity) = state.identity {
            return Ok(Some(identity));
        }
        let hashed = self.store.ensure_hashed(&state.model.id).await?;
        let identity = hashed.identity();
        if let Some(slot) = self.loaded.write().await.as_mut() {
            slot.model = hashed;
            slot.identity = identity.clone();
        }
        Ok(identity)
    }

    pub async fn status(&self) -> RuntimeStatus {
        let loaded = self.reconciled_loaded().await;
        let backend = self.backend().await;
        let available = backend.availability().await.is_available();
        self.status_from(loaded.as_ref(), available).await
    }

    /// What is loaded, after asking the engine whether it is still there.
    ///
    /// The Studio's own record says "loaded" from the load until the unload. The engine's process
    /// can leave in between — a driver fault, the OS taking its memory back, a signal — and says
    /// nothing. This is the one place both are compared, so a dead engine turns into "nothing is
    /// loaded" the next time anyone asks, instead of a model that refuses every connection.
    async fn reconciled_loaded(&self) -> Option<LoadedState> {
        let state = self.loaded().await?;
        if self.backend().await.loaded().await.is_none() {
            tracing::warn!(model = %state.model.id, "the engine holding this model is gone; marking it unloaded");
            *self.loaded.write().await = None;
            return None;
        }
        Some(state)
    }

    async fn status_from(&self, state: Option<&LoadedState>, available: bool) -> RuntimeStatus {
        let backend = self.backend().await;
        match state {
            Some(s) => RuntimeStatus {
                backend: s.backend.clone(),
                backend_available: available,
                model_id: Some(s.model.id.clone()),
                context_size: Some(s.loaded.context_size),
                gpu_layers: s.loaded.gpu_layers,
                offload: s.loaded.offload.clone(),
                offload_note: s.offload_note.clone(),
                load_ms: Some(s.loaded.load_ms),
                runtime_hash: Some(s.runtime.h_r.to_hex()),
                runtime_class_id: Some(s.runtime.class_id.to_hex()),
                model_hash: s.identity.as_ref().map(|i| i.h_m.to_hex()),
                descriptor: Some(s.runtime.descriptor.clone()),
            },
            None => RuntimeStatus {
                backend: backend.name().to_string(),
                backend_available: available,
                model_id: None,
                context_size: None,
                gpu_layers: None,
                offload: None,
                offload_note: None,
                load_ms: None,
                runtime_hash: None,
                runtime_class_id: None,
                model_hash: None,
                descriptor: None,
            },
        }
    }

    /// Generate, with metrics and provenance attached.
    ///
    /// The returned stream is the backend's, wrapped: text is accumulated as it passes so the
    /// completion can be committed to, and the record is written when the stream ends. Wrapping
    /// rather than buffering matters — the user sees tokens as they arrive, and the record still
    /// covers the whole answer.
    pub async fn generate(
        self: &Arc<Self>,
        messages: Vec<ChatMessage>,
        prompt: Option<String>,
        params: SamplingCommitment,
        stop: Vec<String>,
    ) -> Result<BoxStream<'static, Result<StreamEvent>>> {
        Ok(self.generate_managed(messages, prompt, params, stop, Vec::new()).await?.1)
    }

    /// [`Self::generate`], through the context manager, returning what it decided.
    ///
    /// `pinned` are the conversation's pinned notes. The report is `None` for a raw completion,
    /// which has no conversation to manage.
    pub async fn generate_managed(
        self: &Arc<Self>,
        messages: Vec<ChatMessage>,
        prompt: Option<String>,
        params: SamplingCommitment,
        stop: Vec<String>,
        pinned: Vec<String>,
    ) -> Result<(Option<ContextReport>, BoxStream<'static, Result<StreamEvent>>)> {
        let state = self.loaded().await.ok_or(Error::NoModelLoaded)?;
        let backend = self.backend().await;
        // An engine that died between two messages is found out here, not by the connection
        // refused that would otherwise be this message's answer.
        if backend.loaded().await.is_none() {
            *self.loaded.write().await = None;
            return Err(Error::Engine {
                backend: backend.name(),
                message: format!("the engine that held {} is no longer running — load the model again", state.model.id),
            });
        }

        // **What the model sees** — the context manager's decision (see `crate::context`): the
        // system prompt and the question always, then the pinned notes, recent turns whole, and a
        // memory of the older ones, inside the window less the answer's reserve.
        let window = state.loaded.context_size as u64;
        let counter = if prompt.is_none() { Some(self.token_counter_for(&state.model).await) } else { None };
        let mut report: Option<ContextReport> = None;
        // Kept for a refusal's retry, which plans again from what the client sent.
        let client_messages = if counter.is_some() { messages.clone() } else { Vec::new() };
        let messages = match &counter {
            Some(counter) => {
                let plan = self.plan_context(&backend, &messages, &pinned, window, params.max_tokens, counter, 1000).await?;
                if plan.report.managed {
                    tracing::info!(
                        window,
                        prompt_tokens = plan.report.prompt_tokens,
                        recent = plan.report.recent_messages,
                        older = plan.report.older_messages,
                        memory = ?plan.report.memory.as_ref().map(|m| m.source),
                        "context managed"
                    );
                }
                report = Some(plan.report);
                plan.messages
            }
            None => messages,
        };

        // The bytes the record commits to. Canonical and length-prefixed — see
        // `canonical_prompt_bytes`, which exists because the obvious `role: content` flattening
        // lets two different conversations produce the same commitment.
        let prompt_bytes = match &prompt {
            Some(raw) => canonical_raw_prompt_bytes(raw),
            None => {
                let pairs: Vec<(&str, &str)> = messages.iter().map(|m| (m.role.as_str(), m.content.as_str())).collect();
                canonical_prompt_bytes(&pairs)
            }
        };

        let exact_tokens = counter.as_ref().filter(|c| c.is_exact()).and(report.as_ref()).map(|r| r.prompt_tokens);
        let request = GenerationRequest {
            model: state.model.id.clone(),
            messages,
            prompt,
            params,
            stop: stop.clone(),
            prompt_tokens: exact_tokens,
            disable_thinking: false,
        };
        let is_chat = request.prompt.is_none();

        self.metrics.generation_started();
        let inner = match backend.generate(request).await {
            Ok(stream) => stream,
            Err(e) => {
                // The counter must come back down on the failure path too, or "1 generation
                // active" sticks forever after one bad request.
                self.metrics.generation_finished(0, 0.0, 0);
                return Err(e);
            }
        };

        // **The refusal arrives in the stream, not from the call.** The engine answers 200 and
        // puts "prompt 257 + decode ceiling 256 exceeds max_context_tokens 512" in the first event,
        // so a retry that only watches `generate`'s return value never fires. Nothing has reached
        // the user at this point, which is what makes taking one item and deciding safe.
        //
        // The refusal names the window and the engine's count of the prompt. Planned again with
        // both — the count scaled by how far the app's own count fell short — the request that
        // fits is arithmetic, and it is asked for once.
        let mut inner = inner;
        let first = inner.next().await;
        let (mut inner, first) = match (&first, &counter, &report) {
            (Some(Err(e)), Some(counter), Some(sent)) if is_chat => {
                let message = e.to_string();
                match crate::backend::context_limit_from_refusal(&message) {
                    Some(limit) => {
                        let engine_count = crate::backend::refusal_prompt_tokens(&message).unwrap_or(0);
                        let scale = if sent.prompt_tokens > 0 && engine_count > sent.prompt_tokens {
                            (engine_count * 1000).div_ceil(sent.prompt_tokens) + 50
                        } else {
                            1100
                        };
                        let window = if window == 0 { limit } else { window.min(limit) };
                        let before = sent.prompt_tokens;
                        match self.plan_context(&backend, &client_messages, &pinned, window, params.max_tokens, counter, scale).await {
                            Ok(plan) if plan.report.prompt_tokens < before => {
                                tracing::info!(
                                    limit,
                                    engine_count,
                                    scale,
                                    "the engine refused the prompt's length; planned again in its count and retried"
                                );
                                let retry = GenerationRequest {
                                    model: state.model.id.clone(),
                                    messages: plan.messages.clone(),
                                    prompt: None,
                                    params,
                                    stop: stop.clone(),
                                    prompt_tokens: None,
                                    disable_thinking: false,
                                };
                                match backend.generate(retry).await {
                                    Ok(mut s2) => {
                                        let f2 = s2.next().await;
                                        report = Some(plan.report);
                                        (s2, f2)
                                    }
                                    Err(_) => (inner, first),
                                }
                            }
                            _ => (inner, first),
                        }
                    }
                    None => (inner, first),
                }
            }
            _ => (inner, first),
        };

        let app = self.clone();
        let started = Instant::now();
        let started_at_unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);

        let (tx, rx) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            let mut pending_first = first;
            let mut text = String::new();
            let mut first_token: Option<Duration> = None;
            let mut usage = Usage::default();

            while let Some(event) = match pending_first.take() {
                Some(e) => Some(e),
                None => inner.next().await,
            } {
                match &event {
                    Ok(StreamEvent::Delta(delta)) => {
                        if first_token.is_none() {
                            first_token = Some(started.elapsed());
                        }
                        text.push_str(delta);
                    }
                    Ok(StreamEvent::Done { usage: u, .. }) => usage = *u,
                    Err(_) => {}
                }
                if tx.send(event).await.is_err() {
                    break; // client hung up
                }
            }

            let duration_ms = started.elapsed().as_millis() as u64;
            let ttft = first_token.map(|d| d.as_millis() as u64);
            let tps = if duration_ms > 0 { usage.completion_tokens as f64 * 1000.0 / duration_ms as f64 } else { 0.0 };
            app.metrics.generation_finished(usage.completion_tokens, tps, ttft.unwrap_or(0));
            app.record(&state, &prompt_bytes, &text, usage, started_at_unix_ms, duration_ms, ttft, params).await;
        });

        Ok((report, Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))))
    }

    /// Plan the context for one request: the pure decision in `crate::context`, with the memory
    /// filled in from the summariser when one is configured and needed.
    #[allow(clippy::too_many_arguments)]
    async fn plan_context(
        &self,
        backend: &SharedBackend,
        messages: &[ChatMessage],
        pinned: &[String],
        window: u64,
        max_tokens: u64,
        counter: &TokenCounter,
        scale_permille: u64,
    ) -> Result<ContextPlan> {
        let continues = backend.continues_assistant_turn();
        let draft = crate::context::plan(&PlanInputs {
            messages,
            pinned,
            window,
            max_tokens,
            counter,
            engine_continues_turns: continues,
            count_scale_permille: scale_permille,
        })
        .map_err(|message| Error::BadRequest { message })?;

        let (supplied, note) = if draft.wants_memory() {
            let settings = self.settings.read().await.clone();
            match settings.context.summarizer_model.clone() {
                None => (None, None),
                Some(model_id) => match self.store.get(&model_id).await {
                    None => (None, Some(format!("the summariser model {model_id} is not installed; the older turns were extracted"))),
                    Some(model) => {
                        let target = draft.memory_budget.clamp(48, 240);
                        match self
                            .summarizer
                            .summarize(&model, &settings, &self.hardware, &draft.older, target, draft.japanese())
                            .await
                        {
                            Ok(text) => (Some(SuppliedMemory { text, source: crate::context::MemorySource::Summary }), None),
                            Err(e) => {
                                tracing::warn!("summariser: {e}");
                                (None, Some(format!("the summariser failed ({e}); the older turns were extracted")))
                            }
                        }
                    }
                },
            }
        } else {
            (None, None)
        };
        let mut plan = draft.finish(supplied, counter, note);
        plan.report.backend = backend.name().to_string();

        // A cut-off reply for an engine that closes every turn: the question and the reply's end,
        // spelled as one instruction (see `continuation_as_instruction`).
        if plan.report.continuation && !continues {
            let budget = plan.report.prompt_budget;
            let rewritten = crate::backend::continuation_as_instruction(&plan.messages, budget, counter)
                .map_err(|message| Error::BadRequest { message })?;
            plan.report.prompt_tokens = counter.messages(&rewritten);
            plan.report.managed = true;
            plan.report.sent = Some(rewritten.clone());
            plan.messages = rewritten;
        }
        Ok(plan)
    }

    /// **The counter for this model's tokens**: its tokenizer when one is at hand, the estimate
    /// otherwise.
    ///
    /// Looked for in order: `context.tokenizer_path`; for a class artifact, the class's pinned
    /// tokenizer beside it (`<stem>.tokenizer.json`), a `tokenizer.json` beside it, and
    /// `backend.misaka_tokenizer_path`. A GGUF carries its vocabulary inside and has a window wide
    /// enough that the estimate is not what decides a turn, so it is estimated.
    pub async fn token_counter_for(&self, model: &LocalModel) -> Arc<TokenCounter> {
        let Some(path) = self.tokenizer_path_for(model).await else {
            return Arc::new(TokenCounter::estimate());
        };
        self.start_tokenizer_load(&path);
        // **A chat never waits on a tokenizer for long.** Measured: a daemon whose tokenizer sat in
        // a folder the OS asked permission for blocked in `open` forever, and — the load holding
        // the lock every request took — so did every chat after it. The load runs on its own; a
        // request waits a moment for it and is otherwise counted with the estimate.
        let deadline = Instant::now() + TOKENIZER_WAIT;
        loop {
            match self.counters.lock().expect("counters").get(&path) {
                Some(CounterSlot::Ready(counter)) => return counter.clone(),
                Some(CounterSlot::Failed { .. }) | None => return Arc::new(TokenCounter::estimate()),
                Some(CounterSlot::Loading) => {}
            }
            if Instant::now() >= deadline {
                tracing::info!(path = %path.display(), "the tokenizer is still loading; this request is counted with the estimate");
                return Arc::new(TokenCounter::estimate());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Begin loading a tokenizer unless it is loaded, loading, or failed recently.
    fn start_tokenizer_load(&self, path: &std::path::Path) {
        {
            let mut counters = self.counters.lock().expect("counters");
            match counters.get(path) {
                Some(CounterSlot::Ready(_)) | Some(CounterSlot::Loading) => return,
                Some(CounterSlot::Failed { at, .. }) if at.elapsed() < TOKENIZER_RETRY => return,
                _ => {}
            }
            counters.insert(path.to_path_buf(), CounterSlot::Loading);
        }
        let counters = self.counters.clone();
        let path = path.to_path_buf();
        tokio::spawn(async move {
            let load_path = path.clone();
            let outcome = tokio::task::spawn_blocking(move || TokenCounter::from_tokenizer_file(&load_path)).await;
            let slot = match outcome {
                Ok(Ok(counter)) => {
                    tracing::info!(path = %path.display(), "tokenizer loaded; prompts are counted exactly");
                    CounterSlot::Ready(Arc::new(counter))
                }
                Ok(Err(e)) => {
                    tracing::warn!("tokenizer {e}; counting with the estimate");
                    CounterSlot::Failed { at: Instant::now(), why: e }
                }
                Err(e) => CounterSlot::Failed { at: Instant::now(), why: format!("the load did not run: {e}") },
            };
            counters.lock().expect("counters").insert(path, slot);
        });
    }

    async fn tokenizer_path_for(&self, model: &LocalModel) -> Option<PathBuf> {
        let settings = self.settings.read().await;
        if let Some(path) = settings.context.tokenizer_path.clone().filter(|p| p.is_file()) {
            return Some(path);
        }
        let file_name = model.path.file_name().and_then(|n| n.to_str())?;
        if !palw::is_artifact_filename(file_name) {
            return None;
        }
        let dir = model.path.parent()?;
        let pinned =
            palw::class_for_artifact_filename(file_name).and_then(|class| class.tokenizer_filename()).map(|name| dir.join(name));
        [pinned, Some(dir.join("tokenizer.json")), settings.backend.misaka_tokenizer_path.clone()]
            .into_iter()
            .flatten()
            .find(|p| p.is_file())
    }

    /// Fetch a class's pinned tokenizer beside its artifact when it is not already at hand, in the
    /// background and through the download list — 7 MB, verified against the pinned digest.
    async fn prepare_tokenizer(&self, model: &LocalModel) {
        if let Some(path) = self.tokenizer_path_for(model).await {
            self.start_tokenizer_load(&path);
            return;
        }
        if !self.settings.read().await.context.fetch_class_tokenizer {
            return;
        }
        let Some(file_name) = model.path.file_name().and_then(|n| n.to_str()) else { return };
        let Some(class) = palw::class_for_artifact_filename(file_name) else { return };
        let (Some(pin), Some(name), Some(dir)) = (class.tokenizer, class.tokenizer_filename(), model.path.parent()) else { return };
        let url = self.catalog().await.download_url(pin.hf_repo, "main", pin.repo_path);
        let id = format!("{}/{}", pin.hf_repo, name);
        match self
            .downloads
            .fetch(
                id,
                pin.hf_repo.to_string(),
                name.clone(),
                format!("{} tokenizer", class.name),
                url,
                dir.join(&name),
                Some(pin.sha256.to_string()),
                Some(pin.size_bytes),
            )
            .await
        {
            Ok(_) => tracing::info!(class = class.name, "fetching the class tokenizer beside its artifact"),
            Err(e) => tracing::warn!(class = class.name, "could not fetch the class tokenizer: {e}"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn record(
        &self,
        state: &LoadedState,
        prompt: &[u8],
        completion: &str,
        usage: Usage,
        started_at_unix_ms: u64,
        duration_ms: u64,
        time_to_first_token_ms: Option<u64>,
        params: SamplingCommitment,
    ) {
        let records = self.records.read().await.clone();
        if !records.is_enabled() {
            return;
        }
        let keep_transcripts = self.settings.read().await.provenance.keep_transcripts;
        // Use the identity if it is already known; do not hash a 40 GB file on the completion
        // path. `model: None` then says plainly that this run is not attributed to an artifact.
        let identity = state.identity.clone();
        let record = InferenceRecord::new(
            uuid::Uuid::new_v4().to_string(),
            InferenceInputs {
                model: identity.as_ref(),
                runtime: &state.runtime,
                params,
                prompt,
                output: completion.as_bytes(),
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                started_at_unix_ms,
                duration_ms,
                time_to_first_token_ms,
            },
        );
        records
            .append(StoredRecord {
                record,
                // The transcript is the readable text, not the canonical commitment bytes:
                // a person auditing this log wants the conversation, and the commitment is
                // already the hash beside it.
                prompt: keep_transcripts.then(|| String::from_utf8_lossy(prompt).into_owned()),
                completion: keep_transcripts.then(|| completion.to_string()),
                model_id: Some(state.model.id.clone()),
            })
            .await;
    }
}

/// How long a request waits for a tokenizer that is still loading before it is counted with the
/// estimate instead.
const TOKENIZER_WAIT: Duration = Duration::from_secs(3);
/// How long a tokenizer that failed to load is left alone before it is tried again.
const TOKENIZER_RETRY: Duration = Duration::from_secs(300);

/// A tokenizer file's state in the counter cache.
enum CounterSlot {
    Ready(Arc<TokenCounter>),
    Loading,
    Failed {
        at: Instant,
        #[allow(dead_code)]
        why: String,
    },
}

/// Build the backend a settings value asks for.
pub fn build_backend(settings: &Settings, hardware: &HardwareSnapshot) -> SharedBackend {
    build_backend_kind(settings.backend.kind, settings, hardware)
}

/// Build one particular engine, from the same settings.
///
/// Split out because the engine is not only a preference: a `.palwart` can be run by the integer
/// runtime or a gateway and by nothing else, and a GGUF by neither. The setting says which engine
/// to prefer FOR THE FILES IT CAN RUN; the file decides the rest.
pub fn build_backend_kind(kind: BackendKind, settings: &Settings, hardware: &HardwareSnapshot) -> SharedBackend {
    let timeout = Duration::from_secs(settings.backend.startup_timeout_secs);
    let tag = accelerator_tag(hardware);
    match kind {
        BackendKind::Mock => Arc::new(MockBackend::default()),
        BackendKind::Mlx => Arc::new(MlxBackend::new(settings.backend.mlx_server_path.clone(), timeout)),
        BackendKind::LlamaCpp => Arc::new(LlamaCppBackend::new(settings.backend.llama_server_path.clone(), tag, timeout)),
        // The integer runtime, driven through the same child-engine supervisor as the others. It
        // refuses rather than substituting when its server is missing: a record naming MISAKA must
        // come from the MISAKA runtime.
        BackendKind::Gateway => Arc::new(crate::backend::gateway::GatewayBackend::new(
            settings.node.palw_gateway_url.clone().unwrap_or_else(|| "http://127.0.0.1:8790".to_string()),
            // A pool-hosted gateway is the slot's, and the slot's token is what says so — the same
            // token the Network tab already holds. There is one slot, not one per feature.
            settings.node.pool_slot_token.clone(),
        )),
        BackendKind::Misaka => Arc::new(MisakaBackend::new(
            settings.backend.misaka_serve_path.clone(),
            settings.backend.misaka_tokenizer_path.clone(),
            timeout,
        )),
        // Auto: MLX where it can run, llama.cpp everywhere else. MLX is chosen only on Apple
        // Silicon, and only when its server is actually installed — the check happens at load,
        // where a missing engine is reported with a remedy.
        BackendKind::Auto => {
            if MlxBackend::platform_supported() && settings.backend.mlx_server_path.is_some() {
                Arc::new(MlxBackend::new(settings.backend.mlx_server_path.clone(), timeout))
            } else {
                Arc::new(LlamaCppBackend::new(settings.backend.llama_server_path.clone(), tag, timeout))
            }
        }
    }
}

/// How many layers to put on the accelerator.
///
/// The arithmetic is the same as the fit estimate: the accelerator's budget less the KV cache and
/// the compute overhead, divided by the per-layer weight size. What is left is what can be
/// offloaded, and offloading one layer more is an out-of-memory error at load time — the failure
/// this function exists to avoid.
///
/// `engine_devices` is what the engine itself can drive, when it could be asked. It outranks the
/// hardware probe in both directions: a CPU-only build gets no layers however good the card, and a
/// Vulkan build that sees a card `nvidia-smi` and `rocm-smi` do not gets the card's memory as its
/// budget. `None` — an engine too old to say — leaves the probe's answer as it was.
pub fn plan_gpu_layers(
    model: &LocalModel,
    hardware: &HardwareSnapshot,
    engine_devices: Option<&[EngineDevice]>,
    context: u64,
    setting: GpuLayers,
) -> Option<u32> {
    let total_layers = model.block_count.unwrap_or(0) as u32;
    match setting {
        GpuLayers::None => return Some(0),
        // 999 is llama.cpp's idiom for "all of them", and it is right even when the layer count
        // is unknown.
        GpuLayers::All => return Some(if total_layers > 0 { total_layers + 1 } else { 999 }),
        GpuLayers::Fixed { layers } => return Some(layers),
        GpuLayers::Auto => {}
    }

    let budget = match engine_devices {
        // The engine has a device: its own free-memory figure is the budget, or the probe's for
        // the same device when the engine did not report one.
        Some(devices) => match first_gpu(devices) {
            Some(gpu) => gpu.usable_bytes().or_else(|| probed_budget(hardware)),
            None => return Some(0),
        },
        None => probed_budget(hardware),
    };
    let Some(budget) = budget else {
        return Some(0);
    };

    let requirements = model.requirements(context);
    if requirements.total_bytes <= budget {
        return Some(if total_layers > 0 { total_layers + 1 } else { 999 });
    }
    if total_layers == 0 {
        // No layer count and it does not all fit: let the engine decide rather than guess a
        // number that could be far too high.
        return None;
    }
    let per_layer = (requirements.weights_bytes / total_layers as u64).max(1);
    let for_weights = budget.saturating_sub(requirements.kv_cache_bytes).saturating_sub(requirements.overhead_bytes);
    Some(((for_weights / per_layer) as u32).min(total_layers))
}

/// The largest accelerator pool the hardware probe found, if it found one.
fn probed_budget(hardware: &HardwareSnapshot) -> Option<u64> {
    hardware
        .accelerators
        .iter()
        .filter(|a| a.kind != misaka_studio_core::hardware::AcceleratorKind::Cpu)
        .filter_map(|a| a.usable_memory)
        .max()
}

/// **Why the model is on the CPU, when it was not meant to be.**
///
/// One sentence, or none. Written for the person who set "Auto" or "All layers", watched a model
/// answer at CPU speed, and has a right to know whether that was the engine, the card, or the
/// setting — and what to do about it. Silent when what happened is what was asked for.
pub fn offload_note(setting: GpuLayers, offload: Option<&Offload>, hardware: &HardwareSnapshot) -> Option<String> {
    let offload = offload?;
    let probed_gpu =
        hardware.accelerators.iter().find(|a| a.kind != misaka_studio_core::hardware::AcceleratorKind::Cpu).map(|a| a.name.clone());
    let idle_card = probed_gpu.as_deref().map(|name| format!(" This machine's {name} is idle.")).unwrap_or_default();
    let install = " Settings → Backend can install a llama.cpp build that uses the GPU.";

    match (setting, offload.evidence) {
        // Nothing asked for, nothing to explain.
        (GpuLayers::None, _) => None,
        // Asked for layers, got none, and the engine's own device list says why.
        (_, OffloadEvidence::DeviceList) if offload.asked_but_none_landed() => {
            Some(format!("No layers were offloaded: this llama-server is a CPU-only build.{idle_card}{install}"))
        }
        // Auto found nothing to offload to, from the engine's list.
        (GpuLayers::Auto, OffloadEvidence::DeviceList) if offload.asked == Some(0) => {
            Some(format!("Running on the CPU: this llama-server lists no GPU device.{idle_card}{install}"))
        }
        // The log itself shows the weights on the CPU after a request to offload them.
        (_, OffloadEvidence::EngineLog) if offload.asked_but_none_landed() => Some(format!(
            "No layers were offloaded: the engine put every model buffer in system memory (asked for {}).{idle_card}{install}",
            offload.asked.unwrap_or(0)
        )),
        // Auto, planned from the probe, and the probe had nothing.
        (GpuLayers::Auto, OffloadEvidence::EngineLog) if offload.asked == Some(0) && offload.layers == Some(0) => {
            probed_gpu.is_none().then(|| "Running on the CPU: no GPU was detected on this machine.".to_string())
        }
        // The engine could not be asked and did not say: the number shown is the request.
        (_, OffloadEvidence::Unverified) if offload.asked.is_some_and(|n| n > 0) => Some(format!(
            "This llama-server is too old to say whether it used the GPU; the {} layers shown are the request, not a measurement.{install}",
            offload.asked.unwrap_or(0)
        )),
        _ => None,
    }
}

/// **The file and the engine, checked against each other.**
///
/// Returns why a load cannot work, or `None` when the pair is fine. A free function, and pure, so
/// both directions are tested without a store, an engine, or a machine that has either installed —
/// the pairing IS the decision, and it is the part that was getting answered by another program's
/// stderr.
fn engine_pairing_refusal(model_id: &str, file_name: Option<&str>, backend: &str) -> Option<String> {
    let is_artifact = file_name.is_some_and(palw::is_artifact_filename);
    // Two engines read a class artifact: the integer runtime directly, and the free-prompt gateway,
    // which runs that same runtime under the lane that prices the work. Spelled as a set because a
    // third one arriving must not have to be remembered in a comparison written as `==`.
    let reads_artifacts = [MisakaBackend::NAME, crate::backend::gateway::NAME].contains(&backend);
    match (is_artifact, reads_artifacts) {
        (true, false) => Some(format!(
            "{model_id} is a PALW class artifact, not a GGUF. Mining does not load it here — the node runs it, \
             and the Network tab is where it is named to the node. Chatting with it needs the `{misaka}` engine: \
             build `misaka-palw-serve`, set it under Settings → Backend, and choose `{misaka}` as the engine. \
             The {backend} backend cannot read this file.",
            misaka = MisakaBackend::NAME,
        )),
        (false, true) => Some(format!(
            "{model_id} is a GGUF, and the `{backend}` engine runs PALW class artifacts — the integer runtime a \
             class registers, not llama.cpp's format. Set the engine back to `auto` under Settings → Backend to \
             chat with this model."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both directions of the same mistake. On 2026-09-04 only the first existed as a check, and
    /// only after a `.palwart` reached `llama-server`, which read `PALW` where `GGUF` should be and
    /// aborted with fifteen lines of stderr that landed in a notification the window was too short
    /// to close. The second direction is the one a user reaches by FOLLOWING the first message:
    /// switch to the integer runtime, and now every GGUF in the list is the wrong file.
    #[test]
    fn each_engine_refuses_the_other_kind_of_file_and_says_where_to_go() {
        let artifact_on_llamacpp = engine_pairing_refusal("qwen25-1.5b-a16", Some("qwen25-1.5b-a16.palwart"), "llamacpp")
            .expect("llama.cpp cannot read a class artifact");
        assert!(artifact_on_llamacpp.contains("Network tab"), "{artifact_on_llamacpp}");
        assert!(artifact_on_llamacpp.contains("misaka"), "{artifact_on_llamacpp}");
        assert_eq!(artifact_on_llamacpp.lines().count(), 1, "a notification has to hold it");

        let gguf_on_misaka =
            engine_pairing_refusal("qwen2.5-1.5b-instruct-q4_k_m", Some("qwen2.5-1.5b-instruct-q4_k_m.gguf"), "misaka")
                .expect("the integer runtime cannot read a GGUF");
        assert!(gguf_on_misaka.contains("auto"), "the way back has to be named: {gguf_on_misaka}");

        // The two pairings that work are silent.
        assert!(engine_pairing_refusal("a", Some("class.palwart"), "misaka").is_none());
        assert!(engine_pairing_refusal("b", Some("model.gguf"), "llamacpp").is_none());
        // An MLX model is a directory: no file name, and no artifact, so no refusal from here.
        assert!(engine_pairing_refusal("c", None, "mlx").is_none());
    }
    use misaka_studio_core::hardware::{Accelerator, AcceleratorKind};
    use misaka_studio_core::model::ModelSource;

    fn model(size_gb: u64, layers: u64) -> LocalModel {
        LocalModel {
            id: "m".into(),
            name: "m".into(),
            path: PathBuf::from("/models/m.gguf"),
            size_bytes: size_gb << 30,
            quantization: None,
            architecture: Some("llama".into()),
            parameter_count: None,
            context_length: Some(32768),
            block_count: Some(layers),
            expert_count: None,
            kv_cache_bytes_per_token: Some(128 << 10),
            has_chat_template: true,
            source: ModelSource::default(),
            sha256: None,
            modified_at: None,
        }
    }

    fn machine(ram_gb: u64, vram_gb: Option<u64>) -> HardwareSnapshot {
        HardwareSnapshot {
            os: "test".into(),
            arch: "x86_64".into(),
            cpu_name: "cpu".into(),
            physical_cores: Some(8),
            logical_cores: 16,
            total_memory: ram_gb << 30,
            available_memory: ram_gb << 30,
            accelerators: vram_gb
                .map(|v| Accelerator {
                    kind: AcceleratorKind::Cuda,
                    name: "GPU".into(),
                    total_memory: Some(v << 30),
                    free_memory: Some(v << 30),
                    usable_memory: Some(v << 30),
                    driver: None,
                    index: 0,
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn a_model_that_fits_is_fully_offloaded() {
        let layers = plan_gpu_layers(&model(8, 32), &machine(64, Some(24)), None, 4096, GpuLayers::Auto);
        assert_eq!(layers, Some(33), "every layer plus the output tensor");
    }

    fn engine_gpu(free_mib: u64) -> Vec<EngineDevice> {
        vec![EngineDevice {
            id: "Vulkan0".into(),
            description: "Test GPU".into(),
            backend: "vulkan".into(),
            kind: crate::backend::devices::EngineDeviceKind::Gpu,
            total_mib: Some(free_mib + 512),
            free_mib: Some(free_mib),
        }]
    }

    /// The tester's machine: a card the probe cannot see (no `nvidia-smi`, no `rocm-smi`), and a
    /// Vulkan engine that can. The engine's word is enough to offload.
    #[test]
    fn an_engine_that_sees_a_gpu_the_probe_missed_offloads_to_it() {
        let devices = engine_gpu(24 * 1024);
        let layers = plan_gpu_layers(&model(8, 32), &machine(32, None), Some(&devices), 4096, GpuLayers::Auto);
        assert_eq!(layers, Some(33));
    }

    /// The other direction, and the one the field reported: a card the probe DOES see, and an
    /// engine built without any GPU backend. Offloading into it would be a request the engine
    /// silently ignores — and a `28/28 on GPU` in the UI.
    #[test]
    fn a_cpu_only_engine_gets_no_layers_however_good_the_card() {
        let layers = plan_gpu_layers(&model(8, 32), &machine(64, Some(24)), Some(&[]), 4096, GpuLayers::Auto);
        assert_eq!(layers, Some(0));
        // An explicit setting is still passed through: the engine ignores it, and the note says so.
        assert_eq!(plan_gpu_layers(&model(8, 32), &machine(64, Some(24)), Some(&[]), 4096, GpuLayers::All), Some(33));
    }

    /// The engine's free-memory figure is the budget when it gives one: it is measured on the
    /// device the layers will actually go to.
    #[test]
    fn the_engines_own_memory_figure_sets_the_budget() {
        let small = engine_gpu(4 * 1024);
        let layers = plan_gpu_layers(&model(40, 60), &machine(128, Some(24)), Some(&small), 4096, GpuLayers::Auto).expect("a plan");
        assert!(layers < 10, "a 4 GiB card cannot take much of a 40 GiB model: {layers}");
    }

    fn offload(asked: Option<u32>, layers: Option<u32>, evidence: OffloadEvidence) -> Offload {
        Offload { asked, layers, total_layers: None, device: None, accelerator_bytes: None, cpu_bytes: None, evidence }
    }

    /// The sentences a person gets, and — as important — the silences.
    #[test]
    fn the_note_names_the_cause_and_stays_quiet_when_nothing_is_wrong() {
        let with_card = machine(64, Some(24));
        let no_card = machine(32, None);

        // A CPU-only build on a machine with a card: the exact field report.
        let note =
            offload_note(GpuLayers::Auto, Some(&offload(Some(33), Some(0), OffloadEvidence::DeviceList)), &with_card).expect("a note");
        assert!(note.contains("CPU-only build"), "{note}");
        assert!(note.contains("GPU is idle"), "the card is named as idle: {note}");
        assert!(note.contains("Settings"), "and the remedy is where to click: {note}");

        // Auto planned zero because the engine lists no device.
        let note =
            offload_note(GpuLayers::Auto, Some(&offload(Some(0), Some(0), OffloadEvidence::DeviceList)), &no_card).expect("a note");
        assert!(note.contains("lists no GPU device"), "{note}");

        // The log shows the weights in system memory after a request.
        let note =
            offload_note(GpuLayers::All, Some(&offload(Some(29), Some(0), OffloadEvidence::EngineLog)), &no_card).expect("a note");
        assert!(note.contains("system memory"), "{note}");

        // Too old to say.
        let note =
            offload_note(GpuLayers::Auto, Some(&offload(Some(9), Some(9), OffloadEvidence::Unverified)), &no_card).expect("a note");
        assert!(note.contains("too old"), "{note}");

        // Silences: the user chose the CPU; the offload happened; nothing is loaded.
        assert_eq!(offload_note(GpuLayers::None, Some(&offload(Some(0), Some(0), OffloadEvidence::DeviceList)), &with_card), None);
        assert_eq!(offload_note(GpuLayers::Auto, Some(&offload(Some(29), Some(29), OffloadEvidence::EngineLog)), &with_card), None);
        assert_eq!(offload_note(GpuLayers::Auto, None, &with_card), None);
    }

    /// The case the whole function exists for: too big for the card, so some layers stay on the
    /// CPU. Offloading them all would be an out-of-memory error at load.
    #[test]
    fn a_model_that_does_not_fit_is_split() {
        let layers = plan_gpu_layers(&model(40, 60), &machine(128, Some(24)), None, 4096, GpuLayers::Auto).expect("a plan");
        assert!(layers > 0 && layers < 60, "expected a partial offload, got {layers}");
    }

    #[test]
    fn without_a_gpu_nothing_is_offloaded() {
        assert_eq!(plan_gpu_layers(&model(8, 32), &machine(32, None), None, 4096, GpuLayers::Auto), Some(0));
    }

    #[test]
    fn explicit_settings_win_over_the_estimate() {
        let m = model(40, 60);
        let h = machine(128, Some(24));
        assert_eq!(plan_gpu_layers(&m, &h, None, 4096, GpuLayers::All), Some(61));
        assert_eq!(plan_gpu_layers(&m, &h, None, 4096, GpuLayers::None), Some(0));
        assert_eq!(plan_gpu_layers(&m, &h, None, 4096, GpuLayers::Fixed { layers: 7 }), Some(7));
    }

    /// A long context eats the offload budget: the same model and card must offload fewer layers
    /// at 128 k than at 4 k.
    #[test]
    fn context_length_takes_layers_off_the_gpu() {
        let m = model(20, 48);
        let h = machine(128, Some(24));
        let short = plan_gpu_layers(&m, &h, None, 4096, GpuLayers::Auto).expect("a plan");
        let long = plan_gpu_layers(&m, &h, None, 131_072, GpuLayers::Auto).expect("a plan");
        assert!(long < short, "short={short} long={long}");
    }
}
