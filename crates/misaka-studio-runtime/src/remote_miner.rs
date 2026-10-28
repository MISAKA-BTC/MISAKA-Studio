//! **Mining with no node here** (RFC-0009 stage A): the Studio supervises `palw-remote-miner`.
//!
//! The miner reads the chain through at least two public wRPC nodes (testnet-12 publishes two, `misaka_endpoints::
//! testnet12_public_wrpc_v1` in misakas), runs the official executor `palw-attempt-exec` on this machine, signs only a won
//! draw and submits the finished block to every node. **The nodes validate the block themselves; nothing here is an
//! authority.** The Studio's part is the same as for a supervised node: assemble the command line as data so the user can
//! read it, check what can be checked before anything runs, start and stop the process, and fold its JSON event lines
//! into a picture.
//!
//! What the Studio does NOT decide: the checkpoint (the trust root the nodes are held to — the Studio only suggests one it
//! read from two nodes, the user confirms it) and the consent to mine on a view that is not fully verified (named by its
//! label, or the miner stops at its signing gate). On testnet-12 the fork-choice fence is dormant, so the best view a
//! node-free miner can reach is `HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED`.
//!
//! The capture of every published attempt is kept under the state dir, and **this machine must stay up and serve it** until
//! the claim is `Final` (or until an evidence provider holds it) — the Panel reads it from there.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use misaka_studio_core::palw::{GENESIS_CLASSES, PalwArtifactSource};
use misaka_studio_core::settings::{NodeNetwork, NodeSettings};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;

use crate::node::wrpc_call;
use crate::{Error, Result};

/// One public wRPC node: Borsh for the miner, JSON for the Studio's own reads (the same node).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PublicWrpc {
    pub name: &'static str,
    pub wrpc_borsh: &'static str,
    pub wrpc_json: &'static str,
}

/// The public testnet-12 wRPC list — a copy of misakas `misaka_endpoints::testnet12_public_wrpc_v1` (main `366763a64`,
/// `docs/testnet12-node-free.md`). Two hosts, each fronting its own node.
pub const TESTNET12_PUBLIC_WRPC: &[PublicWrpc] = &[
    PublicWrpc {
        name: "misakascan",
        wrpc_borsh: "wss://wallet.misakascan.com/t12/wrpc/borsh",
        wrpc_json: "wss://wallet.misakascan.com/t12/wrpc/json",
    },
    PublicWrpc {
        name: "llm.misakascan",
        wrpc_borsh: "wss://llm.misakascan.com/t12/wrpc/borsh",
        wrpc_json: "wss://llm.misakascan.com/t12/wrpc/json",
    },
];

/// The labels the miner's `--accept-unverified-state` takes. Anything else is refused here, before the miner refuses it.
pub const UNVERIFIED_LABELS: &[&str] = &["HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED", "UNVERIFIED_REMOTE"];

const LOG_CAPACITY: usize = 600;
const EVENT_CAPACITY: usize = 200;

/// The published list for a network; empty where none is published.
pub fn public_wrpc(network: NodeNetwork) -> &'static [PublicWrpc] {
    match network {
        NodeNetwork::Testnet12 => TESTNET12_PUBLIC_WRPC,
        _ => &[],
    }
}

/// The Borsh endpoints the miner is given, and the JSON endpoints the Studio reads the same nodes through.
///
/// A configured list is used as typed (Borsh); the Studio then has no JSON twin for it and checks only what the miner will
/// check itself. The published list carries both.
fn endpoints(settings: &NodeSettings) -> (Vec<String>, Vec<String>) {
    if !settings.remote.rpc.is_empty() {
        return (settings.remote.rpc.clone(), Vec::new());
    }
    let list = public_wrpc(settings.network);
    (list.iter().map(|n| n.wrpc_borsh.to_string()).collect(), list.iter().map(|n| n.wrpc_json.to_string()).collect())
}

/// Find a binary: the configured path, beside the Studio's executable, or PATH (as the node and CLI are found).
pub fn resolve_binary(configured: Option<&PathBuf>, stem: &str) -> PathBuf {
    let name = if cfg!(windows) { format!("{stem}.exe") } else { stem.to_string() };
    if let Some(path) = configured {
        return path.clone();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let beside = dir.join(&name);
        if beside.is_file() {
            return beside;
        }
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).map(|dir| dir.join(&name)).find(|c| c.is_file()))
        .unwrap_or(None)
        .unwrap_or_else(|| PathBuf::from(name))
}

