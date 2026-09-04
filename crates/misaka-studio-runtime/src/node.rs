//! The MISAKA node, watched or supervised — how the Studio joins the network.
//!
//! Participation on this chain is a ladder, and every rung is the same binary (`kaspad`, the
//! misakas node) run with more at stake:
//!
//! * **Observer** — read someone's node over RPC. Nothing to run.
//! * **Verifier** — run a full node. Syncing IS verifying on this chain: every block's PALW claim
//!   is re-derived by the nodes that accept it, so an unbonded node is already doing the work the
//!   network pays bonded panels for.
//! * **Producer** — the same node with `--palw-produce`: a bonded key, a pay address, and (for a
//!   model class) the class artifact. There is no external miner on this network — the thing
//!   that runs the model is the thing that makes the block — so "mining software" means
//!   supervising `kaspad` well.
//!
//! # The RPC dialect, exactly
//!
//! kaspad's JSON RPC is a **bare websocket** (any path, no handshake) speaking the
//! `workflow-rpc` JSON envelope — which is *not* JSON-RPC 2.0, and the differences are load
//! bearing:
//!
//! * request: `{"id":1,"method":"getInfo","params":{}}` — `id` must be a **number** and present,
//!   `params` must be present (`{}` when empty), method names are lowerCamelCase;
//! * reply: the result arrives in **`params`** (there is no `result` field), errors as
//!   `{"error":{"code":0,"message":…}}`;
//! * a malformed envelope or unknown method string **drops the socket** without a reply, so this
//!   client never interpolates method names and always round-trips ids;
//! * server-pushed notifications are the same envelope with no `id` — responses are matched by
//!   `id` presence first.
//!
//! The endpoint only exists when the node was started with `--rpclisten-json`; the supervisor
//! below always passes it, and the attach path says exactly that when a connection is refused.
//!
//! # Why one-shot connections
//!
//! Each status poll opens a fresh websocket, performs its calls, and closes. A held connection
//! with a demux map would be faster per call — and would need reconnect logic, id routing, and a
//! liveness story for a node that restarts (which, on a chain where operators are told to restart
//! with new flags to change role, is normal). Polling a loopback socket once a second costs
//! microseconds; the complexity is the thing worth not paying.

use crate::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use misaka_studio_core::settings::{NetworkRole, NodeNetwork, NodeSettings};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;

/// Default wRPC-JSON ports, from the node's `network.rs` (upstream Kaspa's + 10000).
pub fn default_json_rpc_port(network: NodeNetwork) -> u16 {
    match network {
        NodeNetwork::Testnet11 => 28210,
        NodeNetwork::Devnet => 28610,
        NodeNetwork::Simnet => 28510,
    }
}

/// P2P entry nodes for testnet-11, from the join runbook — used only as `--addpeer` fallbacks
/// when DNS is unavailable, which is exactly the situation the runbook names them for.
pub const TESTNET11_FALLBACK_PEERS: &[&str] = &["169.58.232.113:26311", "169.58.232.114:26311", "169.58.39.220:26311"];

/// Turn what a user types into a websocket URL. Accepts `host:port`, `ws://…`, or a bare host.
pub fn normalize_rpc_url(input: &str, network: NodeNetwork) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return format!("ws://127.0.0.1:{}", default_json_rpc_port(network));
    }
    if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        return trimmed.to_string();
    }
    if trimmed.contains(':') {
        return format!("ws://{trimmed}");
    }
    format!("ws://{trimmed}:{}", default_json_rpc_port(network))
}

/// One call against a node's JSON endpoint.
///
/// Waits for the frame whose `id` matches ours; notification frames (no `id`) that arrive in
/// between are skipped, because a node with an active subscription interleaves them freely.
pub async fn wrpc_call(url: &str, method: &str, params: Value, timeout: Duration) -> Result<Value> {
    let attempt = async {
        let (mut socket, _) =
            tokio_tungstenite::connect_async(url).await.map_err(|e| Error::Node { message: format!("{url}: {e}") })?;

        // A fixed id per connection is enough: the connection carries exactly one request.
        let id = 1u64;
        let frame = json!({ "id": id, "method": method, "params": params }).to_string();
        socket.send(Message::Text(frame)).await.map_err(|e| Error::Node { message: format!("{url}: send: {e}") })?;

        while let Some(message) = socket.next().await {
            let message = message.map_err(|e| Error::Node { message: format!("{url}: {e}") })?;
            let Message::Text(text) = message else { continue };
            let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
            // No `id` = a notification, not our reply.
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error")
                && !error.is_null()
            {
                let message = error.get("message").and_then(Value::as_str).unwrap_or("unknown node error");
                return Err(Error::Node { message: format!("{method}: {message}") });
            }
            let _ = socket.close(None).await;
            return Ok(value.get("params").cloned().unwrap_or(Value::Null));
        }
        Err(Error::Node {
            message: format!("{url}: the connection closed before a reply — a malformed frame or an unknown method drops the socket"),
        })
    };

    tokio::time::timeout(timeout, attempt).await.map_err(|_| Error::Node { message: format!("{url}: no reply within {timeout:?}") })?
}

/// What the Studio shows about a node, assembled from `getInfo` + `getBlockDagInfo` +
/// `getConnectedPeerInfo`.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NodeStatus {
    pub reachable: bool,
    pub rpc_url: String,
    /// `supervised` when this Studio launched the process, `attached` otherwise.
    pub source: String,
    pub server_version: Option<String>,
    pub network: Option<String>,
    pub is_synced: Option<bool>,
    pub virtual_daa_score: Option<u64>,
    pub block_count: Option<u64>,
    pub header_count: Option<u64>,
    pub difficulty: Option<f64>,
    pub peer_count: Option<usize>,
    pub mempool_size: Option<u64>,
    pub sink: Option<String>,
    /// Why the node is unreachable, when it is.
    pub error: Option<String>,
}

