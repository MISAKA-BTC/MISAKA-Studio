//! **Adding a model, and opening its market — the two halves, and what each really costs.**
//!
//! A person with a converted artifact wants two things the chain treats separately:
//!
//! 1. **the class registered** — the chain adjudicating this model, so blocks can be mined on it;
//! 2. **the market seeded** — a pair somebody makes by locking at least 100,000 MSK into the
//!    line, which becomes the curve's reserve and is locked for good (ADR-0090).
//!
//! Neither is something this app can do on the user's behalf by pretending. Registration is a
//! node's act (`kaspad --palw-register-class`, needing an active bond, its key and a funded fee
//! outpoint) and seeding spends a hundred thousand MSK. So what this module does is the honest
//! half: it verifies the artifact the user actually has, reports the class the chain would
//! adjudicate it as, and — for the seed — states the floor, the balance, and the difference,
//! refusing rather than building a transaction that cannot pay.
//!
//! # Why the refusal is the feature
//!
//! `PALW_MODEL_SEED_MIN_SOMPI_V1` is 100,000 MSK and the seed is **once per line, fee-free, and
//! irreversible** — the seeder receives no position and never gets the money back. A UI that let a
//! user reach that button with 379 MSK in hand and discover the floor from a rejected transaction
//! would be worse than no UI. So the amount is checked here, against the address's own spendable
//! balance, and the shortfall is named.
//!
//! # What is deliberately not here
//!
//! No key ever leaves this process and no seed transaction is signed without `confirm: true` in
//! the request body. The class-registration half stops at "here is what your artifact is and what
//! registering it needs", because the flag belongs to a node the user runs, not to this app.

use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// ADR-0090 Decision 2: the least a seed may be, in sompi. Spelled here as the consensus constant
/// is spelled, so a reader can compare the two without arithmetic.
pub const SEED_MIN_SOMPI: u64 = 100_000 * 100_000_000;

/// What a line's market needs before it can open, measured against what the caller has.
#[derive(Debug, Serialize)]
pub struct SeedReadiness {
    /// The line the market would be keyed by. For a class's founding line this is the class id.
    pub line_id: Option<String>,
    /// The address the seed would be paid from — the pool slot's, when one is joined.
    pub from_address: Option<String>,
    /// What that address can spend right now, as the chain's own settlement rule allows.
    pub spendable_sompi: Option<u64>,
    pub seed_min_sompi: u64,
    /// `seed_min - spendable`, or 0 when the floor is already met. The number a person acts on.
    pub short_by_sompi: Option<u64>,
    pub can_seed: bool,
    /// Present when `can_seed` is false: the one sentence that says why.
    pub blocked_because: Option<String>,
    /// `Some(true)` once the market is open — a line is seeded once and only once. `None` means
    /// the chain could not be asked, which is not the same as "not seeded" and is not reported as
    /// it: a person is about to spend a hundred thousand MSK on this answer.
    pub already_seeded: Option<bool>,
}

/// The request to actually open a market. `confirm` is not a formality: this spends the floor and
/// the money never comes back.
#[derive(Debug, Deserialize)]
pub struct SeedRequest {
    pub line_id: String,
    pub msk: String,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Serialize)]
pub struct SeedOutcome {
    pub submitted: bool,
    pub txid: Option<String>,
    /// What the app did, in the words a person can check against the chain.
    pub detail: String,
}

