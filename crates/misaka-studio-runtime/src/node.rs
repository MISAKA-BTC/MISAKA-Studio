//! **The chain, read through the network's public wRPC nodes** — the Studio runs no node.
//!
//! MISAKA Studio mines over the RFC-0009 node-free path: the gateway, the rail and the remote miner talk to public nodes,
//! and so does this module, for the status the Network tab shows. Every fact read here is one node's word
//! (`UNVERIFIED_REMOTE_STATE`), shown as such; nothing here signs, submits or decides.
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

use std::path::PathBuf;
use std::time::Duration;

use crate::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use misaka_studio_core::settings::{NodeNetwork, NodeSettings};
use serde::Serialize;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

pub use crate::remote_miner::{PublicWrpc, public_wrpc};

/// The wRPC Borsh endpoint the CLI, gateway and rail are given: the configured one, else the network's first public node.
pub fn borsh_endpoint(settings: &NodeSettings) -> Option<String> {
    settings
        .misaka_rpc
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| public_wrpc(settings.network).first().map(|n| n.wrpc_borsh.to_string()))
}

/// Where the `misaka` CLI is: the configured path, beside the Studio, or PATH.
pub fn resolve_misaka_cli(configured: Option<&PathBuf>) -> PathBuf {
    crate::remote_miner::resolve_binary(configured, "misaka")
}

/// The chain as each public node of the network reports it.
pub async fn public_status(network: NodeNetwork) -> Vec<NodeStatus> {
    let mut out = Vec::new();
    for node in public_wrpc(network) {
        let mut status = query_status(node.wrpc_json).await;
        status.source = node.name.to_string();
        out.push(status);
    }
    out
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
    /// The public node's name.
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
    /// The sink block's header timestamp, ms. With the wall clock this says how long the chain has
    /// been still — the one number that separates "quiet by design" from "stopped".
    pub sink_timestamp_ms: Option<u64>,
    /// The sink block's lane (`powAlgoId`). It decides how long the heartbeat lane will wait
    /// before it moves the chain on its own: see [`heartbeat_stand_down_secs`].
    pub sink_algo_id: Option<u8>,
    /// How long the heartbeat lane will hold off, given that lane — the consensus rule resolved
    /// here rather than re-spelled in the UI, so there is one copy of it.
    pub sink_stand_down_secs: Option<u64>,
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
            status.error = Some(e.to_string());
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
    // The sink's own header, for its timestamp and lane. A second call because getBlockDagInfo
    // names the sink and says nothing else about it.
    if let Some(sink) = status.sink.clone()
        && let Ok(answer) = wrpc_call(url, "getBlock", json!({ "hash": sink, "includeTransactions": false }), timeout).await
    {
        let header = answer.get("block").unwrap_or(&answer).get("header").cloned().unwrap_or(Value::Null);
        status.sink_timestamp_ms = header.get("timestamp").and_then(Value::as_u64);
        status.sink_algo_id = header.get("powAlgoId").and_then(Value::as_u64).map(|id| id as u8);
        status.sink_stand_down_secs = status.sink_algo_id.and_then(heartbeat_stand_down_secs);
    }
    if let Ok(peers) = wrpc_call(url, "getConnectedPeerInfo", json!({}), timeout).await {
        status.peer_count = peers.get("peerInfo").and_then(Value::as_array).map(Vec::len);
    }
    status
}

/// What `address` can spend now, in sompi, as the network's first public node reports it: every unspent output there,
/// a coinbase one only once it has matured. One node's word (`UNVERIFIED_REMOTE_STATE`) — a readiness hint for a button,
/// never what signs; the CLI re-reads the chain when it builds the transaction.
pub async fn spendable_sompi(network: NodeNetwork, address: &str) -> Result<u64> {
    let node = public_wrpc(network).first().ok_or_else(|| Error::bad_request("this network publishes no public wRPC"))?;
    let timeout = Duration::from_secs(10);
    let dag = wrpc_call(node.wrpc_json, "getBlockDagInfo", json!({}), timeout).await?;
    let virtual_daa = dag.get("virtualDaaScore").and_then(Value::as_u64).unwrap_or(0);
    let utxos = wrpc_call(node.wrpc_json, "getUtxosByAddresses", json!({ "addresses": [address] }), timeout).await?;
    let number = |v: Option<&Value>| v.and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
    let mut total = 0u64;
    for entry in utxos.get("entries").and_then(Value::as_array).into_iter().flatten() {
        let utxo = entry.get("utxoEntry").unwrap_or(entry);
        let coinbase = utxo.get("isCoinbase").and_then(Value::as_bool).unwrap_or(false);
        if !coinbase || number(utxo.get("blockDaaScore")).saturating_add(misaka_studio_core::palw::COINBASE_MATURITY_DAA) <= virtual_daa {
            total = total.saturating_add(number(utxo.get("amount")));
        }
    }
    Ok(total)
}

