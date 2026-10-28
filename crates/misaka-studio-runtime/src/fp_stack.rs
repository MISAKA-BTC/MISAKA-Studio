//! **Prompt mining with no node here** — the reason MISAKA Studio exists.
//!
//! A prompt is mined when one inference both answers it and commits the work (ADR-0044), and that commitment reaches the
//! chain as a signed carrier. Before RFC-0009 that needed a node on this machine. Now it needs two processes that talk to
//! the network's public wRPC instead, and the Studio runs both:
//!
//! ```text
//! Chat / prompt-mining ──▶ misaka-palw-gateway ──▶ palw-a16-fp-worker   (ONE run: the answer and the roots)
//!                              │   reads the chain: --rpc <public wRPC>      holds no key (ADR-0079 D4)
//!                              │   publishes each job's evidence: --evidence-provider <public provider>
//!                              ▼
//!                          <outbox>  ──▶ misaka-palw-fp-rail --watch   (holds the bond key; signs; relays through the public wRPC)
//! ```
//!
//! What the Studio does is what it does for the node-free attempt miner: build each command line as data so the user can
//! read it, check what can be checked before anything runs, start and stop the processes, and keep their output. What it
//! does not do: decide to sign on a public node's word. The rail's signing gate refuses `UNVERIFIED_REMOTE` unless the
//! user said so, by name, and the Studio passes that consent only when the user gave it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use misaka_studio_core::palw::default_class;
use misaka_studio_core::settings::{NodeNetwork, NodeSettings};
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;

use crate::remote_miner::resolve_binary;
use crate::{Error, Result};

/// The public evidence provider of testnet-12 (`misaka_endpoints::testnet12_evidence_provider_v1`, misakas main `366763a64`).
pub const TESTNET12_EVIDENCE_PROVIDER: &str = "http://169.58.232.113:8088";

const LOG_CAPACITY: usize = 400;

/// The published evidence provider for a network.
pub fn public_evidence_provider(network: NodeNetwork) -> Option<&'static str> {
    match network {
        NodeNetwork::Testnet12 => Some(TESTNET12_EVIDENCE_PROVIDER),
        _ => None,
    }
}

/// Where the chat and the mining queue reach the gateway: `node.palw_gateway_url` when set, else the gateway this Studio
/// runs itself, at its listen address.
pub fn gateway_url(settings: &NodeSettings) -> String {
    settings
        .palw_gateway_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| format!("http://{}", settings.free_prompt.listen))
}

/// Every path the stack runs on, resolved once from the settings.
#[derive(Clone, Debug, Serialize)]
pub struct StackPaths {
    pub gateway: PathBuf,
    pub rail: PathBuf,
    pub worker: PathBuf,
    pub artifact: Option<PathBuf>,
    pub tokenizer: Option<PathBuf>,
    pub key: Option<PathBuf>,
    pub outbox: PathBuf,
    pub identity: PathBuf,
    pub rpc: Option<String>,
    pub evidence_provider: Option<String>,
}

/// Resolve the stack's paths. `default_artifact` is the default class's file in the models directory, when it is there.
pub fn paths(settings: &NodeSettings, data_dir: &Path, default_artifact: Option<PathBuf>) -> StackPaths {
    let f = &settings.free_prompt;
    let artifact = f.artifact.clone().or(default_artifact);
    let tokenizer = f.tokenizer.clone().or_else(|| {
        let name = default_class().tokenizer_filename()?;
        artifact.as_ref().and_then(|a| a.parent()).map(|dir| dir.join(name))
    });
    StackPaths {
        gateway: resolve_binary(f.gateway_path.as_ref(), "misaka-palw-gateway"),
        rail: resolve_binary(f.rail_path.as_ref(), "misaka-palw-fp-rail"),
        worker: resolve_binary(f.worker_path.as_ref(), "palw-a16-fp-worker"),
        artifact,
        tokenizer,
        key: settings.producer_key_path.clone(),
        outbox: data_dir.join("fp-outbox").join(settings.network.id()),
        identity: data_dir.join("fp-identity").join(settings.network.id()).join("identity.json"),
        rpc: crate::node::borsh_endpoint(settings),
        evidence_provider: f
            .evidence_provider
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| public_evidence_provider(settings.network).map(str::to_string)),
    }
}

fn need<'a, T>(value: &'a Option<T>, why: &str) -> Result<&'a T> {
    value.as_ref().ok_or_else(|| Error::bad_request(why.to_string()))
}