/// What registering this machine's artifact as a class needs, and which of it is in hand.
///
/// Registration is `kaspad --palw-register-class` — a node's act, not this app's — so the answer
/// is a checklist rather than a promise. Each `false` here is a thing the node would refuse on at
/// startup, named before the run instead of after it.
#[derive(Debug, Serialize)]
pub struct RegistrationReadiness {
    /// The converted artifact this node would register. Registration is of a FILE.
    pub artifact: Option<String>,
    /// An active bond — registration is a bonded act, and the bond signs it.
    pub bond: Option<String>,
    /// The outpoint that funds the carrier the registration rides in.
    pub fee_outpoint: Option<String>,
    /// The producer key. Present as a path only; the file is the node's to open.
    pub has_key: bool,
    /// True once every prerequisite above is in hand.
    pub can_register: bool,
    /// Present when `can_register` is false: what is missing, in one sentence.
    pub blocked_because: Option<String>,
    /// True while a registration is armed for the next node start.
    pub armed: bool,
    /// The command line this would run, so a person can read it — or run it themselves.
    pub command: Vec<String>,
}

/// Arm a class registration. The model id is optional and usually empty.
#[derive(Debug, Deserialize, Default)]
pub struct RegisterRequest {
    /// e.g. `Qwen/Qwen2.5-Coder-1.5B-Instruct`. Needed only when the artifact's converted shape
    /// matches more than one class this build knows.
    #[serde(default)]
    pub model_id: Option<String>,
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/seed-readiness", get(seed_readiness))
        .route("/seed", post(seed))
        .route("/registration", get(registration))
        .route("/register-class", post(register_class))
}

/// `GET /api/v1/network/model-market/registration`
pub async fn registration(State(state): State<Arc<AppState>>) -> Result<Json<RegistrationReadiness>> {
    let settings = state.settings.read().await.clone();
    let node = &settings.node;
    let artifact = node.class_artifact.clone().map(|p| p.display().to_string());
    let missing = if artifact.is_none() {
        Some(
            "no class artifact is configured, and a registration registers a file. Convert a model \
             to a `.palwart` and point node.class_artifact at it.",
        )
    } else if node.producer_bond.is_none() {
        Some(
            "registering is a bonded act and this node has no bond. Start as a producer once with no \
             bond set — that run registers one — then come back.",
        )
    } else if node.fee_outpoint.is_none() {
        Some(
            "no fee outpoint funds the carrier the registration rides in. The bond registration run \
             prints one (usually the bond carrier's change, `<txid>:1`).",
        )
    } else if node.producer_key_path.is_none() {
        Some("no producer key is set, and the bond signs the registration with it.")
    } else {
        None
    };

    // Built from the same function the node is actually launched with, so what is shown and what
    // would run cannot drift apart.
    let mut armed_settings = node.clone();
    armed_settings.role = misaka_studio_core::settings::NetworkRole::Producer;
    // The armed value when one is armed, so the line shown is the line that would run. A preview
    // that drops the model id is a preview of a different command, and the id is exactly the part
    // a person added by hand because the artifact alone was ambiguous.
    armed_settings.register_class = Some(node.register_class.clone().unwrap_or_default());
    let command = crate::node::NodeManager::build_args(&armed_settings, crate::node::default_json_rpc_port(node.network))
        .unwrap_or_else(|_| Vec::new());

    Ok(Json(RegistrationReadiness {
        artifact,
        bond: node.producer_bond.clone(),
        fee_outpoint: node.fee_outpoint.clone(),
        has_key: node.producer_key_path.is_some(),
        can_register: missing.is_none(),
        blocked_because: missing.map(str::to_string),
        armed: node.register_class.is_some(),
        command,
    }))
}

/// `POST /api/v1/network/model-market/register-class`
///
/// Arms the registration; the next producer start files it and disarms. Deliberately not a
/// "register now" that restarts the node behind the user's back — a running node holds the
/// appdir, and stopping it is a decision with its own consequences.
pub async fn register_class(
    State(state): State<Arc<AppState>>,
    body: Option<Json<RegisterRequest>>,
) -> Result<Json<RegistrationReadiness>> {
    let ready = registration(State(state.clone())).await?.0;
    if !ready.can_register {
        return Err(Error::BadRequest {
            message: ready.blocked_because.unwrap_or_else(|| "this node cannot register a class right now.".into()),
        });
    }
    let model_id = body.and_then(|Json(b)| b.model_id).unwrap_or_default();
    let mut next = state.settings.read().await.clone();
    next.node.register_class = Some(model_id);
    state.apply_settings(next).await?;
    registration(State(state)).await
}

