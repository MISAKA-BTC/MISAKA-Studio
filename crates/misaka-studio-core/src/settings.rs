//! Settings, and where on each platform they live.
//!
//! Two rules shape this file.
//!
//! **An unknown key is kept, not dropped.** Settings are edited by a newer build and read by an
//! older one (a user rolls back; a sidecar lags the UI). `serde(default)` on every field plus a
//! save path that rewrites only what it understands means a downgrade loses nothing.
//!
//! **A partial write is never a settings file.** The save is write-to-temp-then-rename, because
//! the failure mode of the obvious implementation is a truncated JSON file that makes the app
//! refuse to start — the one bug where the fix ("delete this file") is invisible to the person
//! hitting it.

use crate::provenance::SamplingCommitment;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Which engine runs the model.
///
/// The Studio's whole architecture is `Studio UI → MISAKA Runtime API → backend → GPU/CPU`, and
/// this enum is the seam: a value here selects an implementation of the runtime's backend trait
/// and nothing above it changes. `Misaka` is reserved for the deterministic in-house runtime the
/// PALW work already has in this repository — named now so the setting does not have to be
/// invented later, and refused at load time until it exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Pick by platform: MLX on Apple Silicon when present, llama.cpp everywhere else.
    #[default]
    Auto,
    /// llama.cpp's `llama-server`, driven as a child process.
    LlamaCpp,
    /// Apple's MLX, via `mlx_lm.server`. macOS/Apple Silicon only.
    Mlx,
    /// The deterministic integer runtime (`misaka-palw-serve`), driven as a child process. Its
    /// arithmetic is the class a chain registers, so two machines running one artifact agree.
    Misaka,
    /// The free-prompt gateway (`misaka-palw-gateway`), over HTTP — not a child process.
    ///
    /// The same integer runtime, run under the lane that PRICES the work: one execution answers the
    /// user and produces the commitment a panel seat can re-execute. Chatting through it is how a
    /// chat becomes adjudicable — and, once a submitter beside that gateway signs and sends the
    /// commitment, mined. `node.palw_gateway_url` says which gateway.
    Gateway,
    /// A built-in fake that streams a canned reply. For UI work and tests with no model.
    Mock,
}

/// How many transformer layers to put on the GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum GpuLayers {
    /// Offload as much as the estimate says will fit — the right answer for most people, and
    /// the only one that adapts when they load a bigger model.
    #[default]
    Auto,
    /// Everything. Fails loudly if it does not fit, which is sometimes what you want.
    All,
    None,
    Fixed {
        layers: u32,
    },
}

/// Whether to ask the engine for flash attention.
///
/// Three states rather than a bool, because the honest default is "let the engine decide". Current
/// llama.cpp defaults to `auto` — it enables flash attention where the backend supports it and
/// falls back where it does not, which is a better decision than this app can make from outside.
///
/// It is also the only option that works on every engine. `-fa` as a bare flag was accepted for
/// years and is now an error (`expected value for argument`); `--flash-attn on` is accepted now
/// and was not then. Passing nothing is compatible with both, so `Auto` passes nothing — measured
/// against a real build, after the bare flag turned every load into a usage message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlashAttention {
    /// Say nothing and let the engine choose.
    #[default]
    Auto,
    On,
    Off,
}

