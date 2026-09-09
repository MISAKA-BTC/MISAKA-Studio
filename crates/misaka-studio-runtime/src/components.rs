//! **The components the Studio spawns or maps, and the one rule that finds each of them.**
//!
//! ADR-0096 Decision 10. Before this module the Studio said what to install in four tables and
//! looked for a binary in four spellings — `resolve_program` twice (beside the executable, then
//! `engines/`, then `PATH`), `resolve_kaspad` and `resolve_misaka_cli` a third and fourth time
//! with a different order (no `engines/`) — and one of the tables drove `misaka-palw-serve`, a
//! binary the node tree deleted on 2026-09-02 (`2f688bc1`). Nothing in either repository's CI
//! knew. Three things close that:
//!
//! * [`ComponentId`] is every binary and class artifact the Studio can spawn or map, and
//!   [`resolve_component`] is the one search order — configured path, beside the executable,
//!   `engines/` beside the executable, the models directory (artifacts only), `PATH`. The four
//!   resolvers are now thin wrappers over it, and a test greps the runtime for a second spelling.
//! * [`Manifest`] mirrors the node repository's `components.json` (schema `misaka/components/v1`,
//!   `docs/components-manifest.md` in the misakas tree) with the same validator rules its writer
//!   enforces under `--validate`. A row is a pointer with a digest; a file that does not match
//!   the digest is not that component, whatever its name.
//! * [`check_spawnable_ids`] is the cross-repository check: every id in [`SPAWNABLE`] must be a
//!   row of the manifest, and a retired id must not be — reported by name, so a binary one tree
//!   stopped building fails this tree's check and not a person's evening.
//!
//! The install path goes through the existing download manager (sha256 and size verified there).
//! A row that names a `member` inside an archive is refused by name for now: extracting a zip
//! member and verifying it against the row's own digest is a second verification step this
//! module does not yet implement, and half of it — extract without verifying — would be exactly
//! the pointer-without-a-digest the manifest exists to forbid.

use crate::catalog::Catalog;
use misaka_studio_core::palw::{PalwArtifactSource, PalwClassSpec, TESTNET11_CLASSES};
use misaka_studio_core::settings::Settings;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// The schema this module reads. Another schema is refused, not tolerated.
pub const SCHEMA: &str = "misaka/components/v1";

/// The target triple this binary was built for, as a manifest row spells `platform`. Set by
/// `build.rs` from cargo's `TARGET`; the runtime cannot derive the vendor or the libc itself.
pub const HOST_PLATFORM: &str = env!("MISAKA_STUDIO_TARGET");

/// A manifest is kilobytes. A megabyte is the cap on what `load_manifest` will read from a URL or
/// a file, so a wrong address that answers with a web page cannot become a memory problem.
pub const MANIFEST_MAX_BYTES: usize = 1 << 20;

/// The directory beside the executable where installed engines and node binaries land.
pub const ENGINES_DIR: &str = "engines";

/// What a row is. A consumer dispatches on it and refuses a kind it does not know (the validator
/// does the refusing, before any row reaches typed code).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Node,
    Cli,
    Worker,
    Gateway,
    Rail,
    Engine,
    Artifact,
    TokenizerTable,
    Runtime,
    Shell,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Node => "node",
            Kind::Cli => "cli",
            Kind::Worker => "worker",
            Kind::Gateway => "gateway",
            Kind::Rail => "rail",
            Kind::Engine => "engine",
            Kind::Artifact => "artifact",
            Kind::TokenizerTable => "tokenizer-table",
            Kind::Runtime => "runtime",
            Kind::Shell => "shell",
        }
    }
}

/// The kinds the validator knows, in the writer's order.
const KINDS: [&str; 10] = ["node", "cli", "worker", "gateway", "rail", "engine", "artifact", "tokenizer-table", "runtime", "shell"];
/// Rows of these kinds are files of no platform: `platform` MUST be `any`.
const PLATFORM_ANY_KINDS: [&str; 2] = ["artifact", "tokenizer-table"];
const TOP_REQUIRED: [&str; 4] = ["schema", "release", "network", "components"];
const TOP_OPTIONAL: [&str; 1] = ["node_manifest"];
const ROW_REQUIRED: [&str; 8] = ["id", "kind", "version", "platform", "url", "sha256", "size", "requires"];
const ROW_OPTIONAL: [&str; 9] = [
    "member",
    "archive_sha256",
    "archive_size",
    "class_id",
    "artifact_root",
    "tokenizer_commitment",
    "model_id",
    "convert_command",
    "notes",
];
const URL_SCHEMES: [&str; 2] = ["https://", "hf://"];

/// Every component the Studio can spawn or map.
///
/// The binaries are fixed names; an artifact is named by its class in the class table, because
/// the table is what carries its file name and digest (the offline copy of the manifest's
/// artifact rows). `MisakaPalwServe` stays as an id so the check can NAME it as retired rather
/// than forget it: the node tree stopped building it on 2026-09-02 and the Studio's `misaka`
/// backend still spawns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComponentId {
    Kaspad,
    Misaka,
    LlamaServer,
    MlxServer,
    PalwA16FpWorker,
    PalwQwen36FpWorker,
    MisakaPalwGateway,
    MisakaPalwFpRail,
    MisakaPalwServe,
    /// A class artifact, by the class's name in `TESTNET11_CLASSES`.
    Artifact {
        class: &'static str,
    },
}

/// The binaries, in the order the components table lists them.
pub const BINARIES: [ComponentId; 9] = [
    ComponentId::Kaspad,
    ComponentId::Misaka,
    ComponentId::PalwA16FpWorker,
    ComponentId::PalwQwen36FpWorker,
    ComponentId::MisakaPalwGateway,
    ComponentId::MisakaPalwFpRail,
    ComponentId::LlamaServer,
    ComponentId::MlxServer,
    ComponentId::MisakaPalwServe,
];

/// **The ids the Studio can spawn** — what the cross-repository check demands rows for.
///
/// `llama-server` and `mlx_lm.server` are third-party and exempt: the node tree does not build
/// them, so their absence from a node manifest says nothing. `misaka-palw-serve` is here because
/// `backend/misaka.rs` still spawns it, and the check reports it as retired for exactly that
/// reason; a test holds this list and the source's mention of the name to the same answer, so
/// removing the backend without removing the id (or the reverse) fails the build.
pub const SPAWNABLE: [ComponentId; 7] = [
    ComponentId::Kaspad,
    ComponentId::Misaka,
    ComponentId::PalwA16FpWorker,
    ComponentId::PalwQwen36FpWorker,
    ComponentId::MisakaPalwGateway,
    ComponentId::MisakaPalwFpRail,
    ComponentId::MisakaPalwServe,
];

/// What the check says about `misaka-palw-serve`, verbatim where it is printed.
pub const MISAKA_PALW_SERVE_RETIRED: &str = "retired 2026-09-02 (misakas 2f688bc1): one runtime — v3-serve on both family workers; \
                                            the Studio's local integer engine is the gateway in --answer-never-commit mode over the \
                                            family worker (ADR-0096 Decision 10)";

impl ComponentId {
    /// The binaries, then every class artifact the class table publishes as a file.
    pub fn all() -> Vec<ComponentId> {
        let mut out: Vec<ComponentId> = BINARIES.to_vec();
        out.extend(
            TESTNET11_CLASSES
                .iter()
                .filter(|class| matches!(class.artifact, PalwArtifactSource::Download { .. }))
                .map(|class| ComponentId::Artifact { class: class.name }),
        );
        out
    }

    /// The id a manifest row carries: a binary's file name without `.exe`, an artifact's file
    /// stem (`qwen25-1.5b-a16` for `qwen25-1.5b-a16.palwart`).
    pub fn id(&self) -> String {
        match self {
            ComponentId::Kaspad => "kaspad".into(),
            ComponentId::Misaka => "misaka".into(),
            ComponentId::LlamaServer => "llama-server".into(),
            ComponentId::MlxServer => "mlx-server".into(),
            ComponentId::PalwA16FpWorker => "palw-a16-fp-worker".into(),
            ComponentId::PalwQwen36FpWorker => "palw-qwen36-fp-worker".into(),
            ComponentId::MisakaPalwGateway => "misaka-palw-gateway".into(),
            ComponentId::MisakaPalwFpRail => "misaka-palw-fp-rail".into(),
            ComponentId::MisakaPalwServe => "misaka-palw-serve".into(),
            ComponentId::Artifact { .. } => {
                let name = self.file_name();
                // `rfind`, for the same reason the class table's `file_extension` uses it: every
                // artifact name carries a version dot before its extension.
                match name.rfind('.') {
                    Some(dot) => name[..dot].to_string(),
                    None => name,
                }
            }
        }
    }

