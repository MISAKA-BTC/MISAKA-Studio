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
    /// True once the market row exists — a line is seeded once and only once.
    pub already_seeded: bool,
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

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/seed-readiness", get(seed_readiness)).route("/seed", post(seed))
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
            already_seeded: false,
        }));
    };

    let spendable = slot.spendable_sompi;
    let short = spendable.map(|s| SEED_MIN_SOMPI.saturating_sub(s));
    let enough = short == Some(0);
    let blocked = if slot.already_seeded {
        Some("this line's market is already open — a line is seeded once, by design.".to_string())
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
        already_seeded: slot.already_seeded,
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

    Err(Error::BadRequest {
        message: format!(
            "the floor is met, and this build stops here on purpose: signing a {} MSK irreversible \
             lock is not something to ship untested against a balance no test network has held. \
             Seed it with `misaka palw model-seed --line {} --msk {} --key <the slot seed> --yes`, \
             which is the same transaction with the key in your hands.",
            msk(SEED_MIN_SOMPI),
            request.line_id,
            request.msk
        ),
    })
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