/// The class the miner draws in: the configured id, else the floor.
fn class_of(settings: &NodeSettings) -> Result<&'static misaka_studio_core::palw::PalwClassSpec> {
    let id = settings.remote.class.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let found = match id {
        None => GENESIS_CLASSES.iter().find(|c| c.is_base),
        Some(id) => GENESIS_CLASSES.iter().find(|c| c.class_id_complete && c.class_id_hex.eq_ignore_ascii_case(id)),
    };
    found.ok_or_else(|| {
        Error::bad_request(
            "remote.class is not a genesis class of this network. A class registered later needs its artifact root, which this \
             build does not know; mine it with a node, or name a genesis class",
        )
    })
}

/// Where the miner keeps its journal and captures.
pub fn state_dir(settings: &NodeSettings, data_dir: &Path) -> PathBuf {
    settings.remote.state_dir.clone().unwrap_or_else(|| data_dir.join("remote-miner").join(settings.network.id()))
}

fn checkpoint_of(text: &str) -> Option<(u64, String)> {
    let (daa, hash) = text.trim().split_once(':')?;
    let daa: u64 = daa.trim().parse().ok()?;
    let hash = hash.trim().to_ascii_lowercase();
    // A block hash on this chain is a Hash64: 128 hex.
    (hash.len() == 128).then_some(())?;
    hash.chars().all(|c| c.is_ascii_hexdigit()).then_some((daa, hash))
}

/// **The command line**: the miner, then its arguments. Built as data so the UI can show it verbatim and the user can run
/// it without the Studio. Refuses with the remedy when a value the miner needs is missing.
pub fn build_command(settings: &NodeSettings, data_dir: &Path) -> Result<(PathBuf, Vec<String>)> {
    let r = &settings.remote;
    let (rpc, _) = endpoints(settings);
    if rpc.len() < 2 {
        return Err(Error::bad_request(
            "node-free mining needs at least two independent wRPC nodes: this network publishes none — set remote.rpc",
        ));
    }
    let bond = r.bond.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| {
        Error::bad_request("remote.bond is required — register one without a node (`misaka bond register-export / sign / submit`)")
    })?;
    let pay = r
        .pay_address
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::bad_request("remote.pay_address is required: the miner pays only where it is told"))?;
    let key = r
        .key_path
        .as_ref()
        .or(settings.producer_key_path.as_ref())
        .ok_or_else(|| Error::bad_request("a key is required — remote.key_path, or generate one with `misaka key gen`"))?;
    let checkpoint = r.checkpoint.as_deref().ok_or_else(|| {
        Error::bad_request("remote.checkpoint is required (<daa>:<block hash>): the trust root the nodes are held to")
    })?;
    let (daa, hash) =
        checkpoint_of(checkpoint).ok_or_else(|| Error::bad_request("remote.checkpoint is not <daa>:<block hash in hex>"))?;
    if let Some(label) = &r.accept_unverified
        && !UNVERIFIED_LABELS.contains(&label.as_str())
    {
        return Err(Error::bad_request(format!("remote.accept_unverified must be one of {UNVERIFIED_LABELS:?}")));
    }
    let class = class_of(settings)?;
    if class.artifact_root_hex.is_empty() {
        return Err(Error::bad_request(format!("{} has no artifact root in this build", class.name)));
    }
    let state = state_dir(settings, data_dir);
    let exec = resolve_binary(r.attempt_exec_path.as_ref(), "palw-attempt-exec");

    let mut args: Vec<String> = vec!["--network".into(), settings.network.id().into(), "--rpc".into(), rpc.join(",")];
    args.extend(["--bond".into(), bond.into(), "--class".into(), class.class_id_hex.into()]);
    args.extend(["--artifact-root".into(), class.artifact_root_hex.into(), "--pay-address".into(), pay.into()]);
    args.extend(["--key-file".into(), key.display().to_string(), "--checkpoint".into(), format!("{daa}:{hash}")]);
    args.extend(["--executor-cmd".into(), exec.display().to_string()]);
    let mut exec_arg = |a: String| {
        args.push("--executor-arg".into());
        args.push(a);
    };
    exec_arg("--network".into());
    exec_arg(settings.network.id().into());
    if !class.is_base {
        let artifact = r.artifact.as_ref().ok_or_else(|| {
            let file = match &class.artifact {
                PalwArtifactSource::Download { filename, .. } | PalwArtifactSource::ConvertLocally { filename, .. } => filename,
                PalwArtifactSource::DerivedFromSeed => "",
            };
            Error::bad_request(format!("{} needs its artifact: set remote.artifact to {file}", class.name))
        })?;
        exec_arg("--class".into());
        exec_arg(class.class_id_hex.into());
        exec_arg("--artifact-root".into());
        exec_arg(class.artifact_root_hex.into());
        exec_arg("--artifact".into());
        exec_arg(artifact.display().to_string());
    }
    exec_arg("--material-out".into());
    exec_arg(state.join("capture.bin").display().to_string());
    args.extend(["--state-dir".into(), state.display().to_string()]);
    if r.verify_headers {
        args.extend(["--verify-headers".into(), "--checkpoint-trust".into(), "pinned".into()]);
    }
    if let Some(label) = &r.accept_unverified {
        args.extend(["--accept-unverified-state".into(), label.clone()]);
    }
    if let Some(poll) = r.poll_secs {
        args.extend(["--poll-secs".into(), poll.to_string()]);
    }
    Ok((resolve_binary(r.remote_miner_path.as_ref(), "palw-remote-miner"), args))
}

