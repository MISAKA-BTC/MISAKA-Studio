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

use super::{
    Availability, ChatMessage, GenerationRequest, InferenceBackend, LoadRequest, LoadedModel, SseParser, StreamEvent,
    approximate_tokens,
};
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
            let fallback_prompt_tokens =
                approximate_tokens(&request.messages.iter().map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n"));

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
            const TEMPLATE_MARGIN_TOKENS: u64 = 24;
            let fallback_prompt_tokens = prompt_upper_bound(&request.messages).max(fallback_prompt_tokens);
            let n_ctx = self.facts.read().await.as_ref().map(|f| f.n_ctx as u64).filter(|n| *n > 0);
            let ceiling = match n_ctx {
                Some(n_ctx) => {
                    let used = fallback_prompt_tokens.saturating_add(TEMPLATE_MARGIN_TOKENS);
                    let room = n_ctx.saturating_sub(used);
                    if room == 0 {
                        return Err(Error::BadRequest {
                            message: format!(
                                "this class holds {n_ctx} tokens and the conversation is already about {used}. \
                                 Start a new chat, or shorten it — the context is the class's, registered on chain, \
                                 and not something this app can raise."
                            ),
                        });
                    }
                    // **256 is the lane's own ceiling today, not a preference.**
                    //
                    // A free-prompt result is validated with
                    // `trace_chunk_count == ceil(decode_tokens_executed / PALW_FP_TRACE_CHUNK_EVENTS_V3)`,
                    // and that constant is 256 — while the producer hardcodes ONE chunk
                    // (`misaka-palw-base0/src/produce.rs`, the attempt lane's rule under ADR-0072
                    // Decision 8). So a run of more than 256 tokens is refused by the gateway's own
                    // binding check and its resident worker is dropped as if the transport had
                    // failed. Measured against the live gateway, one token apart:
                    //
                    //     decode=256/256 … ok
                    //     decode=257/257 … the retained-trace chunk count is not the executed shape's
                    //
                    // Asking for more cannot succeed, so it is clamped rather than sent — an
                    // opaque failure four minutes in is worse than a shorter answer now. When the
                    // producer learns the free-prompt manifest, this line is what should move.
                    const LANE_CHUNK_EVENTS: u64 = 256;
                    // Room is not a target either: given the whole remaining context this model
                    // produced 438 of 438 tokens and took 6.7 minutes for a two-line question.
                    request.params.max_tokens.min(room).min(LANE_CHUNK_EVENTS)
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
                        && let Some(claim) = claim_id_in(&tail)
                    {
                        claim_seen.store(true, std::sync::atomic::Ordering::Relaxed);
                        tracing::info!(claim = %claim, "free-prompt claim committed");
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

/// An UPPER bound on a conversation's tokens — a different job from `approximate_tokens`.
///
/// That one is "about four characters per token" and its own doc says it is never for sizing a
/// context window, which is exactly what this is for. Measured: a two-turn Japanese conversation of
/// 51 real tokens estimated as 12, the ceiling was computed from the gap, and the worker refused
/// the whole request — "prompt 51 + decode ceiling 476 exceeds max_context_tokens 512".
///
/// So: one token per non-ASCII character (CJK sits at roughly one, sometimes more), a quarter of
/// the ASCII, and the chat template's markers per message. Over-counting shortens an answer;
/// under-counting loses the request — and [`ceiling_from_refusal`] repairs the rest.
fn prompt_upper_bound(messages: &[ChatMessage]) -> u64 {
    const PER_MESSAGE_MARKERS: u64 = 8;
    messages
        .iter()
        .map(|m| {
            let ascii = m.content.chars().filter(char::is_ascii).count() as u64;
            let other = m.content.chars().count() as u64 - ascii;
            ascii.div_ceil(4) + other + PER_MESSAGE_MARKERS
        })
        .sum()
}

/// **The ceiling the worker's own refusal implies.**
///
/// The refusal names all three numbers — "prompt 51 + decode ceiling 476 exceeds
/// max_context_tokens 512" — so the request that fits is arithmetic, not another guess. Retrying
/// once with it turns the one failure a person cannot act on (an empty reply) into an answer.
fn ceiling_from_refusal(message: &str) -> Option<u64> {
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

/// The claim id out of whatever of the stream has arrived, once the gateway's final event lands.
fn claim_id_in(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let start = text.find("\"fp_claim_id\"")?;
    let rest = &text[start + "\"fp_claim_id\"".len()..];
    let open = rest.find('"')? + 1;
    let end = rest[open..].find('"')? + open;
    Some(rest[open..end].to_string())
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
    /// is a quarter of that — the gap that lost a whole request.
    #[test]
    fn the_prompt_bound_does_not_undercount_japanese() {
        let jp = [ChatMessage::new("user", "小林は誰")];
        assert!(prompt_upper_bound(&jp) >= 4 + 8, "one token per kanji, plus the template's markers");
        let en = [ChatMessage::new("user", "who is Kobayashi")];
        assert!(prompt_upper_bound(&en) >= 4, "ascii is cheaper, but never free");
    }

    #[test]
    fn the_claim_id_is_read_out_of_the_gateways_last_event() {
        let sse =
            b"data: {\"choices\":[]}\n\ndata: {\"misaka\":{\"fp_job_id\":\"aa\",\"fp_claim_id\":\"d6730d8aca86\"},\"usage\":{}}\n\n";
        assert_eq!(claim_id_in(sse).as_deref(), Some("d6730d8aca86"));
        // Nothing to find yet is not an error: the id arrives in the last event, after every delta.
        assert_eq!(claim_id_in(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"), None);
    }
}