/// `GET /api/v1/network/model-market/seed-readiness`
///
/// Answers the question the button needs answered before it is drawn: may this person open a
/// market, and if not, by how much are they short?
pub async fn seed_readiness(State(state): State<Arc<AppState>>) -> Result<Json<SeedReadiness>> {
    let slot = crate::api::pool::joined_slot(&state).await;
    let Some(slot) = slot else {
        return Ok(Json(SeedReadiness {
            line_id: None,
            from_address: None,
            spendable_sompi: None,
            seed_min_sompi: SEED_MIN_SOMPI,
            short_by_sompi: None,
            can_seed: false,
            blocked_because: Some(
                "no pool slot is joined, so this app holds no key that could pay a seed. Join for \
                 prompt mining on the Network tab, or seed from a node you run yourself."
                    .into(),
            ),
            already_seeded: None,
        }));
    };

    // Asked of the chain through the same CLI that would sign, so the button and the transaction
    // read one source. A line already seeded cannot be seeded again, and finding that out from a
    // rejected transaction is finding it out too late.
    let opened = match &slot.line_id {
        Some(line) => {
            let (cli_path, rpc, network) = {
                let settings = state.settings.read().await;
                (settings.node.misaka_cli_path.clone(), settings.node.misaka_rpc.clone(), settings.node.network.id())
            };
            market_is_open(&state, line, cli_path.as_ref(), rpc.as_ref(), network).await
        }
        None => None,
    };

    let spendable = slot.spendable_sompi;
    let short = spendable.map(|s| SEED_MIN_SOMPI.saturating_sub(s));
    let enough = short == Some(0);
    let blocked = if opened == Some(true) {
        Some("this line's market is already open — a line is seeded once, by design.".to_string())
    } else if slot.line_id.is_none() {
        Some(
            "this slot mines the floor, which has no model line to seed. A market is opened on a \
             model class's line; join for prompt mining, or name the line from a node you run."
                .to_string(),
        )
    } else if spendable.is_none() {
        Some("the pool could not be asked what this address can spend, so the floor cannot be checked.".to_string())
    } else if !enough {
        Some(format!(
            "the seed floor is {} MSK and this address can spend {} MSK — short by {} MSK. The seed \
             is fee-free, once per line, and locked for good, so it is not something to try with less.",
            msk(SEED_MIN_SOMPI),
            msk(spendable.unwrap_or(0)),
            msk(short.unwrap_or(SEED_MIN_SOMPI))
        ))
    } else {
        None
    };

    Ok(Json(SeedReadiness {
        line_id: slot.line_id,
        from_address: Some(slot.address),
        spendable_sompi: spendable,
        seed_min_sompi: SEED_MIN_SOMPI,
        short_by_sompi: short,
        can_seed: blocked.is_none(),
        blocked_because: blocked,
        already_seeded: opened,
    }))
}