    /// The name the file has on disk. `mlx_lm.server` is a console script the `mlx-lm` package
    /// installs, so its id and its file name differ; every other binary is named by its id.
    pub fn file_name(&self) -> String {
        let exe = |name: &str| if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
        match self {
            ComponentId::MlxServer => "mlx_lm.server".into(),
            ComponentId::Artifact { class } => match class_by_name(class).map(|c| &c.artifact) {
                Some(PalwArtifactSource::Download { filename, .. }) => (*filename).to_string(),
                _ => format!("{class}.palwart"),
            },
            binary => exe(&binary.id()),
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            ComponentId::Kaspad => Kind::Node,
            ComponentId::Misaka => Kind::Cli,
            ComponentId::LlamaServer | ComponentId::MlxServer | ComponentId::MisakaPalwServe => Kind::Engine,
            ComponentId::PalwA16FpWorker | ComponentId::PalwQwen36FpWorker => Kind::Worker,
            ComponentId::MisakaPalwGateway => Kind::Gateway,
            ComponentId::MisakaPalwFpRail => Kind::Rail,
            ComponentId::Artifact { .. } => Kind::Artifact,
        }
    }

    pub fn is_artifact(&self) -> bool {
        matches!(self, ComponentId::Artifact { .. })
    }

    /// Third-party: the node tree does not build it, so no node manifest is expected to list it.
    pub fn is_third_party(&self) -> bool {
        matches!(self, ComponentId::LlamaServer | ComponentId::MlxServer)
    }

    /// Why this id must not be in a manifest, when it must not.
    pub fn retired(&self) -> Option<&'static str> {
        match self {
            ComponentId::MisakaPalwServe => Some(MISAKA_PALW_SERVE_RETIRED),
            _ => None,
        }
    }

    /// The class an artifact id names.
    pub fn class(&self) -> Option<&'static PalwClassSpec> {
        match self {
            ComponentId::Artifact { class } => class_by_name(class),
            _ => None,
        }
    }

    /// The id, parsed — the inverse of [`ComponentId::id`] over [`ComponentId::all`].
    pub fn parse(id: &str) -> Option<ComponentId> {
        ComponentId::all().into_iter().find(|c| c.id() == id)
    }

    /// The settings field that names this component's path, when one exists. One table, so the
    /// components view and every spawn site read the same field.
    pub fn configured_path(&self, settings: &Settings) -> Option<PathBuf> {
        match self {
            ComponentId::Kaspad => settings.node.kaspad_path.clone(),
            ComponentId::Misaka => settings.node.misaka_cli_path.clone(),
            ComponentId::LlamaServer => settings.backend.llama_server_path.clone(),
            ComponentId::MlxServer => settings.backend.mlx_server_path.clone(),
            ComponentId::MisakaPalwServe => settings.backend.misaka_serve_path.clone(),
            // The producer's artifact setting names ONE file; it configures this component only
            // when it is this class's file.
            ComponentId::Artifact { .. } => settings
                .node
                .class_artifact
                .clone()
                .filter(|path| path.file_name().and_then(|n| n.to_str()) == Some(self.file_name().as_str())),
            ComponentId::PalwA16FpWorker
            | ComponentId::PalwQwen36FpWorker
            | ComponentId::MisakaPalwGateway
            | ComponentId::MisakaPalwFpRail => None,
        }
    }
}

impl fmt::Display for ComponentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id())
    }
}

fn class_by_name(name: &str) -> Option<&'static PalwClassSpec> {
    TESTNET11_CLASSES.iter().find(|class| class.name == name)
}

/// Which step of the search order produced a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Candidate {
    #[serde(rename = "configured")]
    Configured,
    #[serde(rename = "beside the executable")]
    BesideExecutable,
    #[serde(rename = "engines/")]
    EnginesDir,
    #[serde(rename = "models_dir")]
    ModelsDir,
    #[serde(rename = "PATH")]
    Path,
    #[serde(rename = "not found")]
    NotFound,
}

impl Candidate {
    pub fn as_str(self) -> &'static str {
        match self {
            Candidate::Configured => "configured",
            Candidate::BesideExecutable => "beside the executable",
            Candidate::EnginesDir => "engines/",
            Candidate::ModelsDir => "models_dir",
            Candidate::Path => "PATH",
            Candidate::NotFound => "not found",
        }
    }
}

impl fmt::Display for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a component is, and which step of the search said so.
///
/// `found` is false in two cases the path still distinguishes: a configured path that is not a
/// file (the path is the person's, returned verbatim so the error names it), and nothing found
/// anywhere (the path is the bare file name, so a spawn error names the thing that is missing
/// rather than an absolute path that never existed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Resolution {
    pub path: PathBuf,
    pub found: bool,
    pub candidate: Candidate,
}

/// **The one search order**: the configured path, beside the executable, `engines/` beside the
/// executable, the models directory (artifacts only), `PATH` (binaries only).
///
/// A configured path wins without being checked, because it is the person's decision and a
/// wrong one must fail with that path in the error. `models_dir` is `None` for the callers that
/// resolve a binary from a path and a timeout rather than from a settings value; it is only read
/// for artifacts.
pub fn resolve_component(id: &ComponentId, configured: Option<&Path>, models_dir: Option<&Path>) -> Resolution {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    locate(id, configured, models_dir, executable_dir().as_deref(), &path_dirs)
}

/// The search order over an explicit environment — what [`resolve_component`] binds to this
/// process's, and what the tests drive over a directory they built.
pub(crate) fn locate(
    id: &ComponentId,
    configured: Option<&Path>,
    models_dir: Option<&Path>,
    exe_dir: Option<&Path>,
    path_dirs: &[PathBuf],
) -> Resolution {
    if let Some(path) = configured {
        return Resolution { path: path.to_path_buf(), found: path.is_file(), candidate: Candidate::Configured };
    }
    let name = id.file_name();
    if let Some(dir) = exe_dir {
        for (candidate, dir) in [(Candidate::BesideExecutable, dir.to_path_buf()), (Candidate::EnginesDir, dir.join(ENGINES_DIR))] {
            let path = dir.join(&name);
            if path.is_file() {
                return Resolution { path, found: true, candidate };
            }
        }
    }
    if id.is_artifact() {
        if let Some(dir) = models_dir {
            let path = dir.join(&name);
            if path.is_file() {
                return Resolution { path, found: true, candidate: Candidate::ModelsDir };
            }
        }
    } else if let Some(path) = path_dirs.iter().map(|dir| dir.join(&name)).find(|c| c.is_file()) {
        return Resolution { path, found: true, candidate: Candidate::Path };
    }
    Resolution { path: PathBuf::from(name), found: false, candidate: Candidate::NotFound }
}

/// The directory holding this executable — where a packaged Studio ships its engines.
pub fn executable_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// `engines/` beside the executable: where an installed binary lands.
pub fn engines_dir() -> Option<PathBuf> {
    executable_dir().map(|dir| dir.join(ENGINES_DIR))
}

// --- the manifest ------------------------------------------------------------------------------

/// `components.json`, schema `misaka/components/v1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    /// The release this manifest belongs to.
    pub release: String,
    /// The network the release is cut for.
    pub network: String,
    /// Studio manifests only: the node manifest this one was built against, by URL and digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_manifest: Option<NodeManifestRef>,
    /// Sorted by id — the canonical form; the validator refuses another order.
    pub components: Vec<ComponentRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeManifestRef {
    pub url: String,
    pub sha256: String,
}

