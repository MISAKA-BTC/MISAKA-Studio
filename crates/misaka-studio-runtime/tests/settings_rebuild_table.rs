//! **Every settings field, classified: does changing it replace the engine?** — ADR-0096
//! invariant 9, the converse of `gateway_follows_the_slot.rs`.
//!
//! The rebuild predicate no longer enumerates fields (it compares fingerprints), so this table
//! is where the fields are enumerated — on purpose, as documentation, with a completeness guard:
//! every leaf of `Settings` (walked from the struct's own JSON, not written down) must appear in
//! the table, so a new settings field fails this test until someone says whether an engine
//! copies it. Each row says which running engines a change must REPLACE; under every other
//! engine the instance must stay, because rebuilding on a field the engine never read is the
//! other bug — the model unloaded on a theme change.
//!
//! Every row is applied to a live `AppState` through `apply_settings`, once per engine kind, and
//! `Arc::ptr_eq` before and after is the whole observation.

use misaka_studio_core::settings::{
    BackendKind, BackendSettings, FlashAttention, GpuLayers, MiningMode, NetworkRole, NodeNetwork, SamplingPolicy, Settings, Theme,
};
use misaka_studio_runtime::AppState;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

const ALL_KINDS: [BackendKind; 5] =
    [BackendKind::LlamaCpp, BackendKind::Mlx, BackendKind::Misaka, BackendKind::Gateway, BackendKind::Mock];
const CHILD_ENGINES: [BackendKind; 3] = [BackendKind::LlamaCpp, BackendKind::Mlx, BackendKind::Misaka];
const NONE: [BackendKind; 0] = [];

/// One settings field: its JSON path, a mutation that changes it to a value it does not hold
/// (`n` makes each application distinct), and the engines whose instance the change replaces.
struct Row {
    path: &'static str,
    rebuilds: &'static [BackendKind],
    mutate: fn(&mut Settings, u64, &std::path::Path),
}

macro_rules! row {
    ($path:literal, $rebuilds:expr, |$s:ident, $n:ident, $tmp:ident| $body:expr) => {
        Row { path: $path, rebuilds: &$rebuilds, mutate: |$s: &mut Settings, $n: u64, $tmp: &std::path::Path| $body }
    };
}