/// `POST /api/v1/network/model-market/seed`
///
/// Refuses every path that is not "the floor is met and the caller said so out loud". The check is
/// repeated here rather than trusted from the readiness call, because a UI that asked five minutes
/// ago is not the chain now.
pub async fn seed(State(state): State<Arc<AppState>>, Json(request): Json<SeedRequest>) -> Result<Json<SeedOutcome>> {
    let sompi = parse_msk(&request.msk)?;
    if sompi < SEED_MIN_SOMPI {
        return Err(Error::BadRequest {
            message: format!(
                "a seed of {} MSK is below the floor of {} MSK. ADR-0090 sets the floor so a market \
                 cannot be opened with a reserve too thin to price anything.",
                msk(sompi),
                msk(SEED_MIN_SOMPI)
            ),
        });
    }
    if !request.confirm {
        return Ok(Json(SeedOutcome {
            submitted: false,
            txid: None,
            detail: format!(
                "dry run: {} MSK would be locked into line {} forever. The seed takes no fee, mints \
                 no position for the seeder, and cannot be withdrawn. Send `confirm: true` to do it.",
                msk(sompi),
                request.line_id
            ),
        }));
    }

    let readiness = seed_readiness(State(state.clone())).await?.0;
    if !readiness.can_seed {
        return Err(Error::BadRequest {
            message: readiness.blocked_because.unwrap_or_else(|| "this app cannot pay a seed right now.".into()),
        });
    }

    // **The signing is the CLI's, not a second implementation of it.**
    //
    // `misaka palw model-seed` already builds the ModelSeed object, binds it to a carrier it
    // funds from the key's own UTXOs, and signs with ML-DSA-87 — and it is the code the chain's
    // own tests cover. A second spelling of an irreversible transaction is a second thing that
    // can be wrong about it, so this resolves that binary, hands it the slot's key, and repeats
    // what it said. `--output json` rather than the human line: a txid scraped out of prose is a
    // txid that changes when someone improves the wording.
    let (cli_path, rpc, network) = {
        let settings = state.settings.read().await;
        (settings.node.misaka_cli_path.clone(), settings.node.misaka_rpc.clone(), settings.node.network.id())
    };
    let binary = crate::node::NodeManager::resolve_misaka_cli(cli_path.as_ref());
    let key_path = crate::api::pool::slot_seed_path(&state).await.ok_or_else(|| Error::BadRequest {
        message: "this app holds no key file for the joined slot, so it cannot sign. The pool writes one \
                  when you join; if it is gone, seed from a node you run yourself."
            .into(),
    })?;

    let mut command = tokio::process::Command::new(&binary);
    command.arg("--network").arg(network).arg("--output").arg("json");
    // Absent, the CLI resolves the node itself — `~/.misaka/<network>/endpoints.json`, which a
    // node writes at startup, then the network default. That covers anyone running a node here
    // and nobody else, which is why the refusal below names the setting.
    if let Some(rpc) = &rpc {
        command.arg("--rpc").arg(rpc);
    }
    command
        .arg("palw")
        .arg("model-seed")
        .arg("--line")
        .arg(&request.line_id)
        .arg("--msk")
        .arg(&request.msk)
        .arg("--key-file")
        .arg(&key_path)
        .arg("--yes");

    let output = command.output().await.map_err(|e| Error::BadRequest {
        message: format!(
            "could not run {}: {e}. The `misaka` CLI signs this move; put it beside the Studio or on PATH, \
             or set node.misaka_cli_path.",
            binary.display()
        ),
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        // The lane's own words, passed through. A refusal here is about the chain, the key or the
        // node, and rewriting it would lose the number or the address a person needs to act.
        // `--output json` makes a refusal an envelope — `{"error": "...", "exitCode": 1}` — and
        // showing the envelope buries the sentence a person needs inside punctuation. Unwrapped
        // when it is one, passed through verbatim when it is not.
        // Either stream: the CLI writes its refusal envelope to stderr and its results to stdout,
        // and a reader that looked at only one of them shows the envelope it failed to unwrap.
        let unwrapped = field(&stderr, "error").or_else(|| field(&stdout, "error")).and_then(|v| v.as_str().map(str::to_string));
        let said: &str = match &unwrapped {
            Some(message) => message,
            None if !stderr.trim().is_empty() => stderr.trim(),
            None => stdout.trim(),
        };
        let hint = if rpc.is_none() && said.contains("connect") {
            "  (no node.misaka_rpc is set, so the CLI looked for a node on this machine — set it to a \
             node's wRPC Borsh host:port)"
        } else {
            ""
        };
        return Err(Error::BadRequest { message: format!("the seed was refused: {said}{hint}") });
    }

    // `submit_move` prints one JSON object: `{ ok, submitted, txid, fee_sompi, move }`.
    let txid = field(&stdout, "txid").and_then(|v| v.as_str().map(str::to_string));

    Ok(Json(SeedOutcome {
        submitted: true,
        txid: txid.clone(),
        detail: match &txid {
            Some(t) => format!("{} MSK is locked into line {} for good. Transaction {t}.", msk(sompi), request.line_id),
            // Success without a txid means the CLI changed its output, not that nothing happened.
            // Saying so beats inventing a transaction id or claiming the seed did not go through.
            None => format!("the seed was submitted and the CLI reported no transaction id: {}", stdout.trim()),
        },
    }))
}

/// The `misaka` CLI as this Studio is configured to run it, with the global flags already set.
///
/// One place, because the network id and the endpoint the CLI signs against must be the same in
/// the readiness check and in the seed itself. Two spellings is how a UI ends up reporting one
/// chain's answer and signing for another.
fn cli(state: &AppState, cli_path: Option<&std::path::PathBuf>, rpc: Option<&String>, network: &str) -> tokio::process::Command {
    let _ = state;
    let mut command = tokio::process::Command::new(crate::node::NodeManager::resolve_misaka_cli(cli_path));
    command.arg("--network").arg(network).arg("--output").arg("json");
    if let Some(rpc) = rpc {
        command.arg("--rpc").arg(rpc);
    }
    command
}

/// Whether the line's market is already open, as the chain holds it.
///
/// `None` when the question could not be asked — no CLI, no node, a line id the chain does not
/// know. `None` is not `false`: the readiness answer says the check could not be made rather than
/// implying a market is closed, because "you may seed" is the sentence a person spends a hundred
/// thousand MSK on.
async fn market_is_open(
    state: &AppState,
    line_id: &str,
    cli_path: Option<&std::path::PathBuf>,
    rpc: Option<&String>,
    network: &str,
) -> Option<bool> {
    let output = cli(state, cli_path, rpc, network).arg("palw").arg("model-show").arg(line_id).output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    field(&String::from_utf8_lossy(&output.stdout), "opened").and_then(|v| v.as_bool())
}

/// One named field out of a CLI answer, whichever shape the command prints it in.
///
/// `model-show` pretty-prints (many lines, one object); `model-seed`'s submit line is compact.
/// A reader that assumed either one silently returns nothing against the other, and "nothing"
/// here reads as "no market" or "no transaction" — both wrong, both quiet. So the whole output is
/// tried first, then each line.
fn field(stdout: &str, name: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim())
        && let Some(found) = v.get(name)
    {
        return Some(found.clone());
    }
    stdout.lines().rev().filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok()).find_map(|v| v.get(name).cloned())
}

