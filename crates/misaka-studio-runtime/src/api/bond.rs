//! `/api/v1/network/bond` — **from an empty app to a bonded producer, in the order a person does it.**
//!
//! On testnet-12 (release `0e8ec984e`) a producer is one ML-DSA-87 key: it signs the bond, it is the
//! operator key (`palw_operator_id_unique` is armed from DAA 0, so the carrier carries both
//! signatures), and its own address is where rewards land and where the collateral is spent from.
//! Getting from nothing to producing takes five facts, and until now the Studio asked the person to
//! assemble them from log lines and settings fields:
//!
//! 1. **a key** — minted here (`POST /network/producer-key`, 0600 under the data directory);
//! 2. **its address** — derived by the `misaka` CLI from the key file, so it is known before any
//!    node runs (the node's own `[palw] producer pay address` line is the fallback);
//! 3. **one output at that address holding the collateral plus fees** — registration spends a
//!    single input (the node picks the largest), so the balance alone is not the answer and the
//!    largest output is shown beside it;
//! 4. **the registration run** — only `kaspad --palw-register-bond` can file a bond; no `misaka`
//!    command registers one. The node prints `registered bond <txid>:0 …` and then **keeps
//!    running** (only its registration worker stops), so the Studio watches for that line;
//! 5. **a declaration** — a new bond declares no capability, and an undeclared bond is never drawn
//!    onto a panel. `misaka bond capability --declare` files it, and it needs **a second output**:
//!    the registration carrier's change (`<carrier>:1`) is the node's fee float, which the node
//!    reserves the moment it persists it, and the CLI will not spend a reserved output. A deposit
//!    that arrived as one output leaves nothing to pay the declaration with — `misaka mining setup`
//!    has the same gap — so the card asks for a second, small deposit and waits for it.
//!
//! `POST /register` runs 4 and then finishes by itself: the bond goes into the settings, the
//! Studio waits for a second output, declares the floor through the still-running registration node,
//! waits until the chain's registry lists the declaration (`getPalwClaims … bondCapableClasses`, the
//! same read the setup wizard waits on — a restarted node's mempool is empty, and a declaration that
//! was only in it is lost), and restarts the node as a producer. The fee float is the node's own:
//! the one its registration persisted in the app dir, or what its scan finds.
//! **One process per bond throughout**: the registration node is stopped before the producing one
//! starts — two processes under one bond double-sign round permits and are slashed.
//!
//! Nothing here signs anything itself. The bond is the node's transaction and the declaration is
//! the CLI's, the two implementations the chain's own tests cover.

use crate::node::{NodeManager, normalize_rpc_url, wrpc_call};
use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use misaka_studio_core::palw::classes_for;
use misaka_studio_core::settings::{NetworkRole, NodeNetwork, NodeSettings};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const SOMPI_PER_MSK: u64 = 100_000_000;

/// testnet-12's producer floor, 13,000 MSK (`PALW_MAINNET_MIN_COLLATERAL_SOMPI`, "mainnet-assumed
/// bonds", decided 2026-09-24). The registration refuses less.
pub const TESTNET12_PRODUCER_FLOOR_SOMPI: u64 = 13_000 * SOMPI_PER_MSK;

/// What the registration carrier needs on top of the collateral: the node's own margin is 0.1 MSK
/// (`misaka mining setup`'s `REGISTRATION_MARGIN_SOMPI`). The Studio asks for 1 MSK, because the same
/// output's change then pays the capability declaration and becomes the fee float the seat duties
/// carry their objects with (the join guide asks for ≥ 0.1 MSK of float).
pub const REGISTRATION_MARGIN_SOMPI: u64 = SOMPI_PER_MSK / 10;
pub const RECOMMENDED_MARGIN_SOMPI: u64 = SOMPI_PER_MSK;

/// **Collateral per claim held at once** on testnet-12, re-checked against release `0e8ec984e` on
/// 2026-09-26 (the release's `palw_claim_bond_reservation_v1` on the testnet-12 parameters, and the
/// live chain's per-claim `escrowSompi` / `reservedSompi`). A claim reserves escrow + weight from the
/// block that accepts it — 3,200.85 MSK of escrow at the block-one subsidy, plus 0.1075 MSK (floor) or
/// 24.716 MSK (8k) of weight — and the room is `collateral × 500‰ − what the bond already backs`, so
/// one claim held at once costs twice its reservation. The escrow part is released early, at licence,
/// when every seat of an unredrawn panel returned Valid; a full bond waits for a release, it does not
/// wedge. The escrow scales with the subsidy, so these are figures for choosing an amount; `misaka
/// bond status` (`exposure_ceiling`, `reserved_exposure`) is the live answer.
pub const TESTNET12_FLOOR_CLAIM_SOMPI: u64 = 640_190_805_480;
pub const TESTNET12_8K_CLAIM_SOMPI: u64 = 645_112_500_620;

/// **The node's own default collateral is not the rule.** Without `--palw-bond-collateral` the node
/// locks `palw_v2_collateral_for_claim_lifetime_v1`, a devnet weight-only formula R-core+ did not
/// replace: 3,119,145,986,560 sompi for the floor (holds 4 claims) and ≈ 2,000,332,625 MSK for the 8k
/// class (read off the live chain), and it warns that a smaller named bond "may then hold forever",
/// which is wrong on testnet-12. So on testnet-12 the Studio always names the amount.
pub const TESTNET12_NODE_DEFAULT_FLOOR_SOMPI: u64 = 3_119_145_986_560;

