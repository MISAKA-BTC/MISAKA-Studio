//! **The free-prompt gateway as the chat engine: the answer IS the work.**
//!
//! Every other backend here runs a model and stops. This one runs the same model under the
//! free-prompt lane (ADR-0044), so one execution produces two things that cannot disagree — the
//! text the user reads, and the commitment that prices it: schedule, trace and output roots, the
//! work leaves, and a claim id a panel seat can re-execute against.
//!
//! ```text
//! Chat ──▶ this ──▶ misaka-palw-gateway ──▶ palw-a16-fp-worker   (ONE run)
//!                          │                        │
//!                    the answer            roots · work_leaves · claim id
//! ```
//!
//! # What this backend does not do, and why it is not a gap
//!
//! **It does not spawn the gateway and it does not hold a key.** The gateway is an ordinary HTTP
//! endpoint — on this machine or a pool's — and by ADR-0079 Decision 4 it holds no signing secret
//! at all: the ML-DSA-87 signature over a claim belongs to the rail or a signer sidecar, a
//! separate process with the bond key. So a commitment produced here is adjudicable work sitting
//! in the gateway's outbox, and what carries it to the chain is the submitter beside that gateway,
//! not this process.
//!
//! **It does not choose the model.** The gateway is resident on one registered class; `load`
//! confirms it is up and reports what it holds. A model picker that appeared to switch the class
//! would be describing something that did not happen.

use super::{Availability, GenerationRequest, InferenceBackend, LoadRequest, LoadedModel, SseParser, StreamEvent};
use crate::{Error, Result};
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::RuntimeDescriptor;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// The name this backend answers to, everywhere.
pub const NAME: &str = "gateway";

/// A gateway's `/health`, as much of it as this backend reads.
#[derive(Clone, Debug, Default)]
pub struct GatewayFacts {
    pub class_id: String,
    pub bond: String,
    pub n_ctx: u32,
    /// The worker's manifest hash — what identifies the engine that will run the job. All zeros
    /// from a gateway whose worker does not publish one, and recorded as `unknown` rather than as
    /// a plausible-looking string of zeros.
    pub runtime_manifest_hash: String,
    pub can_submit: bool,
    pub fp_certified: bool,
}

pub struct GatewayBackend {
    url: String,
    /// A pool slot's token, when the gateway is reached through the pool that hosts it.
    ///
    /// Sent as a header, never in the URL: a pool gateway sits behind an HTTPS proxy, and a secret
    /// in a query string is a secret in every access log between here and the slot. A gateway on
    /// this machine needs none — which is why this is an option rather than a requirement.
    token: Option<String>,
    http: reqwest::Client,
    loaded: RwLock<Option<LoadedModel>>,
    facts: RwLock<Option<GatewayFacts>>,
}

impl GatewayBackend {
    pub fn new(url: String, token: Option<String>) -> Self {
        GatewayBackend {
            url: url.trim_end_matches('/').to_string(),
            token: token.filter(|t| !t.is_empty()),
            // No overall timeout: one free-prompt inference is a whole model over a real prompt and
            // legitimately runs for minutes. The connect timeout still makes a dead gateway quick.
            http: reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build().expect("http client builds"),
            loaded: RwLock::new(None),
            facts: RwLock::new(None),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    async fn health(&self) -> std::result::Result<GatewayFacts, String> {
        let mut request = self.http.get(format!("{}/health", self.url)).timeout(Duration::from_secs(10));
        if let Some(token) = &self.token {
            request = request.header("x-pool-token", token);
        }
        let response = request.send().await.map_err(|e| format!("{}: {e}", self.url))?;
        if !response.status().is_success() {
            return Err(format!("{} answered {}", self.url, response.status()));
        }
        let body: Value = response.json().await.map_err(|e| format!("{} did not answer JSON: {e}", self.url))?;
        let string = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let facts = GatewayFacts {
            class_id: string("class_id"),
            bond: string("bond"),
            n_ctx: body.get("n_ctx").and_then(Value::as_u64).unwrap_or(0) as u32,
            runtime_manifest_hash: string("runtime_manifest_hash"),
            can_submit: body.get("can_submit").and_then(Value::as_bool).unwrap_or(false),
            fp_certified: body.get("chain").and_then(|c| c.get("fp_certified")).and_then(Value::as_bool).unwrap_or(false),
        };
        *self.facts.write().await = Some(facts.clone());
        Ok(facts)
    }
}

impl InferenceBackend for GatewayBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn descriptor(&self) -> BoxFuture<'_, RuntimeDescriptor> {
        Box::pin(async {
            let facts = self.facts.read().await.clone().unwrap_or_default();
            let manifest = facts.runtime_manifest_hash.trim_start_matches('0');
            RuntimeDescriptor {
                backend: NAME.into(),
                // The worker's manifest is what identifies the engine that ran the job. A gateway
                // that publishes zeros has not said which build it is, and `unknown` is that fact
                // rather than a hash nothing will ever match.
                engine_commit: if manifest.is_empty() { "unknown".into() } else { facts.runtime_manifest_hash.clone() },
                engine_patch_sha256: "unknown".into(),
                engine_build_number: 0,
                build_profile: "misaka-palw-fp-gateway".into(),
                // The determinism class is the chain's, not this app's: a run under this gateway is
                // expected to agree bit-for-bit with every seat that re-executes the class.
                class_tag: if facts.class_id.is_empty() { "misaka-palw-fp/unknown-class".into() } else { facts.class_id.clone() },
            }
        })
    }