/// Read a node's status. Partial answers are kept: a node that answers `getInfo` but not the DAG
/// call is still reachable, and half a picture beats an error page.
pub async fn query_status(url: &str) -> NodeStatus {
    let timeout = Duration::from_secs(4);
    let mut status = NodeStatus { rpc_url: url.to_string(), ..Default::default() };

    match wrpc_call(url, "getInfo", json!({}), timeout).await {
        Ok(info) => {
            status.reachable = true;
            status.server_version = info.get("serverVersion").and_then(Value::as_str).map(str::to_string);
            status.is_synced = info.get("isSynced").and_then(Value::as_bool);
            status.mempool_size = info.get("mempoolSize").and_then(Value::as_u64);
        }
        Err(e) => {
            status.error = Some(format!(
                "{e}. The node's JSON RPC only exists when it was started with --rpclisten-json; a supervised node gets the flag automatically."
            ));
            return status;
        }
    }

    if let Ok(dag) = wrpc_call(url, "getBlockDagInfo", json!({}), timeout).await {
        status.network = dag.get("network").and_then(Value::as_str).map(str::to_string);
        status.virtual_daa_score = dag.get("virtualDaaScore").and_then(Value::as_u64);
        status.block_count = dag.get("blockCount").and_then(Value::as_u64);
        status.header_count = dag.get("headerCount").and_then(Value::as_u64);
        status.difficulty = dag.get("difficulty").and_then(Value::as_f64);
        status.sink = dag.get("sink").and_then(Value::as_str).map(str::to_string);
    }
    if let Ok(peers) = wrpc_call(url, "getConnectedPeerInfo", json!({}), timeout).await {
        status.peer_count = peers.get("peerInfo").and_then(Value::as_array).map(Vec::len);
    }
    status
}

/// One row of the node's own class table, parsed from its `[palw-dump]` log line —
/// `class=<id> base=<bool> status=<status> share=<permille|NONE> budget=<blocks>`.
///
/// Log-scraped because the node exposes no class-enumeration RPC (its `palw_dump.rs` says as
/// much); the dump flag exists for exactly this consumer. Only a supervised node's table is
/// visible — for an attached node the Studio shows the built-in registry snapshot instead.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeClassRow {
    pub class_id: String,
    pub base: bool,
    pub status: String,
    pub share_permille: Option<u16>,
    pub budget_blocks: Option<u64>,
}

pub(crate) fn parse_class_row(line: &str) -> Option<NodeClassRow> {
    if !line.contains("[palw-dump]") || !line.contains("class=") {
        return None;
    }
    let field = |key: &str| line.split_whitespace().find_map(|word| word.strip_prefix(key)).map(str::to_string);
    Some(NodeClassRow {
        class_id: field("class=")?,
        base: field("base=").is_some_and(|v| v == "true"),
        status: field("status=").unwrap_or_default(),
        share_permille: field("share=").and_then(|v| v.parse().ok()),
        budget_blocks: field("budget=").and_then(|v| v.parse().ok()),
    })
}

/// `[palw] producer pay address <addr> (derived from --palw-producer-key; …)` → the address.
pub(crate) fn parse_pay_address(line: &str) -> Option<String> {
    let rest = line.split("[palw] producer pay address ").nth(1)?;
    let address = rest.split_whitespace().next()?;
    (address.contains(':') && address.len() > 20).then(|| address.to_string())
}

/// `[palw-panel] registered bond <txid>:<index> …` → `<txid>:<index>`.
pub(crate) fn parse_registered_bond(line: &str) -> Option<String> {
    let rest = line.split("[palw-panel] registered bond ").nth(1)?;
    let outpoint = rest.split_whitespace().next()?;
    let (txid, index) = outpoint.split_once(':')?;
    (txid.len() >= 64 && txid.chars().all(|c| c.is_ascii_hexdigit()) && index.chars().all(|c| c.is_ascii_digit()))
        .then(|| outpoint.to_string())
}

/// A line worth surfacing in the activity feed: production, panel work, holds, and the identity
/// lines an operator is told to check.
pub(crate) fn is_activity_line(line: &str) -> bool {
    [
        "[palw-producer]",
        "[palw-panel]",
        "[palw-dump]",
        "Consensus params fingerprint",
        "[palw] producer pay address",
        "Genesis mismatch",
        "Genesis not found",
        "accepted block",
        "Accepted block",
    ]
    .iter()
    .any(|needle| line.contains(needle))
}

/// The node's own words for "this data directory holds a different chain".
///
/// Matched on the sentence rather than an exit code, because the exit code is the same `0` the
/// node uses for every declined prompt — and the sentence is what tells the user which prompt it
/// was. Both halves are matched: the question alone appears in the runbooks, and the refusal alone
/// is printed for any declined question.
/// Read the producer's own lines. `produced block #N` is the only line that proves a block was
/// made, so it is the only line that flips this to `Producing`; `holding: <reason>` is what the
/// node says instead, and it carries the reason the operator needs.
/// Read the producer's own draw counter — the newest line wins, because it is cumulative.
///
/// The line reads `[palw-producer] 12 draws this run, 0 produced, 0 won the class ticket …;
/// class ticket p = 3.159e-4 per draw (1 in 3.166e3)`. Anything it cannot parse is left out rather
/// than guessed: a missing odds figure is `None`, not a zero that reads as "certain".
pub(crate) fn producer_effort(log: &VecDeque<String>) -> Option<Effort> {
    let line = log.iter().rev().find(|l| l.contains("[palw-producer]") && l.contains(" draws this run"))?;
    let number_before = |marker: &str| -> Option<u64> {
        let head = line.split(marker).next()?;
        head.split_whitespace().last()?.replace(',', "").parse().ok()
    };
    let draws = number_before(" draws this run")?;
    let produced = number_before(" produced").unwrap_or(0);
    let ticket_one_in = line
        .split("(1 in ")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .and_then(|n| n.trim().parse::<f64>().ok());
    Some(Effort { draws, produced, ticket_one_in })
}