/// How long the registration run may take before the Studio stops waiting for its line. The node
/// itself waits up to ten minutes for the carrier after it is funded; the rest is the person's time
/// to fund it, which the watcher does not need to hurry.
const REGISTRATION_WATCH: Duration = Duration::from_secs(6 * 3600);

/// How long to wait for the chain to list the declaration. On a timeout the registration node is
/// left running — its mempool still holds the declaration — and Finish picks up from there.
const DECLARATION_WATCH: Duration = Duration::from_secs(20 * 60);

/// How long to wait for the second deposit that pays the declaration.
const SECOND_DEPOSIT_WATCH: Duration = Duration::from_secs(6 * 3600);

/// The second deposit the card asks for: it pays the declaration, and what it leaves is the float
/// the node's scan falls back to when the registration's change runs out.
pub const SECOND_DEPOSIT_SOMPI: u64 = SOMPI_PER_MSK;

/// The least a second output may hold to pay a carrier: the join guide's fee-float floor.
const CARRIER_FUNDING_MIN_SOMPI: u64 = SOMPI_PER_MSK / 10;

/// No `misaka` call here needs more than this; one that hangs must not hold the setup forever.
const CLI_TIMEOUT: Duration = Duration::from_secs(180);

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(status)).route("/register", post(register)).route("/finish", post(finish))
}

/// Where the setup stands, as one word the UI can switch on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BondPhase {
    /// No key file yet.
    NeedKey,
    /// A key, but its address holds no single output big enough (or the node could not say).
    NeedFunds,
    /// Enough in one output: the registration can run.
    ReadyToRegister,
    /// The registration run is up and waiting — for funds, a synced chain, or its carrier.
    Registering,
    /// The bond landed; declaring, waiting for the declaration, restarting.
    Finishing,
    /// A bond is saved but the chain lists no declaration for it: it would never be drawn.
    NeedsDeclaration,
    /// `node.producer_bond` is set and declared (or the node could not be asked).
    Bonded,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Funds {
    /// Everything at the address, bond collateral included once there is one.
    pub total_sompi: u64,
    /// The largest single ordinary (non-coinbase) output — what a registration can spend.
    pub largest_output_sompi: u64,
    /// The next largest: what pays the capability declaration after the registration took the
    /// largest (the registration's own change is reserved by the node as its fee float).
    pub second_output_sompi: u64,
    /// Ordinary outputs, counted.
    pub outputs: usize,
    /// Rewards at the address. The node can spend a matured one, but the wizard and the join guide
    /// ask for an ordinary output, so these are shown and not counted toward registering.
    pub coinbase_sompi: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct CollateralChoice {
    pub label: String,
    /// `None` = no `--palw-bond-collateral`: the node sizes it (offered only off testnet-12).
    pub collateral_sompi: Option<u64>,
    /// The deposit this choice needs, as far as it is known before the node says.
    pub approx_sompi: u64,
    /// Floor claims this collateral holds at once ([`TESTNET12_FLOOR_CLAIM_SOMPI`]); `None` off
    /// testnet-12.
    pub floor_claims_at_once: Option<u64>,
    /// 8k claims it holds at once.
    pub claims_8k_at_once: Option<u64>,
    pub note: String,
}

/// The background job that runs the registration to the end, as the UI polls it.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BondJob {
    pub running: bool,
    /// What it is doing now, in a sentence.
    pub step: Option<String>,
    /// Why it stopped, when it did not finish.
    pub error: Option<String>,
    /// Every step it took, oldest first.
    pub history: Vec<String>,
    /// The capability declaration's transaction, once filed.
    pub declaration_txid: Option<String>,
}

#[derive(Serialize)]
pub struct BondSetup {
    pub network: NodeNetwork,
    pub phase: BondPhase,
    pub key_path: Option<String>,
    pub key_present: bool,
    /// The key's funding address — where to send MSK.
    pub address: Option<String>,
    /// `cli` when the `misaka` CLI derived it from the key file, `node` when it came from the
    /// node's own line.
    pub address_source: Option<&'static str>,
    pub address_error: Option<String>,
    /// `None` when the node could not be asked (not running, no utxo index, not synced).
    pub funds: Option<Funds>,
    pub funds_error: Option<String>,
    /// The chain's floor for a producer bond, where this build knows it (testnet-12).
    pub floor_sompi: Option<u64>,
    pub margin_sompi: u64,
    pub recommended_margin_sompi: u64,
    /// Collateral per floor claim held at once, where this build knows it (testnet-12).
    pub floor_claim_sompi: Option<u64>,
    pub choices: Vec<CollateralChoice>,
    /// The collateral the next registration run locks (`node.bond_collateral_sompi`); `None` lets
    /// the node size it.
    pub collateral_sompi: Option<u64>,
    /// What the registration run says it will spend ("send at least N sompi plus a fee"), once it
    /// has said it — the number a deposit has to reach.
    pub node_wanted_sompi: Option<u64>,
    /// The node's whole-claim-lifetime sizing, when it printed it (it does so for a named amount
    /// below it).
    pub node_lifetime_sompi: Option<u64>,
    /// `node.producer_bond`.
    pub bond: Option<String>,
    /// The bond the node reported for this key, before the settings carry it.
    pub reported_bond: Option<String>,
    /// The registration run's own reason for waiting.
    pub registration_wait: Option<String>,
    /// The classes the chain lists this bond as declaring (`getPalwClaims`); `None` when there is
    /// no bond or the node could not be asked.
    pub declared: Option<Vec<String>>,
    pub second_deposit_sompi: u64,
    pub job: BondJob,
}