/// One node's answer to the Studio's read.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NodeRead {
    pub endpoint: String,
    pub reachable: bool,
    pub network: Option<String>,
    pub virtual_daa_score: Option<u64>,
    pub sink: Option<String>,
    pub error: Option<String>,
}

/// What can be checked before the miner runs. Every line is a local observation of remote nodes, not a proof.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Preflight {
    pub nodes: Vec<NodeRead>,
    /// At least two nodes answered, all on this network, within the miner's DAA skew.
    pub quorum: bool,
    pub remote_miner: String,
    pub remote_miner_found: bool,
    pub attempt_exec: String,
    pub attempt_exec_found: bool,
    /// Why the command could not be built, if it could not.
    pub command_error: Option<String>,
    pub problems: Vec<String>,
    pub label: &'static str,
}

/// The miner's own DAA skew bound (`QuorumPolicy::max_daa_skew`, 12 on testnet-12).
const MAX_DAA_SKEW: u64 = 12;

async fn read_node(url: &str) -> NodeRead {
    let mut read = NodeRead { endpoint: url.to_string(), ..Default::default() };
    match wrpc_call(url, "getBlockDagInfo", json!({}), Duration::from_secs(8)).await {
        Ok(dag) => {
            read.reachable = true;
            read.network = dag.get("network").and_then(Value::as_str).map(str::to_string);
            read.virtual_daa_score = dag.get("virtualDaaScore").and_then(Value::as_u64);
            read.sink = dag.get("sink").and_then(Value::as_str).map(str::to_string);
        }
        Err(e) => read.error = Some(e.to_string()),
    }
    read
}