/// One row: a pointer with a digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRow {
    pub id: String,
    pub kind: Kind,
    /// The release tag for a binary; the registration that pinned an artifact's root.
    pub version: String,
    /// A Rust target triple, or `any` for an artifact or a tokenizer table.
    pub platform: String,
    /// `https://…` or `hf://<repo>/<path>`.
    pub url: String,
    /// Of the component's OWN bytes — the file that will be executed or mapped.
    pub sha256: String,
    pub size: u64,
    /// Ids that must be installed for this row to be useful.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Path inside the archive `url` points at, when it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer_commitment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub convert_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Manifest {
    /// Parse and validate. Every fault is returned, not the first: a person fixing a manifest by
    /// hand wants the whole list once, which is also what the node writer's `--validate` prints.
    pub fn parse(text: &str) -> std::result::Result<Manifest, Vec<String>> {
        let value: Value = serde_json::from_str(text).map_err(|e| vec![format!("not valid JSON: {e}")])?;
        let findings = validate_value(&value);
        if !findings.is_empty() {
            return Err(findings);
        }
        serde_json::from_value(value).map_err(|e| vec![format!("the manifest validates but did not deserialize: {e}")])
    }

    /// The validator over a typed manifest — the same rules, so a manifest built in code is held
    /// to what a file is held to.
    pub fn validate(&self) -> Vec<String> {
        validate_value(&serde_json::to_value(self).unwrap_or(Value::Null))
    }

    pub fn row(&self, id: &str) -> Option<&ComponentRow> {
        self.components.iter().find(|row| row.id == id)
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_id(s: &str) -> bool {
    let mut bytes = s.bytes();
    let first_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    first_ok && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

/// `^[a-z0-9_]+-[a-z0-9_.-]+$`: `aarch64-apple-darwin`, `x86_64-unknown-linux-musl`.
fn is_triple(s: &str) -> bool {
    let Some((head, tail)) = s.split_once('-') else { return false };
    !head.is_empty()
        && head.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.' || b == b'-')
}

/// A JSON integer that is not a bool and not negative. `1.0` is not one, as in the writer.
fn non_negative_int(v: &Value) -> bool {
    v.as_u64().is_some()
}

/// Every fault in one row, each naming `where`. Mirrors the node writer's `validate_row`.
fn validate_row(row: &Value, where_: &str, ids: &BTreeSet<String>, has_node_manifest: bool) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(obj) = row.as_object() else {
        return vec![format!("{where_}: a row must be an object")];
    };
    let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    let missing: Vec<&str> = ROW_REQUIRED.iter().copied().filter(|k| !keys.contains(k)).collect();
    if !missing.is_empty() {
        errs.push(format!("{where_}: missing required key(s) {}", missing.join(", ")));
    }
    let unknown: Vec<&str> = keys.iter().copied().filter(|k| !ROW_REQUIRED.contains(k) && !ROW_OPTIONAL.contains(k)).collect();
    if !unknown.is_empty() {
        errs.push(format!("{where_}: unknown key(s) {} (a new key is a schema change, not a row)", unknown.join(", ")));
    }
    if !missing.is_empty() {
        return errs;
    }

    let rid = obj["id"].as_str();
    let tag = format!("{where_} ({})", rid.unwrap_or("?"));
    if !rid.is_some_and(is_id) {
        errs.push(format!("{tag}: id must match ^[a-z0-9][a-z0-9.-]*$"));
    }
    let kind = obj["kind"].as_str().unwrap_or("");
    if !KINDS.contains(&kind) {
        errs.push(format!("{tag}: kind {:?} is not one of {}", obj["kind"], KINDS.join("|")));
    }
    if !obj["version"].as_str().is_some_and(|v| !v.is_empty()) {
        errs.push(format!("{tag}: version must be a non-empty string"));
    }
    match obj["platform"].as_str() {
        None => errs.push(format!("{tag}: platform must be a string")),
        Some(platform) if PLATFORM_ANY_KINDS.contains(&kind) => {
            if platform != "any" {
                errs.push(format!("{tag}: kind {kind} is a file of no platform; platform must be `any`, not {platform:?}"));
            }
        }
        Some(platform) => {
            if platform == "any" || !is_triple(platform) {
                errs.push(format!("{tag}: platform must be a Rust target triple, not {platform:?}"));
            }
        }
    }
    let url = obj["url"].as_str().unwrap_or("");
    if !URL_SCHEMES.iter().any(|s| url.starts_with(s)) || url.ends_with('/') {
        errs.push(format!("{tag}: url must start with one of {} and name a file", URL_SCHEMES.join(", ")));
    }
    if !obj["sha256"].as_str().is_some_and(|s| is_hex(s, 64)) {
        errs.push(format!("{tag}: sha256 must be 64 lowercase hex characters"));
    }
    if !non_negative_int(&obj["size"]) {
        errs.push(format!("{tag}: size must be a non-negative integer"));
    }
    match obj["requires"].as_array() {
        Some(reqs) if reqs.iter().all(Value::is_string) => {
            for r in reqs.iter().filter_map(Value::as_str) {
                if Some(r) == rid {
                    errs.push(format!("{tag}: requires itself"));
                } else if !ids.contains(r) && !has_node_manifest {
                    errs.push(format!("{tag}: requires {r:?}, which is not a row of this manifest (and no node_manifest is named)"));
                }
            }
        }
        _ => errs.push(format!("{tag}: requires must be a list of ids")),
    }
    let kind_required: &[&str] = match kind {
        "artifact" => &["class_id", "artifact_root"],
        "tokenizer-table" => &["tokenizer_commitment"],
        _ => &[],
    };
    for key in kind_required {
        if !keys.contains(key) {
            errs.push(format!("{tag}: kind {kind} requires {key}"));
        }
    }
    for key in ["class_id", "artifact_root", "tokenizer_commitment"] {
        if let Some(v) = obj.get(key)
            && !v.as_str().is_some_and(|s| is_hex(s, 128))
        {
            errs.push(format!("{tag}: {key} must be 128 lowercase hex characters"));
        }
    }
    if let Some(member) = obj.get("member") {
        if !member.as_str().is_some_and(|m| !m.is_empty()) {
            errs.push(format!("{tag}: member must be a non-empty path inside the archive"));
        }
        for key in ["archive_sha256", "archive_size"] {
            if !keys.contains(key) {
                errs.push(format!("{tag}: member names an archive, so {key} is required"));
            }
        }
        if let Some(v) = obj.get("archive_sha256")
            && !v.as_str().is_some_and(|s| is_hex(s, 64))
        {
            errs.push(format!("{tag}: archive_sha256 must be 64 lowercase hex characters"));
        }
        if let Some(v) = obj.get("archive_size")
            && !non_negative_int(v)
        {
            errs.push(format!("{tag}: archive_size must be a non-negative integer"));
        }
    } else {
        for key in ["archive_sha256", "archive_size"] {
            if keys.contains(key) {
                errs.push(format!("{tag}: {key} without member"));
            }
        }
    }
    for key in ["model_id", "convert_command", "notes"] {
        if let Some(v) = obj.get(key)
            && !v.is_string()
        {
            errs.push(format!("{tag}: {key} must be a string"));
        }
    }
    errs
}

/// Every fault in a manifest, as strings. Empty means valid. Mirrors the node writer's
/// `validate_manifest` rule for rule, so a file both trees read is refused by both for the same
/// reason.
pub fn validate_value(m: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(obj) = m.as_object() else {
        return vec!["the manifest must be a JSON object".into()];
    };
    let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    let missing: Vec<&str> = TOP_REQUIRED.iter().copied().filter(|k| !keys.contains(k)).collect();
    if !missing.is_empty() {
        errs.push(format!("missing top-level key(s) {}", missing.join(", ")));
    }
    let unknown: Vec<&str> = keys.iter().copied().filter(|k| !TOP_REQUIRED.contains(k) && !TOP_OPTIONAL.contains(k)).collect();
    if !unknown.is_empty() {
        errs.push(format!("unknown top-level key(s) {}", unknown.join(", ")));
    }
    if !missing.is_empty() {
        return errs;
    }
    if obj["schema"].as_str() != Some(SCHEMA) {
        errs.push(format!("schema is {}, this checker knows {SCHEMA:?}", obj["schema"]));
    }
    for key in ["release", "network"] {
        if !obj[key].as_str().is_some_and(|s| !s.is_empty()) {
            errs.push(format!("{key} must be a non-empty string"));
        }
    }
    let has_node_manifest = keys.contains("node_manifest");
    if has_node_manifest {
        let nm = &obj["node_manifest"];
        let exact = nm.as_object().is_some_and(|o| o.len() == 2 && o.contains_key("url") && o.contains_key("sha256"));
        if !exact {
            errs.push("node_manifest must be exactly {url, sha256}".into());
        } else {
            if !nm["url"].as_str().is_some_and(|u| u.starts_with("https://")) {
                errs.push("node_manifest.url must be an https:// URL".into());
            }
            if !nm["sha256"].as_str().is_some_and(|s| is_hex(s, 64)) {
                errs.push("node_manifest.sha256 must be 64 lowercase hex characters".into());
            }
        }
    }
    let rows = match obj["components"].as_array() {
        Some(rows) if !rows.is_empty() => rows,
        _ => {
            errs.push("components must be a non-empty list".into());
            return errs;
        }
    };
    let ids: Vec<Option<&str>> = rows.iter().map(|r| r.get("id").and_then(Value::as_str)).collect();
    let id_set: BTreeSet<String> = ids.iter().flatten().map(|s| s.to_string()).collect();
    for (i, row) in rows.iter().enumerate() {
        errs.extend(validate_row(row, &format!("components[{i}]"), &id_set, has_node_manifest));
    }
    let mut seen = BTreeSet::new();
    for id in ids.iter().flatten() {
        if !seen.insert(*id) {
            errs.push(format!("id {id:?} appears twice"));
        }
    }
    let str_ids: Vec<&str> = ids.iter().flatten().copied().collect();
    let mut sorted = str_ids.clone();
    sorted.sort_unstable();
    if str_ids != sorted {
        errs.push(
            "components are not sorted by id (the canonical form is sorted; a reordered manifest is a different file with the same meaning)"
                .into(),
        );
    }
    errs
}

/// Why a manifest could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    /// The file or URL could not be read at all.
    Unreadable { source: String, reason: String },
    /// Read, and refused by the validator — every finding, in order.
    Invalid { source: String, findings: Vec<String> },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Unreadable { source, reason } => write!(f, "cannot read manifest {source}: {reason}"),
            ManifestError::Invalid { source, findings } => {
                write!(f, "{source} does not validate:")?;
                for finding in findings {
                    write!(f, "\n  {finding}")?;
                }
                Ok(())
            }
        }
    }
}