fn job() -> &'static Mutex<BondJob> {
    static JOB: OnceLock<Mutex<BondJob>> = OnceLock::new();
    JOB.get_or_init(|| Mutex::new(BondJob::default()))
}

/// Whether a bond setup is running — the node's own start/stop routes refuse while one is, because
/// a restart in the middle of it is a second process under one bond, or a lost declaration.
pub fn job_running() -> bool {
    job().lock().expect("job lock").running
}

/// Take the job, or say someone else has it. One lock, one check-and-set: a double click or a
/// second tab must not start two finishers.
fn claim_job() -> Result<()> {
    let mut job = job().lock().expect("job lock");
    if job.running {
        return Err(Error::bad_request("a bond setup is already running — its progress is on this card"));
    }
    *job = BondJob { running: true, ..Default::default() };
    Ok(())
}

fn job_done(line: String) {
    tracing::info!("[bond setup] {line}");
    let mut job = job().lock().expect("job lock");
    job.history.push(line);
    job.running = false;
    job.step = None;
}

fn job_step(step: impl Into<String>) {
    let step = step.into();
    tracing::info!("[bond setup] {step}");
    let mut job = job().lock().expect("job lock");
    job.history.push(step.clone());
    job.step = Some(step);
}

fn job_fail(error: impl Into<String>) {
    let error = error.into();
    tracing::warn!("[bond setup] stopped: {error}");
    let mut job = job().lock().expect("job lock");
    job.history.push(format!("stopped: {error}"));
    job.error = Some(error);
    job.running = false;
    job.step = None;
}

/// (key path, network) → address. The derivation is a pure function of the file, so it is asked of
/// the CLI once and not on every poll.
fn address_cache() -> &'static Mutex<Option<(PathBuf, NodeNetwork, String)>> {
    static CACHE: OnceLock<Mutex<Option<(PathBuf, NodeNetwork, String)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// The `misaka` CLI with this Studio's network and endpoint — the same two global flags every
/// signing call here must share with the readiness reads.
fn cli(settings: &NodeSettings) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(NodeManager::resolve_misaka_cli(settings.misaka_cli_path.as_ref()));
    command.arg("--network").arg(settings.network.id()).arg("--output").arg("json");
    if let Some(rpc) = &settings.misaka_rpc {
        command.arg("--rpc").arg(rpc);
    }
    command
}