/// The table. A path that is not here fails `every_settings_leaf_is_classified`.
fn table() -> Vec<Row> {
    vec![
        // --- fields a constructor copies: the engine that copied them is replaced ---------
        row!("backend.llama_server_path", [BackendKind::LlamaCpp], |s, n, _t| s.backend.llama_server_path =
            Some(PathBuf::from(format!("/engines/llama-server-{n}")))),
        row!("backend.mlx_server_path", [BackendKind::Mlx], |s, n, _t| s.backend.mlx_server_path =
            Some(PathBuf::from(format!("/engines/mlx_lm.server-{n}")))),
        row!("backend.misaka_serve_path", [BackendKind::Misaka], |s, n, _t| s.backend.misaka_serve_path =
            Some(PathBuf::from(format!("/engines/misaka-palw-serve-{n}")))),
        row!("backend.misaka_gateway_path", [BackendKind::Misaka], |s, n, _t| s.backend.misaka_gateway_path =
            Some(PathBuf::from(format!("/engines/misaka-palw-gateway-{n}")))),
        row!("backend.misaka_tokenizer_path", [BackendKind::Misaka], |s, n, _t| s.backend.misaka_tokenizer_path =
            Some(PathBuf::from(format!("/models/tokenizer-{n}.json")))),
        row!("backend.startup_timeout_secs", CHILD_ENGINES, |s, n, _t| s.backend.startup_timeout_secs = 100 + n),
        row!("node.palw_gateway_url", [BackendKind::Gateway], |s, n, _t| s.node.palw_gateway_url =
            Some(format!("https://pool.example/pool/v1/slots/slot-{n}/fp"))),
        row!("node.pool_slot_token", [BackendKind::Gateway], |s, n, _t| s.node.pool_slot_token = Some(format!("token-{n}"))),
        // --- routing inputs: which engine answers is decided again on the next load ------
        row!("node.mining_mode", ALL_KINDS, |s, _n, _t| s.node.mining_mode =
            flip(s.node.mining_mode, MiningMode::Inline, MiningMode::Background)),
        // --- everything else: read at load, by another subsystem, or by nobody ----------
        row!("models_dir", NONE, |s, n, t| s.models_dir = mkdir(t, &format!("models-{n}"))),
        row!("load_on_start", NONE, |s, n, _t| s.load_on_start = Some(format!("model-{n}"))),
        row!("server.host", NONE, |s, _n, _t| s.server.host = flip(s.server.host.clone(), "127.0.0.1".into(), "localhost".into())),
        row!("server.port", NONE, |s, n, _t| s.server.port = 2000 + n as u16),
        row!("server.api_key", NONE, |s, n, _t| s.server.api_key = Some(format!("key-{n}"))),
        row!("server.cors_origins", NONE, |s, n, _t| s.server.cors_origins = vec![format!("https://origin-{n}.example")]),
        row!("backend.gpu_layers.mode", NONE, |s, _n, _t| s.backend.gpu_layers =
            flip(s.backend.gpu_layers, GpuLayers::Auto, GpuLayers::None)),
        row!("backend.threads", NONE, |s, n, _t| s.backend.threads = Some(n as u32 + 1)),
        row!("backend.flash_attention", NONE, |s, _n, _t| s.backend.flash_attention =
            flip(s.backend.flash_attention, FlashAttention::Auto, FlashAttention::On)),
        row!("backend.use_mmap", NONE, |s, _n, _t| s.backend.use_mmap = !s.backend.use_mmap),
        row!("backend.use_mlock", NONE, |s, _n, _t| s.backend.use_mlock = !s.backend.use_mlock),
        row!("backend.extra_args", NONE, |s, n, _t| s.backend.extra_args = vec![format!("--flag-{n}")]),
        row!("node.kaspad_path", NONE, |s, n, _t| s.node.kaspad_path = Some(PathBuf::from(format!("/bin/kaspad-{n}")))),
        row!("node.misaka_rpc", NONE, |s, n, _t| s.node.misaka_rpc = Some(format!("127.0.0.1:{}", 17000 + n))),
        row!("node.misaka_cli_path", NONE, |s, n, _t| s.node.misaka_cli_path = Some(PathBuf::from(format!("/bin/misaka-{n}")))),
        row!("node.rpc_url", NONE, |s, n, _t| s.node.rpc_url = Some(format!("ws://127.0.0.1:{}", 18000 + n))),
        // The `misaka` engine hands the network's id to its worker (`MISAKA_PALW_NETWORK_ID`,
        // ADR-0096 Decision 10), so the network is one of that engine's constructor inputs.
        row!("node.network", [BackendKind::Misaka], |s, _n, _t| s.node.network =
            flip(s.node.network, NodeNetwork::Testnet11, NodeNetwork::Devnet)),
        row!("node.role", NONE, |s, _n, _t| s.node.role = flip(s.node.role, NetworkRole::Observer, NetworkRole::Verifier)),
        row!("node.mining_address", NONE, |s, n, _t| s.node.mining_address = Some(format!("misakatest:addr{n}"))),
        row!("node.producer_key_path", NONE, |s, n, _t| s.node.producer_key_path =
            Some(PathBuf::from(format!("/keys/producer-{n}.seed")))),
        row!("node.producer_bond", NONE, |s, n, _t| s.node.producer_bond = Some(format!("{n:064x}:0"))),
        row!("node.fee_outpoint", NONE, |s, n, _t| s.node.fee_outpoint = Some(format!("{n:064x}:1"))),
        row!("node.producer_class", NONE, |s, n, _t| s.node.producer_class = Some(format!("{n:0128x}"))),
        row!("node.class_artifact", NONE, |s, n, _t| s.node.class_artifact =
            Some(PathBuf::from(format!("/models/class-{n}.palwart")))),
        row!("node.register_class", NONE, |s, n, _t| s.node.register_class = Some(format!("Org/Model-{n}"))),
        row!("node.appdir", NONE, |s, n, _t| s.node.appdir = Some(PathBuf::from(format!("/data/node-{n}")))),
        row!("node.extra_args", NONE, |s, n, _t| s.node.extra_args = vec![format!("--node-flag-{n}")]),
        row!("node.install_default_class_artifact", NONE, |s, _n, _t| s.node.install_default_class_artifact =
            !s.node.install_default_class_artifact),
        row!("node.pool_url", NONE, |s, n, _t| s.node.pool_url = Some(format!("https://pool-{n}.example/pool"))),
        // The slot id is a label the engine never copies: the engine holds the URL and the token.
        row!("node.pool_slot_id", NONE, |s, n, _t| s.node.pool_slot_id = Some(format!("slot-{n}"))),
        row!("node.sampling_policy", NONE, |s, _n, _t| s.node.sampling_policy =
            flip(s.node.sampling_policy, SamplingPolicy::GreedyWithNotice, SamplingPolicy::Refuse)),
        row!("node.summarize_after_turns", NONE, |s, n, _t| s.node.summarize_after_turns = 10 + n as u32),
        row!("node.continue_max_legs", NONE, |s, n, _t| s.node.continue_max_legs = 10 + n as u32),
        row!("generation.system_prompt", NONE, |s, n, _t| s.generation.system_prompt = format!("prompt {n}")),
        row!("generation.context_size", NONE, |s, n, _t| s.generation.context_size = Some(1024 + n as u32)),
        row!("generation.temperature", NONE, |s, n, _t| s.generation.temperature = 0.1 + n as f64 * 0.01),
        row!("generation.top_p", NONE, |s, n, _t| s.generation.top_p = 0.5 + n as f64 * 0.01),
        row!("generation.top_k", NONE, |s, n, _t| s.generation.top_k = 100 + n as i64),
        row!("generation.min_p", NONE, |s, n, _t| s.generation.min_p = 0.1 + n as f64 * 0.01),
        row!("generation.repeat_penalty", NONE, |s, n, _t| s.generation.repeat_penalty = 1.2 + n as f64 * 0.01),
        row!("generation.max_tokens", NONE, |s, n, _t| s.generation.max_tokens = 100 + n),
        row!("generation.seed", NONE, |s, n, _t| s.generation.seed = Some(n)),
        // The catalog is rebuilt on these — a different object, not the engine.
        row!("huggingface.endpoint", NONE, |s, n, _t| s.huggingface.endpoint = format!("https://hub-{n}.example")),
        row!("huggingface.token", NONE, |s, n, _t| s.huggingface.token = Some(format!("hf-{n}"))),
        row!("huggingface.max_concurrent_downloads", NONE, |s, n, _t| s.huggingface.max_concurrent_downloads = 3 + n as usize),
        row!("ui.theme", NONE, |s, _n, _t| s.ui.theme = flip(s.ui.theme, Theme::System, Theme::Dark)),
        row!("ui.show_provenance", NONE, |s, _n, _t| s.ui.show_provenance = !s.ui.show_provenance),
        row!("ui.show_performance", NONE, |s, _n, _t| s.ui.show_performance = !s.ui.show_performance),
        // The record store is reopened on these — again another object.
        row!("provenance.record_inferences", NONE, |s, _n, _t| s.provenance.record_inferences = !s.provenance.record_inferences),
        row!("provenance.keep_transcripts", NONE, |s, _n, _t| s.provenance.keep_transcripts = !s.provenance.keep_transcripts),
        row!("provenance.max_records", NONE, |s, n, _t| s.provenance.max_records = 100 + n as usize),
        row!("components.manifest", NONE, |s, n, _t| s.components.manifest = Some(format!("/manifests/components-{n}.json"))),
        row!("components.auto_check", NONE, |s, _n, _t| s.components.auto_check = !s.components.auto_check),
    ]
}