pub async fn preflight(settings: &NodeSettings, data_dir: &Path) -> Preflight {
    let (_, json_urls) = endpoints(settings);
    let mut nodes = Vec::new();
    for url in &json_urls {
        nodes.push(read_node(url).await);
    }
    let mut problems = Vec::new();
    let answered: Vec<&NodeRead> = nodes.iter().filter(|n| n.reachable).collect();
    let on_network = answered.iter().all(|n| n.network.as_deref() == Some(settings.network.id()));
    let daas: Vec<u64> = answered.iter().filter_map(|n| n.virtual_daa_score).collect();
    let skew = daas.iter().max().zip(daas.iter().min()).map(|(a, b)| a - b).unwrap_or(0);
    let quorum = answered.len() >= 2 && on_network && skew <= MAX_DAA_SKEW;
    if json_urls.is_empty() {
        problems.push(
            "the configured remote.rpc has no JSON twin; the Studio cannot read those nodes — the miner checks them itself".into(),
        );
    } else if answered.len() < 2 {
        problems.push(format!("{} of {} nodes answered; the miner needs two", answered.len(), json_urls.len()));
    }
    if !on_network {
        problems.push(format!("a node is not on {}", settings.network.id()));
    }
    if skew > MAX_DAA_SKEW {
        problems.push(format!("the nodes are {skew} DAA apart (the miner stops past {MAX_DAA_SKEW})"));
    }
    let miner = resolve_binary(settings.remote.remote_miner_path.as_ref(), "palw-remote-miner");
    let exec = resolve_binary(settings.remote.attempt_exec_path.as_ref(), "palw-attempt-exec");
    for (path, what) in [(&miner, "palw-remote-miner"), (&exec, "palw-attempt-exec")] {
        if !path.is_file() {
            problems.push(format!(
                "{what} not found ({}). Build it in misakas main: cargo build --release -p kaspad --bin palw-attempt-exec \
                 -p misaka-palw-remote --features rpc --bin palw-remote-miner",
                path.display()
            ));
        }
    }
    let command_error = build_command(settings, data_dir).err().map(|e| e.to_string());
    if settings.remote.accept_unverified.is_none() {
        problems.push(
            "no unverified-state consent: on this network the fork-choice fence is dormant, so the miner stops at its signing \
             gate until remote.accept_unverified names HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED"
                .into(),
        );
    }
    Preflight {
        nodes,
        quorum,
        remote_miner_found: miner.is_file(),
        remote_miner: miner.display().to_string(),
        attempt_exec_found: exec.is_file(),
        attempt_exec: exec.display().to_string(),
        command_error,
        problems,
        label: "UNVERIFIED_REMOTE_STATE",
    }
}

/// A checkpoint the Studio read: one node's sink, which the other node also holds. The user confirms it.
#[derive(Clone, Debug, Serialize)]
pub struct CheckpointSuggestion {
    pub checkpoint: String,
    pub daa_score: u64,
    pub block_hash: String,
    pub read_from: String,
    pub confirmed_by: Vec<String>,
    pub note: &'static str,
}

fn header_daa(block: &Value) -> Option<u64> {
    block.get("block").unwrap_or(block).get("header").and_then(|h| h.get("daaScore")).and_then(Value::as_u64)
}

pub async fn suggest_checkpoint(settings: &NodeSettings) -> Result<CheckpointSuggestion> {
    let (_, json_urls) = endpoints(settings);
    if json_urls.len() < 2 {
        return Err(Error::bad_request("a checkpoint is suggested from the published nodes only; type yours into remote.checkpoint"));
    }
    let first = read_node(&json_urls[0]).await;
    let sink = first.sink.clone().ok_or_else(|| Error::Node { message: format!("{}: no sink", json_urls[0]) })?;
    let timeout = Duration::from_secs(8);
    let block = wrpc_call(&json_urls[0], "getBlock", json!({ "hash": sink, "includeTransactions": false }), timeout).await?;
    let daa = header_daa(&block).ok_or_else(|| Error::Node { message: "the sink's header carries no daaScore".into() })?;
    let mut confirmed_by = Vec::new();
    for url in &json_urls[1..] {
        if let Ok(other) = wrpc_call(url, "getBlock", json!({ "hash": sink, "includeTransactions": false }), timeout).await
            && header_daa(&other) == Some(daa)
        {
            confirmed_by.push(url.clone());
        }
    }
    if confirmed_by.is_empty() {
        return Err(Error::Node { message: "the other node does not hold that block yet; ask again in a moment".into() });
    }
    Ok(CheckpointSuggestion {
        checkpoint: format!("{daa}:{sink}"),
        daa_score: daa,
        block_hash: sink,
        read_from: json_urls[0].clone(),
        confirmed_by,
        note: "read from the nodes you are about to trust: compare it with a source you trust (your own node, the explorer) \
               before using it",
    })
}

/// What the miner has said, folded from its JSON event lines.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MinerFacts {
    /// The latest `mode` label (`VERIFIED_REMOTE`, `HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED`, `UNVERIFIED_REMOTE`).
    pub mode: Option<String>,
    pub verification: Option<String>,
    pub draws_lost: u64,
    pub published: u64,
    pub halts: u64,
    pub last_halt: Option<String>,
    pub last_fatal: Option<String>,
    /// Published block hashes, newest last.
    pub blocks: Vec<String>,
    /// The latest JSON events, newest last.
    pub events: VecDeque<Value>,
}