/// Run a CLI command to completion; its JSON on success, its own sentence on refusal.
async fn run_cli(mut command: tokio::process::Command, what: &str) -> std::result::Result<String, String> {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(CLI_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("the `misaka` CLI did not finish {what} within {} s", CLI_TIMEOUT.as_secs()))?
        .map_err(|e| {
            format!(
                "could not run the `misaka` CLI for {what}: {e}. Put it beside the Studio or on PATH, or set node.misaka_cli_path."
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let said = super::model_market::field(&stderr, "error")
        .or_else(|| super::model_market::field(&stdout, "error"))
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| if stderr.trim().is_empty() { stdout.trim().to_string() } else { stderr.trim().to_string() });
    Err(format!("{what} was refused: {said}"))
}

async fn derive_address(settings: &NodeSettings, key: &PathBuf) -> std::result::Result<String, String> {
    if let Some((path, network, address)) = address_cache().lock().expect("cache").as_ref()
        && path == key
        && *network == settings.network
    {
        return Ok(address.clone());
    }
    let mut command = cli(settings);
    command.arg("key").arg("address").arg("--key-file").arg(key);
    let stdout = run_cli(command, "deriving the key's address").await?;
    let address = super::model_market::field(&stdout, "address")
        .and_then(|v| v.as_str().map(str::to_string))
        .ok_or_else(|| format!("`misaka key address` printed no address: {}", stdout.trim()))?;
    *address_cache().lock().expect("cache") = Some((key.clone(), settings.network, address.clone()));
    Ok(address)
}

/// The node the Studio talks to: its supervised one, or the configured attach URL.
fn node_url(settings: &NodeSettings) -> String {
    normalize_rpc_url(settings.rpc_url.as_deref().unwrap_or(""), settings.network)
}

/// One `getUtxosByAddresses` entry, as far as this module reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Utxo {
    outpoint: String,
    amount: u64,
    coinbase: bool,
}

fn utxos_from(value: &Value) -> Vec<Utxo> {
    let Some(entries) = value.get("entries").and_then(Value::as_array) else { return Vec::new() };
    entries
        .iter()
        .filter_map(|entry| {
            let outpoint = entry.get("outpoint")?;
            let txid = outpoint.get("transactionId").and_then(Value::as_str)?;
            let index = outpoint.get("index").and_then(Value::as_u64)?;
            let utxo = entry.get("utxoEntry")?;
            Some(Utxo {
                outpoint: format!("{txid}:{index}"),
                amount: utxo.get("amount").and_then(Value::as_u64).unwrap_or(0),
                coinbase: utxo.get("isCoinbase").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

fn funds_of(utxos: &[Utxo]) -> Funds {
    let mut funds = Funds::default();
    for utxo in utxos {
        funds.total_sompi = funds.total_sompi.saturating_add(utxo.amount);
        if utxo.coinbase {
            funds.coinbase_sompi = funds.coinbase_sompi.saturating_add(utxo.amount);
        } else {
            funds.outputs += 1;
            if utxo.amount > funds.largest_output_sompi {
                funds.second_output_sompi = funds.largest_output_sompi;
                funds.largest_output_sompi = utxo.amount;
            } else {
                funds.second_output_sompi = funds.second_output_sompi.max(utxo.amount);
            }
        }
    }
    funds
}

async fn utxos_at(settings: &NodeSettings, address: &str) -> Result<Vec<Utxo>> {
    let value =
        wrpc_call(&node_url(settings), "getUtxosByAddresses", serde_json::json!({ "addresses": [address] }), Duration::from_secs(5))
            .await?;
    Ok(utxos_from(&value))
}

/// The amounts to offer. On testnet-12, named amounts from the producer floor up, each with the
/// claims it holds at once; the node's own default is never offered there (see
/// [`TESTNET12_NODE_DEFAULT_FLOOR_SOMPI`]). Elsewhere the node's sizing is the only thing known.
fn choices(network: NodeNetwork) -> Vec<CollateralChoice> {
    if floor_for(network).is_none() {
        return vec![CollateralChoice {
            label: "The node sizes it".into(),
            collateral_sompi: None,
            approx_sompi: 0,
            floor_claims_at_once: None,
            claims_8k_at_once: None,
            note: "This network's amounts are not known to this build; the node derives one and says it once it runs.".into(),
        }];
    }
    [
        (
            "The chain's minimum",
            13_000,
            "The least a producer bond may lock. Holds 2 floor claims at once; a full bond waits for one to be licensed or Final.",
        ),
        ("Room for about 5 claims", 32_100, "More claims in flight at once, so fewer waits while panels judge."),
        ("Room for about 10 claims", 64_100, "For a machine that produces continuously."),
    ]
    .into_iter()
    .map(|(label, msk_, note)| {
        let collateral = msk_ * SOMPI_PER_MSK;
        CollateralChoice {
            label: label.into(),
            collateral_sompi: Some(collateral),
            approx_sompi: collateral,
            floor_claims_at_once: Some(collateral / TESTNET12_FLOOR_CLAIM_SOMPI),
            claims_8k_at_once: Some(collateral / TESTNET12_8K_CLAIM_SOMPI),
            note: note.into(),
        }
    })
    .collect()
}

fn floor_for(network: NodeNetwork) -> Option<u64> {
    (network == NodeNetwork::Testnet12).then_some(TESTNET12_PRODUCER_FLOOR_SOMPI)
}

/// The phase, from the facts. Pure, so the order of the questions is testable.
fn phase_of(
    key_present: bool,
    bond: Option<&str>,
    declared: Option<bool>,
    job_running: bool,
    registering: bool,
    funds: Option<&Funds>,
    needed_sompi: u64,
) -> BondPhase {
    if bond.is_some() && !job_running {
        // Only a registry that answered "not declared" says so; a node that could not be asked is
        // not evidence against a bond that may well be declared.
        return if declared == Some(false) { BondPhase::NeedsDeclaration } else { BondPhase::Bonded };
    }
    if job_running && bond.is_some() {
        return BondPhase::Finishing;
    }
    if job_running || registering {
        return BondPhase::Registering;
    }
    if !key_present {
        return BondPhase::NeedKey;
    }
    match funds {
        Some(funds) if funds.largest_output_sompi >= needed_sompi => BondPhase::ReadyToRegister,
        _ => BondPhase::NeedFunds,
    }
}

async fn status(State(state): State<Arc<AppState>>) -> Json<BondSetup> {
    let settings = state.settings.read().await.node.clone();
    let key_path = settings.producer_key_path.clone();
    let key_present = key_path.as_ref().is_some_and(|p| p.is_file());
    let facts = state.node.registration_facts();
    let (reported_bond, registration_wait) = (facts.bond.clone(), facts.wait.clone());

    let (mut address, mut address_source, mut address_error) = (None, None, None);
    if let (Some(key), true) = (&key_path, key_present) {
        match derive_address(&settings, key).await {
            Ok(a) => (address, address_source) = (Some(a), Some("cli")),
            Err(e) => address_error = Some(e),
        }
    }
    if address.is_none()
        && let Ok(view) = state.node.view(&settings).await
        && let Some(a) = view.pay_address
    {
        (address, address_source) = (Some(a), Some("node"));
    }

    let (funds, funds_error) = match &address {
        Some(a) => match utxos_at(&settings, a).await {
            Ok(utxos) => (Some(funds_of(&utxos)), None),
            Err(e) => (None, Some(format!("the node could not be asked for this address's funds: {e}"))),
        },
        None => (None, None),
    };

    // What a deposit must reach: the node's own words when it has said them, else the named amount,
    // else the last measured sizing.
    let floor_id = floor_class_id(settings.network);
    let declared = match &settings.producer_bond {
        Some(bond) => declared_classes(&settings, bond).await,
        None => None,
    };
    let declared_ok = match (&declared, floor_id) {
        (Some(list), Some(floor)) => Some(list.iter().any(|c| c.eq_ignore_ascii_case(floor))),
        _ => None,
    };
    let collateral = facts.amounts.wanted_sompi.or(settings.bond_collateral_sompi).or(floor_for(settings.network)).unwrap_or(0);
    let job = job().lock().expect("job lock").clone();
    let registering = state.node.supervised_role().await == Some(NetworkRole::Producer) && settings.producer_bond.is_none();
    let phase = phase_of(
        key_present,
        settings.producer_bond.as_deref(),
        declared_ok,
        job.running,
        registering,
        funds.as_ref(),
        collateral.saturating_add(REGISTRATION_MARGIN_SOMPI),
    );

    Json(BondSetup {
        network: settings.network,
        phase,
        key_path: key_path.map(|p| p.display().to_string()),
        key_present,
        address,
        address_source,
        address_error,
        funds,
        funds_error,
        floor_sompi: floor_for(settings.network),
        margin_sompi: REGISTRATION_MARGIN_SOMPI,
        floor_claim_sompi: floor_for(settings.network).map(|_| TESTNET12_FLOOR_CLAIM_SOMPI),
        recommended_margin_sompi: RECOMMENDED_MARGIN_SOMPI,
        choices: choices(settings.network),
        collateral_sompi: settings.bond_collateral_sompi,
        node_wanted_sompi: facts.amounts.wanted_sompi,
        node_lifetime_sompi: facts.amounts.lifetime_sompi,
        bond: settings.producer_bond.clone(),
        reported_bond,
        registration_wait,
        declared,
        second_deposit_sompi: SECOND_DEPOSIT_SOMPI,
        job,
    })
}

#[derive(Deserialize)]
struct RegisterBody {
    /// `None` lets the node size the bond (recommended).
    #[serde(default)]
    collateral_sompi: Option<u64>,
}

/// Start the registration run with the chosen collateral, and see it through to a producing node.
async fn register(State(state): State<Arc<AppState>>, Json(body): Json<RegisterBody>) -> Result<Json<BondSetup>> {
    let settings = state.settings.read().await.clone();
    let node = &settings.node;
    if job_running() {
        return Err(Error::bad_request("a bond setup is already running — its progress is on this card"));
    }
    if let Some(bond) = &node.producer_bond {
        return Err(Error::bad_request(format!(
            "this Studio already produces with bond {bond}. One key registers one bond for the life of the chain; \
             a second bond needs a new key (clear the bond outpoint and the key file in Network settings first)"
        )));
    }
    let Some(key) = node.producer_key_path.clone().filter(|p| p.is_file()) else {
        return Err(Error::bad_request("there is no producer key yet — generate one first"));
    };
    if node.rpc_url.is_some() {
        return Err(Error::bad_request(
            "this Studio is attached to another node (Attach to RPC). Only a node started here can run the registration — \
             clear that field, or register on that node with `kaspad --palw-register-bond`",
        ));
    }
    // testnet-12: always a named amount. The node's own default is a legacy formula — ≈ 31,191 MSK
    // for the floor and ≈ 2 billion MSK for the 8k class — not the chain's rule.
    if node.network == NodeNetwork::Testnet12 && body.collateral_sompi.is_none() {
        return Err(Error::bad_request(
            "name the collateral on testnet-12 (13,000 MSK or more): the node's own default is a legacy formula, \
             ≈ 31,191 MSK for the floor and ≈ 2 billion MSK for the 8k class",
        ));
    }
    if let (Some(floor), Some(named)) = (floor_for(node.network), body.collateral_sompi)
        && named < floor
    {
        return Err(Error::bad_request(format!(
            "{} MSK is below {}'s producer floor of {} MSK — the registration would be refused",
            msk(named),
            node.network.id(),
            msk(floor)
        )));
    }
    // A check where one can be made: a node that answers says what the address holds. A node that
    // is not running yet cannot, and the registration run itself then waits for the funds and says
    // so in its own words — refusing here would make the first start impossible.
    // Only for a named amount: the node's own sizing is the node's to check, and it says what it
    // wants in its first waiting line.
    if let Some(named) = body.collateral_sompi
        && let Ok(address) = derive_address(node, &key).await
        && let Ok(utxos) = utxos_at(node, &address).await
    {
        let funds = funds_of(&utxos);
        let needed = named.saturating_add(REGISTRATION_MARGIN_SOMPI);
        if funds.largest_output_sompi < needed {
            return Err(Error::bad_request(format!(
                "registration spends ONE output, and the largest at {address} holds {} MSK against the {} MSK needed \
                 (collateral + {} MSK). {}",
                msk(funds.largest_output_sompi),
                msk(needed),
                msk(REGISTRATION_MARGIN_SOMPI),
                if funds.total_sompi >= needed {
                    "The address holds enough in total — merge its outputs first (`misaka wallet utxo consolidate --key-file <key> --yes`)."
                } else {
                    "Send more MSK to it."
                }
            )));
        }
    }

    // Claimed here, after every check and before the first side effect: from this line on, a second
    // Register, Finish, Start or Stop is refused until this one is done.
    claim_job()?;
    let mut next = settings.clone();
    next.node.role = NetworkRole::Producer;
    next.node.bond_collateral_sompi = body.collateral_sompi;
    next.node.producer_bond = None;
    // The float the old chain's carrier left is not this chain's; the registration writes its own.
    next.node.fee_outpoint = None;
    let applied = match state.apply_settings(next).await {
        Ok(applied) => applied,
        Err(e) => {
            job_fail(format!("could not save the settings: {e}"));
            return Err(e);
        }
    };

    job_step(match body.collateral_sompi {
        Some(named) => format!("starting the registration run with {} MSK of collateral", msk(named)),
        None => "starting the registration run; the node sizes the collateral itself".to_string(),
    });

    // One node on this data directory, ever: whatever runs now (a verifier, an old producer) stops
    // first. Two processes under one appdir corrupt it, and two under one bond are slashed.
    if let Err(e) = state.node.stop().await {
        job_fail(format!("could not stop the running node: {e}"));
        return Err(e);
    }
    let mut node_settings = applied.node.clone();
    if node_settings.class_artifact.is_none() {
        node_settings.class_artifact = super::network::default_class_artifact(node_settings.network, &applied.models_dir).await;
    }
    if let Err(e) = state.node.start(&node_settings).await {
        job_fail(format!("the registration run did not start: {e}"));
        return Err(e);
    }
    job_step("waiting for the node to sync, see the funds and confirm the bond carrier");

    let watcher = state.clone();
    tokio::spawn(async move { watch_registration(watcher).await });
    Ok(status(State(state)).await)
}

/// Finish by hand: the bond is known (from the node or the settings) and the automatic run stopped
/// or was never started — a Studio restarted mid-setup, a declaration that timed out.
async fn finish(State(state): State<Arc<AppState>>) -> Result<Json<BondSetup>> {
    let settings = state.settings.read().await.node.clone();
    let Some(bond) = settings.producer_bond.clone().or(state.node.registration_facts().bond) else {
        return Err(Error::bad_request("no bond is known yet — register one first"));
    };
    if settings.rpc_url.is_some() {
        return Err(Error::bad_request("this Studio is attached to another node; finish the bond on that node"));
    }
    claim_job()?;
    let worker = state.clone();
    tokio::spawn(async move { complete(worker, bond).await });
    Ok(status(State(state)).await)
}

async fn watch_registration(state: Arc<AppState>) {
    let started = std::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if let Some(bond) = state.node.registration_facts().bond {
            job_step(format!("the node registered bond {bond}"));
            return complete(state, bond).await;
        }
        if state.node.supervised_role().await.is_none() {
            return job_fail("the registration node stopped before it reported a bond — its log says why");
        }
        if started.elapsed() > REGISTRATION_WATCH {
            return job_fail(
                "no bond after six hours. The node is still running and will register once it can; this card stops \
                 watching — press Finish once the node reports the bond",
            );
        }
    }
}

/// The floor's full class id on `network`, where this build's table knows it.
fn floor_class_id(network: NodeNetwork) -> Option<&'static str> {
    classes_for(network).iter().find(|c| c.is_base && c.class_id_complete).map(|c| c.class_id_hex)
}

/// The classes the chain's registry lists `bond` as declaring — `getPalwClaims(bond, "seat")`'s
/// `bondCapableClasses`, the read `misaka mining setup` waits on. `None` when the node could not be
/// asked or holds no such bond.
async fn declared_classes(node: &NodeSettings, bond: &str) -> Option<Vec<String>> {
    let params = serde_json::json!({ "bond": bond, "role": "seat", "includeTerminal": false, "limit": 1 });
    let value = wrpc_call(&node_url(node), "getPalwClaims", params, Duration::from_secs(5)).await.ok()?;
    if !value.get("available").and_then(Value::as_bool).unwrap_or(false)
        || !value.get("bondKnown").and_then(Value::as_bool).unwrap_or(false)
    {
        return None;
    }
    Some(value.get("bondCapableClasses")?.as_array()?.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
}

/// An output at the address that can pay the declaration: ordinary, big enough for a carrier, not
/// the bond, and not the registration's change (`<carrier>:1`, the node's reserved fee float).
fn declaration_funding(utxos: &[Utxo], bond: &str) -> Option<Utxo> {
    let carrier = bond.split_once(':').map(|(txid, _)| txid).unwrap_or(bond);
    let change = format!("{carrier}:1");
    utxos
        .iter()
        .filter(|u| !u.coinbase && u.amount >= CARRIER_FUNDING_MIN_SOMPI && u.outpoint != bond && u.outpoint != change)
        .max_by_key(|u| u.amount)
        .cloned()
}

/// Bond known → settings → a second output → the declaration (through the still-running
/// registration node) → the registry lists it → producing node.
async fn complete(state: Arc<AppState>, bond: String) {
    let settings = state.settings.read().await.clone();
    if settings.node.producer_bond.as_deref() != Some(bond.as_str()) {
        let mut next = settings.clone();
        next.node.producer_bond = Some(bond.clone());
        next.node.role = NetworkRole::Producer;
        if let Err(e) = state.apply_settings(next).await {
            return job_fail(format!("could not save the bond outpoint {bond}: {e}"));
        }
        job_step(format!("saved {bond} as the producer bond"));
    }
    let node = state.settings.read().await.node.clone();
    let key = node.producer_key_path.clone().unwrap_or_default();

    // **The floor only.** A seat is convicted for a class it declared and cannot serve, and this
    // app cannot tell that it can serve the 8k class (the file's root and the memory for a replay
    // are the node's to prove); `misaka palw panel join --class --artifact` is the move for that.
    match floor_class_id(node.network) {
        None => job_step(format!(
            "no full floor class id is known for {} — declare this bond's capability yourself (`misaka bond capability --declare …`)",
            node.network.id()
        )),
        Some(floor) => {
            let already = declared_classes(&node, &bond).await.is_some_and(|c| c.iter().any(|id| id.eq_ignore_ascii_case(floor)));
            if already {
                job_step("the chain already lists the floor as declared for this bond");
            } else {
                if let Err(e) = wait_for_declaration_funding(&node, &key, &bond).await {
                    return job_fail(e);
                }
                job_step("declaring the floor as what this bond judges");
                let mut command = cli(&node);
                command
                    .arg("bond")
                    .arg("capability")
                    .arg("--key-file")
                    .arg(&key)
                    .arg("--bond")
                    .arg(&bond)
                    .arg("--class-id")
                    .arg(floor)
                    .arg("--declare")
                    .arg(floor)
                    .arg("--yes");
                match run_cli(command, "the capability declaration").await {
                    Ok(stdout) => {
                        let txid = super::model_market::field(&stdout, "txid").and_then(|v| v.as_str().map(str::to_string));
                        job().lock().expect("job lock").declaration_txid = txid.clone();
                        job_step(format!(
                            "declaration {} filed; waiting for the chain to list it before the restart",
                            txid.as_deref().unwrap_or("(no txid printed)")
                        ));
                    }
                    Err(e) => return job_fail(format!("{e}. The bond is saved; press Finish to try the declaration again")),
                }
                if !wait_for_declared(&node, &bond, floor).await {
                    // Not restarting is the point: the declaration is in THIS node's mempool, and a
                    // restart would drop it.
                    return job_fail(format!(
                        "the chain has not listed the declaration after {} minutes. The registration node is left running so it keeps \
                         the declaration; press Finish to check again",
                        DECLARATION_WATCH.as_secs() / 60
                    ));
                }
                job_step("the chain lists the floor as declared");
            }
        }
    }

    job_step("restarting the node as a producer");
    // One process per bond: the registration node is gone before the producing one starts.
    if let Err(e) = state.node.stop().await {
        return job_fail(format!("could not stop the registration node: {e}"));
    }
    let applied = state.settings.read().await.clone();
    let mut node_settings = applied.node.clone();
    if node_settings.class_artifact.is_none() {
        node_settings.class_artifact = super::network::default_class_artifact(node_settings.network, &applied.models_dir).await;
    }
    if let Err(e) = state.node.start(&node_settings).await {
        return job_fail(format!("the producing node did not start: {e}"));
    }
    job_done(format!("producing with bond {bond}; its fee float is the one the registration saved, or what the node's scan finds"));
}

/// Wait until the address holds an output the declaration can spend.
async fn wait_for_declaration_funding(node: &NodeSettings, key: &PathBuf, bond: &str) -> std::result::Result<(), String> {
    let address = derive_address(node, key).await?;
    let started = std::time::Instant::now();
    let mut asked = false;
    while started.elapsed() < SECOND_DEPOSIT_WATCH {
        if let Ok(utxos) = utxos_at(node, &address).await
            && declaration_funding(&utxos, bond).is_some()
        {
            return Ok(());
        }
        if !asked {
            job_step(format!(
                "waiting for a second deposit of about {} MSK at {address}: the registration's change is the node's reserved fee \
                 float, so the declaration needs an output of its own",
                msk(SECOND_DEPOSIT_SOMPI)
            ));
            asked = true;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    Err("no second deposit arrived within six hours. The bond is saved; send about 1 MSK to the address and press Finish".into())
}

/// Wait until the registry lists `class` among the bond's declared classes. `false` on timeout.
async fn wait_for_declared(node: &NodeSettings, bond: &str, class: &str) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < DECLARATION_WATCH {
        if declared_classes(node, bond).await.is_some_and(|c| c.iter().any(|id| id.eq_ignore_ascii_case(class))) {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    false
}

fn msk(sompi: u64) -> String {
    let whole = sompi / SOMPI_PER_MSK;
    let frac = sompi % SOMPI_PER_MSK;
    if frac == 0 { whole.to_string() } else { format!("{whole}.{}", format!("{frac:08}").trim_end_matches('0')) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_largest_ordinary_output_is_what_a_registration_can_spend() {
        let value = serde_json::json!({ "entries": [
            { "outpoint": { "transactionId": "aa", "index": 0 }, "utxoEntry": { "amount": 5_000 * SOMPI_PER_MSK, "isCoinbase": false } },
            { "outpoint": { "transactionId": "bb", "index": 1 }, "utxoEntry": { "amount": 9_000 * SOMPI_PER_MSK, "isCoinbase": false } },
            { "outpoint": { "transactionId": "cc", "index": 0 }, "utxoEntry": { "amount": 20_000 * SOMPI_PER_MSK, "isCoinbase": true } },
        ]});
        let utxos = utxos_from(&value);
        assert_eq!(utxos[1].outpoint, "bb:1");
        let funds = funds_of(&utxos);
        assert_eq!(funds.second_output_sompi, 5_000 * SOMPI_PER_MSK);
        assert_eq!(funds.total_sompi, 34_000 * SOMPI_PER_MSK);
        assert_eq!(funds.largest_output_sompi, 9_000 * SOMPI_PER_MSK, "a reward is not counted toward registering");
        assert_eq!(funds.coinbase_sompi, 20_000 * SOMPI_PER_MSK);
        assert_eq!(funds.outputs, 2);
    }

    #[test]
    fn the_phase_follows_the_facts_in_order() {
        let enough = Funds { largest_output_sompi: 14_000 * SOMPI_PER_MSK, ..Default::default() };
        let short = Funds { total_sompi: 14_000 * SOMPI_PER_MSK, largest_output_sompi: 7_000 * SOMPI_PER_MSK, ..Default::default() };
        let need = 13_000 * SOMPI_PER_MSK + REGISTRATION_MARGIN_SOMPI;
        assert_eq!(phase_of(false, None, None, false, false, None, need), BondPhase::NeedKey);
        assert_eq!(phase_of(true, None, None, false, false, None, need), BondPhase::NeedFunds, "an unanswered node is not funds");
        assert_eq!(phase_of(true, None, None, false, false, Some(&short), need), BondPhase::NeedFunds, "one output, not the total");
        assert_eq!(phase_of(true, None, None, false, false, Some(&enough), need), BondPhase::ReadyToRegister);
        assert_eq!(phase_of(true, None, None, false, true, Some(&enough), need), BondPhase::Registering);
        assert_eq!(phase_of(true, Some("x:0"), None, true, true, None, need), BondPhase::Finishing);
        assert_eq!(
            phase_of(true, Some("x:0"), None, false, false, None, need),
            BondPhase::Bonded,
            "an unanswered registry is not evidence"
        );
        assert_eq!(phase_of(true, Some("x:0"), Some(true), false, false, None, need), BondPhase::Bonded);
        assert_eq!(phase_of(true, Some("x:0"), Some(false), false, false, None, need), BondPhase::NeedsDeclaration);
    }

    /// The join guide's re-checked figures: 13,000 holds 2, 31,191 holds 4, 100,000 holds 15 (floor).
    #[test]
    fn named_amounts_are_offered_with_the_claims_they_hold() {
        let offered = choices(NodeNetwork::Testnet12);
        assert!(offered.iter().all(|c| c.collateral_sompi.is_some()), "never the node's legacy default on testnet-12");
        assert_eq!(offered[0].collateral_sompi, Some(TESTNET12_PRODUCER_FLOOR_SOMPI));
        assert_eq!(offered[0].floor_claims_at_once, Some(2));
        assert_eq!(offered[1].floor_claims_at_once, Some(5));
        assert_eq!(offered[2].floor_claims_at_once, Some(10));
        assert_eq!(offered[0].claims_8k_at_once, Some(2));
        assert_eq!(TESTNET12_NODE_DEFAULT_FLOOR_SOMPI / TESTNET12_FLOOR_CLAIM_SOMPI, 4);
        assert_eq!(100_000 * SOMPI_PER_MSK / TESTNET12_FLOOR_CLAIM_SOMPI, 15);
        assert_eq!(choices(NodeNetwork::Devnet)[0].collateral_sompi, None, "elsewhere only the node's own sizing");
    }

    /// The registration's change is the node's reserved float and the bond is the bond: neither
    /// pays the declaration, and a single-output deposit therefore leaves nothing that can.
    #[test]
    fn the_declaration_is_funded_from_a_second_output_only() {
        let bond = format!("{}:0", "ab".repeat(64));
        let change = format!("{}:1", "ab".repeat(64));
        let u = |outpoint: &str, msk_: u64, coinbase: bool| Utxo { outpoint: outpoint.into(), amount: msk_ * SOMPI_PER_MSK, coinbase };
        let one_deposit = vec![u(&bond, 31_192, false), u(&change, 1, false)];
        assert_eq!(declaration_funding(&one_deposit, &bond), None);
        let with_reward = vec![u(&bond, 31_192, false), u(&change, 1, false), u("cc:0", 5, true)];
        assert_eq!(declaration_funding(&with_reward, &bond), None, "a reward is not taken for it");
        let two = vec![u(&bond, 31_192, false), u(&change, 1, false), u("dd:0", 1, false)];
        assert_eq!(declaration_funding(&two, &bond).map(|x| x.outpoint), Some("dd:0".to_string()));
        let dust = vec![u(&bond, 31_192, false), Utxo { outpoint: "ee:0".into(), amount: 1_000, coinbase: false }];
        assert_eq!(declaration_funding(&dust, &bond), None, "too little to pay a carrier");
    }

    #[test]
    fn amounts_read_as_msk() {
        assert_eq!(msk(TESTNET12_PRODUCER_FLOOR_SOMPI), "13000");
        assert_eq!(msk(REGISTRATION_MARGIN_SOMPI), "0.1");
        assert_eq!(msk(TESTNET12_FLOOR_CLAIM_SOMPI), "6401.9080548");
    }
}