/// `backend.kind` is the one field the matrix cannot hold — changing it changes which column
/// the state is in — so it is covered on its own below.
const COVERED_SEPARATELY: [&str; 1] = ["backend.kind"];

fn flip<T: PartialEq>(current: T, a: T, b: T) -> T {
    if current == a { b } else { a }
}

fn mkdir(tmp: &std::path::Path, name: &str) -> PathBuf {
    let dir = tmp.join(name);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// Every leaf path of `Settings`, from the struct's own serialization.
fn leaves(value: &serde_json::Value, prefix: &str, out: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                leaves(child, &path, out);
            }
        }
        _ => {
            out.insert(prefix.to_string());
        }
    }
}

async fn studio(kind: BackendKind, tmp: &std::path::Path) -> Arc<AppState> {
    let mut settings =
        Settings { models_dir: mkdir(tmp, "models"), backend: BackendSettings { kind, ..Default::default() }, ..Default::default() };
    settings.node.palw_gateway_url = Some("https://pool.example/pool/v1/slots/slot-0/fp".into());
    settings.node.pool_slot_token = Some("token-0".into());
    let data = mkdir(tmp, &format!("data-{kind:?}"));
    AppState::new(settings, data.join("settings.json"), data).await
}

/// The completeness guard: the table names every leaf the struct has, and nothing it does not.
#[test]
fn every_settings_leaf_is_classified() {
    let mut expected = BTreeSet::new();
    leaves(&serde_json::to_value(Settings::default()).expect("json"), "", &mut expected);
    let mut listed: BTreeSet<String> = table().iter().map(|r| r.path.to_string()).collect();
    listed.extend(COVERED_SEPARATELY.iter().map(|s| s.to_string()));
    let unclassified: Vec<_> = expected.difference(&listed).collect();
    assert!(unclassified.is_empty(), "settings fields no row classifies (does an engine copy them?): {unclassified:?}");
    let phantom: Vec<_> = listed.difference(&expected).collect();
    assert!(phantom.is_empty(), "rows for fields the struct does not have: {phantom:?}");
    assert_eq!(table().len(), expected.len() - COVERED_SEPARATELY.len(), "one row per leaf");
}

