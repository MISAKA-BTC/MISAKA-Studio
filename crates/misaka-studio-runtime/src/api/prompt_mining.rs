//! `/api/v1/network/prompt-mining` — the free-prompt lane (ADR-0044): the inference that answers
//! you is the inference that does the work.
//!
//! ```text
//! browser ──prompt──▶ this ──▶ misaka-palw-gateway ──▶ palw-worker (ONE run)
//!                                     │                      │
//!                              OpenAI-style answer      trace/output/schedule roots + CU
//!                                     └──────────┬───────────┘
//!                                        the same run, both halves
//! ```
//!
//! **What this module refuses to do is say the word "mined".** A commitment is not a block, and
//! the distance between them is not a detail: `palw_fp_admission_v3` admits a receipt block only
//! when the claim is *Final* — certified through bind, receipt, challenge and court windows — and
//! only under a class the chain has registered and has not frozen. A panel that showed a CU count
//! next to the word "mining" would be claiming the whole lattice from the first half of it.
//!
//! So the status this returns is layered, and each layer is answered from a fact:
//!
//! * the gateway answers `/health` — it exists, and it names the class and bond it is accountable
//!   to (a gateway that will not say is one nobody can hold to anything);
//! * that class id is compared against the ids this Studio knows for the network, and the answer
//!   distinguishes *no match* from *cannot tell* — most catalog ids here are documented prefixes,
//!   and a prefix that does not match is not proof of absence;
//! * whether anything reached the chain is reported as what it is today: nothing does. The rail
//!   builds and signs a commitment transaction and deliberately stops there.

use crate::state::AppState;
use crate::{Error, Result};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use misaka_studio_core::palw::TESTNET11_CLASSES;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// A gateway on this machine, which is where one runs when the Studio starts it. A pool-hosted
/// gateway is a URL away — the shape of the exchange is identical, which is the point of the
/// gateway being an ordinary HTTP endpoint rather than something the Studio embeds.
const DEFAULT_GATEWAY_URL: &str = "http://127.0.0.1:8790";

/// One inference is not a request to be retried. The worker holds a job lock, a 35 B model on a
/// laptop is tens of seconds, and a client timeout that fires mid-run leaves a commitment in the
/// outbox with nobody waiting for it.
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/", get(status)).route("/run", post(run))
}

fn http(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .user_agent(concat!("misaka-studio/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client builds")
}

/// What the chain does with a commitment produced here — stated as steps, because the user is
/// entitled to know which one they are on rather than being told a yes or a no.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChainReach {
    /// The commitment exists in the gateway's outbox. Nothing has been submitted: the executor
    /// rail signs a transaction and stops, by its own design.
    CommittedNotSubmitted,
}

/// Whether the gateway's class is one this network registers.
///
/// Three answers, not two. `Unknown` is the honest one when the catalog holds a documented prefix
/// rather than a full id: a prefix that fails to match rules out nothing.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ClassMatch {
    /// The gateway's class id equals a class id this Studio holds in full.
    Registered { name: String },
    /// Every catalog id is complete and none of them is this one.
    NotRegistered,
    /// No match among the complete ids, and some catalog entries carry only a prefix — so this
    /// says "cannot tell", and names how many.
    Unknown { complete_ids: usize, total_classes: usize },
}