    fn availability(&self) -> BoxFuture<'_, Availability> {
        Box::pin(async {
            match self.health().await {
                Ok(facts) => Availability::Available {
                    detail: format!(
                        "class {}… · n_ctx {} · {}",
                        facts.class_id.chars().take(16).collect::<String>(),
                        facts.n_ctx,
                        if facts.fp_certified { "free-prompt lane certified" } else { "lane NOT certified on this chain" }
                    ),
                },
                Err(reason) => Availability::Unavailable {
                    reason,
                    remedy: "Start `misaka-palw-gateway` (it holds the class artifact and the worker), or point \
                             node.palw_gateway_url at one that is running."
                        .into(),
                },
            }
        })
    }

    fn load(&self, request: LoadRequest) -> BoxFuture<'_, Result<LoadedModel>> {
        Box::pin(async move {
            let started = Instant::now();
            let facts = self.health().await.map_err(|reason| Error::BackendUnavailable {
                backend: NAME.to_string(),
                reason,
                remedy: "Start the gateway, or set node.palw_gateway_url.".into(),
            })?;
            // The gateway is already resident on its class; there is nothing to load and nothing to
            // wait for. The elapsed time is the health round trip, reported as what it is rather
            // than as a load that did not happen.
            let loaded = LoadedModel {
                model_id: request.model_id,
                context_size: if facts.n_ctx > 0 { facts.n_ctx } else { request.context_size },
                gpu_layers: None,
                load_ms: started.elapsed().as_millis() as u64,
                offload: None,
            };
            *self.loaded.write().await = Some(loaded.clone());
            Ok(loaded)
        })
    }

    fn unload(&self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            // Never stops the gateway: this process did not start it, other clients may be using
            // it, and a resident 1.7 GiB artifact is not ours to drop.
            *self.loaded.write().await = None;
            Ok(())
        })
    }

    fn loaded(&self) -> BoxFuture<'_, Option<LoadedModel>> {
        Box::pin(async { self.loaded.read().await.clone() })
    }

    fn generate(&self, request: GenerationRequest) -> BoxFuture<'_, Result<BoxStream<'static, Result<StreamEvent>>>> {
        Box::pin(async move {
            let url = format!("{}/v1/chat/completions", self.url);
            // The gateway's surface is deliberately small (ADR-0077 Decision 2): messages, a decode
            // ceiling, and the stream flag. Sampling knobs are not sent because the lane's
            // execution is what a seat re-runs — a temperature the seat does not know about is a
            // claim nobody can reproduce.

            // **The ceiling has to fit the class, not the app's default.**
            //
            // A class is registered at a fixed context — 512 tokens for graph-v5@512 — and the
            // worker checks `prompt + the DECODE CEILING` against it, not `prompt + what is
            // actually generated`. So a request asking for the Studio's default 2048 is refused
            // outright however short its answer would have been, and a conversation with any
            // history behind it never gets past the first turn: "prompt 344 + decode ceiling 1024
            // exceeds max_context_tokens 512". Measured, on a chat whose second message returned
            // nothing at all.
            //
            // The prompt is estimated rather than tokenized here — the class's tokenizer lives with
            // the worker — so a margin is left for the estimate being low and for the chat
            // template's own markers.
            // Counted with the class's tokenizer when the context manager had it (then the template is
            // already in the count and a small margin is enough), estimated otherwise.
            const TEMPLATE_MARGIN_TOKENS: u64 = 24;
            const EXACT_MARGIN_TOKENS: u64 = 4;
            let (fallback_prompt_tokens, margin) = match request.prompt_tokens {
                Some(counted) => (counted, EXACT_MARGIN_TOKENS),
                None => (crate::context::tokens::TokenCounter::estimate().messages(&request.messages), TEMPLATE_MARGIN_TOKENS),
            };
            let n_ctx = self.facts.read().await.as_ref().map(|f| f.n_ctx as u64).filter(|n| *n > 0);
            let ceiling = match n_ctx {
                Some(n_ctx) => {
                    let used = fallback_prompt_tokens.saturating_add(margin);
                    match decode_ceiling(request.params.max_tokens, used, n_ctx) {
                        Some(ceiling) => ceiling,
                        None => {
                            return Err(Error::BadRequest {
                                message: format!(
                                    "this class holds {n_ctx} tokens and the conversation is already about {used}. \
                                     Start a new chat, or shorten it — the context is the class's, registered on chain, \
                                     and not something this app can raise."
                                ),
                            });
                        }
                    }
                }
                None => request.params.max_tokens,
            };

            // One request, issued as a closure because it may have to be issued twice — see the
            // refusal branch below, where the worker's own numbers give the ceiling that fits.
            let messages: Vec<serde_json::Value> =
                request.messages.iter().map(|m| serde_json::json!({ "role": m.role, "content": m.content })).collect();
            let http = self.http.clone();
            let token = self.token.clone();
            let send = move |ceiling: u64| {
                let (http, token, url, messages) = (http.clone(), token.clone(), url.clone(), messages.clone());
                async move {
                    let body = serde_json::json!({
                        "model": "misaka-palw-fp-v3",
                        "messages": messages,
                        "max_tokens": ceiling,
                        "stream": true,
                    });
                    let mut request = http.post(&url).json(&body);
                    if let Some(token) = &token {
                        request = request.header("x-pool-token", token);
                    }
                    let response = request.send().await.map_err(|e| Error::Engine {
                        backend: NAME,
                        message: format!("the gateway did not accept the request: {e}"),
                    })?;
                    if !response.status().is_success() {
                        let status = response.status();
                        let text = response.text().await.unwrap_or_default();
                        return Err(Error::Engine { backend: NAME, message: format!("gateway returned {status}: {}", text.trim()) });
                    }
                    Ok(response)
                }
            };

            let response = send(ceiling).await?;
            let claim_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
            Ok(crate::backend::mock::async_stream(move |tx| async move {
                let mut retried = false;
                let mut parser = SseParser::new(true);
                let mut byte_stream = response.bytes_stream();
                use futures_util::StreamExt;
                let mut tail = Vec::new();

                while let Some(chunk) = byte_stream.next().await {
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = tx.send(Err(Error::Engine { backend: NAME, message: format!("stream broke: {e}") })).await;
                            return;
                        }
                    };
                    // The gateway's last event carries `misaka` — the job and claim ids. It is not
                    // part of the OpenAI shape, so the parser drops it; it is logged here because a
                    // chat that produced a claim and never said which one is a chat nobody can
                    // follow to the chain.
                    tail.extend_from_slice(&chunk);
                    if !claim_seen.load(std::sync::atomic::Ordering::Relaxed)
                        && let Some(outcome) = claim_outcome_in(&tail)
                    {
                        claim_seen.store(true, std::sync::atomic::Ordering::Relaxed);
                        // The gateway says whether this answer became a claim. A claim id alone
                        // is not a commitment: it is what the job WOULD claim, reported even when
                        // the bond's room or the operator's budget kept it off the chain.
                        match outcome.committed {
                            Some(true) => tracing::info!(claim = %outcome.claim, "free-prompt claim committed"),
                            Some(false) => tracing::warn!(
                                claim = %outcome.claim,
                                reason = %outcome.not_committed_because.as_deref().unwrap_or("the gateway gave no reason"),
                                "answered, not committed: this chat is not on its way to the chain"
                            ),
                            None => {
                                tracing::info!(claim = %outcome.claim, "free-prompt claim (this gateway does not say whether it committed)")
                            }
                        }
                    }
                    for event in parser.push(&chunk) {
                        if tx.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                    // The gateway answers 200 and puts a refusal in the stream — a job over the
                    // class's context, a lane the chain does not certify. Silence would be the
                    // worst rendering of that.
                    if let Some(message) = parser.take_error() {
                        // The worker sized the request for us in the act of refusing it. One retry,
                        // and only when the numbers are there: a second refusal is a real answer.
                        if let (false, Some(room)) = (retried, ceiling_from_refusal(&message)) {
                            retried = true;
                            match send(room).await {
                                Ok(next) => {
                                    tracing::info!(ceiling = room, "retrying at the ceiling the worker named");
                                    parser = SseParser::new(true);
                                    byte_stream = next.bytes_stream();
                                    continue;
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(e)).await;
                                    return;
                                }
                            }
                        }
                        let _ = tx.send(Err(Error::Engine { backend: NAME, message })).await;
                        return;
                    }
                }
                let _ = tx.send(Ok(parser.finish(fallback_prompt_tokens))).await;
            }))
        })
    }
}