/// The matrix: every row under every engine kind. A row's change replaces the instance under
/// exactly the kinds it names and keeps it under every other.
#[tokio::test(flavor = "multi_thread")]
async fn a_change_to_a_constructor_input_replaces_the_instance_and_a_change_to_anything_else_keeps_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut n = 1u64;
    for kind in ALL_KINDS {
        let state = studio(kind, tmp.path()).await;
        for row in table() {
            n += 1;
            let before = state.backend().await;
            let mut next = state.settings.read().await.clone();
            (row.mutate)(&mut next, n, tmp.path());
            state.apply_settings(next).await.unwrap_or_else(|e| panic!("{kind:?} / {}: {e}", row.path));
            let after = state.backend().await;
            let replaced = !Arc::ptr_eq(&before, &after);
            let expected = row.rebuilds.contains(&kind);
            assert_eq!(
                replaced,
                expected,
                "{kind:?} engine, change to {}: {}",
                row.path,
                if expected {
                    "a constructor input changed, so the instance must be replaced"
                } else {
                    "no constructor input changed, so the instance must stay"
                }
            );
            assert_eq!(after.name(), before.name(), "{kind:?}: the kind did not change, so the engine's name must not");
        }
    }
}

/// `backend.kind`: switching engines replaces the instance under every kind, and switching to
/// the kind already running is a no-op — a rebuild nobody asked for unloads the model.
#[tokio::test(flavor = "multi_thread")]
async fn switching_the_kind_replaces_the_engine_and_restating_it_does_not() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for kind in ALL_KINDS {
        let state = studio(kind, tmp.path()).await;
        let before = state.backend().await;

        let mut same = state.settings.read().await.clone();
        same.backend.kind = kind;
        state.apply_settings(same).await.expect("applies");
        assert!(Arc::ptr_eq(&before, &state.backend().await), "{kind:?}: restating the kind is not a change");

        let other = ALL_KINDS.into_iter().find(|k| *k != kind).expect("another kind");
        let mut switched = state.settings.read().await.clone();
        switched.backend.kind = other;
        state.apply_settings(switched).await.expect("applies");
        let after = state.backend().await;
        assert!(!Arc::ptr_eq(&before, &after), "{kind:?} → {other:?}: the engine must be replaced");
        assert_ne!(after.name(), before.name());
    }
}