impl<'de> Deserialize<'de> for FlashAttention {
    /// Accepts `"auto"`, `"on"`, `"off"` — and the `true`/`false` this field used to be.
    ///
    /// A settings file written by an earlier build must still load. The alternative is an app that
    /// refuses to start after an update, with a parse error naming a field the user never set.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Text(String),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Bool(true) => FlashAttention::On,
            Raw::Bool(false) => FlashAttention::Off,
            Raw::Text(text) => match text.to_ascii_lowercase().as_str() {
                "on" | "true" | "enabled" => FlashAttention::On,
                "off" | "false" | "disabled" => FlashAttention::Off,
                _ => FlashAttention::Auto,
            },
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendSettings {
    pub kind: BackendKind,
    /// Path to `llama-server`. `None` means "look on PATH and in the app's own bundle".
    pub llama_server_path: Option<PathBuf>,
    /// Path to the MLX server entry point (macOS).
    pub mlx_server_path: Option<PathBuf>,
    /// Path to `misaka-palw-serve`, the integer runtime's OpenAI server. `None` looks beside the
    /// Studio's own executable and on PATH, the same way the other engines are found.
    pub misaka_serve_path: Option<PathBuf>,
    /// The tokenizer the MISAKA runtime renders prompts with. `None` looks for `tokenizer.json`
    /// beside the artifact — a `.palwart` carries weights and a tokenizer COMMITMENT, never the
    /// tokenizer file itself (the class identity includes what the ids mean; consensus never runs
    /// one), so the file has to come from somewhere and this is where the Studio is told.
    pub misaka_tokenizer_path: Option<PathBuf>,
    pub gpu_layers: GpuLayers,
    /// Generation threads. `None` lets the engine choose, which it does better than a fixed
    /// default copied from someone else's machine.
    pub threads: Option<u32>,
    /// Flash attention. A large memory win on long contexts, and not every build has it.
    pub flash_attention: FlashAttention,
    pub use_mmap: bool,
    /// Lock the model in RAM. Prevents the OS swapping weights out mid-generation, at the cost
    /// of being unable to load anything that does not fit.
    pub use_mlock: bool,
    /// Extra arguments appended verbatim to the engine's command line.
    pub extra_args: Vec<String>,
    /// Seconds to wait for an engine to become healthy after launch. A 70B model off a spinning
    /// disk genuinely takes minutes.
    pub startup_timeout_secs: u64,
}

impl Default for BackendSettings {
    fn default() -> Self {
        BackendSettings {
            kind: BackendKind::Auto,
            llama_server_path: None,
            mlx_server_path: None,
            misaka_serve_path: None,
            misaka_tokenizer_path: None,
            gpu_layers: GpuLayers::Auto,
            threads: None,
            flash_attention: FlashAttention::default(),
            use_mmap: true,
            use_mlock: false,
            extra_args: Vec::new(),
            startup_timeout_secs: 600,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    /// Bind address. **127.0.0.1 by default, deliberately.**
    ///
    /// This process answers `/v1/chat/completions` with no authentication out of the box. On
    /// `0.0.0.0` that is an open inference endpoint for everyone on the café wifi, so exposing
    /// it is a decision someone has to make on purpose — and [`Self::requires_api_key`] refuses
    /// the combination of a public bind and no key.
    pub host: String,
    pub port: u16,
    /// Optional bearer token for the OpenAI-compatible surface.
    pub api_key: Option<String>,
    /// Extra CORS origins. The local UI is same-origin and needs none of these.
    pub cors_origins: Vec<String>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        ServerSettings { host: "127.0.0.1".into(), port: 1338, api_key: None, cors_origins: Vec::new() }
    }
}

impl ServerSettings {
    /// True when this configuration would expose an unauthenticated endpoint beyond the machine.
    pub fn requires_api_key(&self) -> bool {
        let local = self.host == "127.0.0.1" || self.host == "localhost" || self.host == "::1";
        !local && self.api_key.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HuggingFaceSettings {
    /// API endpoint. Configurable for mirrors and for corporate proxies — `HF_ENDPOINT` is the
    /// variable the rest of the ecosystem already uses for this.
    pub endpoint: String,
    /// Access token, for gated repositories and higher rate limits.
    pub token: Option<String>,
    /// Parallel download connections.
    pub max_concurrent_downloads: usize,
}

impl Default for HuggingFaceSettings {
    fn default() -> Self {
        HuggingFaceSettings {
            endpoint: std::env::var("HF_ENDPOINT").unwrap_or_else(|_| "https://huggingface.co".into()),
            token: None,
            max_concurrent_downloads: 2,
        }
    }
}

/// Which MISAKA network a supervised or attached node is on.
///
/// `Testnet11` is the live public network; `Devnet` and `Simnet` are the local, permissionless
/// presets — the ones a sandboxed machine (or anyone who just wants to see PALW produce blocks)
/// can run without reaching the internet at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeNetwork {
    #[default]
    Testnet11,
    Devnet,
    Simnet,
}

impl NodeNetwork {
    /// The network id the CLI and the node spell it by — the string `--network` takes.
    ///
    /// Spelled once, here, because the Studio hands it to another process: a network id that
    /// disagrees with the node's is how a transaction gets signed for the wrong chain, and the
    /// CLI's own `getServerInfo` check can only catch it if the two names came from one place.
    pub fn id(self) -> &'static str {
        match self {
            NodeNetwork::Testnet11 => "testnet-11",
            NodeNetwork::Devnet => "devnet",
            NodeNetwork::Simnet => "simnet",
        }
    }
}

/// How this machine participates in the MISAKA network.
///
/// The ladder is honest about what each rung requires. Observing needs a reachable node's RPC.
/// Verifying needs a full node — syncing and re-deriving claims is what a node does by being a
/// node. Producing additionally needs a bonded seat on-chain and, for a model class, that class's
/// artifact on disk; the Studio can launch the node and check the prerequisites, but a bond is an
/// on-chain fact it cannot conjure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkRole {
    #[default]
    Observer,
    Verifier,
    Producer,
}

/// The MISAKA node this Studio watches or supervises.
///
/// `Default` is written out rather than derived because one field has to default to *on*, and a
/// derived `Default` would silently make it `false` — which is the difference between a fresh
/// install that can mine and one that cannot, decided by a missing line.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeSettings {
    /// Path to the `kaspad` binary (the misakas node keeps upstream's binary name). `None` means
    /// look beside the Studio's executable and on PATH, same as the engine resolution.
    pub kaspad_path: Option<PathBuf>,
    /// The node wRPC **Borsh** endpoint (`host:port`) the `misaka` CLI should talk to.
    ///
    /// Separate from `rpc_url` because they are different protocols on different ports: the
    /// Studio watches the node over wRPC-JSON, and the CLI speaks Borsh. `None` lets the CLI
    /// resolve it the way it always does — `~/.misaka/<network>/endpoints.json`, which a node
    /// writes when it starts, then the network default. Set it when the node is somebody
    /// else's, which is the case for a Studio that only joined a pool.
    pub misaka_rpc: Option<String>,
    /// Path to the `misaka` CLI. `None` looks beside the Studio's executable and on PATH.
    ///
    /// The Studio shells out to it for the moves that spend money — seeding a line's market is
    /// the first — rather than carrying a second ML-DSA-87 signer. One signer means one thing
    /// that can be wrong about an irreversible transaction, and it is the one the chain's own
    /// tests cover.
    pub misaka_cli_path: Option<PathBuf>,
    /// Attach to an already-running node's wRPC endpoint instead of launching one. Takes
    /// precedence over launching: two nodes sharing one appdir is a corrupted database.
    pub rpc_url: Option<String>,
    pub network: NodeNetwork,
    pub role: NetworkRole,
    /// Coinbase payout address for producing. `None` = the producer key's own address, which the
    /// node derives and prints at start (`[palw] producer pay address …`); set it only to send
    /// rewards elsewhere. An address, not a key — the Studio never holds wallet key material.
    pub mining_address: Option<String>,
    /// Path to the producer's 32-byte ML-DSA-87 seed file — `POST /api/v1/network/producer-key`
    /// writes one (0600) under the data directory, or bring your own (`misaka key gen`). A path
    /// the node reads — the Studio passes it on the command line and never opens the file after
    /// writing it.
    pub producer_key_path: Option<PathBuf>,
    /// The bond outpoint (`<txid>:<index>`) printed once by the registration run. Absent means
    /// the next producer start registers a bond instead of mining with one.
    pub producer_bond: Option<String>,
    /// The fee outpoint that funds the panel submitter (usually the bond carrier's change,
    /// `<txid>:1`). Absent runs the panel receipts-only, which the node states at startup.
    pub fee_outpoint: Option<String>,
    /// Class id to produce in (128 hex). Absent mines the floor — BASE-0, no artifact.
    pub producer_class: Option<String>,
    /// The class artifact file, for a model class.
    pub class_artifact: Option<PathBuf>,
    /// **Armed once**: the next producer start files ONE `ClassRegistered` for `class_artifact`
    /// and this clears itself.
    ///
    /// The value is the model id (`Qwen/Qwen2.5-Coder-1.5B-Instruct`) and may be empty, which
    /// lets the node infer it — needed only when a converted shape matches more than one class
    /// this build knows, because sibling models convert to the same shape and the file alone
    /// cannot say which one it is.
    ///
    /// Arm-once rather than a standing setting, because the flag submits a transaction: left on,
    /// every restart would file another registration for a class the chain already holds.
    pub register_class: Option<String>,
    /// The node's data directory. `None` uses the node's own default.
    pub appdir: Option<PathBuf>,
    /// Extra arguments appended verbatim to the node's command line.
    pub extra_args: Vec<String>,
    /// Fetch the default class's artifact (`palw::DEFAULT_CLASS`) on first run, so a machine that
    /// just downloaded the Studio can mine a model class without hunting for a file.
    ///
    /// On by default. It is a 1.7 GiB verified download that appears in the download list like
    /// any other and can be cancelled there; turn it off for a metered connection, or for a
    /// machine that is only ever going to chat.
    pub install_default_class_artifact: bool,
    /// Base URL of a miner pool (`…/pool`), for mining without running a node here. The pool
    /// hosts the producer; joining it needs nothing but funding the slot it hands back.
    pub pool_url: Option<String>,
    /// The slot this Studio joined, if any — the pool's identifier for "your producer".
    pub pool_slot_id: Option<String>,
    /// The bearer that lets this Studio read its slot's status. Not a wallet key.
    pub pool_slot_token: Option<String>,
    /// Base URL of a free-prompt gateway (`misaka-palw-gateway`), the endpoint that turns one
    /// prompt into one inference carrying its own commitment (ADR-0044). `None` uses the local
    /// default; a pool-hosted gateway is the same field with someone else's host in it, which is
    /// what "mine with a prompt and no node here" means.
    pub palw_gateway_url: Option<String>,
    /// Where a chat's mining happens relative to the chat itself — see [`MiningMode`].
    pub mining_mode: MiningMode,
    /// What the app does with a sampling knob the free-prompt lane cannot honour — see
    /// [`SamplingPolicy`]. Only consulted when the engine answering is the gateway.
    pub sampling_policy: SamplingPolicy,
    /// ADR-0096 Decision 5: when trimming a conversation to the class's context would drop MORE
    /// than this many turns, the dropped turns are first sent to the lane as their own job — a
    /// short summary — and the summary rides the answer's prompt as a system-level turn. Below
    /// it the turns are simply dropped and the count is reported. The summary is a real inference
    /// and a real claim (one job per leg, ADR-0077 R0), which is why the threshold is not zero.
    pub summarize_after_turns: u32,
    /// ADR-0096 Decision 5: how many follow-up jobs may continue an answer the class's row cut
    /// short (`finish_reason: "length"` while the request asked for more). Each leg is its own
    /// claim; the client sees one stream and `misaka.jobs[]` lists the seams.
    pub continue_max_legs: u32,
}