/// `misaka-palw-fp-rail --print-identity`: the executor identity the gateway is accountable to (class, bond, operator),
/// read from the chain through the public node. The key is read by the rail, never by the Studio.
pub fn identity_command(settings: &NodeSettings, p: &StackPaths) -> Result<Vec<String>> {
    let key = need(&p.key, "a bond key is required — mint one here or set node.producer_key_path (`misaka key gen`)")?;
    let rpc = need(&p.rpc, "this network publishes no public wRPC — set node.misaka_rpc")?;
    let mut args = vec![
        "--print-identity".into(),
        "--bond-key-seed".into(),
        key.display().to_string(),
        "--rpc".into(),
        rpc.clone(),
        "--class-id".into(),
        default_class().class_id_hex.into(),
    ];
    if let Some(bond) = settings.free_prompt.bond.as_deref().filter(|b| !b.trim().is_empty()) {
        args.extend(["--bond".into(), bond.trim().into()]);
    }
    Ok(args)
}

/// The gateway's command line and the environment its worker reads.
pub type GatewayCommand = (Vec<String>, Vec<(String, String)>);

pub fn gateway_command(settings: &NodeSettings, p: &StackPaths) -> Result<GatewayCommand> {
    let artifact = need(&p.artifact, "the class artifact is missing — download qwen25-1.5b-a16-8k.palwart (Models → Discover)")?;
    let tokenizer = need(&p.tokenizer, "the class tokenizer is missing — it downloads beside the artifact")?;
    let rpc = need(&p.rpc, "this network publishes no public wRPC — set node.misaka_rpc")?;
    let provider = need(&p.evidence_provider, "this network publishes no evidence provider — set node.free_prompt.evidence_provider")?;
    let args = vec![
        "--listen".into(),
        settings.free_prompt.listen.clone(),
        "--worker".into(),
        p.worker.display().to_string(),
        "--outbox".into(),
        p.outbox.display().to_string(),
        "--identity".into(),
        p.identity.display().to_string(),
        "--rpc".into(),
        rpc.clone(),
        "--evidence-provider".into(),
        provider.clone(),
        "--evidence-min-copies".into(),
        "1".into(),
    ];
    let env = vec![
        ("MISAKA_PALW_NETWORK_ID".into(), settings.network.id().into()),
        ("MISAKA_PALW_ARTIFACT".into(), artifact.display().to_string()),
        ("MISAKA_PALW_TOKENIZER".into(), tokenizer.display().to_string()),
    ];
    Ok((args, env))
}

/// The watcher: signs every committed job in the outbox with the bond key and relays it through the public node.
pub fn rail_command(settings: &NodeSettings, p: &StackPaths) -> Result<Vec<String>> {
    let key = need(&p.key, "a bond key is required — mint one here or set node.producer_key_path (`misaka key gen`)")?;
    let rpc = need(&p.rpc, "this network publishes no public wRPC — set node.misaka_rpc")?;
    let mut args = vec![
        "--watch".into(),
        p.outbox.display().to_string(),
        "--bond-key-seed".into(),
        key.display().to_string(),
        "--rpc".into(),
        rpc.clone(),
    ];
    if let Some(bond) = settings.free_prompt.bond.as_deref().filter(|b| !b.trim().is_empty()) {
        args.extend(["--bond".into(), bond.trim().into()]);
    }
    if settings.free_prompt.accept_unverified {
        args.extend(["--accept-unverified-state".into(), "UNVERIFIED_REMOTE".into()]);
    }
    Ok(args)
}

/// What can be checked before the stack runs.
#[derive(Clone, Debug, Serialize)]
pub struct StackPreflight {
    pub paths: StackPaths,
    pub binaries: Vec<(String, bool)>,
    pub artifact_present: bool,
    pub tokenizer_present: bool,
    pub key_present: bool,
    pub public_node: Option<crate::node::NodeStatus>,
    /// The evidence provider answered over HTTP (any status). Unreachable means a Panel cannot read the material.
    pub evidence_provider_reachable: bool,
    pub problems: Vec<String>,
}