/// Read a manifest from a local path or an `https://` URL.
///
/// The URL goes through the catalog's HTTP client without the hub token: a manifest host is a
/// release page, not the hub, and a bearer meant for one must not be presented to the other.
/// `http://` is refused — the digest binds a row to bytes, and what binds the manifest to a
/// release is that the release published it over TLS.
pub async fn load_manifest(source: &str, catalog: &Catalog) -> std::result::Result<Manifest, ManifestError> {
    let unreadable = |reason: String| ManifestError::Unreadable { source: source.to_string(), reason };
    let text = if source.starts_with("https://") {
        catalog.fetch_text(source, MANIFEST_MAX_BYTES).await.map_err(|e| unreadable(e.to_string()))?
    } else if source.starts_with("http://") {
        return Err(unreadable("a manifest travels over TLS or from disk; http:// is refused".into()));
    } else {
        let path = Path::new(source);
        let meta = tokio::fs::metadata(path).await.map_err(|e| unreadable(e.to_string()))?;
        if meta.len() > MANIFEST_MAX_BYTES as u64 {
            return Err(unreadable(format!("{} bytes is larger than a manifest ({MANIFEST_MAX_BYTES} bytes)", meta.len())));
        }
        tokio::fs::read_to_string(path).await.map_err(|e| unreadable(e.to_string()))?
    };
    Manifest::parse(&text).map_err(|findings| ManifestError::Invalid { source: source.to_string(), findings })
}

// --- the cross-repository check ----------------------------------------------------------------

/// What [`check_spawnable_ids`] found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "finding", rename_all = "snake_case")]
pub enum Finding {
    /// A binary the Studio spawns is not a row: the release does not build it, or stopped.
    MissingSpawnable { id: String, kind: Kind },
    /// A code path still spawns a binary the node tree retired.
    RetiredStillSpawned { id: String, note: String },
    /// A manifest lists a retired binary — a row for something no release builds.
    RetiredInManifest { id: String, note: String },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Finding::MissingSpawnable { id, kind } => {
                write!(f, "MISSING {id} ({}): the Studio can spawn it and the manifest has no row for it", kind.as_str())
            }
            Finding::RetiredStillSpawned { id, note } => write!(f, "RETIRED {id}: {note}; a code path here still names it"),
            Finding::RetiredInManifest { id, note } => write!(f, "RETIRED {id}: {note}; the manifest must not carry a row for it"),
        }
    }
}

impl Finding {
    /// Whether `--check --strict` exits non-zero on this finding. A retired id still named here is
    /// this tree's debt, reported every run; a missing spawnable id is the manifest's, and the one
    /// a release gate should stop on.
    pub fn is_strict_failure(&self) -> bool {
        matches!(self, Finding::MissingSpawnable { .. })
    }
}

/// **Every id the Studio can spawn is a row, and no retired id is** — ADR-0096 invariant 10.
pub fn check_spawnable_ids(manifest: &Manifest) -> Vec<Finding> {
    let mut findings = Vec::new();
    for id in SPAWNABLE {
        let name = id.id();
        match id.retired() {
            Some(note) => {
                findings.push(Finding::RetiredStillSpawned { id: name.clone(), note: note.to_string() });
                if manifest.row(&name).is_some() {
                    findings.push(Finding::RetiredInManifest { id: name, note: note.to_string() });
                }
            }
            None => {
                if manifest.row(&name).is_none() {
                    findings.push(Finding::MissingSpawnable { id: name, kind: id.kind() });
                }
            }
        }
    }
    findings
}

// --- the components table ----------------------------------------------------------------------

/// Where a component stands on this machine against the manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ComponentState {
    /// Found, and its SHA-256 equals the row's.
    #[serde(rename = "installed")]
    Installed,
    /// Found, the size equals the row's, and the digest was not computed (it is, on `?verify=1`).
    #[serde(rename = "installed-unverified")]
    InstalledUnverified,
    /// Found, and the size or the digest differs from the row's: not this component, whatever
    /// its name.
    #[serde(rename = "mismatch")]
    Mismatch,
    #[serde(rename = "missing")]
    Missing,
    #[serde(rename = "retired")]
    Retired,
    /// Found, and no manifest (or no row) to hold it to.
    #[serde(rename = "not-in-manifest")]
    NotInManifest,
}

impl ComponentState {
    pub fn as_str(self) -> &'static str {
        match self {
            ComponentState::Installed => "installed",
            ComponentState::InstalledUnverified => "installed-unverified",
            ComponentState::Mismatch => "mismatch",
            ComponentState::Missing => "missing",
            ComponentState::Retired => "retired",
            ComponentState::NotInManifest => "not-in-manifest",
        }
    }
}

/// What is on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstalledInfo {
    pub path: PathBuf,
    pub candidate: Candidate,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Computed only on request: a class artifact is 34 GiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The first line of `--version`, when it was asked for (`?verify=1`) and the binary answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// What the manifest says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ManifestRowInfo {
    pub version: String,
    pub sha256: String,
    pub size: u64,
    pub url: String,
    pub platform: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
}

impl From<&ComponentRow> for ManifestRowInfo {
    fn from(row: &ComponentRow) -> Self {
        ManifestRowInfo {
            version: row.version.clone(),
            sha256: row.sha256.clone(),
            size: row.size,
            url: row.url.clone(),
            platform: row.platform.clone(),
            member: row.member.clone(),
        }
    }
}

/// One line of the components table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ComponentReport {
    pub id: String,
    pub kind: Kind,
    pub installed: InstalledInfo,
    pub manifest: Option<ManifestRowInfo>,
    pub state: ComponentState,
    /// Why the state is what it is, when a word is not enough.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Whether a row built for `row_platform` is meant for a host built for `host`: the same triple,
/// `any`, or the same architecture and operating system with another vendor or libc — the node
/// releases Linux as musl and this runtime may be built gnu, and the static binary runs.
pub fn platform_compatible(row_platform: &str, host: &str) -> bool {
    if row_platform == "any" || row_platform == host {
        return true;
    }
    let arch = |t: &str| t.split('-').next().unwrap_or("").to_string();
    let os = |t: &str| ["linux", "darwin", "windows"].into_iter().find(|o| t.contains(o));
    arch(row_platform) == arch(host) && os(row_platform).is_some() && os(row_platform) == os(host)
}

/// The components table: every id the Studio can spawn plus every artifact in the class table,
/// resolved on this machine and held to the manifest when there is one.
///
/// `verify` computes each found file's SHA-256 and asks each found binary for `--version`; off,
/// the cheap checks (presence, size) still run and a matching file is `installed-unverified`.
pub async fn report(settings: &Settings, manifest: Option<&Manifest>, verify: bool) -> Vec<ComponentReport> {
    let mut out = Vec::new();
    for id in ComponentId::all() {
        let configured = id.configured_path(settings);
        let resolution = resolve_component(&id, configured.as_deref(), Some(&settings.models_dir));
        let size = if resolution.found { tokio::fs::metadata(&resolution.path).await.ok().map(|m| m.len()) } else { None };
        let sha256 = match (verify, resolution.found) {
            (true, true) => crate::download::sha256_file(&resolution.path).await.ok(),
            _ => None,
        };
        let version = match (verify, resolution.found, id.is_artifact()) {
            (true, true, false) => version_of(&resolution.path).await,
            _ => None,
        };
        let row = manifest.and_then(|m| m.row(&id.id()));
        let mut notes: Vec<String> = Vec::new();
        if let Some(row) = row
            && !platform_compatible(&row.platform, HOST_PLATFORM)
        {
            notes.push(format!("the manifest row is for {}; this build is {HOST_PLATFORM}", row.platform));
        }
        let state = if let Some(note) = id.retired() {
            notes.insert(0, note.to_string());
            ComponentState::Retired
        } else if !resolution.found {
            ComponentState::Missing
        } else {
            match row {
                None => ComponentState::NotInManifest,
                Some(row) => {
                    if size.is_some_and(|s| s != row.size) {
                        notes.push(format!("on disk {} bytes; the manifest says {}", size.unwrap_or(0), row.size));
                        ComponentState::Mismatch
                    } else {
                        match &sha256 {
                            Some(actual) if actual.eq_ignore_ascii_case(&row.sha256) => ComponentState::Installed,
                            Some(actual) => {
                                notes.push(format!("on disk sha256 {actual}; the manifest says {}", row.sha256));
                                ComponentState::Mismatch
                            }
                            None => ComponentState::InstalledUnverified,
                        }
                    }
                }
            }
        };
        out.push(ComponentReport {
            id: id.id(),
            kind: id.kind(),
            installed: InstalledInfo {
                path: resolution.path,
                candidate: resolution.candidate,
                found: resolution.found,
                size,
                sha256,
                version,
            },
            manifest: row.map(ManifestRowInfo::from),
            state,
            note: (!notes.is_empty()).then(|| notes.join("; ")),
        });
    }
    out
}