/// **What the app does with `temperature: 0.8` when the lane will replay a greedy decode.**
///
/// ADR-0096 Decision 4. The gateway refuses a non-greedy temperature or a seed outright while
/// `palw_fp_decode_rules` is dormant (ADR-0082 Decision 11): a seat re-executes the job, and a
/// sampler the seat does not know about is a claim nobody can reproduce. The Studio is the
/// person's own app and never sent those knobs to the lane at all — it now has to SAY so, or the
/// person is told a false thing about what ran. Either way nothing is downgraded silently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingPolicy {
    /// The request goes through with the knobs dropped, and the answer carries
    /// `misaka.sampling = { requested, applied, reason }` so what ran is printed beside what
    /// was asked.
    #[default]
    GreedyWithNotice,
    /// Refuse the request by name before anything is sent, the way the gateway would.
    Refuse,
}

/// **Whether the Chat tab waits for the lane, or the lane runs behind it.**
///
/// The free-prompt lane executes for minutes per answer and refuses for reasons about the slot
/// (unfunded, a full bond, a node mid-restart). `Inline` puts all of that in the chat: the answer
/// IS the mined answer, and every lane condition is a chat error. `Background` answers the chat
/// from an engine that can answer now and queues the same prompt for the slot's gateway, where a
/// worker mines it on its own cadence and the chat is told, under the message, what came of it —
/// the shape hash mining always had.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiningMode {
    /// The chat's answer is the mined answer; the chat waits for the lane.
    #[default]
    Inline,
    /// The chat answers locally; prompts are mined from a queue behind it.
    Background,
}