pub(crate) fn mining_state(log: &VecDeque<String>, role: NetworkRole, reachable: bool) -> MiningState {
    if role != NetworkRole::Producer || !reachable {
        return MiningState::NotMining;
    }
    let mut blocks = 0u64;
    let mut latest_number = None;
    let mut holding = None;
    for line in log {
        if let Some(rest) = line.split("produced block #").nth(1) {
            blocks += 1;
            // Last line wins: the field names the LATEST block, and the log is in order. A line
            // whose number will not parse leaves the previous one standing rather than blanking it.
            if let Some(n) = rest.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()) {
                latest_number = Some(n);
            }
        } else if let Some(rest) = line.split("[palw-producer] holding: ").nth(1) {
            holding = Some(rest.trim().to_string());
        } else if line.contains("[palw-producer]") && line.contains(" draws this run") {
            // **A draw clears a hold.** The producer prints its holds and, since the node learned
            // to count (2026-09-04), a periodic "N draws this run, M produced …" line while it is
            // drawing. Without this, the hold a node announced in its first seconds — usually
            // `peers=false` before the anchors answer — stayed on the app's face for the rest of
            // the run, telling a person their machine was stopped while it was mining.
            holding = None;
        }
    }
    if blocks > 0 { MiningState::Producing { blocks, latest_number } } else { MiningState::Starting { holding } }
}

/// Split a `getUtxosByAddresses` answer into the pay a producer has, and the pay it is waiting for.
///
/// Only coinbase outputs count: the funds an operator sent to register the bond sit at the same
/// address and are not earnings, and calling them earnings is the one mistake this panel exists to
/// avoid.
pub(crate) fn rewards_from_utxos(value: &Value, virtual_daa: u64) -> Rewards {
    let mut rewards =
        Rewards { blocks_paid: 0, total_sompi: 0, spendable_sompi: 0, maturing_sompi: 0, next_mature_daa: None };
    let Some(entries) = value.get("entries").and_then(Value::as_array) else { return rewards };
    for entry in entries {
        let Some(utxo) = entry.get("utxoEntry") else { continue };
        if !utxo.get("isCoinbase").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let amount = utxo.get("amount").and_then(Value::as_u64).unwrap_or(0);
        let daa = utxo.get("blockDaaScore").and_then(Value::as_u64).unwrap_or(0);
        let matures_at = daa.saturating_add(misaka_studio_core::palw::TESTNET11_COINBASE_MATURITY_DAA);
        rewards.blocks_paid += 1;
        rewards.total_sompi = rewards.total_sompi.saturating_add(amount);
        if virtual_daa >= matures_at {
            rewards.spendable_sompi = rewards.spendable_sompi.saturating_add(amount);
        } else {
            rewards.maturing_sompi = rewards.maturing_sompi.saturating_add(amount);
            rewards.next_mature_daa = Some(match rewards.next_mature_daa {
                Some(soonest) => soonest.min(matures_at),
                None => matures_at,
            });
        }
    }
    rewards
}

pub(crate) fn stale_chain_line(log: &VecDeque<String>) -> Option<String> {
    log.iter().rev().find(|line| line.contains("Genesis not found in active consensus DB")).cloned()
}

/// The argument the node refused, from the clap error it printed before exiting.
///
/// Matched on clap's own prefix rather than on any flag name: the set of ways a command line can
/// be wrong is the node's to define, and a list here would go stale the first time a flag is
/// added. A usage banner alone is not evidence — the `error:` line is.
pub(crate) fn refused_arguments_line(log: &VecDeque<String>) -> Option<String> {
    log.iter()
        .rev()
        .find(|line| {
            let l = line.trim_start();
            l.starts_with("error: ")
                && (l.contains("argument") || l.contains("unexpected") || l.contains("value") || l.contains("required"))
        })
        .cloned()
}

/// How many log lines the supervisor keeps, and how many activity lines.
const LOG_CAPACITY: usize = 600;
const ACTIVITY_CAPACITY: usize = 120;

#[derive(Default)]
struct NodeLogState {
    log: VecDeque<String>,
    activity: VecDeque<String>,
    classes: Vec<NodeClassRow>,
    /// `[palw] producer pay address <addr> (…)` — the node derives it from the producer key when
    /// no pay address is configured; the address a newcomer funds.
    pay_address: Option<String>,
    /// `[palw-panel] registered bond <txid>:<index> …` — printed once by the registration run;
    /// the value `node.producer_bond` must carry from then on.
    registered_bond: Option<String>,
}

struct SupervisedNode {
    child: tokio::process::Child,
    rpc_url: String,
    role: NetworkRole,
    args_shown: Vec<String>,
}

/// The node this Studio watches or runs.
pub struct NodeManager {
    supervised: RwLock<Option<SupervisedNode>>,
    logs: Arc<Mutex<NodeLogState>>,
}

/// What `/api/v1/network/node` returns.
#[derive(Clone, Debug, Serialize)]
pub struct NodeView {
    pub status: NodeStatus,
    pub role: NetworkRole,
    /// Present when supervised: the exact command line, because an operator must be able to see
    /// — and reproduce without the Studio — what is running under their key.
    pub command_line: Option<Vec<String>>,
    pub classes_from_node: Vec<NodeClassRow>,
    pub activity: Vec<String>,
    /// Why the node is not running, when it said so before exiting.
    pub blocker: Option<NodeBlocker>,
    /// Whether this machine is actually producing blocks, and the evidence for saying so.
    pub mining: MiningState,
    /// The pay address the node printed at start (derived from the producer key when none is
    /// configured). `None` until the node says it.
    pub pay_address: Option<String>,
    /// The bond outpoint the node printed when its registration carrier confirmed. `None` until
    /// the node says it; the settings' `producer_bond` should be set to it before the next start.
    pub registered_bond: Option<String>,
    /// How hard the producer is working right now, from its own draw counter. `None` before it
    /// has printed one.
    pub effort: Option<Effort>,
    /// The blocks this producer has been paid for, as the chain holds them. `None` when there is
    /// no address yet or the node did not answer.
    pub rewards: Option<Rewards>,
    /// What the chain holds at the pay address, in sompi, read from the node's own utxo index
    /// (`getBalanceByAddress`). `None` when there is no address yet or the node did not answer.
    /// This is the number a person means by "have I been paid" — it includes the funds they sent
    /// to register the bond, and it grows by a block's reward only once that claim is Final.
    pub pay_balance_sompi: Option<u64>,
}

