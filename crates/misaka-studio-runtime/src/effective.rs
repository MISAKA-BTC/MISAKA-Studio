//! **What the running objects were built from, beside what the settings say** — ADR-0096
//! Decision 11.
//!
//! `/api/v1/runtime` reports the engine's own view of what it loaded. Nothing reported, beside
//! it, what the running engine was constructed FROM — so on 2026-09-05 every panel named the new
//! pool slot while the chat mined on the old one, and no view could have said "the file says X,
//! the engine holds Y". This module is that view: per subsystem, `configured` (read from the
//! settings the process holds), `effective` (read from the RUNNING object — the engine's
//! fingerprint, the node manager's argument list, the record store's path, the catalog's
//! endpoint), `source` (which file, flag, environment variable or discovery produced each
//! effective value, where that is known), `since` (when the running object was built) and
//! `differs` (whether configured and effective disagree in any field the fingerprint covers).
//!
//! `source` says `settings file (<path>)` when nothing else can be known. The daemon records the
//! settings it overrode from a flag or the environment in [`SettingOrigins`], and a value that
//! came from discovery names the step of the search order that found it.

use crate::backend::RuntimeFingerprint;
use crate::components::{Candidate, ComponentId, resolve_component};
use crate::state::{AppState, fingerprint_for, kind_for_backend_name};
use misaka_studio_core::settings::{BackendKind, Settings};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// The settings the daemon overrode for this run, and from where.
///
/// Keyed by the settings path (`server.port`, `backend.llama_server_path`) and, for the two
/// values that are not settings at all, `data_dir` and `settings_path`. The value is the
/// sentence the effective view prints: `--port`, `env MISAKA_STUDIO_PORT`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SettingOrigins {
    pub overrides: BTreeMap<String, String>,
}

impl SettingOrigins {
    pub fn record(&mut self, path: &str, source: impl Into<String>) {
        self.overrides.insert(path.to_string(), source.into());
    }

    /// Where a settings value came from: the recorded override, else the settings file.
    pub fn source_of(&self, path: &str) -> String {
        self.overrides.get(path).cloned().unwrap_or_else(|| format!("settings file ({path})"))
    }
}