impl Default for NodeSettings {
    fn default() -> Self {
        NodeSettings {
            kaspad_path: None,
            misaka_cli_path: None,
            misaka_rpc: None,
            rpc_url: None,
            network: NodeNetwork::default(),
            role: NetworkRole::default(),
            mining_address: None,
            producer_key_path: None,
            producer_bond: None,
            fee_outpoint: None,
            producer_class: None,
            class_artifact: None,
            register_class: None,
            appdir: None,
            extra_args: Vec::new(),
            // Off since testnet-11 Relaunch 5f (2026-09-03): the dense class's material (~750 MB per
            // job) is above the gossip cap, so its blocks do not cross the public links and a node that
            // holds the artifact only pays bandwidth for it. Rewards today come from the floor; the
            // artifact is a click away in Models → Discover when the transport fix lands.
            install_default_class_artifact: false,
            pool_url: None,
            pool_slot_id: None,
            pool_slot_token: None,
            palw_gateway_url: None,
            mining_mode: MiningMode::default(),
            sampling_policy: SamplingPolicy::default(),
            summarize_after_turns: 4,
            continue_max_legs: 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    pub theme: Theme,
    /// Show the provenance panel (model hash, runtime identity, inference hash) in chat.
    pub show_provenance: bool,
    /// Show tokens/sec and the memory gauges while generating.
    pub show_performance: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings { theme: Theme::System, show_provenance: true, show_performance: true }
    }
}

/// What the Studio records about its own inferences.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProvenanceSettings {
    /// Write an [`crate::InferenceRecord`] per completion.
    pub record_inferences: bool,
    /// Also keep the prompt and completion **text** alongside the record.
    ///
    /// Off by default. The record commits to the bytes with a hash, which is what verification
    /// needs; keeping the plaintext as well turns a provenance log into a transcript of
    /// everything the user ever typed, sitting in a second place they do not know about. Anyone
    /// who wants replayable evidence can turn it on knowing that.
    pub keep_transcripts: bool,
    /// Cap on records kept on disk; the oldest are dropped past it.
    pub max_records: usize,
}