/// **Is this machine mining?** — one question, answered from the node's own output.
///
/// It needs its own answer because every nearby signal is a false friend. A loaded model means
/// llama.cpp is running, which is chat and not mining. A reachable node means the chain is being
/// followed, which is verification. `role: producer` means the operator asked to mine, not that
/// anything was mined. The chain's own producer says `produced block #N`, and until it does, the
/// honest answer is no.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MiningState {
    /// No node, or a node that is not configured to produce.
    NotMining,
    /// Configured to produce and the node is up, but it has not said `produced block` yet —
    /// syncing, waiting for its first win, or held.
    Starting {
        /// The producer's last `holding: <reason>` line, if it gave one. This is where the real
        /// answer usually is: no bond, no budget, exposure ceiling, no fee outpoint.
        holding: Option<String>,
    },
    /// The node has produced blocks. `blocks` counts what this supervision has seen — the chain's
    /// own `#N` is carried separately because a restart resets the first and not the second.
    Producing { blocks: u64, latest_number: Option<u64> },
}

/// **The work a producer is doing while it has won nothing** — which is the whole of mining most
/// of the time, and was invisible.
///
/// A person watching a miner that has produced no block wants to know whether it is trying. The
/// node counts its own draws and prints the class ticket's odds; this carries those numbers up so
/// the app can say "it is drawing, at these odds" instead of only "nothing won yet".
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Effort {
    /// Draws since this run of the producer started. Resets on restart, like the node's counter.
    pub draws: u64,
    /// Blocks produced in this run.
    pub produced: u64,
    /// One in how many draws wins the class ticket, from the node's own `1 in N`. The ticket is
    /// the first of two gates: a winner still has to beat the network's bits.
    pub ticket_one_in: Option<f64>,
}

/// **The pay a producer has actually received**, split the way a person asks about it.
///
/// Every field comes from the node's own utxo index over the pay address; nothing is inferred from
/// the log. `blocks_paid` counts COINBASE outputs — one per block whose reward has landed — so it
/// is the number of blocks this machine has actually been paid for, which is not the same as the
/// number it has produced: an attempt-lane block's reward is escrowed until its claim is Final,
/// and a voided claim burns it rather than paying it.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Rewards {
    /// Coinbase outputs at this address: one per paid block.
    pub blocks_paid: u64,
    /// Their total, in sompi.
    pub total_sompi: u64,
    /// The part that is spendable now (older than the maturity window).
    pub spendable_sompi: u64,
    /// The part still maturing.
    pub maturing_sompi: u64,
    /// The DAA at which the next maturing reward becomes spendable, when one is waiting.
    pub next_mature_daa: Option<u64>,
}

/// **A startup the node refused, named rather than left as "connection refused".**
///
/// The node asks its questions on a terminal, and the Studio starts it with pipes — so a question
/// is an exit. The RPC poll that follows reports what a poll can report: nothing answered on the
/// port. That is true and useless, and it is the message a user gets today for a condition with a
/// one-click remedy.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeBlocker {
    /// A testnet was re-minted under this data directory. The node will not touch the old chain
    /// without being told to, and it cannot ask.
    StaleChainData {
        /// The line the node printed, verbatim — the remedy is destructive, so the evidence for
        /// it is shown rather than summarised.
        said: String,
    },
    /// The node refused the command line and exited before it opened anything. Every value on
    /// that line came from this app's settings, so the person who can fix it is looking at the
    /// screen — but the RPC poll only ever reported "nothing answered", and the reason sat in a
    /// log nobody opens. Seen for real on 2026-09-04: an operator's `extra_args` repeated a flag
    /// the producer role already passes (`--palw-panel`), kaspad answered "the argument
    /// '--palw-panel' cannot be used multiple times", and the tab showed a node that simply
    /// never came up.
    RefusedArguments {
        /// The node's own words, verbatim.
        said: String,
    },
}

impl NodeManager {
    pub fn new() -> Self {
        NodeManager { supervised: RwLock::new(None), logs: Arc::new(Mutex::new(NodeLogState::default())) }
    }