/// **The decode ceiling for one request: what was asked for, or what the class has left.**
///
/// The lane decodes to its ceiling whatever the answer's length — an end-of-generation id is a
/// DISPLAY stop and not an execution stop, because the worker's step leaves bind the executed
/// count and cannot be hashed before it is fixed — so this number is the latency of a turn as
/// much as its length.
///
/// It used to fall back to 256 whenever the ask did not fit, on the argument that room is not a
/// target. Measured against the live slot, that argument cost more than it saved: `n_ctx` is 512,
/// a one-line system prompt and a one-line question leave 446, and the app's own default ask is
/// 2048 — so EVERY chat was decided by the fallback, every answer stopped at 256, mid-sentence,
/// and nothing in the app said why. Leaving 43% of a class's context unused is not a smaller
/// default; it is a shorter answer nobody chose.
///
/// So the ask is honoured up to the room, and the control for a turn that should be quicker is
/// Max tokens — the person's, and obeyed here in both directions. `None` when the conversation has
/// already filled the class: the caller says so and stops, because a ceiling of zero is not a
/// request.
fn decode_ceiling(requested: u64, used: u64, n_ctx: u64) -> Option<u64> {
    let room = n_ctx.saturating_sub(used);
    (room > 0).then(|| requested.min(room))
}