pub async fn preflight(settings: &NodeSettings, data_dir: &Path, default_artifact: Option<PathBuf>) -> StackPreflight {
    let p = paths(settings, data_dir, default_artifact);
    let mut problems = Vec::new();
    let binaries: Vec<(String, bool)> =
        [&p.gateway, &p.rail, &p.worker].iter().map(|b| (b.display().to_string(), b.is_file())).collect();
    for (bin, found) in &binaries {
        if !found {
            problems.push(format!(
                "{bin} not found. Build in misakas main: cargo build --release -p misaka-palw-gateway --bin misaka-palw-gateway \
                 --bin misaka-palw-fp-rail -p misaka-palw-base0 --bin palw-a16-fp-worker"
            ));
        }
    }
    let artifact_present = p.artifact.as_ref().is_some_and(|a| a.is_file());
    let tokenizer_present = p.tokenizer.as_ref().is_some_and(|t| t.is_file());
    let key_present = p.key.as_ref().is_some_and(|k| k.is_file());
    if !artifact_present {
        problems.push(format!("the {} artifact is not in the models directory (Models → Discover)", default_class().name));
    }
    if !tokenizer_present {
        problems.push("the class tokenizer is not beside the artifact".into());
    }
    if !key_present {
        problems.push("no bond key file — mint one here or set node.producer_key_path".into());
    }
    if !settings.free_prompt.accept_unverified {
        problems.push(
            "no consent to sign on a public node's word: the rail's signing gate refuses UNVERIFIED_REMOTE until you accept it".into(),
        );
    }
    let public_node = match crate::node::public_wrpc(settings.network).first() {
        Some(n) => Some(crate::node::query_status(n.wrpc_json).await),
        None => None,
    };
    if !public_node.as_ref().is_some_and(|s| s.reachable) {
        problems.push("the public wRPC node did not answer".into());
    }
    let evidence_provider_reachable = match &p.evidence_provider {
        Some(url) => reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(8))
            .build()
            .expect("http client builds")
            .get(url)
            .send()
            .await
            .is_ok(),
        None => false,
    };
    if !evidence_provider_reachable {
        problems.push(format!(
            "the evidence provider {} did not answer: a Panel could not read the material of a claim, and the claim would not \
             become Final",
            p.evidence_provider.as_deref().unwrap_or("(none)")
        ));
    }
    StackPreflight {
        paths: p,
        binaries,
        artifact_present,
        tokenizer_present,
        key_present,
        public_node,
        evidence_provider_reachable,
        problems,
    }
}

struct Child {
    child: tokio::process::Child,
    command_line: Vec<String>,
}

#[derive(Default)]
struct Logs {
    gateway: VecDeque<String>,
    rail: VecDeque<String>,
    /// The rail's JSON summaries (`misaka.palw.fp-rail-tx.v1`), newest last: one per carrier it built.
    carriers: VecDeque<serde_json::Value>,
}

/// One process's state.
#[derive(Clone, Debug, Serialize)]
pub struct ProcessView {
    pub running: bool,
    pub exited: Option<String>,
    pub command_line: Option<Vec<String>>,
}

/// What `GET /network/prompt-stack` returns.
#[derive(Clone, Debug, Serialize)]
pub struct StackView {
    pub gateway: ProcessView,
    pub rail: ProcessView,
    /// The commands the current settings would run, or why there are none.
    pub planned: std::result::Result<Vec<Vec<String>>, String>,
    pub carriers: Vec<serde_json::Value>,
    pub gateway_log: Vec<String>,
    pub rail_log: Vec<String>,
    pub identity: String,
    pub outbox: String,
    pub label: &'static str,
}

/// The gateway and the rail this Studio runs.
#[derive(Default)]
pub struct FreePromptStack {
    gateway: RwLock<Option<Child>>,
    rail: RwLock<Option<Child>>,
    logs: Arc<Mutex<Logs>>,
}

fn command_line(bin: &Path, args: &[String]) -> Vec<String> {
    std::iter::once(bin.display().to_string()).chain(args.iter().cloned()).collect()
}