    /// Where the node binary is: the configured path, beside the Studio, or PATH.
    pub fn resolve_kaspad(configured: Option<&PathBuf>) -> PathBuf {
        let name = if cfg!(windows) { "kaspad.exe" } else { "kaspad" };
        if let Some(path) = configured {
            return path.clone();
        }
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            let beside = dir.join(name);
            if beside.is_file() {
                return beside;
            }
        }
        std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).map(|dir| dir.join(name)).find(|c| c.is_file()))
            .unwrap_or(None)
            .unwrap_or_else(|| PathBuf::from(name))
    }

    /// The command line for a node in `settings`' network and role.
    ///
    /// Built as data first so the UI can show it verbatim: a person putting a bonded key on the
    /// line gets to read the exact flags before anything runs, and can run them without the
    /// Studio afterwards.
    pub fn build_args(settings: &NodeSettings, rpc_port: u16) -> Result<Vec<String>> {
        let mut args: Vec<String> = Vec::new();
        match settings.network {
            NodeNetwork::Testnet11 => {
                args.push("--testnet".into());
                args.push("--netsuffix=11".into());
            }
            NodeNetwork::Devnet => args.push("--devnet".into()),
            NodeNetwork::Simnet => args.push("--simnet".into()),
        }
        if let Some(appdir) = &settings.appdir {
            args.push(format!("--appdir={}", appdir.display()));
        }
        // The Studio's whole view of the node runs over this endpoint. Loopback, always — the
        // node's JSON RPC has no authentication, so exposing it is an operator's deliberate act
        // via extra_args, not a default.
        args.push(format!("--rpclisten-json=127.0.0.1:{rpc_port}"));
        args.push("--utxoindex".into());
        // One-shot class-table dump after sync: the only place the node reports per-class share
        // and budget, and the source of the class ids the UI shows.
        args.push("--palw-dump-classes".into());

        if settings.role == NetworkRole::Producer {
            let key = settings
                .producer_key_path
                .as_ref()
                .ok_or_else(|| Error::bad_request("producing needs node.producer_key_path — generate one with `misaka key gen`"))?;
            // **Two runs, not one.** Without a bond this is the REGISTRATION run: `--palw-panel
            // --palw-register-bond` waits for funds at the pay address, files the carrier, prints
            // `registered bond <txid>:<i>` and stops. `--palw-produce` is deliberately absent from
            // it: on a ConsensusV2 network a producer without a fee outpoint is refused at startup
            // ("needs a way to carry lifecycle objects"), and a first run has none yet — the
            // registration carrier's change is what the panel then persists as one. With a bond
            // this is the PRODUCING run, and the persisted outpoint (or `fee_outpoint`) carries.
            // The same two phases the hosted pool's `run-slot.sh` runs.
            args.push("--palw-panel".into());
            args.push(format!("--palw-producer-key={}", key.display()));
            // Optional since the node derives the key's own address when the flag is absent
            // (kaspad, 2026-09-04): rewards and the panel's carrier funding then share one
            // address, which is the only arrangement that works without a second tool. A value
            // here is an operator sending rewards elsewhere on purpose.
            if let Some(pay) = settings.mining_address.as_ref() {
                args.push(format!("--palw-producer-pay-address={pay}"));
            }
            match &settings.producer_bond {
                Some(bond) => {
                    args.push("--palw-produce".into());
                    args.push(format!("--palw-producer-bond={bond}"));
                }
                None => args.push("--palw-register-bond".into()),
            }
            if let Some(outpoint) = &settings.fee_outpoint {
                args.push(format!("--palw-fee-outpoint={outpoint}"));
            }
            if let Some(class) = &settings.producer_class {
                args.push(format!("--palw-producer-class={class}"));
            }
            if let Some(artifact) = &settings.class_artifact {
                args.push(format!("--palw-class-artifact={}", artifact.display()));
            }
        }

        args.extend(settings.extra_args.iter().cloned());
        Ok(args)
    }

    /// Launch a supervised node. Refuses when one is already running — two nodes sharing an
    /// appdir corrupt its database, and "start" must never be the thing that does that.
    pub async fn start(&self, settings: &NodeSettings) -> Result<NodeView> {
        self.start_inner(settings, false).await
    }

    /// Start after deleting a data directory that holds a different chain — the remedy for
    /// [`NodeBlocker::StaleChainData`], and the only caller that may pass `true`.
    pub async fn start_accepting_data_loss(&self, settings: &NodeSettings) -> Result<NodeView> {
        self.start_inner(settings, true).await
    }

    async fn start_inner(&self, settings: &NodeSettings, accept_data_loss: bool) -> Result<NodeView> {
        {
            let mut guard = self.supervised.write().await;
            if let Some(node) = guard.as_mut() {
                match node.child.try_wait() {
                    Ok(None) => return Err(Error::bad_request("a supervised node is already running; stop it first")),
                    _ => *guard = None, // it exited on its own; forget it
                }
            }
        }

        let binary = Self::resolve_kaspad(settings.kaspad_path.as_ref());
        let rpc_port = default_json_rpc_port(settings.network);
        let mut args = Self::build_args(settings, rpc_port)?;
        if accept_data_loss {
            // `--yes` answers every interactive question, and on this path exactly one is
            // expected: the re-minted testnet's "your database needs to be fully deleted".
            //
            // It is a per-launch argument and never a setting. The flag's blast radius is every
            // question the node might ever ask, so leaving it on would turn future prompts —
            // ones nobody has read — into silent yeses; and the answer it gives here destroys a
            // chain. A user pressed a button that said so, for this start, and the command line
            // the UI shows carries the flag so the choice is visible afterwards.
            args.push("--yes".into());
        }
        let rpc_url = format!("ws://127.0.0.1:{rpc_port}");

        {
            let mut logs = self.logs.lock().expect("log lock");
            *logs = NodeLogState::default();
        }

        tracing::info!(binary = %binary.display(), ?args, "starting MISAKA node");
        let mut child = tokio::process::Command::new(&binary)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Node {
                message: format!(
                    "could not start {}: {e}. Build the node with `cargo build --release -p kaspad` in the misakas \
                     repository, or set node.kaspad_path.",
                    binary.display()
                ),
            })?;

        for stream in [child.stdout.take().map(NodePipe::Out), child.stderr.take().map(NodePipe::Err)].into_iter().flatten() {
            let logs = self.logs.clone();
            tokio::spawn(async move {
                match stream {
                    NodePipe::Out(s) => drain_node(s, logs).await,
                    NodePipe::Err(s) => drain_node(s, logs).await,
                }
            });
        }

        let view_args = std::iter::once(binary.display().to_string()).chain(args.iter().cloned()).collect();
        *self.supervised.write().await = Some(SupervisedNode { child, rpc_url, role: settings.role, args_shown: view_args });
        self.view(settings).await
    }

    /// Stop the supervised node.
    ///
    /// The producing caveat is real and stated where the button is (the UI), not silently
    /// enforced here: on this chain a producer's in-flight claims are its responsibility to
    /// serve, and a node stopped with claims open defaults them against its bond. Stopping is
    /// still the operator's call — the Studio's job is that they make it knowingly.
    pub async fn stop(&self) -> Result<()> {
        if let Some(mut node) = self.supervised.write().await.take() {
            tracing::info!("stopping the supervised node");
            #[cfg(unix)]
            {
                // SIGTERM first: the node flushes its database on a graceful shutdown, and a
                // RocksDB that was kill -9'd replays its WAL for minutes on the next start.
                if let Some(pid) = node.child.id() {
                    let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status();
                    for _ in 0..50 {
                        if matches!(node.child.try_wait(), Ok(Some(_))) {
                            return Ok(());
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
            let _ = node.child.kill().await;
            let _ = node.child.wait().await;
        }
        Ok(())
    }

    /// The current picture: supervised child if any, else the configured attach URL.
    pub async fn view(&self, settings: &NodeSettings) -> Result<NodeView> {
        let (rpc_url, source, command_line, role) = {
            let mut guard = self.supervised.write().await;
            let exited = match guard.as_mut() {
                Some(node) => matches!(node.child.try_wait(), Ok(Some(_))),
                None => false,
            };
            if exited {
                // Keep the log (it holds the reason); drop the dead handle so start works again.
                *guard = None;
            }
            match guard.as_ref() {
                Some(node) => (node.rpc_url.clone(), "supervised".to_string(), Some(node.args_shown.clone()), node.role),
                None => {
                    let url = normalize_rpc_url(settings.rpc_url.as_deref().unwrap_or(""), settings.network);
                    (url, "attached".to_string(), None, settings.role)
                }
            }
        };

        let mut status = query_status(&rpc_url).await;
        status.source = source;
        let (pay_address, registered_bond) = {
            let logs = self.logs.lock().expect("log lock");
            (logs.pay_address.clone(), logs.registered_bond.clone())
        };
        // Asked of the node the Studio is already talking to, not of an explorer: the answer is the
        // one this machine's own chain view holds, and it is absent rather than wrong when the node
        // is unreachable or has no utxo index.
        let rewards = match (&pay_address, status.reachable) {
            (Some(address), true) => {
                let url = normalize_rpc_url(settings.rpc_url.as_deref().unwrap_or(""), settings.network);
                let virtual_daa = status.virtual_daa_score.unwrap_or(0);
                wrpc_call(&url, "getUtxosByAddresses", serde_json::json!({ "addresses": [address] }), Duration::from_secs(4))
                    .await
                    .ok()
                    .map(|value| rewards_from_utxos(&value, virtual_daa))
            }
            _ => None,
        };
        let pay_balance_sompi = match (&pay_address, status.reachable) {
            (Some(address), true) => {
                let url = normalize_rpc_url(settings.rpc_url.as_deref().unwrap_or(""), settings.network);
                wrpc_call(&url, "getBalanceByAddress", serde_json::json!({ "address": address }), Duration::from_secs(4))
                    .await
                    .ok()
                    .and_then(|v| v.get("balance").and_then(serde_json::Value::as_u64))
            }
            _ => None,
        };
        let (classes_from_node, activity) = {
            let logs = self.logs.lock().expect("log lock");
            (logs.classes.clone(), logs.activity.iter().cloned().collect())
        };
        // Only when nothing is answering: a running node's old log lines are history, not a
        // blocker, and a stale banner on a healthy node is its own kind of lie.
        let (mining, effort) = {
            let logs = self.logs.lock().expect("log lock");
            (mining_state(&logs.log, role, status.reachable), producer_effort(&logs.log))
        };
        let blocker = (!status.reachable)
            .then(|| {
                let logs = self.logs.lock().expect("log lock");
                // A refused command line is checked first: it is the more specific fact, and a
                // node that never parsed its arguments never reached the chain it would have
                // called stale.
                refused_arguments_line(&logs.log)
                    .map(|said| NodeBlocker::RefusedArguments { said })
                    .or_else(|| stale_chain_line(&logs.log).map(|said| NodeBlocker::StaleChainData { said }))
            })
            .flatten();
        Ok(NodeView {
            status,
            role,
            command_line,
            classes_from_node,
            activity,
            blocker,
            mining,
            pay_address,
            registered_bond,
            effort,
            rewards,
            pay_balance_sompi,
        })
    }

    pub async fn is_supervising(&self) -> bool {
        self.supervised.read().await.is_some()
    }

    pub fn recent_log(&self, limit: usize) -> Vec<String> {
        let logs = self.logs.lock().expect("log lock");
        logs.log.iter().rev().take(limit).cloned().collect::<Vec<_>>().into_iter().rev().collect()
    }
}

impl Default for NodeManager {
    fn default() -> Self {
        Self::new()
    }
}

enum NodePipe {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

async fn drain_node<R: tokio::io::AsyncRead + Unpin>(stream: R, logs: Arc<Mutex<NodeLogState>>) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::debug!(target: "node", "{line}");
        let mut state = logs.lock().expect("log lock");
        if state.log.len() == LOG_CAPACITY {
            state.log.pop_front();
        }
        state.log.push_back(line.clone());
        if let Some(row) = parse_class_row(&line) {
            state.classes.retain(|existing| existing.class_id != row.class_id);
            state.classes.push(row);
        }
        if let Some(address) = parse_pay_address(&line) {
            state.pay_address = Some(address);
        }
        if let Some(outpoint) = parse_registered_bond(&line) {
            state.registered_bond = Some(outpoint);
        }
        if is_activity_line(&line) {
            if state.activity.len() == ACTIVITY_CAPACITY {
                state.activity.pop_front();
            }
            state.activity.push_back(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use misaka_studio_core::settings::NodeSettings;

    #[test]
    fn the_draw_counter_is_read_from_the_newest_line() {
        let mut log = VecDeque::new();
        log.push_back("[palw-producer] 1 draws this run, 0 produced, 0 won the class ticket".to_string());
        log.push_back(
            "2026-09-04 [INFO ] [palw-producer] 4,210 draws this run, 2 produced, 3 won the class ticket and \
             lost the network draw against bits; class ticket p = 3.159e-4 per draw (1 in 3.166e3)"
                .to_string(),
        );
        let effort = producer_effort(&log).expect("the newest line parses");
        assert_eq!(effort.draws, 4210, "thousands separators are not a parse failure");
        assert_eq!(effort.produced, 2);
        assert_eq!(effort.ticket_one_in, Some(3.166e3));

        // A line with no odds is still a draw count — the odds are absent, not zero.
        let mut bare = VecDeque::new();
        bare.push_back("[palw-producer] 7 draws this run, 0 produced".to_string());
        assert_eq!(producer_effort(&bare).map(|e| (e.draws, e.ticket_one_in)), Some((7, None)));
        assert!(producer_effort(&VecDeque::new()).is_none(), "no line, no claim");
    }

    #[test]
    fn rewards_count_coinbase_outputs_and_split_them_by_maturity() {
        // Two coinbase rewards and the operator's own funding transfer at the same address.
        let answer = serde_json::json!({"entries": [
            {"utxoEntry": {"amount": 100, "blockDaaScore": 10, "isCoinbase": true}},
            {"utxoEntry": {"amount": 250, "blockDaaScore": 900, "isCoinbase": true}},
            {"utxoEntry": {"amount": 1_200_000_000u64, "blockDaaScore": 830, "isCoinbase": false}},
        ]});
        let rewards = rewards_from_utxos(&answer, 1000);
        assert_eq!(rewards.blocks_paid, 2, "the transfer is not a reward");
        assert_eq!(rewards.total_sompi, 350);
        assert_eq!(rewards.spendable_sompi, 100, "daa 10 + 601 is long past");
        assert_eq!(rewards.maturing_sompi, 250);
        assert_eq!(rewards.next_mature_daa, Some(1501));

        // Nothing paid yet is not an error, and not a zero-with-a-blank.
        let empty = rewards_from_utxos(&serde_json::json!({"entries": []}), 1000);
        assert_eq!(empty.blocks_paid, 0);
        assert_eq!(empty.next_mature_daa, None);
    }

    #[test]
    fn a_draw_after_a_hold_clears_it() {
        let mut log = VecDeque::new();
        log.push_back(
            "[palw-producer] holding: the mining rule engine says this node should not mine [enable_unsynced_mining=false peers=false participation_allowed=true]".to_string(),
        );
        assert!(
            matches!(mining_state(&log, NetworkRole::Producer, true), MiningState::Starting { holding: Some(_) }),
            "a hold with nothing after it stands"
        );
        log.push_back(
            "[palw-producer] 12 draws this run, 0 produced, 0 won the class ticket and lost the network draw against bits; class ticket p = 7.896e-5 per draw".to_string(),
        );
        assert_eq!(
            mining_state(&log, NetworkRole::Producer, true),
            MiningState::Starting { holding: None },
            "a draw after the hold means the node is drawing, whatever it said a minute ago"
        );
        // A hold AFTER the draws is the current state again.
        log.push_back("[palw-producer] holding: the named bond is not registered on this chain".to_string());
        assert!(matches!(mining_state(&log, NetworkRole::Producer, true), MiningState::Starting { holding: Some(_) }));
    }

    #[test]
    fn a_refused_command_line_is_a_blocker_with_the_nodes_own_words() {
        let mut log = VecDeque::new();
        log.push_back("error: the argument '--palw-panel' cannot be used multiple times".to_string());
        log.push_back(String::new());
        log.push_back("Usage: kaspad [OPTIONS]".to_string());
        assert_eq!(
            refused_arguments_line(&log).as_deref(),
            Some("error: the argument '--palw-panel' cannot be used multiple times")
        );
        // A usage banner without the error line is not evidence of which argument was wrong.
        let banner: VecDeque<String> = ["Usage: kaspad [OPTIONS]".to_string()].into_iter().collect();
        assert_eq!(refused_arguments_line(&banner), None);
        // An ordinary runtime error is not an argument refusal.
        let runtime: VecDeque<String> = ["error: cannot write /var/lib/misaka/liveness.json".to_string()].into_iter().collect();
        assert_eq!(refused_arguments_line(&runtime), None);
    }

    #[test]
    fn the_pay_address_and_the_registered_bond_are_read_off_the_node_lines() {
        let addr = parse_pay_address(
            "2026-09-04 07:42:41.875+09:00 [INFO ] [palw] producer pay address misakadev:qg3fzu3xz2f4z8c59q4mc0jl4gdp98kc84xupe7utumym8dt5tytfv9k83hj2gulnrkvws4hukxjycwmj4z56676upssywvh58f9xrsvv28dz3kz (derived from --palw-producer-key; pass --palw-producer-pay-address to override)",
        )
        .expect("parses");
        assert!(addr.starts_with("misakadev:qg3fzu3x"));
        assert!(parse_pay_address("[palw-panel] no bond yet; registering one").is_none());
        let bond = parse_registered_bond(&format!("[palw-panel] registered bond {}:0 with 400000 sompi collateral. Restart with …", "ab".repeat(64)))
            .expect("parses");
        assert_eq!(bond, format!("{}:0", "ab".repeat(64)));
        assert!(parse_registered_bond("[palw-panel] registered bond nothing").is_none());
        assert!(is_activity_line("[palw] producer pay address misakadev:qq (derived)"));
    }

    #[test]
    fn urls_normalize_to_the_network_default_port() {
        assert_eq!(normalize_rpc_url("", NodeNetwork::Devnet), "ws://127.0.0.1:28610");
        assert_eq!(normalize_rpc_url("", NodeNetwork::Testnet11), "ws://127.0.0.1:28210");
        assert_eq!(normalize_rpc_url("10.0.0.5:28210", NodeNetwork::Testnet11), "ws://10.0.0.5:28210");
        assert_eq!(normalize_rpc_url("ws://x:1/", NodeNetwork::Devnet), "ws://x:1/");
        assert_eq!(normalize_rpc_url("myhost", NodeNetwork::Devnet), "ws://myhost:28610");
    }

    /// The dump line as `palw_dump.rs` writes it, share both present and NONE.
    #[test]
    fn class_rows_parse_from_the_dump_lines() {
        let row =
            parse_class_row("[palw-dump]   class=f1c5635c6e47e96e base=true  status=Active share=22  budget=22").expect("parses");
        assert_eq!(row.class_id, "f1c5635c6e47e96e");
        assert!(row.base);
        assert_eq!(row.status, "Active");
        assert_eq!(row.share_permille, Some(22));
        assert_eq!(row.budget_blocks, Some(22));

        let none = parse_class_row("[palw-dump]   class=ec7bbcbf base=false status=Pending share=NONE budget=0").expect("parses");
        assert_eq!(none.share_permille, None);
        assert_eq!(none.budget_blocks, Some(0));

        assert!(parse_class_row("[palw-dump] class table follows").is_none());
        assert!(parse_class_row("unrelated line").is_none());
    }

    #[test]
    fn a_verifier_gets_a_plain_full_node_command() {
        let settings = NodeSettings { network: NodeNetwork::Testnet11, role: NetworkRole::Verifier, ..Default::default() };
        let args = NodeManager::build_args(&settings, 28210).expect("builds");
        let joined = args.join(" ");
        assert!(joined.contains("--testnet --netsuffix=11"));
        assert!(joined.contains("--rpclisten-json=127.0.0.1:28210"));
        assert!(joined.contains("--palw-dump-classes"));
        assert!(!joined.contains("--palw-produce"), "a verifier does not produce: {joined}");
    }

    /// The producer command is the runbook's §4, assembled — and refused while its named
    /// prerequisites are missing, with the remedy in the message.
    #[test]
    fn a_producer_without_a_key_is_refused_with_the_remedy() {
        let settings = NodeSettings { role: NetworkRole::Producer, ..Default::default() };
        let err = NodeManager::build_args(&settings, 28210).unwrap_err();
        assert!(err.to_string().contains("misaka key gen"), "{err}");
    }

    #[test]
    fn a_producer_with_a_bond_mines_and_without_one_registers() {
        let mut settings = NodeSettings {
            role: NetworkRole::Producer,
            producer_key_path: Some("/keys/miner.seed".into()),
            mining_address: Some("misakatest:qqq".into()),
            ..Default::default()
        };
        let register = NodeManager::build_args(&settings, 28210).expect("builds").join(" ");
        assert!(register.contains("--palw-register-bond"));
        assert!(!register.contains("--palw-producer-bond="));
        // The registration run must not produce: a ConsensusV2 node refuses `--palw-produce`
        // without a fee outpoint, and a first run has none until its own carrier confirms.
        // As a whole token: `--palw-producer-key=…` also starts with these bytes.
        assert!(!register.split(' ').any(|a| a == "--palw-produce"), "{register}");
        assert!(register.contains("--palw-panel"));

        settings.producer_bond = Some("abc123:0".into());
        settings.fee_outpoint = Some("abc123:1".into());
        let produce = NodeManager::build_args(&settings, 28210).expect("builds").join(" ");
        assert!(produce.contains("--palw-produce"));
        assert!(produce.contains("--palw-panel"));
        assert!(produce.contains("--palw-producer-bond=abc123:0"));
        assert!(produce.contains("--palw-fee-outpoint=abc123:1"));
        assert!(!produce.contains("--palw-register-bond"));
    }

    #[test]
    fn activity_lines_are_the_palw_ones() {
        assert!(is_activity_line("[palw-producer] holding: budget spent [class=… budget=0]"));
        assert!(is_activity_line("[palw-panel] registered bond abc:0"));
        assert!(is_activity_line("Consensus params fingerprint: 15bab795… (network testnet-11)"));
        assert!(!is_activity_line("2026-08-29 mempool size 3"));
    }

    /// **The regenesis failure, as the node actually prints it** (measured on the live fleet,
    /// 2026-08-30). A re-minted testnet makes every node with the old chain on disk ask a
    /// question, and the Studio starts nodes with pipes — so the question is an exit, and the RPC
    /// poll that follows says only "connection refused". The line is the whole difference between
    /// a mystery and a button.
    #[test]
    fn a_re_minted_testnet_is_recognised_from_the_line_the_node_prints() {
        let mut log = VecDeque::new();
        log.push_back("2026-08-30 10:06:36 [INFO ] Application directory: /var/lib/x/appdir".to_string());
        log.push_back(
            "Genesis not found in active consensus DB. This happens when Testnets are restarted and your              database needs to be fully deleted. Do you confirm the delete? (y/n)"
                .to_string(),
        );
        log.push_back("Operation was rejected (), exiting..".to_string());
        assert!(stale_chain_line(&log).is_some_and(|l| l.contains("needs to be fully deleted")));
    }

    /// A node that is merely down, or one that never started, must not be offered a chain-deleting
    /// remedy for a condition it did not report.
    #[test]
    fn an_ordinary_failure_is_not_a_stale_chain() {
        let mut log = VecDeque::new();
        log.push_back("thread 'main' panicked at kaspad/src/daemon.rs: --palw-produce needs …".to_string());
        log.push_back("Operation was rejected (), exiting..".to_string());
        assert_eq!(stale_chain_line(&log), None, "a declined prompt is not evidence of WHICH prompt");
        assert_eq!(stale_chain_line(&VecDeque::new()), None);
    }

    /// The genesis line is worth showing in the activity feed too — it is the one line that
    /// explains an otherwise silent restart loop.
    #[test]
    fn the_genesis_line_reaches_the_activity_feed() {
        assert!(is_activity_line("Genesis not found in active consensus DB. This happens when Testnets are restarted"));
    }

    /// **The question the UI asks, and the four wrong ways to answer it.** A user with a chat
    /// model loaded, a synced node and `role: producer` set has three green signals and may still
    /// have mined nothing; only the producer's own line settles it.
    #[test]
    fn mining_is_only_true_once_the_producer_says_it_produced() {
        let mut log = VecDeque::new();
        log.push_back("Consensus params fingerprint: f3bf86b4… (network testnet-11)".to_string());

        // Configured and up, nothing produced: not mining, and no reason offered yet.
        assert_eq!(mining_state(&log, NetworkRole::Producer, true), MiningState::Starting { holding: None });

        // The reason, when the node gives one, is the answer the operator actually needs.
        log.push_back("[palw-producer] holding: this class's epoch budget is already spent [budget=0]".to_string());
        match mining_state(&log, NetworkRole::Producer, true) {
            MiningState::Starting { holding: Some(h) } => assert!(h.contains("epoch budget"), "{h}"),
            other => panic!("expected a held producer, got {other:?}"),
        }

        // And the one line that proves a block was made.
        log.push_back("[palw-producer] produced block #691 6ddac6d7fa4e9fb90d656a3da3b1e0a07bb".to_string());
        log.push_back("[palw-producer] produced block #692 b86b4cfb186feb1c393f85bf79a389530d8".to_string());
        // The LATEST number, not the first: the log is in order and the field names the newest
        // block this supervision saw.
        assert_eq!(mining_state(&log, NetworkRole::Producer, true), MiningState::Producing { blocks: 2, latest_number: Some(692) });
    }

    /// A verifier is not mining however healthy it looks, and neither is a producer whose node is
    /// down — the log it left behind describes a chain it is no longer on.
    #[test]
    fn a_verifier_and_an_unreachable_producer_are_both_not_mining() {
        let mut log = VecDeque::new();
        log.push_back("[palw-producer] produced block #1 aa".to_string());
        assert_eq!(mining_state(&log, NetworkRole::Verifier, true), MiningState::NotMining);
        assert_eq!(mining_state(&log, NetworkRole::Producer, false), MiningState::NotMining);
    }
}