/// **The ceiling the worker's own refusal implies.**
///
/// The refusal names all three numbers — "prompt 51 + decode ceiling 476 exceeds
/// max_context_tokens 512" — so the request that fits is arithmetic, not another guess. Retrying
/// once with it turns the one failure a person cannot act on (an empty reply) into an answer.
pub(crate) fn ceiling_from_refusal(message: &str) -> Option<u64> {
    let after = |mark: &str| -> Option<u64> {
        let rest = message.split(mark).nth(1)?;
        let digits: String = rest.trim_start().chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let prompt = after("prompt ")?;
    let ctx = after("max_context_tokens ")?;
    // One token of slack: the template can add a marker the prompt count did not include.
    ctx.checked_sub(prompt + 1).filter(|room| *room > 0)
}

/// What the gateway's last event says about this chat's claim.
#[derive(Debug, PartialEq, Eq)]
struct ClaimOutcome {
    claim: String,
    /// `None` from a gateway that does not report it.
    committed: Option<bool>,
    not_committed_because: Option<String>,
}

/// The gateway's verdict out of whatever of the stream has arrived, once its final event — the
/// one carrying `misaka` — is complete. Parsed as JSON rather than scanned: `committed` and the
/// reason ride beside the claim id, and the claim id alone read as "committed" when it was not.
fn claim_outcome_in(bytes: &[u8]) -> Option<ClaimOutcome> {
    let text = String::from_utf8_lossy(bytes);
    for event in text.split("\n\n") {
        let Some(data) = event.trim().strip_prefix("data:") else { continue };
        let Ok(value) = serde_json::from_str::<Value>(data.trim()) else { continue };
        let Some(misaka) = value.get("misaka") else { continue };
        let Some(claim) = misaka.get("fp_claim_id").and_then(Value::as_str) else { continue };
        return Some(ClaimOutcome {
            claim: claim.to_string(),
            committed: misaka.get("committed").and_then(Value::as_bool),
            not_committed_because: misaka.get("not_committed_because").and_then(Value::as_str).map(str::to_string),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal carries the arithmetic that makes the retry exact. Written against the message
    /// the live worker actually sent, because a parser written against an imagined format is a
    /// parser that silently declines to fix anything.
    #[test]
    fn a_refusal_names_the_ceiling_that_would_have_fit() {
        let refusal = "the worker refused the job: prompt 51 + decode ceiling 476 exceeds max_context_tokens 512";
        assert_eq!(ceiling_from_refusal(refusal), Some(460));
        // Nothing to take from a different failure, and nothing invented.
        assert_eq!(ceiling_from_refusal("the lane is not certified for this class"), None);
        // A prompt that fills the context on its own leaves no room, and a retry would only be a
        // second refusal.
        assert_eq!(ceiling_from_refusal("prompt 512 + decode ceiling 8 exceeds max_context_tokens 512"), None);
    }

    /// A Japanese turn is roughly one token per character, and the app's own `approximate_tokens`
    /// is a quarter of that — the gap that lost a whole request. The estimate the gateway falls
    /// back on counts the template too.
    #[test]
    fn the_prompt_estimate_does_not_undercount_japanese() {
        let counter = crate::context::tokens::TokenCounter::estimate();
        let jp = [crate::backend::ChatMessage::new("user", "東京の天気は")];
        assert!(counter.messages(&jp) >= 6 + 5 + 3, "a token per kana or kanji, plus the template's markers");
        let en = [crate::backend::ChatMessage::new("user", "weather in Tokyo")];
        assert!(counter.messages(&en) >= 4 + 5 + 3, "ascii is cheaper, but never free");
    }

    /// The measured chat that started this: slot-06 reports `n_ctx` 512, the settings carry a
    /// one-line Japanese system prompt, and the question was one line — about 66 tokens with the
    /// margin. The app asks for its default 2048, which does not fit, and the answer people saw
    /// stopped mid-sentence at 256 while 190 tokens of the class sat unused.
    #[test]
    fn an_ask_too_big_for_the_class_gets_the_room_and_not_a_fraction_of_it() {
        assert_eq!(decode_ceiling(2048, 66, 512), Some(446), "the class's room, not a default answer length");
        // An ask that fits is the ask: Max tokens is a ceiling the person set, in both directions.
        assert_eq!(decode_ceiling(384, 66, 512), Some(384));
        assert_eq!(decode_ceiling(64, 66, 512), Some(64));
        // A conversation that has eaten the context has no ceiling to offer, and the caller has to
        // say so rather than send a request for zero tokens.
        assert_eq!(decode_ceiling(2048, 512, 512), None);
        assert_eq!(decode_ceiling(2048, 900, 512), None);
    }

    #[test]
    fn the_claim_id_is_read_out_of_the_gateways_last_event() {
        let sse =
            b"data: {\"choices\":[]}\n\ndata: {\"misaka\":{\"fp_job_id\":\"aa\",\"fp_claim_id\":\"d6730d8aca86\"},\"usage\":{}}\n\n";
        assert_eq!(claim_outcome_in(sse).map(|o| o.claim).as_deref(), Some("d6730d8aca86"));
        // Nothing to find yet is not an error: the id arrives in the last event, after every delta.
        assert_eq!(claim_outcome_in(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"), None);
    }

    /// **A claim id is not a commitment.** The 2026-09-20 economy drill: the gateway answered, did
    /// not commit ("the public-job budget for this window is spent"), and the runtime logged
    /// "free-prompt claim committed" off the claim id alone.
    #[test]
    fn the_gateways_commit_verdict_is_read_beside_the_claim_id() {
        let refused = b"data: {\"misaka\":{\"fp_claim_id\":\"1d1b\",\"committed\":false,\"not_committed_because\":\"the public-job budget for this window is spent\"}}\n\n";
        assert_eq!(
            claim_outcome_in(refused),
            Some(ClaimOutcome {
                claim: "1d1b".into(),
                committed: Some(false),
                not_committed_because: Some("the public-job budget for this window is spent".into()),
            })
        );
        let committed = b"data: {\"misaka\":{\"fp_claim_id\":\"1d1b\",\"committed\":true,\"not_committed_because\":null}}\n\n";
        assert_eq!(claim_outcome_in(committed).and_then(|o| o.committed), Some(true));
        // A final event cut mid-JSON is not read until it is whole.
        assert_eq!(claim_outcome_in(b"data: {\"misaka\":{\"fp_claim_id\":\"1d1b\",\"comm"), None);
    }
}