#[derive(Clone, Debug, Serialize)]
pub struct GatewayHealth {
    pub runtime_manifest_hash: String,
    pub template_id: String,
    /// Present from the gateway build that advertises its identity; `None` from an older one,
    /// which is itself worth showing rather than hiding behind a default.
    pub class_id: Option<String>,
    pub bond: Option<String>,
    pub operator_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PromptMiningStatus {
    pub gateway_url: String,
    /// `None` when the gateway did not answer; the string is why, verbatim.
    pub unreachable: Option<String>,
    pub health: Option<GatewayHealth>,
    pub class: Option<ClassMatch>,
}

#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub prompt: String,
    /// The decode ceiling. The gateway has its own default and cap; this rides along when given.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

/// The two halves of one run, kept together because they came from one run.
#[derive(Clone, Debug, Serialize)]
pub struct PromptMiningRun {
    pub answer: String,
    /// Compute units — what the lane prices the work at. A string because the chain's own field
    /// is a u128 and JSON numbers are not.
    pub cu: String,
    pub fp_job_id: String,
    pub trace_root: String,
    pub output_root: String,
    pub schedule_root: String,
    /// Where the commitment landed on the gateway's host.
    pub artifact: String,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub chain: ChainReach,
}

fn gateway_url(settings: &misaka_studio_core::settings::Settings) -> String {
    settings.node.palw_gateway_url.clone().unwrap_or_else(|| DEFAULT_GATEWAY_URL.to_string()).trim_end_matches('/').to_string()
}

/// Compare against the ids this build knows. Verification of a class is the artifact root and the
/// node does it; this is the client-side question "is the thing I am talking to even on the map".
fn classify(class_id: &str) -> ClassMatch {
    let complete: Vec<&misaka_studio_core::palw::PalwClassSpec> = TESTNET11_CLASSES.iter().filter(|c| c.class_id_complete).collect();
    if let Some(hit) = complete.iter().find(|c| c.class_id_hex.eq_ignore_ascii_case(class_id)) {
        return ClassMatch::Registered { name: hit.name.to_string() };
    }
    if complete.len() == TESTNET11_CLASSES.len() {
        ClassMatch::NotRegistered
    } else {
        ClassMatch::Unknown { complete_ids: complete.len(), total_classes: TESTNET11_CLASSES.len() }
    }
}

async fn status(State(state): State<Arc<AppState>>) -> Json<PromptMiningStatus> {
    let settings = state.settings.read().await.clone();
    let url = gateway_url(&settings);
    let response = http(Duration::from_secs(5)).get(format!("{url}/health")).send().await;
    let body = match response {
        Ok(r) => match r.json::<serde_json::Value>().await {
            Ok(body) => body,
            Err(e) => {
                return Json(PromptMiningStatus {
                    gateway_url: url,
                    unreachable: Some(format!("the gateway's answer was not JSON: {e}")),
                    health: None,
                    class: None,
                });
            }
        },
        Err(e) => {
            return Json(PromptMiningStatus { gateway_url: url, unreachable: Some(e.to_string()), health: None, class: None });
        }
    };
    let string = |key: &str| body.get(key).and_then(|v| v.as_str()).map(str::to_string);
    let health = GatewayHealth {
        runtime_manifest_hash: string("runtime_manifest_hash").unwrap_or_default(),
        template_id: string("template_id").unwrap_or_default(),
        class_id: string("class_id"),
        bond: string("bond"),
        operator_id: string("operator_id"),
    };
    let class = health.class_id.as_deref().map(classify);
    Json(PromptMiningStatus { gateway_url: url, unreachable: None, health: Some(health), class })
}

async fn run(State(state): State<Arc<AppState>>, Json(request): Json<RunRequest>) -> Result<Json<PromptMiningRun>> {
    if request.prompt.trim().is_empty() {
        return Err(Error::bad_request("a prompt with no text is not a job"));
    }
    let settings = state.settings.read().await.clone();
    let url = gateway_url(&settings);

    let mut payload = serde_json::json!({
        "model": "misaka-palw",
        "messages": [{ "role": "user", "content": request.prompt }],
    });
    if let Some(max) = request.max_tokens {
        payload["max_tokens"] = serde_json::json!(max);
    }

    let response = http(RUN_TIMEOUT)
        .post(format!("{url}/v1/chat/completions"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| Error::bad_request(format!("the gateway did not answer: {url}: {e}")))?;
    let status = response.status();
    let body: serde_json::Value =
        response.json().await.map_err(|e| Error::bad_request(format!("the gateway's answer was not JSON: {e}")))?;
    if !status.is_success() {
        let message = body.get("error").and_then(|e| e.as_str()).unwrap_or("unexplained");
        return Err(Error::bad_request(format!("the gateway refused ({status}): {message}")));
    }

    // The mining half. Its absence is an error rather than an empty field: a reply without it is
    // an ordinary chat completion, and presenting one as work would be the exact lie this module
    // exists to avoid.
    let misaka = body
        .get("misaka")
        .ok_or_else(|| Error::bad_request("the gateway answered without a `misaka` block — that reply is a chat, not a job"))?;
    let field = |key: &str| -> Result<String> {
        misaka
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| Error::bad_request(format!("the gateway's job block has no `{key}`")))
    };
    let answer = body
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    let usage = |key: &str| body.get("usage").and_then(|u| u.get(key)).and_then(|v| v.as_u64());

    Ok(Json(PromptMiningRun {
        answer,
        cu: field("cu")?,
        fp_job_id: field("fp_job_id")?,
        trace_root: field("trace_root")?,
        output_root: field("output_root")?,
        schedule_root: field("schedule_root")?,
        artifact: field("artifact")?,
        prompt_tokens: usage("prompt_tokens"),
        completion_tokens: usage("completion_tokens"),
        chain: ChainReach::CommittedNotSubmitted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_catalog_id_is_recognised() {
        let complete = TESTNET11_CLASSES.iter().find(|c| c.class_id_complete).expect("one class publishes its full id");
        assert_eq!(classify(complete.class_id_hex), ClassMatch::Registered { name: complete.name.to_string() });
        // Case is not identity's business: an id is the same id in either case.
        assert_eq!(classify(&complete.class_id_hex.to_uppercase()), ClassMatch::Registered { name: complete.name.to_string() });
    }

    /// The catalog carries documented prefixes for classes whose full id is not published here, so
    /// a stranger's id must come back `Unknown` rather than `NotRegistered` — "no match against a
    /// prefix" is not evidence, and reporting it as evidence is how a UI ends up lying quietly.
    #[test]
    fn an_unknown_id_is_unknown_while_any_catalog_id_is_a_prefix() {
        let stranger = "03a3c66c221fa263da9c2f9077f9eec5f5886ee11eb6132ebffccda716ad0328\
                        f88593cf7ad36cead597e337aa04c3ae7686434f34ed137686ffe2b3b76f776c";
        let complete = TESTNET11_CLASSES.iter().filter(|c| c.class_id_complete).count();
        let expected = if complete == TESTNET11_CLASSES.len() {
            ClassMatch::NotRegistered
        } else {
            ClassMatch::Unknown { complete_ids: complete, total_classes: TESTNET11_CLASSES.len() }
        };
        assert_eq!(classify(stranger), expected);
    }
}