impl Default for ProvenanceSettings {
    fn default() -> Self {
        ProvenanceSettings { record_inferences: true, keep_transcripts: false, max_records: 10_000 }
    }
}

/// The default generation settings a new conversation starts from.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GenerationDefaults {
    pub system_prompt: String,
    pub context_size: Option<u32>,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: i64,
    pub min_p: f64,
    pub repeat_penalty: f64,
    pub max_tokens: u64,
    pub seed: Option<u64>,
}

impl Default for GenerationDefaults {
    fn default() -> Self {
        GenerationDefaults {
            system_prompt: String::new(),
            context_size: None,
            temperature: 0.7,
            top_p: 0.95,
            top_k: 40,
            min_p: 0.05,
            repeat_penalty: 1.1,
            max_tokens: 2048,
            seed: None,
        }
    }
}

impl GenerationDefaults {
    pub fn sampling(&self) -> SamplingCommitment {
        SamplingCommitment {
            temperature: self.temperature,
            top_p: self.top_p,
            top_k: self.top_k,
            min_p: self.min_p,
            repeat_penalty: self.repeat_penalty,
            max_tokens: self.max_tokens,
            seed: self.seed,
        }
    }
}

/// Where the components manifest is, and whether the Studio consults it without being asked.
///
/// ADR-0096 Decision 10: one `components.json` (schema `misaka/components/v1`) names every
/// binary and artifact the Studio can spawn or map, with the bytes' digest. `manifest` is a local
/// path or an `https://` URL; `None` means the Studio only knows what is on disk and says so
/// (`state: "not-in-manifest"` for everything found). `auto_check` is whether
/// `GET /api/v1/components` fetches the manifest on its own — off, it is fetched only when the
/// request asks (`?check=1`), for a metered or offline machine; `misaka-studiod --check` reads
/// it whenever it is set, because a person running `--check` is asking.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ComponentsSettings {
    pub manifest: Option<String>,
    pub auto_check: bool,
}