/// The funding address of the bond key at `key_path`, as the `misaka` CLI derives it (`key pubkey`). The Studio reads
/// no key itself; the CLI reads the file and prints only public values.
pub async fn key_address(settings: &NodeSettings, key_path: &std::path::Path) -> Result<String> {
    let out = tokio::process::Command::new(resolve_misaka_cli(settings.misaka_cli_path.as_ref()))
        .args(["--network", settings.network.id(), "--output", "json", "key", "pubkey", "--key-file"])
        .arg(key_path)
        .output()
        .await
        .map_err(|e| Error::Node { message: format!("could not run the misaka CLI: {e}") })?;
    if !out.status.success() {
        return Err(Error::Node { message: format!("misaka key pubkey: {}", String::from_utf8_lossy(&out.stderr).trim()) });
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| Error::Node { message: format!("misaka key pubkey: {e}") })?;
    v.get("address")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Node { message: "misaka key pubkey printed no address".into() })
}

/// **How long the heartbeat lane will stay out of the way, given the lane of the newest block.**
///
/// The heartbeat lane is the chain's clock, and it deliberately runs at two speeds
/// (`palw_heartbeat_v1.rs`): when the newest block is a BONDED one — an attempt block (algo 6 or
/// 9) or a receipt (7) — the chain is demonstrably producing and the lane stands down for a full
/// hour; when the newest block is itself a heartbeat, the chain is running on the clock alone and
/// the lane returns to the 120 s cadence.
///
/// This matters to a person watching a miner far more than it looks. After this node won its first
/// block on 2026-09-04 the DAA score sat unchanged for the best part of an hour, and nothing said
/// that a still chain right then was the network working exactly as designed rather than a node
/// that had died. `None` for a lane this does not know, so the app says nothing rather than
/// guessing.
pub(crate) fn heartbeat_stand_down_secs(sink_algo_id: u8) -> Option<u64> {
    match sink_algo_id {
        6 | 7 | 9 => Some(3_600), // HEARTBEAT_NOMINAL_INTERVAL_MS
        8 => Some(120),           // HEARTBEAT_RECOVERY_INTERVAL_MS
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stand_down_follows_the_newest_block_s_lane() {
        // A bonded block — attempt (6, 9) or receipt (7) — buys the chain an hour of quiet.
        for bonded in [6u8, 7, 9] {
            assert_eq!(heartbeat_stand_down_secs(bonded), Some(3_600), "lane {bonded} is bonded");
        }
        // A heartbeat parent means the chain is running on its clock, at cadence.
        assert_eq!(heartbeat_stand_down_secs(8), Some(120));
        // Anything else: no claim. Genesis (1) is not a lane this rule speaks about.
        assert_eq!(heartbeat_stand_down_secs(1), None);
    }

    #[test]
    fn the_cli_and_the_gateway_default_to_the_first_public_node() {
        let settings = NodeSettings { network: NodeNetwork::Testnet12, ..Default::default() };
        assert_eq!(borsh_endpoint(&settings).as_deref(), Some("wss://wallet.misakascan.com/t12/wrpc/borsh"));
        let mine = NodeSettings { misaka_rpc: Some("wss://x/borsh".into()), ..settings };
        assert_eq!(borsh_endpoint(&mine).as_deref(), Some("wss://x/borsh"));
        assert_eq!(borsh_endpoint(&NodeSettings { network: NodeNetwork::Devnet, ..Default::default() }), None);
    }
}