impl MinerFacts {
    fn observe(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("[palw-remote-miner] fatal: ") {
            self.last_fatal = Some(rest.to_string());
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else { return };
        let Some(event) = value.get("event").and_then(Value::as_str) else { return };
        let detail = value.get("detail").cloned().unwrap_or(Value::Null);
        match event {
            "mode" => {
                if let Some(mode) = detail.get("mode").and_then(Value::as_str) {
                    self.mode = Some(mode.to_string());
                }
                if let Some(v) = detail.get("verification").and_then(Value::as_str) {
                    self.verification = Some(v.to_string());
                }
            }
            "draw-lost" => self.draws_lost += 1,
            "published" => {
                self.published += 1;
                if let Some(block) = detail.get("block").and_then(Value::as_str) {
                    self.blocks.push(block.to_string());
                }
            }
            "halt" => {
                self.halts += 1;
                self.last_halt = detail.get("reason").and_then(Value::as_str).map(str::to_string);
            }
            _ => {}
        }
        // `mode` is printed every step; keeping it would push every other event out of the window.
        if event != "mode" && event != "draw-lost" {
            if self.events.len() == EVENT_CAPACITY {
                self.events.pop_front();
            }
            self.events.push_back(value);
        }
    }
}

#[derive(Default)]
struct MinerLogState {
    log: VecDeque<String>,
    facts: MinerFacts,
}

struct Supervised {
    child: tokio::process::Child,
    command_line: Vec<String>,
}

/// What `GET /network/remote-miner` returns.
#[derive(Clone, Debug, Serialize)]
pub struct RemoteMinerView {
    pub running: bool,
    /// The process ended on its own; its last fatal line is in `facts.last_fatal`.
    pub exited: Option<String>,
    pub command_line: Option<Vec<String>>,
    /// The command the current settings would run, or why there is none.
    pub planned: std::result::Result<Vec<String>, String>,
    pub state_dir: String,
    pub facts: MinerFacts,
    pub public_wrpc: &'static [PublicWrpc],
    /// Every remote fact is the nodes' word until a proof opens it.
    pub label: &'static str,
}

/// The node-free miner this Studio runs. One at a time: the miner's equivocation journal is per state dir, and two miners on
/// one bond would sign two attempts at one position.
#[derive(Default)]
pub struct RemoteMinerManager {
    supervised: RwLock<Option<Supervised>>,
    logs: Arc<Mutex<MinerLogState>>,
}

impl RemoteMinerManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn start(&self, settings: &NodeSettings, data_dir: &Path) -> Result<RemoteMinerView> {
        {
            let mut guard = self.supervised.write().await;
            if let Some(running) = guard.as_mut() {
                match running.child.try_wait() {
                    Ok(None) => return Err(Error::bad_request("the node-free miner is already running; stop it first")),
                    _ => *guard = None,
                }
            }
        }
        let (binary, args) = build_command(settings, data_dir)?;
        let state = state_dir(settings, data_dir);
        std::fs::create_dir_all(&state).map_err(|e| Error::io(state.display(), e))?;
        *self.logs.lock().expect("log lock") = MinerLogState::default();
        tracing::info!(binary = %binary.display(), ?args, "starting the node-free miner");
        let mut child = tokio::process::Command::new(&binary)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Node {
                message: format!(
                    "could not start {}: {e}. Build it in misakas main: cargo build --release -p misaka-palw-remote --features rpc \
                     --bin palw-remote-miner, or set remote.remote_miner_path.",
                    binary.display()
                ),
            })?;
        if let Some(out) = child.stdout.take() {
            tokio::spawn(drain(out, self.logs.clone()));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(drain(err, self.logs.clone()));
        }
        let command_line = std::iter::once(binary.display().to_string()).chain(args).collect();
        *self.supervised.write().await = Some(Supervised { child, command_line });
        Ok(self.view(settings, data_dir).await)
    }

    /// Stop the miner. The captures stay in the state dir: the Panel still reads them from this machine.
    pub async fn stop(&self) -> Result<()> {
        if let Some(mut running) = self.supervised.write().await.take() {
            #[cfg(unix)]
            if let Some(pid) = running.child.id() {
                let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
                for _ in 0..30 {
                    if matches!(running.child.try_wait(), Ok(Some(_))) {
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            let _ = running.child.kill().await;
            let _ = running.child.wait().await;
        }
        Ok(())
    }

    pub async fn view(&self, settings: &NodeSettings, data_dir: &Path) -> RemoteMinerView {
        let (running, exited, command_line) = {
            let mut guard = self.supervised.write().await;
            match guard.as_mut() {
                Some(s) => match s.child.try_wait() {
                    Ok(None) => (true, None, Some(s.command_line.clone())),
                    Ok(Some(status)) => (false, Some(status.to_string()), Some(s.command_line.clone())),
                    Err(e) => (false, Some(e.to_string()), Some(s.command_line.clone())),
                },
                None => (false, None, None),
            }
        };
        let planned = build_command(settings, data_dir)
            .map(|(bin, args)| std::iter::once(bin.display().to_string()).chain(args).collect())
            .map_err(|e| e.to_string());
        RemoteMinerView {
            running,
            exited,
            command_line,
            planned,
            state_dir: state_dir(settings, data_dir).display().to_string(),
            facts: self.logs.lock().expect("log lock").facts.clone(),
            public_wrpc: public_wrpc(settings.network),
            label: "UNVERIFIED_REMOTE_STATE",
        }
    }

    pub fn recent_log(&self, limit: usize) -> Vec<String> {
        let logs = self.logs.lock().expect("log lock");
        logs.log.iter().rev().take(limit).cloned().collect::<Vec<_>>().into_iter().rev().collect()
    }
}

async fn drain<R: tokio::io::AsyncRead + Unpin>(stream: R, logs: Arc<Mutex<MinerLogState>>) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut state = logs.lock().expect("log lock");
        if state.log.len() == LOG_CAPACITY {
            state.log.pop_front();
        }
        state.log.push_back(line.clone());
        state.facts.observe(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use misaka_studio_core::settings::RemoteMinerSettings;

    fn hash() -> String {
        format!("b59479ad1f8f100a{}", "0".repeat(112))
    }

    fn ready() -> NodeSettings {
        NodeSettings {
            network: NodeNetwork::Testnet12,
            producer_key_path: Some("/keys/miner.seed".into()),
            remote: RemoteMinerSettings {
                bond: Some("abc:0".into()),
                pay_address: Some("misakatest:qqq".into()),
                checkpoint: Some(format!("15538:{}", hash())),
                accept_unverified: Some("HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn the_floor_command_uses_both_public_nodes_the_official_executor_and_the_users_checkpoint() {
        let (bin, args) = build_command(&ready(), Path::new("/data")).expect("builds");
        assert!(bin.ends_with("palw-remote-miner"));
        let line = args.join(" ");
        assert!(line.contains("--network testnet-12"), "{line}");
        assert!(line.contains("--rpc wss://wallet.misakascan.com/t12/wrpc/borsh,wss://llm.misakascan.com/t12/wrpc/borsh"), "{line}");
        let floor = GENESIS_CLASSES.iter().find(|c| c.is_base).unwrap();
        assert!(line.contains(&format!("--class {}", floor.class_id_hex)));
        assert!(line.contains(&format!("--artifact-root {}", floor.artifact_root_hex)));
        assert!(line.contains("--key-file /keys/miner.seed"));
        assert!(line.contains(&format!("--checkpoint 15538:{}", hash())));
        assert!(line.contains("palw-attempt-exec"));
        assert!(line.contains("--executor-arg --material-out --executor-arg /data/remote-miner/testnet-12/capture.bin"), "{line}");
        assert!(!line.contains("--executor-arg --artifact "), "the floor needs no artifact: {line}");
        assert!(line.contains("--state-dir /data/remote-miner/testnet-12"));
        assert!(line.contains("--verify-headers --checkpoint-trust pinned"));
        assert!(line.contains("--accept-unverified-state HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED"));
    }

    #[test]
    fn the_8k_row_passes_its_artifact_to_the_executor_and_refuses_without_one() {
        let mut s = ready();
        let a16 = GENESIS_CLASSES.iter().find(|c| c.name == "PALW-QWEN25-A16-8K").unwrap();
        s.remote.class = Some(a16.class_id_hex.to_uppercase());
        let err = build_command(&s, Path::new("/data")).unwrap_err().to_string();
        assert!(err.contains("qwen25-1.5b-a16-8k.palwart"), "{err}");
        s.remote.artifact = Some("/m/qwen25-1.5b-a16-8k.palwart".into());
        let line = build_command(&s, Path::new("/data")).unwrap().1.join(" ");
        assert!(line.contains(&format!("--artifact-root {}", a16.artifact_root_hex)));
        assert!(line.contains("--executor-arg --artifact --executor-arg /m/qwen25-1.5b-a16-8k.palwart"), "{line}");
    }

    #[test]
    fn every_missing_value_is_refused_with_its_remedy() {
        for (mutate, needle) in [
            (Box::new(|s: &mut NodeSettings| s.remote.bond = None) as Box<dyn Fn(&mut NodeSettings)>, "register-export"),
            (Box::new(|s: &mut NodeSettings| s.remote.pay_address = None), "pay_address"),
            (Box::new(|s: &mut NodeSettings| s.producer_key_path = None), "key gen"),
            (Box::new(|s: &mut NodeSettings| s.remote.checkpoint = None), "trust root"),
            (Box::new(|s: &mut NodeSettings| s.remote.checkpoint = Some("tip".into())), "<daa>:<block hash"),
            (Box::new(|s: &mut NodeSettings| s.remote.accept_unverified = Some("yes".into())), "must be one of"),
            (Box::new(|s: &mut NodeSettings| s.remote.class = Some("00".repeat(64))), "not a genesis class"),
            (Box::new(|s: &mut NodeSettings| s.network = NodeNetwork::Devnet), "at least two"),
        ] {
            let mut s = ready();
            mutate(&mut s);
            let err = build_command(&s, Path::new("/data")).unwrap_err().to_string();
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }

    #[test]
    fn without_consent_the_command_carries_no_unverified_flag() {
        let mut s = ready();
        s.remote.accept_unverified = None;
        let line = build_command(&s, Path::new("/data")).unwrap().1.join(" ");
        assert!(!line.contains("--accept-unverified-state"));
    }

    #[test]
    fn the_event_lines_fold_into_facts() {
        let mut f = MinerFacts::default();
        f.observe(r#"{"event":"mode","detail":{"verification":"L1 header chain + L3 proofs from --checkpoint"}}"#);
        f.observe(r#"{"event":"mode","detail":{"mode":"HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED","claim":"x","l2":null}}"#);
        f.observe(r#"{"event":"draw-lost","detail":{"bucket":3}}"#);
        f.observe(r#"{"event":"published","detail":{"block":"aa","attempt_id":"bb","accepted_by":2}}"#);
        f.observe(r#"{"event":"halt","detail":{"reason":"stale template"}}"#);
        f.observe("[palw-remote-miner] fatal: --bond is required");
        f.observe("not json");
        assert_eq!(f.mode.as_deref(), Some("HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED"));
        assert!(f.verification.as_deref().unwrap().starts_with("L1"));
        assert_eq!((f.draws_lost, f.published, f.halts), (1, 1, 1));
        assert_eq!(f.blocks, vec!["aa".to_string()]);
        assert_eq!(f.last_halt.as_deref(), Some("stale template"));
        assert_eq!(f.last_fatal.as_deref(), Some("--bond is required"));
        assert_eq!(f.events.len(), 2, "mode and draw-lost are counted, not kept");
    }
}

/// Against the live public nodes; run with `cargo test -p misaka-studio-runtime -- --ignored live_`.
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore = "reaches the public testnet-12 nodes"]
    async fn live_preflight_and_checkpoint_read_the_public_testnet_12_nodes() {
        let settings = NodeSettings { network: NodeNetwork::Testnet12, ..Default::default() };
        let p = preflight(&settings, Path::new("/tmp")).await;
        eprintln!("{}", serde_json::to_string_pretty(&p).unwrap());
        assert!(p.quorum, "{:?}", p.nodes);
        let c = suggest_checkpoint(&settings).await.expect("a checkpoint");
        eprintln!("{}", serde_json::to_string_pretty(&c).unwrap());
        assert_eq!(c.block_hash.len(), 128);
    }
}