impl FreePromptStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Write the identity, then start the gateway and the rail. Refuses while either runs.
    pub async fn start(&self, settings: &NodeSettings, data_dir: &Path, default_artifact: Option<PathBuf>) -> Result<StackView> {
        if self.view_process(&self.gateway).await.running || self.view_process(&self.rail).await.running {
            return Err(Error::bad_request("the prompt-mining stack is already running; stop it first"));
        }
        let p = paths(settings, data_dir, default_artifact.clone());
        let identity_args = identity_command(settings, &p)?;
        let (gateway_args, env) = gateway_command(settings, &p)?;
        let rail_args = rail_command(settings, &p)?;
        if !settings.free_prompt.accept_unverified {
            return Err(Error::bad_request(
                "accept signing on a public node's word first (UNVERIFIED_REMOTE): without it the rail signs nothing",
            ));
        }
        for dir in [&p.outbox, p.identity.parent().unwrap_or(data_dir)] {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir.display(), e))?;
        }
        // The identity names the class, bond and operator the gateway is accountable to; read fresh at every start so a new
        // bond or key cannot leave a stale one behind.
        let out = tokio::process::Command::new(&p.rail)
            .args(&identity_args)
            .output()
            .await
            .map_err(|e| Error::Node { message: format!("could not run {}: {e}", p.rail.display()) })?;
        if !out.status.success() {
            return Err(Error::Node {
                message: format!("the rail could not read the identity: {}", String::from_utf8_lossy(&out.stderr).trim()),
            });
        }
        std::fs::write(&p.identity, &out.stdout).map_err(|e| Error::io(p.identity.display(), e))?;
        *self.logs.lock().expect("log lock") = Logs::default();

        let mut gateway = tokio::process::Command::new(&p.gateway);
        gateway.args(&gateway_args).envs(env).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut gateway =
            gateway.spawn().map_err(|e| Error::Node { message: format!("could not start {}: {e}", p.gateway.display()) })?;
        self.drain(&mut gateway, Which::Gateway);
        *self.gateway.write().await = Some(Child { child: gateway, command_line: command_line(&p.gateway, &gateway_args) });

        let mut rail = tokio::process::Command::new(&p.rail);
        rail.args(&rail_args).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut rail = rail.spawn().map_err(|e| Error::Node { message: format!("could not start {}: {e}", p.rail.display()) })?;
        self.drain(&mut rail, Which::Rail);
        *self.rail.write().await = Some(Child { child: rail, command_line: command_line(&p.rail, &rail_args) });
        Ok(self.view(settings, data_dir, default_artifact).await)
    }

    /// Stop both. The outbox stays: a job committed but not yet carried is carried by the next start.
    pub async fn stop(&self) -> Result<()> {
        for slot in [&self.rail, &self.gateway] {
            if let Some(mut c) = slot.write().await.take() {
                #[cfg(unix)]
                if let Some(pid) = c.child.id() {
                    let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
                    for _ in 0..30 {
                        if matches!(c.child.try_wait(), Ok(Some(_))) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
                let _ = c.child.kill().await;
                let _ = c.child.wait().await;
            }
        }
        Ok(())
    }

    async fn view_process(&self, slot: &RwLock<Option<Child>>) -> ProcessView {
        let mut guard = slot.write().await;
        match guard.as_mut() {
            Some(c) => match c.child.try_wait() {
                Ok(None) => ProcessView { running: true, exited: None, command_line: Some(c.command_line.clone()) },
                Ok(Some(status)) => {
                    ProcessView { running: false, exited: Some(status.to_string()), command_line: Some(c.command_line.clone()) }
                }
                Err(e) => ProcessView { running: false, exited: Some(e.to_string()), command_line: Some(c.command_line.clone()) },
            },
            None => ProcessView { running: false, exited: None, command_line: None },
        }
    }

    pub async fn view(&self, settings: &NodeSettings, data_dir: &Path, default_artifact: Option<PathBuf>) -> StackView {
        let p = paths(settings, data_dir, default_artifact);
        let planned = (|| -> Result<Vec<Vec<String>>> {
            let (g, _) = gateway_command(settings, &p)?;
            Ok(vec![command_line(&p.gateway, &g), command_line(&p.rail, &rail_command(settings, &p)?)])
        })()
        .map_err(|e| e.to_string());
        let (gateway_log, rail_log, carriers) = {
            let l = self.logs.lock().expect("log lock");
            let tail =
                |q: &VecDeque<String>| q.iter().rev().take(80).cloned().collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>();
            (tail(&l.gateway), tail(&l.rail), l.carriers.iter().cloned().collect())
        };
        StackView {
            gateway: self.view_process(&self.gateway).await,
            rail: self.view_process(&self.rail).await,
            planned,
            carriers,
            gateway_log,
            rail_log,
            identity: p.identity.display().to_string(),
            outbox: p.outbox.display().to_string(),
            label: "UNVERIFIED_REMOTE_STATE",
        }
    }

    fn drain(&self, child: &mut tokio::process::Child, which: Which) {
        if let Some(out) = child.stdout.take() {
            tokio::spawn(drain(out, self.logs.clone(), which));
        }
        if let Some(err) = child.stderr.take() {
            tokio::spawn(drain(err, self.logs.clone(), which));
        }
    }
}

#[derive(Clone, Copy)]
enum Which {
    Gateway,
    Rail,
}

async fn drain<R: tokio::io::AsyncRead + Unpin>(stream: R, logs: Arc<Mutex<Logs>>, which: Which) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut l = logs.lock().expect("log lock");
        if let Which::Rail = which
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
            && v.get("schema").and_then(|s| s.as_str()) == Some("misaka.palw.fp-rail-tx.v1")
        {
            if l.carriers.len() == 100 {
                l.carriers.pop_front();
            }
            l.carriers.push_back(v);
        }
        let q = match which {
            Which::Gateway => &mut l.gateway,
            Which::Rail => &mut l.rail,
        };
        if q.len() == LOG_CAPACITY {
            q.pop_front();
        }
        q.push_back(line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> NodeSettings {
        let mut s =
            NodeSettings { network: NodeNetwork::Testnet12, producer_key_path: Some("/keys/bond.seed".into()), ..Default::default() };
        s.free_prompt.accept_unverified = true;
        s
    }

    fn p(s: &NodeSettings) -> StackPaths {
        paths(s, Path::new("/data"), Some("/models/qwen25-1.5b-a16-8k.palwart".into()))
    }

    #[test]
    fn the_gateway_reads_the_public_node_publishes_to_the_public_provider_and_runs_the_8k_worker() {
        let s = ready();
        let (args, env) = gateway_command(&s, &p(&s)).unwrap();
        let line = args.join(" ");
        assert!(line.contains("--listen 127.0.0.1:8790"));
        assert!(line.contains("--rpc wss://wallet.misakascan.com/t12/wrpc/borsh"), "{line}");
        assert!(line.contains("--evidence-provider http://169.58.232.113:8088 --evidence-min-copies 1"));
        assert!(line.contains("--outbox /data/fp-outbox/testnet-12"));
        assert!(line.contains("--identity /data/fp-identity/testnet-12/identity.json"));
        assert!(line.contains("palw-a16-fp-worker"));
        assert!(env.contains(&("MISAKA_PALW_NETWORK_ID".into(), "testnet-12".into())));
        assert!(env.contains(&("MISAKA_PALW_ARTIFACT".into(), "/models/qwen25-1.5b-a16-8k.palwart".into())));
        assert!(env.contains(&("MISAKA_PALW_TOKENIZER".into(), "/models/qwen25-1.5b-a16-8k.tokenizer.json".into())));
    }

    #[test]
    fn the_rail_watches_the_outbox_signs_with_the_bond_key_and_names_the_users_consent() {
        let mut s = ready();
        s.free_prompt.bond = Some("abc:0".into());
        let line = rail_command(&s, &p(&s)).unwrap().join(" ");
        assert!(line.starts_with("--watch /data/fp-outbox/testnet-12 --bond-key-seed /keys/bond.seed --rpc wss://"), "{line}");
        assert!(line.contains("--bond abc:0"));
        assert!(line.contains("--accept-unverified-state UNVERIFIED_REMOTE"));
        s.free_prompt.accept_unverified = false;
        assert!(!rail_command(&s, &p(&s)).unwrap().join(" ").contains("--accept-unverified-state"));
    }

    #[test]
    fn the_identity_is_read_for_the_default_class_through_the_public_node() {
        let s = ready();
        let line = identity_command(&s, &p(&s)).unwrap().join(" ");
        assert!(line.contains("--print-identity --bond-key-seed /keys/bond.seed --rpc wss://"));
        assert!(line.contains(&format!("--class-id {}", default_class().class_id_hex)));
    }

    #[test]
    fn the_key_and_the_artifact_are_required_with_their_remedy() {
        let mut s = ready();
        s.producer_key_path = None;
        assert!(rail_command(&s, &p(&s)).unwrap_err().to_string().contains("key gen"));
        let s = ready();
        let none = paths(&s, Path::new("/data"), None);
        assert!(gateway_command(&s, &none).unwrap_err().to_string().contains("Models → Discover"));
    }

    #[test]
    fn the_seed_is_never_inside_the_directories_the_gateway_scans_for_secrets() {
        // The gateway refuses to boot when a signing secret is reachable from --identity's directory or --outbox. The
        // Studio's own key lives at <data>/producer.seed, outside both.
        let s = ready();
        let paths = p(&s);
        let seed = Path::new("/data/producer.seed");
        assert!(!seed.starts_with(paths.identity.parent().unwrap()));
        assert!(!seed.starts_with(&paths.outbox));
    }
}