impl Default for ComponentsSettings {
    fn default() -> Self {
        ComponentsSettings { manifest: None, auto_check: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Where GGUF files live. Movable, because models are the largest thing on most people's
    /// disks and the system drive is rarely where they want them.
    pub models_dir: PathBuf,
    /// A model to load as soon as the runtime is up, so the app opens ready to answer rather than
    /// ready to be told which engine to start.
    ///
    /// It names a MODEL, not an engine: which engine runs is `backend.kind`, and the engine is a
    /// child process that exists because a model is loaded into it — there is no engine to start on
    /// its own. Naming the integer runtime's artifact here is what "start the MISAKA engine at
    /// startup" means in this design, and naming a GGUF is the same sentence for llama.cpp.
    ///
    /// A load that fails is a log line, never a refusal to start: an engine that will not come up
    /// must not be able to keep the Studio from opening, because the Settings page that fixes it is
    /// inside the Studio.
    pub load_on_start: Option<String>,
    pub server: ServerSettings,
    pub backend: BackendSettings,
    pub node: NodeSettings,
    pub generation: GenerationDefaults,
    pub huggingface: HuggingFaceSettings,
    pub ui: UiSettings,
    pub provenance: ProvenanceSettings,
    pub components: ComponentsSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            models_dir: default_models_dir(),
            load_on_start: None,
            server: ServerSettings::default(),
            backend: BackendSettings::default(),
            node: NodeSettings::default(),
            generation: GenerationDefaults::default(),
            huggingface: HuggingFaceSettings::default(),
            ui: UiSettings::default(),
            provenance: ProvenanceSettings::default(),
            components: ComponentsSettings::default(),
        }
    }
}

impl Settings {
    /// Load from `path`, or return the defaults when the file does not exist yet.
    ///
    /// A *corrupt* file is a different thing from a missing one and is reported as an error:
    /// silently starting with defaults after a bad parse throws away settings the user still
    /// has, and they find out by noticing their model directory moved.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| Error::Settings { path: path.display().to_string(), reason: format!("not valid settings JSON: {e}") }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(Error::io(path.display(), e)),
        }
    }

    /// Write atomically: temp file in the same directory, then rename over the target.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Settings { path: path.display().to_string(), reason: e.to_string() })?;
        // Same directory, so the rename stays within one filesystem and is therefore atomic.
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, json).map_err(|e| Error::io(temp.display(), e))?;
        std::fs::rename(&temp, path).map_err(|e| Error::io(path.display(), e))?;
        Ok(())
    }
}

/// Per-platform data directory for the app.
pub fn default_data_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("MISAKA Studio")
    } else if cfg!(target_os = "macos") {
        home().join("Library/Application Support/MISAKA Studio")
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("misaka-studio")
    }
}

/// Where models go by default: `<data dir>/models`.
pub fn default_models_dir() -> PathBuf {
    default_data_dir().join("models")
}