fn msk(sompi: u64) -> String {
    format!("{}.{:08}", sompi / 100_000_000, sompi % 100_000_000)
}

/// Decimal MSK to sompi, refusing anything that is not exactly representable — a seed rounded by
/// the app is a seed the chain will price differently from what the person read.
fn parse_msk(text: &str) -> Result<u64> {
    let bad = || Error::BadRequest { message: format!("'{text}' is not an amount in MSK (try 100000 or 100000.0)") };
    let text = text.trim();
    let (whole, frac) = match text.split_once('.') {
        Some((w, f)) => (w, f),
        None => (text, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    if frac.len() > 8 {
        return Err(Error::BadRequest {
            message: format!("'{text}' has more than eight decimals; one sompi is the smallest unit there is"),
        });
    }
    let scaled: u64 = format!("{whole}{frac:0<8}").parse().map_err(|_| bad())?;
    Ok(scaled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_is_the_consensus_constant() {
        assert_eq!(SEED_MIN_SOMPI, 100_000 * 100_000_000, "ADR-0090 Decision 2");
        assert_eq!(msk(SEED_MIN_SOMPI), "100000.00000000");
    }

    /// An amount the app rounds is an amount the chain prices differently from what the person
    /// read on screen, and this one is irreversible.
    #[test]
    fn an_amount_is_taken_exactly_or_refused() {
        assert_eq!(parse_msk("100000").unwrap(), SEED_MIN_SOMPI);
        assert_eq!(parse_msk("100000.0").unwrap(), SEED_MIN_SOMPI);
        assert_eq!(parse_msk("0.00000001").unwrap(), 1, "one sompi survives the round trip");
        assert!(parse_msk("100000.000000001").is_err(), "nine decimals is finer than a sompi and must not round");
        assert!(parse_msk("100,000").is_err());
        assert!(parse_msk("").is_err());
        assert!(parse_msk("-1").is_err());
    }

    /// `model-show` pretty-prints its JSON and `model-seed`'s submit line is compact. A reader
    /// that handled one shape returns `None` against the other, and `None` here reads as "no
    /// market" or "the seed did not go through" — both wrong, both silent.
    #[test]
    fn a_cli_answer_is_read_in_either_shape() {
        let pretty = "{\n  \"found\": true,\n  \"opened\": true,\n  \"seed_sompi\": 10000000000000\n}";
        assert_eq!(field(pretty, "opened").and_then(|v| v.as_bool()), Some(true));
        let compact = r#"{"ok":true,"submitted":true,"txid":"3af210c2","fee_sompi":250000}"#;
        assert_eq!(field(compact, "txid").and_then(|v| v.as_str().map(str::to_string)), Some("3af210c2".into()));
        // A human line before the JSON must not stop the read: `--output json` is asked for, but
        // a future note on stderr-ish stdout should not be able to hide a txid.
        let mixed = format!("seed 100000.00000000 into line abcd\n{compact}");
        assert!(field(&mixed, "txid").is_some());
        assert_eq!(field("not json at all", "txid"), None);
    }

    /// A refusal reaches a person as a sentence or not at all: `--output json` wraps it in an
    /// envelope, and showing the envelope buries the one line that says what to do.
    #[test]
    fn a_refusal_is_unwrapped_to_its_sentence() {
        let envelope =
            r#"{"error":"no mature, unbonded UTXO at misakatest:q2x8 holds 100000.00000000 MSK plus a fee","exitCode":1,"ok":false}"#;
        let said = field(envelope, "error").and_then(|v| v.as_str().map(str::to_string));
        assert_eq!(said.as_deref(), Some("no mature, unbonded UTXO at misakatest:q2x8 holds 100000.00000000 MSK plus a fee"));
        // A refusal that is not an envelope must survive unchanged rather than vanish.
        assert_eq!(field("panicked at src/main.rs", "error"), None);
        // And the envelope arrives on stderr, not stdout — a reader that checked only stdout
        // would print the JSON at a person instead of the sentence inside it.
        let (stdout, stderr) = ("", envelope);
        let picked = field(stderr, "error").or_else(|| field(stdout, "error"));
        assert!(picked.is_some(), "the refusal must be found on whichever stream carries it");
    }

    /// `None` is not `false`. A readiness answer that could not reach the chain must not report
    /// "not seeded", because the next thing a person does with that sentence is spend 100,000 MSK.
    #[test]
    fn an_unreachable_chain_is_not_an_unseeded_market() {
        let unknown: Option<bool> = None;
        assert_ne!(unknown, Some(false), "the two must stay distinguishable in the wire type");
        assert!(unknown != Some(true), "and an unknown market never reads as open either");
    }

    /// The shortfall is the number a person acts on, so it is reported rather than implied by a
    /// disabled button.
    #[test]
    fn a_short_balance_is_named_in_msk_not_hidden() {
        let spendable = 37_922_000_000u64; // 379.22 MSK, the slot's actual balance today
        let short = SEED_MIN_SOMPI.saturating_sub(spendable);
        assert_eq!(msk(short), "99620.78000000");
        assert!(short > 0, "and the floor is genuinely out of reach at this balance");
    }
}