/// `<binary> --version`, first non-empty line, or nothing within five seconds.
async fn version_of(path: &Path) -> Option<String> {
    let run = tokio::process::Command::new(path)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), run).await.ok()?.ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

// --- installing ----------------------------------------------------------------------------------

/// Where the bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallSource {
    /// `hf://<repo>/<path>`: through the catalog, which knows the hub endpoint (a mirror stays a
    /// mirror) and the download URL shape.
    Hub {
        repo: String,
        path: String,
    },
    Https(String),
}

/// What an install would do, decided before anything is downloaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallPlan {
    pub source: InstallSource,
    pub destination: PathBuf,
    /// Set the executable bit on unix once the file is verified — binaries only.
    pub executable: bool,
    pub sha256: String,
    pub size: u64,
}

/// `hf://<owner>/<name>/<path>` → (`owner/name`, `path`).
pub fn parse_hf_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("hf://")?;
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let name = parts.next().filter(|s| !s.is_empty())?;
    let path = parts.next().filter(|s| !s.is_empty())?;
    Some((format!("{owner}/{name}"), path.to_string()))
}

/// Plan an install from a manifest row, or refuse by name.
///
/// Refused: a retired id; a row whose kind is not what the Studio spawns the id as; a row for
/// another platform; a row that names a `member` inside an archive (see the module note); a URL
/// scheme the validator would not have passed. A binary lands in `engines/` beside the
/// executable, an artifact in the models directory — the two places [`resolve_component`] looks
/// after the configured path, so what was installed is what is found.
pub fn install_plan(id: &ComponentId, row: &ComponentRow, settings: &Settings) -> std::result::Result<InstallPlan, String> {
    if let Some(note) = id.retired() {
        return Err(format!("{id} is {note}"));
    }
    if row.kind != id.kind() {
        return Err(format!("the manifest calls {id} a {}; this Studio spawns it as a {}", row.kind.as_str(), id.kind().as_str()));
    }
    if !platform_compatible(&row.platform, HOST_PLATFORM) {
        return Err(format!("the manifest row for {id} is built for {}; this Studio is {HOST_PLATFORM}", row.platform));
    }
    if let Some(member) = &row.member {
        return Err(format!(
            "archive members are not extracted yet: {id} is `{member}` inside {}. Download the archive, verify it against \
             archive_sha256 {}, and put the member beside the Studio or in engines/ — or wait for a release that publishes \
             loose binaries.",
            row.url,
            row.archive_sha256.as_deref().unwrap_or("?")
        ));
    }
    let source = if let Some((repo, path)) = parse_hf_url(&row.url) {
        InstallSource::Hub { repo, path }
    } else if row.url.starts_with("https://") {
        InstallSource::Https(row.url.clone())
    } else {
        return Err(format!("{id}: the url {} is neither https:// nor hf://", row.url));
    };
    let destination = if id.is_artifact() {
        settings.models_dir.join(id.file_name())
    } else {
        engines_dir().ok_or_else(|| "this process has no executable directory to install beside".to_string())?.join(id.file_name())
    };
    Ok(InstallPlan { source, destination, executable: !id.is_artifact(), sha256: row.sha256.clone(), size: row.size })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, b"x").expect("write");
    }

    /// The order, one step at a time: each step wins only when every earlier one has nothing.
    #[test]
    fn the_search_order_is_configured_then_beside_then_engines_then_models_dir_then_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let exe_dir = tmp.path().join("bin");
        let models = tmp.path().join("models");
        let path_dir = tmp.path().join("path");
        let path_dirs = vec![path_dir.clone()];
        std::fs::create_dir_all(&exe_dir).expect("mkdir");
        let id = ComponentId::Kaspad;
        let name = id.file_name();

        let none = locate(&id, None, Some(&models), Some(&exe_dir), &path_dirs);
        assert_eq!(none, Resolution { path: PathBuf::from(&name), found: false, candidate: Candidate::NotFound });

        touch(&path_dir.join(&name));
        assert_eq!(locate(&id, None, Some(&models), Some(&exe_dir), &path_dirs).candidate, Candidate::Path);

        touch(&exe_dir.join(ENGINES_DIR).join(&name));
        let engines = locate(&id, None, Some(&models), Some(&exe_dir), &path_dirs);
        assert_eq!(engines.candidate, Candidate::EnginesDir);
        assert_eq!(engines.path, exe_dir.join(ENGINES_DIR).join(&name));

        touch(&exe_dir.join(&name));
        assert_eq!(locate(&id, None, Some(&models), Some(&exe_dir), &path_dirs).candidate, Candidate::BesideExecutable);

        let configured = tmp.path().join("elsewhere").join(&name);
        touch(&configured);
        let chosen = locate(&id, Some(&configured), Some(&models), Some(&exe_dir), &path_dirs);
        assert_eq!(chosen, Resolution { path: configured, found: true, candidate: Candidate::Configured });
    }

    /// A configured path is the person's decision: returned verbatim, and `found: false` says
    /// it is not there rather than quietly falling through to a binary they did not name.
    #[test]
    fn a_configured_path_wins_even_when_it_is_missing_and_says_so() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let exe_dir = tmp.path().join("bin");
        touch(&exe_dir.join(ComponentId::Misaka.file_name()));
        let configured = PathBuf::from("/nonexistent/misaka");
        let r = locate(&ComponentId::Misaka, Some(&configured), None, Some(&exe_dir), &[]);
        assert_eq!(r.path, configured);
        assert!(!r.found);
        assert_eq!(r.candidate, Candidate::Configured);
    }

    /// An artifact is a file the models directory holds and `PATH` never does; a binary is the
    /// reverse. The search is one order, and the kind says which steps apply.
    #[test]
    fn an_artifact_is_looked_for_in_the_models_dir_and_never_on_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let exe_dir = tmp.path().join("bin");
        let models = tmp.path().join("models");
        let path_dir = tmp.path().join("path");
        std::fs::create_dir_all(&exe_dir).expect("mkdir");
        let artifact = ComponentId::Artifact { class: "PALW-QWEN25-A16" };
        assert_eq!(artifact.file_name(), "qwen25-1.5b-a16.palwart");
        assert_eq!(artifact.id(), "qwen25-1.5b-a16");

        touch(&path_dir.join(artifact.file_name()));
        let r = locate(&artifact, None, Some(&models), Some(&exe_dir), std::slice::from_ref(&path_dir));
        assert_eq!(r.candidate, Candidate::NotFound, "PATH is for executables");

        touch(&models.join(artifact.file_name()));
        let r = locate(&artifact, None, Some(&models), Some(&exe_dir), std::slice::from_ref(&path_dir));
        assert_eq!(r.candidate, Candidate::ModelsDir);
        assert_eq!(r.path, models.join("qwen25-1.5b-a16.palwart"));

        touch(&models.join(ComponentId::Kaspad.file_name()));
        let r = locate(&ComponentId::Kaspad, None, Some(&models), Some(&exe_dir), &[path_dir]);
        assert_eq!(r.candidate, Candidate::NotFound, "a binary is never taken from the models directory");
    }

    /// The public entry binds the process's own environment and agrees with the pure core on
    /// the one case that needs no fixture: a configured path.
    #[test]
    fn the_public_entry_binds_the_environment() {
        let configured = PathBuf::from("/opt/llama/llama-server");
        let r = resolve_component(&ComponentId::LlamaServer, Some(&configured), None);
        assert_eq!(r.path, configured);
        assert_eq!(r.candidate, Candidate::Configured);
    }

    fn runtime_sources() -> Vec<(PathBuf, String)> {
        fn walk(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).expect("readdir").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push((path.clone(), std::fs::read_to_string(&path).expect("read")));
                }
            }
        }
        let mut out = Vec::new();
        walk(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
        out.sort();
        out
    }

    /// **ADR-0096 invariant 11: the four resolution spellings are gone.** Every `fn resolve_` in
    /// the runtime is either this module's, a thin wrapper that calls it, or a function that
    /// resolves something that is not a component — and the set is written out here, so a fifth
    /// spelling is a test failure that names the file. The wrappers are held to being thin by
    /// their bodies: they call `resolve_component` and contain none of the old search's
    /// ingredients, which is also checked for the whole tree — `split_paths` (a `PATH` scan) and
    /// `join("engines")` appear only here.
    #[test]
    fn the_search_order_is_spelled_once() {
        let allowed: BTreeMap<&str, Vec<&str>> = BTreeMap::from([
            ("src/components.rs", vec!["resolve_component"]),
            // Wrappers: the same public signatures the callers had.
            ("src/backend/llamacpp.rs", vec!["resolve_program"]),
            // `resolve_tokenizer` finds `tokenizer.json` beside an artifact — a file, not a component.
            ("src/backend/misaka.rs", vec!["resolve_program", "resolve_tokenizer"]),
            ("src/node.rs", vec!["resolve_kaspad", "resolve_misaka_cli"]),
            // Hashes a loaded model to its chain identity — not a search for a file.
            ("src/state.rs", vec!["resolve_identity"]),
        ]);
        let wrappers = ["resolve_program", "resolve_kaspad", "resolve_misaka_cli"];
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (path, text) in runtime_sources() {
            let rel = path.strip_prefix(&root).expect("under the crate").to_string_lossy().replace('\\', "/");
            for (i, line) in text.lines().enumerate() {
                let Some(idx) = line.find("fn resolve_") else { continue };
                // A declaration: nothing before `fn` but indentation and visibility. A doc
                // comment or a string literal that mentions the words is not one.
                let before = line[..idx].trim();
                if !before.split_whitespace().all(|w| w == "pub" || w == "pub(crate)" || w == "async") {
                    continue;
                }
                let after = &line[idx + "fn resolve_".len()..];
                let name = format!("resolve_{}", after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>());
                seen.entry(rel.clone()).or_default().push(name.clone());
                if wrappers.contains(&name.as_str()) {
                    // A thin wrapper is a signature, one call and a brace; six lines hold it and
                    // stop short of whatever follows.
                    let body: String = text.lines().skip(i).take(6).collect::<Vec<_>>().join("\n");
                    assert!(
                        body.contains("resolve_component("),
                        "{rel}: {name} must be a thin wrapper over resolve_component:\n{body}"
                    );
                    for ingredient in ["current_exe", "split_paths", "is_file"] {
                        assert!(!body.contains(ingredient), "{rel}: {name} spells the search itself ({ingredient}):\n{body}");
                    }
                }
            }
            if rel != "src/components.rs" {
                assert!(!text.contains("split_paths"), "{rel}: a PATH scan outside components.rs is a second spelling");
                assert!(!text.contains("join(\"engines\")"), "{rel}: an engines/ lookup outside components.rs is a second spelling");
            }
        }
        let expected: BTreeMap<String, Vec<String>> =
            allowed.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect();
        assert_eq!(
            seen, expected,
            "every `fn resolve_` in the runtime, by file; add to the allowed set on purpose or call resolve_component"
        );
    }

    #[test]
    fn every_component_id_round_trips_through_its_manifest_id() {
        let all = ComponentId::all();
        assert!(all.len() >= BINARIES.len() + 2, "the class table publishes two artifacts");
        for id in &all {
            assert_eq!(ComponentId::parse(&id.id()), Some(*id), "{id}");
            assert!(is_id(&id.id()), "{id} is a valid manifest id");
        }
        assert_eq!(ComponentId::parse("rothschild"), None);
        assert_eq!(ComponentId::Artifact { class: "QWEN36" }.id(), "qwen36");
        assert_eq!(ComponentId::MlxServer.file_name(), "mlx_lm.server");
        assert!(ComponentId::LlamaServer.is_third_party() && ComponentId::MlxServer.is_third_party());
        assert!(SPAWNABLE.iter().all(|id| !id.is_third_party()), "third-party engines are exempt from the check");
    }

    /// The manifest the tests build: one row per spawnable id except the one the caller removes,
    /// with digests that are fixtures (a test is the one place a made-up digest is honest).
    fn fixture(without: &[&str]) -> Value {
        let binary = |id: &str, kind: &str| {
            serde_json::json!({
                "id": id, "kind": kind, "version": "testnet-main-selftest", "platform": "aarch64-apple-darwin",
                "url": format!("https://example.invalid/releases/download/testnet-main-selftest/{id}"),
                "sha256": "00".repeat(32), "size": 1234, "requires": []
            })
        };
        let mut rows = vec![
            binary("kaspad", "node"),
            binary("misaka", "cli"),
            binary("palw-a16-fp-worker", "worker"),
            binary("palw-qwen36-fp-worker", "worker"),
            binary("misaka-palw-gateway", "gateway"),
            binary("misaka-palw-fp-rail", "rail"),
            serde_json::json!({
                "id": "qwen25-1.5b-a16", "kind": "artifact", "version": "relaunch-5f", "platform": "any",
                "url": "hf://Misakachain/Qwen2.5-1.5B-PALW-A16-runtime/palw-runtime/qwen25-1.5b-a16.palwart",
                "sha256": "a8c4e53e5b30dd0d4dc6ef791e0513890a07a2b3a22d045e612536bba1240b1f", "size": 1795427276,
                "requires": ["palw-a16-fp-worker"], "class_id": "42".repeat(64), "artifact_root": "1a".repeat(64)
            }),
        ];
        rows.retain(|r| !without.contains(&r["id"].as_str().unwrap()));
        // A manifest that has no row for a worker does not have an artifact requiring it either
        // — the validator would refuse that (the `requires` rule), which is its own check.
        for row in rows.iter_mut() {
            if let Some(requires) = row["requires"].as_array_mut() {
                requires.retain(|r| !without.contains(&r.as_str().unwrap()));
            }
        }
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        serde_json::json!({ "schema": SCHEMA, "release": "testnet-main-selftest", "network": "testnet-11", "components": rows })
    }

    fn parse(value: &Value) -> Manifest {
        Manifest::parse(&value.to_string()).unwrap_or_else(|e| panic!("valid: {e:?}"))
    }

    /// The node writer's `--self-test` lists fourteen validator rules, one violation each; this
    /// is the same list against this validator, plus the row-level rules the writer's other
    /// checks cover. A file both trees read must be refused by both for the same reason.
    #[test]
    fn the_validator_refuses_what_the_node_writer_refuses() {
        let good = fixture(&[]);
        assert_eq!(validate_value(&good), Vec::<String>::new());
        let broken = |mutate: &dyn Fn(&mut Value)| {
            let mut m = good.clone();
            mutate(&mut m);
            validate_value(&m)
        };
        let set_row = |m: &mut Value, id: &str, key: &str, value: Value| {
            for row in m["components"].as_array_mut().unwrap() {
                if row["id"] == id {
                    row[key] = value.clone();
                }
            }
        };
        let refused = |what: &str, errs: Vec<String>, needle: &str| {
            assert!(errs.iter().any(|e| e.contains(needle)), "{what}: expected a finding containing {needle:?}, got {errs:?}");
        };
        refused("a binary of platform any", broken(&|m| set_row(m, "kaspad", "platform", "any".into())), "target triple");
        refused(
            "an artifact with a triple",
            broken(&|m| set_row(m, "qwen25-1.5b-a16", "platform", "aarch64-apple-darwin".into())),
            "must be `any`",
        );
        refused("a misspelled key", broken(&|m| set_row(m, "kaspad", "sha265", "00".repeat(32).into())), "unknown key(s) sha265");
        refused("a non-hex digest", broken(&|m| set_row(m, "kaspad", "sha256", "zz".repeat(32).into())), "64 lowercase hex");
        refused(
            "requires of an unknown id",
            broken(&|m| set_row(m, "kaspad", "requires", serde_json::json!(["misaka-studiod"]))),
            "not a row of this manifest",
        );
        assert_eq!(
            broken(&|m| {
                set_row(m, "kaspad", "requires", serde_json::json!(["misaka-studiod"]));
                m["node_manifest"] = serde_json::json!({"url": "https://example.invalid/components.json", "sha256": "00".repeat(32)});
            }),
            Vec::<String>::new(),
            "…unless a node_manifest is named"
        );
        refused("an unsorted manifest", broken(&|m| m["components"].as_array_mut().unwrap().reverse()), "not sorted by id");
        refused(
            "a duplicate id",
            broken(&|m| {
                let first = m["components"][0].clone();
                m["components"].as_array_mut().unwrap().push(first);
            }),
            "appears twice",
        );
        refused("another schema", broken(&|m| m["schema"] = "misaka/components/v2".into()), "this checker knows");
        refused(
            "an artifact without class_id",
            broken(&|m| {
                for row in m["components"].as_array_mut().unwrap() {
                    if row["kind"] == "artifact" {
                        row.as_object_mut().unwrap().remove("class_id");
                    }
                }
            }),
            "kind artifact requires class_id",
        );
        refused(
            "member without archive digest",
            broken(&|m| set_row(m, "kaspad", "member", "bin/kaspad".into())),
            "member names an archive, so archive_sha256 is required",
        );
        refused(
            "archive_size without member",
            broken(&|m| set_row(m, "kaspad", "archive_size", 3.into())),
            "archive_size without member",
        );
        refused("an unknown top-level key", broken(&|m| m["extra"] = 1.into()), "unknown top-level key(s) extra");
        refused(
            "a malformed node_manifest",
            broken(&|m| m["node_manifest"] = serde_json::json!({"url": "https://x/", "sha256": "0"})),
            "node_manifest.sha256",
        );
        // Row rules the writer's other paths cover.
        refused("an unknown kind", broken(&|m| set_row(m, "kaspad", "kind", "daemon".into())), "is not one of");
        refused("a negative size", broken(&|m| set_row(m, "kaspad", "size", (-1).into())), "non-negative integer");
        refused("a bool size", broken(&|m| set_row(m, "kaspad", "size", true.into())), "non-negative integer");
        refused("a float size", broken(&|m| set_row(m, "kaspad", "size", 1.5.into())), "non-negative integer");
        refused(
            "an http url",
            broken(&|m| set_row(m, "kaspad", "url", "http://example.invalid/kaspad".into())),
            "url must start with",
        );
        refused(
            "a url naming a directory",
            broken(&|m| set_row(m, "kaspad", "url", "https://example.invalid/".into())),
            "name a file",
        );
        refused(
            "a row requiring itself",
            broken(&|m| set_row(m, "kaspad", "requires", serde_json::json!(["kaspad"]))),
            "requires itself",
        );
        refused("an empty version", broken(&|m| set_row(m, "kaspad", "version", "".into())), "version must be a non-empty");
        refused("an uppercase id", broken(&|m| set_row(m, "kaspad", "id", "Kaspad".into())), "id must match");
        refused(
            "a short class_id",
            broken(&|m| set_row(m, "qwen25-1.5b-a16", "class_id", "42".repeat(10).into())),
            "128 lowercase hex",
        );
        refused("an empty components list", broken(&|m| m["components"] = serde_json::json!([])), "non-empty list");
        refused(
            "a missing top-level key",
            broken(&|m| {
                m.as_object_mut().unwrap().remove("release");
            }),
            "missing top-level key(s) release",
        );
        assert_eq!(validate_value(&serde_json::json!([])), vec!["the manifest must be a JSON object".to_string()]);
        // A typed round trip validates too, and an unknown key is refused at the type as well.
        let typed = parse(&good);
        assert_eq!(typed.validate(), Vec::<String>::new());
        assert_eq!(typed.row("kaspad").map(|r| r.kind), Some(Kind::Node));
        assert!(serde_json::from_value::<Manifest>(broken_value(&good)).is_err(), "deny_unknown_fields holds at the type");
        assert!(Manifest::parse("{ not json").is_err());
    }

    fn broken_value(good: &Value) -> Value {
        let mut m = good.clone();
        m["components"][0]["sha265"] = "00".repeat(32).into();
        m
    }

    /// **ADR-0096 invariant 10, in miniature.** A manifest without a row for a binary the Studio
    /// spawns is a finding that names the binary; a complete one is clean apart from the debt this
    /// tree carries itself (the retired server, below).
    #[test]
    fn a_manifest_missing_a_spawnable_id_is_a_finding_that_names_it() {
        let missing = parse(&fixture(&["palw-a16-fp-worker"]));
        let findings = check_spawnable_ids(&missing);
        let named: Vec<&Finding> = findings.iter().filter(|f| matches!(f, Finding::MissingSpawnable { .. })).collect();
        assert_eq!(named, vec![&Finding::MissingSpawnable { id: "palw-a16-fp-worker".into(), kind: Kind::Worker }]);
        assert!(named[0].is_strict_failure(), "--check --strict exits non-zero on it");
        assert!(named[0].to_string().starts_with("MISSING palw-a16-fp-worker (worker)"), "{}", named[0]);

        let complete = parse(&fixture(&[]));
        assert!(
            check_spawnable_ids(&complete).iter().all(|f| matches!(f, Finding::RetiredStillSpawned { .. })),
            "a complete manifest leaves only this tree's own debt: {:?}",
            check_spawnable_ids(&complete)
        );
        // Third-party engines are never demanded.
        assert!(!check_spawnable_ids(&complete).iter().any(|f| f.to_string().contains("llama-server")));
    }

    /// The retired server is reported as retired while a code path still names it — and the
    /// source is asked the same question a second way: `backend/misaka.rs` must still contain
    /// the string exactly as long as `SPAWNABLE` still lists the id. Remove both together.
    #[test]
    fn the_retired_server_is_named_as_retired_and_the_source_still_names_it() {
        let files_naming_it: Vec<String> = runtime_sources()
            .into_iter()
            .filter(|(path, text)| text.contains("misaka-palw-serve") && !path.ends_with("components.rs"))
            .map(|(path, _)| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        let still_spawned = SPAWNABLE.contains(&ComponentId::MisakaPalwServe);
        assert_eq!(
            files_naming_it.contains(&"misaka.rs".to_string()),
            still_spawned,
            "SPAWNABLE lists misaka-palw-serve iff backend/misaka.rs still names it; files naming it: {files_naming_it:?}"
        );

        let manifest = parse(&fixture(&[]));
        let findings = check_spawnable_ids(&manifest);
        let retired = findings.iter().find(|f| matches!(f, Finding::RetiredStillSpawned { .. })).expect("the retired finding");
        assert!(retired.to_string().contains("retired 2026-09-02 (misakas 2f688bc1)"), "{retired}");
        assert!(!retired.is_strict_failure(), "this tree's own debt does not fail a release gate; it is printed every run");

        // And a manifest that DOES carry a row for it is told so.
        let mut with_row = fixture(&[]);
        with_row["components"].as_array_mut().unwrap().push(serde_json::json!({
            "id": "misaka-palw-serve", "kind": "engine", "version": "old", "platform": "aarch64-apple-darwin",
            "url": "https://example.invalid/misaka-palw-serve", "sha256": "00".repeat(32), "size": 1, "requires": []
        }));
        with_row["components"].as_array_mut().unwrap().sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let findings = check_spawnable_ids(&parse(&with_row));
        assert!(findings.iter().any(|f| matches!(f, Finding::RetiredInManifest { .. })), "{findings:?}");
    }

    /// **The offline copy is pinned to the class table.** `contrib/components/testnet-11.json`
    /// holds the artifact rows of testnet-11 in the manifest's own shape, copied from the node
    /// doc's example (which copied them from this class table); this test holds the two to the
    /// same digest, size, URL, class id and root, so neither can drift alone.
    ///
    /// The file does not validate today, and that is pinned too: each artifact row `requires`
    /// its worker, no node release has published a worker row yet, and no node manifest exists
    /// to name — so the validator's `requires` rule refuses it, which the node doc calls "the
    /// cross-repository check in miniature". The day the node ships those rows, the fixture
    /// gains them (or a `node_manifest` reference with a real digest) and this expectation flips
    /// to "validates clean".
    #[test]
    fn the_offline_copy_pins_the_class_table() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contrib/components/testnet-11.json");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let value: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(value["schema"], SCHEMA);
        assert_eq!(value["network"], "testnet-11");
        let rows = value["components"].as_array().expect("rows");

        let published: Vec<&PalwClassSpec> =
            TESTNET11_CLASSES.iter().filter(|c| matches!(c.artifact, PalwArtifactSource::Download { .. })).collect();
        assert_eq!(rows.len(), published.len(), "one row per published artifact, and nothing else");
        for class in published {
            let PalwArtifactSource::Download { filename, repo_path, sha256, size_bytes, hf_repo, convert_command } = class.artifact
            else {
                unreachable!()
            };
            let id = ComponentId::Artifact { class: class.name };
            let row = rows.iter().find(|r| r["id"] == id.id()).unwrap_or_else(|| panic!("{}: no row {}", class.name, id.id()));
            assert_eq!(row["kind"], "artifact", "{}", class.name);
            assert_eq!(row["platform"], "any", "{}", class.name);
            assert_eq!(row["sha256"], sha256, "{}: the pin the ADR asks for", class.name);
            assert_eq!(row["size"], size_bytes, "{}", class.name);
            assert_eq!(row["url"], format!("hf://{hf_repo}/{repo_path}"), "{}", class.name);
            assert_eq!(row["class_id"], class.class_id_hex, "{}", class.name);
            assert_eq!(row["artifact_root"], class.artifact_root_hex, "{}", class.name);
            assert_eq!(row["convert_command"], convert_command, "{}", class.name);
            assert!(row["notes"].as_str().is_some_and(|n| n.contains("OFFLINE COPY")), "{}: marked as the offline copy", class.name);
            assert!(filename.starts_with(row["id"].as_str().unwrap()), "{}: the id is the file's stem", class.name);
        }

        let findings = validate_value(&value);
        let expected: Vec<String> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                format!(
                    "components[{i}] ({}): requires \"{}\", which is not a row of this manifest (and no node_manifest is named)",
                    r["id"].as_str().unwrap(),
                    r["requires"][0].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(findings, expected, "the only refusal is the worker rows the node has not published yet");
    }

    #[test]
    fn platform_compatibility_is_the_arch_and_the_os() {
        assert!(platform_compatible("any", "aarch64-apple-darwin"));
        assert!(platform_compatible("aarch64-apple-darwin", "aarch64-apple-darwin"));
        assert!(
            platform_compatible("x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"),
            "a static musl binary runs on a gnu host"
        );
        assert!(!platform_compatible("x86_64-apple-darwin", "aarch64-apple-darwin"));
        assert!(!platform_compatible("aarch64-apple-darwin", "aarch64-unknown-linux-gnu"));
        assert!(!platform_compatible("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"));
        assert!(!HOST_PLATFORM.is_empty() && HOST_PLATFORM != "unknown", "build.rs sets the triple: {HOST_PLATFORM}");
    }

    #[test]
    fn hf_urls_resolve_to_the_repository_and_the_path() {
        assert_eq!(
            parse_hf_url("hf://Misakachain/Qwen2.5-1.5B-PALW-A16-runtime/palw-runtime/qwen25-1.5b-a16.palwart"),
            Some(("Misakachain/Qwen2.5-1.5B-PALW-A16-runtime".into(), "palw-runtime/qwen25-1.5b-a16.palwart".into()))
        );
        assert_eq!(parse_hf_url("hf://owner/repo"), None, "a repository is not a file");
        assert_eq!(parse_hf_url("https://x/y/z"), None);
    }

    fn row(id: &str, kind: Kind, url: &str) -> ComponentRow {
        ComponentRow {
            id: id.into(),
            kind,
            version: "v".into(),
            platform: if kind == Kind::Artifact { "any".into() } else { HOST_PLATFORM.into() },
            url: url.into(),
            sha256: "00".repeat(32),
            size: 7,
            requires: Vec::new(),
            member: None,
            archive_sha256: None,
            archive_size: None,
            class_id: None,
            artifact_root: None,
            tokenizer_commitment: None,
            model_id: None,
            convert_command: None,
            notes: None,
        }
    }

    /// The install refusals, each by name; and the two destinations, which are the two places the
    /// search looks after the configured path.
    #[test]
    fn install_plans_refuse_by_name_and_land_where_the_search_looks() {
        let settings = Settings { models_dir: PathBuf::from("/models"), ..Default::default() };
        let artifact = ComponentId::Artifact { class: "PALW-QWEN25-A16" };
        let plan = install_plan(&artifact, &row("qwen25-1.5b-a16", Kind::Artifact, "hf://o/r/p/qwen25-1.5b-a16.palwart"), &settings)
            .expect("plans");
        assert_eq!(plan.destination, PathBuf::from("/models/qwen25-1.5b-a16.palwart"));
        assert_eq!(plan.source, InstallSource::Hub { repo: "o/r".into(), path: "p/qwen25-1.5b-a16.palwart".into() });
        assert!(!plan.executable);

        let plan = install_plan(&ComponentId::Kaspad, &row("kaspad", Kind::Node, "https://example.invalid/kaspad"), &settings)
            .expect("plans");
        assert_eq!(plan.destination, engines_dir().expect("exe dir").join(ComponentId::Kaspad.file_name()));
        assert!(plan.executable);
        assert_eq!(plan.source, InstallSource::Https("https://example.invalid/kaspad".into()));

        let mut archived = row("kaspad", Kind::Node, "https://example.invalid/rusty-kaspa-osx.zip");
        archived.member = Some("bin/kaspad".into());
        archived.archive_sha256 = Some("11".repeat(32));
        let err = install_plan(&ComponentId::Kaspad, &archived, &settings).unwrap_err();
        assert!(err.starts_with("archive members are not extracted yet: kaspad is `bin/kaspad` inside"), "{err}");

        let err = install_plan(&ComponentId::MisakaPalwServe, &row("misaka-palw-serve", Kind::Engine, "https://x/y"), &settings)
            .unwrap_err();
        assert!(err.contains("retired 2026-09-02"), "{err}");

        let err = install_plan(&ComponentId::Kaspad, &row("kaspad", Kind::Cli, "https://x/y"), &settings).unwrap_err();
        assert!(err.contains("calls kaspad a cli"), "{err}");

        let mut foreign = row("kaspad", Kind::Node, "https://x/y");
        foreign.platform = "riscv64gc-unknown-none-elf".into();
        let err = install_plan(&ComponentId::Kaspad, &foreign, &settings).unwrap_err();
        assert!(err.contains("built for riscv64gc-unknown-none-elf"), "{err}");
    }

    /// The states, over a file the test controls: the configured artifact path is the one step
    /// of the search a test can drive without touching the executable's directory.
    #[tokio::test]
    async fn states_follow_presence_then_size_then_digest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("qwen25-1.5b-a16.palwart");
        std::fs::write(&file, b"PALW\0\0\0\x01").expect("write");
        let mut settings = Settings { models_dir: tmp.path().join("models"), ..Default::default() };
        settings.node.class_artifact = Some(file.clone());
        let a16 = |reports: &[ComponentReport]| reports.iter().find(|r| r.id == "qwen25-1.5b-a16").cloned().expect("row");

        let none = report(&settings, None, false).await;
        let r = a16(&none);
        assert_eq!(r.state, ComponentState::NotInManifest);
        assert_eq!(r.installed.candidate, Candidate::Configured);
        assert_eq!(r.installed.size, Some(8));
        assert_eq!(none.iter().find(|r| r.id == "misaka-palw-serve").map(|r| r.state), Some(ComponentState::Retired));

        let mut manifest = parse(&fixture(&[]));
        let row = manifest.components.iter_mut().find(|r| r.id == "qwen25-1.5b-a16").expect("row");
        row.size = 8;
        row.sha256 = crate::download::sha256_file(&file).await.expect("hash");
        let r = a16(&report(&settings, Some(&manifest), false).await);
        assert_eq!(r.state, ComponentState::InstalledUnverified, "size matches; the digest was not asked for");
        assert!(r.installed.sha256.is_none());
        let r = a16(&report(&settings, Some(&manifest), true).await);
        assert_eq!(r.state, ComponentState::Installed);
        assert_eq!(r.installed.sha256.as_deref(), Some(manifest.row("qwen25-1.5b-a16").unwrap().sha256.as_str()));

        manifest.components.iter_mut().find(|r| r.id == "qwen25-1.5b-a16").unwrap().sha256 = "ff".repeat(32);
        let r = a16(&report(&settings, Some(&manifest), true).await);
        assert_eq!(r.state, ComponentState::Mismatch);
        assert!(r.note.as_deref().is_some_and(|n| n.contains("on disk sha256")), "{:?}", r.note);

        manifest.components.iter_mut().find(|r| r.id == "qwen25-1.5b-a16").unwrap().size = 9;
        let r = a16(&report(&settings, Some(&manifest), false).await);
        assert_eq!(r.state, ComponentState::Mismatch, "the cheap check before the expensive one");

        std::fs::remove_file(&file).expect("rm");
        let r = a16(&report(&settings, Some(&manifest), false).await);
        assert_eq!(r.state, ComponentState::Missing);
    }

    #[tokio::test]
    async fn a_manifest_loads_from_a_file_and_refuses_plaintext_http() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("components.json");
        std::fs::write(&path, fixture(&[]).to_string()).expect("write");
        let catalog = Catalog::new("https://hub.invalid", None);
        let manifest = load_manifest(&path.display().to_string(), &catalog).await.expect("loads");
        assert_eq!(manifest.release, "testnet-main-selftest");

        let err = load_manifest("http://example.invalid/components.json", &catalog).await.unwrap_err();
        assert!(matches!(err, ManifestError::Unreadable { .. }) && err.to_string().contains("http:// is refused"), "{err}");

        std::fs::write(&path, fixture(&["kaspad"]).to_string().replace("\"release\"", "\"relaese\"")).expect("write");
        let err = load_manifest(&path.display().to_string(), &catalog).await.unwrap_err();
        assert!(matches!(&err, ManifestError::Invalid { findings, .. } if findings.iter().any(|f| f.contains("relaese"))), "{err}");
    }
}