/// The settings file itself.
pub fn default_settings_path() -> PathBuf {
    default_data_dir().join("settings.json")
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_a_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.server.port = 4242;
        s.backend.kind = BackendKind::LlamaCpp;
        s.save(&path).expect("saves");
        let back = Settings::load(&path).expect("loads");
        assert_eq!(back.server.port, 4242);
        assert_eq!(back.backend.kind, BackendKind::LlamaCpp);
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_a_broken_one_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope.json");
        assert_eq!(Settings::load(&missing).expect("defaults").server.port, 1338);

        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "{ this is not json").expect("write");
        assert!(matches!(Settings::load(&broken), Err(Error::Settings { .. })));
    }

    /// A file written by a newer build must not lose the fields this build does not know, and
    /// must not fail to load either.
    #[test]
    fn unknown_and_missing_fields_both_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("partial.json");
        std::fs::write(&path, r#"{"server":{"port":9000},"someFutureKey":{"a":1}}"#).expect("write");
        let s = Settings::load(&path).expect("loads");
        assert_eq!(s.server.port, 9000);
        assert_eq!(s.server.host, "127.0.0.1", "absent fields fall back to defaults");
        assert_eq!(s.generation.temperature, 0.7);
    }

    /// The check that stops a convenience setting from becoming an open inference endpoint.
    #[test]
    fn a_public_bind_without_a_key_is_flagged() {
        let mut s = ServerSettings::default();
        assert!(!s.requires_api_key(), "the loopback default needs no key");
        s.host = "0.0.0.0".into();
        assert!(s.requires_api_key());
        s.api_key = Some("secret".into());
        assert!(!s.requires_api_key());
    }

    /// A settings file written before ADR-0096 has none of the lane's three fields, and it must
    /// load with the ADR's defaults — greedy-with-notice, summarize past four dropped turns, two
    /// continue legs — rather than refuse to start over keys the person never set.
    #[test]
    fn an_older_settings_file_without_the_lane_fields_loads_with_their_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("old.json");
        std::fs::write(&path, r#"{"node":{"network":"devnet","pool_slot_id":"slot-06"}}"#).expect("write");
        let s = Settings::load(&path).expect("loads");
        assert_eq!(s.node.network, NodeNetwork::Devnet, "the keys that were there still load");
        assert_eq!(s.node.sampling_policy, SamplingPolicy::GreedyWithNotice);
        assert_eq!(s.node.summarize_after_turns, 4);
        assert_eq!(s.node.continue_max_legs, 2);
    }

    /// A settings file written before the components manifest existed has no `components` key
    /// and must load with the manifest unset and the automatic check on — the default that makes a
    /// later `components.manifest` take effect without a second edit.
    #[test]
    fn an_older_settings_file_without_components_loads_with_the_check_on_and_no_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("old.json");
        std::fs::write(&path, r#"{"server":{"port":9001}}"#).expect("write");
        let s = Settings::load(&path).expect("loads");
        assert_eq!(s.components.manifest, None);
        assert!(s.components.auto_check);
        let s: Settings = serde_json::from_str(r#"{"components":{"manifest":"contrib/components/testnet-11.json"}}"#).expect("parses");
        assert_eq!(s.components.manifest.as_deref(), Some("contrib/components/testnet-11.json"));
        assert!(s.components.auto_check, "an unset auto_check is on");
    }

    /// The policy is spelled on disk the way the ADR spells it, so a person following the text
    /// can set it by hand.
    #[test]
    fn the_sampling_policy_is_spelled_as_the_adr_spells_it() {
        assert_eq!(serde_json::to_string(&SamplingPolicy::GreedyWithNotice).expect("json"), "\"greedy_with_notice\"");
        assert_eq!(serde_json::to_string(&SamplingPolicy::Refuse).expect("json"), "\"refuse\"");
        let s: NodeSettings = serde_json::from_str(r#"{"sampling_policy":"refuse"}"#).expect("parses");
        assert_eq!(s.sampling_policy, SamplingPolicy::Refuse);
    }

    #[test]
    fn saving_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        Settings::default().save(&path).expect("saves");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left {leftovers:?}");
    }
}