/// One subsystem of the effective view.
#[derive(Clone, Debug, Serialize)]
pub struct Subsystem {
    /// What the settings the process holds would build or mean.
    pub configured: Value,
    /// What the running object holds. `null` when nothing is running; `source.effective` then
    /// says why.
    pub effective: Option<Value>,
    /// Per effective field, what produced it.
    pub source: BTreeMap<String, String>,
    /// Unix seconds when the running object was built. `null` when nothing is running.
    pub since: Option<u64>,
    /// Whether `configured` and `effective` disagree in a field the running object's fingerprint
    /// covers — the mark a panel puts beside a field.
    pub differs: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectiveSettings {
    pub backend: Subsystem,
    pub node: Subsystem,
    pub records: Subsystem,
    pub catalog: Subsystem,
    pub pool: Subsystem,
    pub gateway: Subsystem,
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn prefix(token: Option<&str>) -> Option<String> {
    token.filter(|t| !t.is_empty()).map(RuntimeFingerprint::token_prefix)
}

/// The default a gateway engine is built against when `node.palw_gateway_url` is unset — the
/// same literal `build_backend_kind` uses.
const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:8790";

/// The slot a pool gateway URL names: `…/v1/slots/<id>/fp` → `<id>`. The pool's join handler
/// writes the URL in exactly this shape, so the id a running engine posts to can be read back
/// off it and held against `node.pool_slot_id`.
pub fn slot_id_from_gateway_url(url: &str) -> Option<String> {
    let rest = url.split("/v1/slots/").nth(1)?;
    let id = rest.split('/').next()?;
    (!id.is_empty()).then(|| id.to_string())
}

/// The settings path that names the program of an engine of this kind.
fn program_setting(kind: BackendKind) -> Option<&'static str> {
    match kind {
        BackendKind::LlamaCpp => Some("backend.llama_server_path"),
        BackendKind::Mlx => Some("backend.mlx_server_path"),
        BackendKind::Misaka => Some("backend.misaka_serve_path"),
        _ => None,
    }
}

/// Where a resolved program came from: the setting (and whatever overrode it) when the person
/// configured it, else the step of the search that found it.
fn program_source(origins: &SettingOrigins, candidate: &str, setting: Option<&str>) -> String {
    match (candidate == Candidate::Configured.as_str(), setting) {
        (true, Some(path)) => origins.source_of(path),
        (true, None) => "configured".into(),
        (false, _) => format!("discovery ({candidate})"),
    }
}

/// The whole view. Reads every running object once; nothing here rebuilds or restarts anything.
pub async fn effective_settings(state: &AppState) -> EffectiveSettings {
    let settings = state.settings.read().await.clone();
    let built_at = state.built_at().await;
    let origins = &state.origins;
    let running = state.backend().await;
    let loaded = state.loaded().await;
    let running_fingerprint = running.fingerprint();
    let running_kind = kind_for_backend_name(running.name());

    // --- backend ---------------------------------------------------------------------------
    let selected = fingerprint_for(settings.backend.kind, &settings, &state.hardware);
    let would_build = running_kind.map(|kind| fingerprint_for(kind, &settings, &state.hardware));
    let backend_differs = would_build.as_ref() != Some(&running_fingerprint);
    let mut source = BTreeMap::new();
    source.insert("kind".into(), origins.source_of("backend.kind"));
    if let Some(candidate) = running_fingerprint.extra.get("program_candidate") {
        source.insert("program".into(), program_source(origins, candidate, running_kind.and_then(program_setting)));
    }
    if running_fingerprint.startup_timeout_secs.is_some() {
        source.insert("startup_timeout_secs".into(), origins.source_of("backend.startup_timeout_secs"));
    }
    if running_kind == Some(BackendKind::Misaka) {
        source.insert(
            "tokenizer".into(),
            match settings.backend.misaka_tokenizer_path {
                Some(_) => origins.source_of("backend.misaka_tokenizer_path"),
                None => "discovery (tokenizer.json beside the artifact, at load)".into(),
            },
        );
    }
    if running_kind == Some(BackendKind::Gateway) {
        source.insert("url".into(), gateway_url_source(&settings, origins));
        source.insert("token".into(), gateway_token_source(&settings, origins));
    }
    if loaded.is_some() {
        source.insert("model_id".into(), "the load request".into());
        source.insert("context_size".into(), "the engine's report at load".into());
    }
    let backend = Subsystem {
        configured: json!({
            "kind": settings.backend.kind,
            "selects": selected.kind,
            "fingerprint": would_build.clone().unwrap_or_else(|| selected.clone()),
            "raw": {
                "llama_server_path": settings.backend.llama_server_path,
                "mlx_server_path": settings.backend.mlx_server_path,
                "misaka_serve_path": settings.backend.misaka_serve_path,
                "misaka_tokenizer_path": settings.backend.misaka_tokenizer_path,
                "startup_timeout_secs": settings.backend.startup_timeout_secs,
                "palw_gateway_url": settings.node.palw_gateway_url,
                "pool_slot_token_sha256_prefix": prefix(settings.node.pool_slot_token.as_deref()),
                "mining_mode": settings.node.mining_mode,
            },
        }),
        effective: Some(json!({
            "name": running.name(),
            "fingerprint": running_fingerprint,
            "model_id": loaded.as_ref().map(|l| l.model.id.clone()),
            "context_size": loaded.as_ref().map(|l| l.loaded.context_size),
        })),
        source,
        since: Some(unix(built_at.backend)),
        differs: backend_differs,
    };

    // --- node ------------------------------------------------------------------------------
    let binary = resolve_component(&ComponentId::Kaspad, settings.node.kaspad_path.as_deref(), None);
    let rpc_port = crate::node::default_json_rpc_port(settings.node.network);
    let args = crate::node::NodeManager::build_args(&settings.node, rpc_port);
    let configured_args = match &args {
        Ok(args) => json!(args),
        Err(e) => json!({ "error": e.to_string() }),
    };
    let running_node = state.node.effective().await;
    let mut source = BTreeMap::new();
    source.insert(
        "binary".into(),
        program_source(
            origins,
            running_node.as_ref().map(|n| n.binary_candidate).unwrap_or(binary.candidate).as_str(),
            Some("node.kaspad_path"),
        ),
    );
    source.insert("args".into(), "settings file (node.*), at the start".into());
    if running_node.is_none() {
        source.insert("effective".into(), "not running".into());
    }
    let node_differs = match (&running_node, &args) {
        (Some(node), Ok(args)) => node.args != *args || node.binary != binary.path,
        (Some(_), Err(_)) => true,
        (None, _) => false,
    };
    let node = Subsystem {
        configured: json!({
            "binary": binary,
            "args": configured_args,
            "role": settings.node.role,
            "network": settings.node.network,
            "rpc_url": settings.node.rpc_url,
        }),
        since: running_node.as_ref().map(|n| n.started_at_unix),
        effective: running_node.map(|n| json!(n)),
        source,
        differs: node_differs,
    };

    // --- records ---------------------------------------------------------------------------
    let records = state.records.read().await.clone();
    let configured_path = state.data_dir.join("inference-records.jsonl");
    let mut source = BTreeMap::new();
    source.insert("enabled".into(), origins.source_of("provenance.record_inferences"));
    source.insert("max_records".into(), origins.source_of("provenance.max_records"));
    source.insert(
        "path".into(),
        origins.overrides.get("data_dir").cloned().unwrap_or_else(|| "default (the platform data directory)".into()),
    );
    let records_differs = records.is_enabled() != settings.provenance.record_inferences
        || records.max_records() != settings.provenance.max_records
        || records.path() != configured_path;
    let records = Subsystem {
        configured: json!({
            "path": configured_path,
            "enabled": settings.provenance.record_inferences,
            "max_records": settings.provenance.max_records,
        }),
        effective: Some(json!({ "path": records.path(), "enabled": records.is_enabled(), "max_records": records.max_records() })),
        source,
        since: Some(unix(built_at.records)),
        differs: records_differs,
    };

    // --- catalog ---------------------------------------------------------------------------
    let catalog = state.catalog().await;
    let mut source = BTreeMap::new();
    source.insert(
        "endpoint".into(),
        if std::env::var("HF_ENDPOINT").ok().as_deref() == Some(catalog.endpoint()) {
            "env HF_ENDPOINT (the settings file holds the same value)".into()
        } else {
            origins.source_of("huggingface.endpoint")
        },
    );
    source.insert(
        "token".into(),
        if settings.huggingface.token.is_some() { origins.source_of("huggingface.token") } else { "none".into() },
    );
    let configured_endpoint = settings.huggingface.endpoint.trim_end_matches('/');
    let catalog_differs =
        catalog.endpoint() != configured_endpoint || prefix(catalog.token()) != prefix(settings.huggingface.token.as_deref());
    let catalog = Subsystem {
        configured: json!({ "endpoint": configured_endpoint, "token_sha256_prefix": prefix(settings.huggingface.token.as_deref()) }),
        effective: Some(json!({ "endpoint": catalog.endpoint(), "token_sha256_prefix": prefix(catalog.token()) })),
        source,
        since: Some(unix(built_at.catalog)),
        differs: catalog_differs,
    };

    // --- gateway ---------------------------------------------------------------------------
    let gateway_running = running_kind == Some(BackendKind::Gateway);
    let configured_gateway = fingerprint_for(BackendKind::Gateway, &settings, &state.hardware);
    let mut source = BTreeMap::new();
    source.insert("url".into(), gateway_url_source(&settings, origins));
    source.insert("token".into(), gateway_token_source(&settings, origins));
    if !gateway_running {
        source.insert(
            "effective".into(),
            format!(
                "the chat engine is `{}`; the gateway is reached only by the mining queue, which reads the settings per job",
                running.name()
            ),
        );
    }
    let gateway = Subsystem {
        configured: json!({
            "url": configured_gateway.url,
            "token_sha256_prefix": configured_gateway.token_sha256_prefix,
            "from": if settings.node.palw_gateway_url.is_some() { "node.palw_gateway_url" } else { "default" },
        }),
        effective: gateway_running
            .then(|| json!({ "url": running_fingerprint.url, "token_sha256_prefix": running_fingerprint.token_sha256_prefix })),
        source,
        since: gateway_running.then(|| unix(built_at.backend)),
        differs: gateway_running && configured_gateway != running_fingerprint,
    };

    // --- pool ------------------------------------------------------------------------------
    let last_mined_via = state.mining.list().await.into_iter().rev().find(|j| !j.gateway_url.is_empty()).map(|j| j.gateway_url);
    let mut source = BTreeMap::new();
    source.insert("slot_id".into(), origins.source_of("node.pool_slot_id"));
    source.insert("token".into(), gateway_token_source(&settings, origins));
    let engine_url = gateway_running.then(|| running_fingerprint.url.clone()).flatten();
    let engine_slot = engine_url.as_deref().and_then(slot_id_from_gateway_url);
    if engine_url.is_some() {
        source.insert("gateway_url".into(), "the running gateway engine".into());
    } else if last_mined_via.is_some() {
        source.insert("gateway_url".into(), "the mining queue's most recent job".into());
    } else {
        source.insert("effective".into(), "no gateway engine is running and nothing has been mined from the queue".into());
    }
    let pool_differs = match (&engine_slot, &settings.node.pool_slot_id) {
        (Some(engine), Some(configured)) => engine != configured,
        _ => false,
    } || (gateway_running
        && running_fingerprint.token_sha256_prefix != prefix(settings.node.pool_slot_token.as_deref()));
    let pool = Subsystem {
        configured: json!({
            "pool_url": settings.node.pool_url,
            "pool_slot_id": settings.node.pool_slot_id,
            "pool_slot_token_sha256_prefix": prefix(settings.node.pool_slot_token.as_deref()),
        }),
        effective: (engine_url.is_some() || last_mined_via.is_some()).then(|| {
            json!({
                "gateway_url": engine_url.clone().or_else(|| last_mined_via.clone()),
                "slot_id": engine_slot.clone().or_else(|| last_mined_via.as_deref().and_then(slot_id_from_gateway_url)),
                "token_sha256_prefix": gateway_running.then(|| running_fingerprint.token_sha256_prefix.clone()).flatten(),
                "last_mined_via": last_mined_via,
            })
        }),
        source,
        since: gateway_running.then(|| unix(built_at.backend)),
        differs: pool_differs,
    };

    EffectiveSettings { backend, node, records, catalog, pool, gateway }
}

fn gateway_url_source(settings: &Settings, origins: &SettingOrigins) -> String {
    match settings.node.palw_gateway_url {
        Some(_) => origins.source_of("node.palw_gateway_url"),
        None => format!("default (node.palw_gateway_url unset: {DEFAULT_GATEWAY_URL})"),
    }
}

fn gateway_token_source(settings: &Settings, origins: &SettingOrigins) -> String {
    match settings.node.pool_slot_token {
        Some(_) => origins.source_of("node.pool_slot_token"),
        None => "none".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slot_id_is_read_off_the_gateway_url_the_pool_writes() {
        assert_eq!(slot_id_from_gateway_url("https://pool.example/pool/v1/slots/slot-06/fp"), Some("slot-06".into()));
        assert_eq!(slot_id_from_gateway_url("https://pool.example/pool/v1/slots/slot-06"), Some("slot-06".into()));
        assert_eq!(slot_id_from_gateway_url("http://127.0.0.1:8790"), None, "a local gateway names no slot");
        assert_eq!(slot_id_from_gateway_url("https://pool.example/pool/v1/slots//fp"), None);
    }

    #[test]
    fn an_override_names_its_flag_and_everything_else_names_the_file() {
        let mut origins = SettingOrigins::default();
        origins.record("server.port", "--port");
        origins.record("backend.llama_server_path", "env MISAKA_STUDIO_LLAMA_SERVER");
        assert_eq!(origins.source_of("server.port"), "--port");
        assert_eq!(origins.source_of("backend.llama_server_path"), "env MISAKA_STUDIO_LLAMA_SERVER");
        assert_eq!(origins.source_of("ui.theme"), "settings file (ui.theme)");
        assert_eq!(program_source(&origins, "configured", Some("backend.llama_server_path")), "env MISAKA_STUDIO_LLAMA_SERVER");
        assert_eq!(
            program_source(&origins, "beside the executable", Some("backend.llama_server_path")),
            "discovery (beside the executable)"
        );
        assert_eq!(program_source(&origins, "PATH", None), "discovery (PATH)");
    }

    /// The whole view over a real state: the engine's effective fingerprint is what the settings
    /// would build (nothing differs right after construction); a token rotated in the settings
    /// WITHOUT going through `apply_settings` — the 2026-09-05 shape — is reported as a
    /// difference on the backend, the gateway and the pool, and the source names the slot.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_view_marks_a_slot_the_engine_does_not_hold() {
        use misaka_studio_core::settings::{BackendSettings, Settings};
        let data = tempfile::tempdir().expect("tempdir");
        let mut settings = Settings {
            models_dir: data.path().join("models"),
            backend: BackendSettings { kind: BackendKind::Gateway, ..Default::default() },
            ..Default::default()
        };
        std::fs::create_dir_all(&settings.models_dir).expect("models dir");
        settings.node.pool_url = Some("https://pool.example/pool".into());
        settings.node.pool_slot_id = Some("slot-04".into());
        settings.node.pool_slot_token = Some("token-04".into());
        settings.node.palw_gateway_url = Some("https://pool.example/pool/v1/slots/slot-04/fp".into());
        let mut origins = SettingOrigins::default();
        origins.record("backend.kind", "--backend");
        let state = AppState::with_origins(settings, data.path().join("settings.json"), data.path().to_path_buf(), origins).await;

        let view = effective_settings(&state).await;
        assert!(!view.backend.differs && !view.gateway.differs && !view.pool.differs, "freshly built, nothing differs");
        assert_eq!(view.backend.effective.as_ref().unwrap()["name"], "gateway");
        assert_eq!(view.backend.source["kind"], "--backend", "the daemon's override is named");
        assert_eq!(view.backend.source["url"], "settings file (node.palw_gateway_url)");
        assert_eq!(view.gateway.effective.as_ref().unwrap()["url"], "https://pool.example/pool/v1/slots/slot-04/fp");
        assert_eq!(view.pool.effective.as_ref().unwrap()["slot_id"], "slot-04");
        assert!(view.backend.since.is_some() && view.gateway.since == view.backend.since);
        assert_eq!(view.node.effective, None);
        assert_eq!(view.node.source["effective"], "not running");
        assert!(!view.records.differs && !view.catalog.differs);
        let printed = serde_json::to_string(&view).expect("json");
        assert!(!printed.contains("token-04"), "no token in the view: {printed}");

        // The 2026-09-05 shape: the settings move to another slot behind the engine's back.
        {
            let mut s = state.settings.write().await;
            s.node.pool_slot_id = Some("slot-06".into());
            s.node.pool_slot_token = Some("token-06".into());
            s.node.palw_gateway_url = Some("https://pool.example/pool/v1/slots/slot-06/fp".into());
        }
        let view = effective_settings(&state).await;
        assert!(view.backend.differs, "the engine was built from slot-04's address and token");
        assert!(view.gateway.differs);
        assert!(view.pool.differs, "the engine posts to slot-04 while the settings name slot-06");
        assert_eq!(view.pool.configured["pool_slot_id"], "slot-06");
        assert_eq!(view.pool.effective.as_ref().unwrap()["slot_id"], "slot-04");

        // And through the front door, the rebuild makes them agree again.
        let current = state.settings.read().await.clone();
        state.apply_settings(current).await.expect("applies");
        let view = effective_settings(&state).await;
        assert!(!view.backend.differs && !view.gateway.differs && !view.pool.differs);
        assert_eq!(view.pool.effective.as_ref().unwrap()["slot_id"], "slot-06");
    }
}
