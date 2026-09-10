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
//! # A long thread is a chain of jobs, and the chain is reported (ADR-0096 Decision 5)
//!
//! The class's row is 512 tokens and does not move (ADR-0092 Decision 4), so a conversation that
//! outgrows it is TRIMMED — oldest turns first, the system prompt and the newest turn kept — and
//! the count is reported in `misaka.context`. When the trim would drop more than
//! `summarize_after_turns` turns, the dropped turns go to the lane first as their own job, a short
//! summary, which rides the answer's prompt as a system-level "Earlier in this conversation: …"
//! turn. When the row cuts the answer short (`finish_reason: "length"` while the request asked for
//! more), up to `continue_max_legs` follow-up jobs continue it, and their deltas join the one
//! stream the client sees. Every leg is one inference and one claim (ADR-0077 R0), listed in
//! `misaka.jobs[]` with its role; nothing here fabricates a summary, and a summary or continue leg
//! that fails is reported on its entry rather than failing the answer. [`plan_context`],
//! [`summary_job_messages`], [`with_summary`], [`continue_leg_messages`] and [`LegPlanner`] are
//! the decisions, pure; [`Lane`] is the HTTP.
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
//!
//! **It does not send sampling knobs.** The lane's execution is what a seat re-runs, and a
//! temperature the seat does not know about is a claim nobody can reproduce. What was asked and
//! what ran are printed beside each other by the caller (`misaka.sampling`, ADR-0096 Decision 4);
//! [`sampling_notice`] and [`sampling_divergence`] are that sentence's two halves.

use super::{
    Availability, ChatMessage, GenerationRequest, InferenceBackend, LegLimits, LoadRequest, LoadedModel, RuntimeFingerprint,
    SseParser, StreamEvent, Usage, fit_messages_to_budget, prompt_tokens_upper_bound, text_tokens_upper_bound,
};
use crate::{Error, Result};
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use misaka_studio_core::provenance::{RuntimeDescriptor, SamplingCommitment};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::sync::mpsc::Sender;

/// The name this backend answers to, everywhere.
pub const NAME: &str = "gateway";

/// The model id the gateway serves every job under.
pub const LANE_MODEL: &str = "misaka-palw-fp-v3";

/// Room left for the chat template's own markers, which the prompt estimate does not see.
const TEMPLATE_MARGIN_TOKENS: u64 = 24;

/// **Room is not a target.** A decode ceiling is what the model is ALLOWED to generate, and this
/// one does not reliably stop early: given the whole remaining context it produced 438 of 438
/// tokens and took 6.7 minutes for a two-line question. So an ask that does not fit the class —
/// the app's default is 2048, meant for a 32K GGUF — is sized like an answer rather than like the
/// context. A smaller ask, or a larger one that fits, is honoured as given.
///
/// This was 256 for a while, and for a different reason: a producer that hardcoded one
/// retained-trace chunk made every run past 256 tokens fail its own binding check. That is fixed
/// in the producer, and measured here at the token that used to break — decode 257/257,
/// committed — so the number is back to being a default answer length and not a wall.
const DEFAULT_ANSWER_TOKENS: u64 = 256;

/// The decode ceiling of a summary job: 120 words of English is about 160 tokens, and the lane
/// decodes to its ceiling whatever the answer's length, so this is the job's cost as much as its
/// length.
const SUMMARY_TOKENS: u64 = 160;

/// What the summary turn is expected to cost in the answer's prompt — its ceiling, the prefix,
/// the message's markers — reserved BEFORE the trim so that everything the summary does not cover
/// is exactly what was dropped, with no hole between the summary and the kept turns.
const SUMMARY_RESERVE_TOKENS: u64 = SUMMARY_TOKENS + 40;

/// The summary job's instruction, verbatim from ADR-0096 Decision 5.
pub const SUMMARY_INSTRUCTION: &str =
    "Summarize the following conversation in at most 120 words, keeping names, numbers and decisions. Reply with the summary only.";

/// How the summary rides the next prompt.
pub const SUMMARY_PREFIX: &str = "Earlier in this conversation: ";

/// The continue leg's user turn.
pub const CONTINUE_INSTRUCTION: &str = "Continue exactly where you stopped, without repeating.";

/// The knobs ADR-0096 Decision 4 says have no consensus rule on this lane and never will under
/// it: a per-lane key is the only sampler an exact court can carry (ADR-0082 Decision 11), and
/// none of these is one.
pub const NOT_A_RULE_ON_THIS_LANE: [&str; 4] = ["top_p", "top_k", "min_p", "repeat_penalty"];

/// The seed the lane draws under while `palw_fp_decode_rules` is dormant: the zero seed, 64 hex
/// characters, as the gateway spells it.
pub const GREEDY_SEED_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// **The limits object's schema** (ADR-0097 Decision 2). A gateway that names another schema is
/// read as one that published no limits, never as one whose fields mean what these meant.
pub const LIMITS_SCHEMA_V1: &str = "misaka.palw.limits.v1";

/// **What the gateway says its limits are, before the first token** — `/health`'s `limits`
/// (ADR-0097 Decision 2). Read instead of inferred: the window, the most one job decodes, the
/// prompt's byte ceiling, the tokenizer the ids are counted in, and whether the committed format
/// is served. `None` on [`GatewayFacts`] is a gateway from before ADR-0097, read the old way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GatewayLimits {
    /// `context_window` — the class's `n_ctx`, prompt and answer together.
    pub context_window: u32,
    /// `max_output_tokens` — `min(--max-decode-cap, n_ctx − 1)`: the most ONE job decodes. A
    /// larger ask is clamped at the gateway, so the lane never sends one.
    pub max_output_tokens: u32,
    /// `max_prompt_bytes` — the rendered prompt's byte ceiling.
    pub max_prompt_bytes: u64,
    /// `tokenizer_id` — which tokenizer the gateway counts in.
    pub tokenizer_id: String,
    /// `features.require_committed_format == "served"`.
    pub committed_format_served: bool,
}

/// `/health`'s `limits`, when it is there and names [`LIMITS_SCHEMA_V1`].
pub fn limits_from_health(health: &Value) -> Option<GatewayLimits> {
    let limits = health.get("limits")?;
    if limits.get("schema").and_then(Value::as_str) != Some(LIMITS_SCHEMA_V1) {
        return None;
    }
    let window = |key: &str| limits.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
    Some(GatewayLimits {
        context_window: window("context_window")?,
        max_output_tokens: window("max_output_tokens")?,
        max_prompt_bytes: limits.get("max_prompt_bytes").and_then(Value::as_u64).unwrap_or(u64::MAX),
        tokenizer_id: limits.get("tokenizer_id").and_then(Value::as_str).unwrap_or_default().to_string(),
        committed_format_served: limits.get("features").and_then(|f| f.get("require_committed_format")).and_then(Value::as_str)
            == Some("served"),
    })
}

/// **A gateway's `/health`, read** — pure, so the precedence is a test and not a comment.
///
/// With [`GatewayLimits`] present, the window IS `limits.context_window` and the committed-format
/// fence IS `limits.features.require_committed_format`: the gateway's statement of what it serves,
/// one reading rather than two that could disagree. The bare `n_ctx` and `chain.*` flag are read
/// only from a gateway that predates the limits.
pub fn facts_from_health(body: &Value) -> GatewayFacts {
    let string = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let chain_flag = |key: &str| body.get("chain").and_then(|c| c.get(key)).and_then(Value::as_bool).unwrap_or(false);
    let limits = limits_from_health(body);
    GatewayFacts {
        class_id: string("class_id"),
        bond: string("bond"),
        n_ctx: limits
            .as_ref()
            .map(|l| l.context_window)
            .unwrap_or_else(|| body.get("n_ctx").and_then(Value::as_u64).unwrap_or(0) as u32),
        runtime_manifest_hash: string("runtime_manifest_hash"),
        can_submit: body.get("can_submit").and_then(Value::as_bool).unwrap_or(false),
        fp_certified: chain_flag("fp_certified"),
        fp_decode_constraint_armed: limits
            .as_ref()
            .map(|l| l.committed_format_served)
            .unwrap_or_else(|| chain_flag("fp_decode_constraint_armed")),
        limits,
    }
}

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
    /// ADR-0096 Decision 8's fence, as the chain reports it (`chain.fp_decode_constraint_armed`).
    /// Absent from an older gateway's health is `false`, which is also the truth on every shipped
    /// network: arming needs a build that reports the field.
    pub fp_decode_constraint_armed: bool,
    /// ADR-0097 Decision 2: the limits the gateway published, or `None` from an older gateway.
    /// When present, `n_ctx` and `fp_decode_constraint_armed` above were read from it
    /// ([`facts_from_health`]).
    pub limits: Option<GatewayLimits>,
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

    /// What `/health` said, for a test that must not reach a gateway.
    #[cfg(test)]
    pub(crate) async fn set_facts_for_test(&self, facts: GatewayFacts) {
        *self.facts.write().await = Some(facts);
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
        let facts = facts_from_health(&body);
        *self.facts.write().await = Some(facts.clone());
        Ok(facts)
    }
}

impl InferenceBackend for GatewayBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    /// The address and the token — the two values whose omission from the old rebuild list was
    /// the 2026-09-05 bug. The token rides as a digest prefix, never as itself.
    fn fingerprint(&self) -> RuntimeFingerprint {
        let mut fingerprint = RuntimeFingerprint::new(NAME);
        fingerprint.url = Some(self.url.clone());
        fingerprint.token_sha256_prefix = self.token.as_deref().map(RuntimeFingerprint::token_prefix);
        fingerprint
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
                        "class {}… · n_ctx {}{} · {}",
                        facts.class_id.chars().take(16).collect::<String>(),
                        facts.n_ctx,
                        facts.limits.as_ref().map(|l| format!(" (one answer ≤ {})", l.max_output_tokens)).unwrap_or_default(),
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
            let facts = self.facts.read().await.clone();
            let n_ctx = facts.as_ref().map(|f| f.n_ctx as u64).filter(|n| *n > 0);
            // ADR-0097 Decision 2: the most one job decodes, as the gateway states it. `None` from
            // a gateway that predates its limits, where the gateway's own clamp was invisible.
            let max_output = facts.as_ref().and_then(|f| f.limits.as_ref()).map(|l| l.max_output_tokens as u64);

            // ADR-0096 Decision 3, before any job runs: an integration that needs the committed
            // guarantee must never receive an advisory lookalike. The fence is the chain's, read
            // from the gateway's health, and every shipped network has it dormant.
            if requires_committed_format(request.misaka.as_ref()) && !facts.as_ref().is_some_and(|f| f.fp_decode_constraint_armed) {
                return Err(Error::BadRequest { message: COMMITTED_FORMAT_REFUSAL.to_string() });
            }

            let shape = RequestShape::from_request(&request);
            let extra_prompt_tokens = shape.prompt_tokens();
            // A raw prompt is one user turn: the lane has only the chat entrance, and its template
            // is the class's (ADR-0077 Decision 6), so a raw completion cannot bypass it.
            let messages: Vec<ChatMessage> = match &request.prompt {
                Some(prompt) if request.messages.is_empty() => vec![ChatMessage::new("user", prompt.clone())],
                _ => request.messages.clone(),
            };
            let requested_tokens = request.params.max_tokens;
            let plan = plan_context(&messages, n_ctx, requested_tokens, extra_prompt_tokens, request.legs);
            let original_turns = messages.iter().filter(|m| m.role != "system").count();

            let lane = Lane {
                http: self.http.clone(),
                token: self.token.clone(),
                url: format!("{}/v1/chat/completions", self.url),
                n_ctx,
                max_output,
                extra_prompt_tokens,
            };
            let mut planner = LegPlanner::new(request.legs);
            let mut jobs: Vec<Value> = Vec::new();
            let mut context = plan.kept.clone();
            let mut summarized: Option<usize> = None;

            // **The first leg's request goes out before the stream is handed back**, so a gateway
            // that will not take it — the lane not certified, the queue full, a slot unfunded —
            // is a status code, as it always was, and not an error event behind a 200. A summary
            // leg that fails here does not fail the answer: it is recorded and the answer goes.
            let mut role = planner.first(plan.dropped.len());
            let mut first: Option<Leg> = None;
            if role == LegRole::Summary {
                match summary_job_messages(&plan.dropped, lane.summary_budget()) {
                    Some((messages, covered)) => match lane.send(&messages, SUMMARY_TOKENS, &RequestShape::default()).await {
                        Ok(response) => {
                            summarized = Some(covered);
                            first = Some(Leg { messages, shape: RequestShape::default(), response });
                        }
                        Err(e) => jobs.push(job_error(LegRole::Summary, e.to_string())),
                    },
                    None => jobs.push(job_error(LegRole::Summary, "the dropped turns do not fit a summary job on this class".into())),
                }
            }
            let mut first = match first {
                Some(leg) => leg,
                None => {
                    role = LegRole::Answer;
                    let ceiling = answer_ceiling(
                        prompt_tokens_upper_bound(&context) + extra_prompt_tokens,
                        n_ctx,
                        max_output,
                        requested_tokens,
                    )?;
                    let response = lane.send(&context, ceiling, &shape).await?;
                    Leg { messages: context.clone(), shape: shape.clone(), response }
                }
            };

            Ok(crate::backend::mock::async_stream(move |tx| async move {
                let mut answer = String::new();
                let mut usage = Usage::default();
                let mut finish_reason = "stop".to_string();
                let mut answer_misaka: Option<Value> = None;
                loop {
                    let forward = (role != LegRole::Summary).then_some(&tx);
                    let result = lane.run_leg(first, forward).await;
                    let outcome = match (role, result) {
                        (LegRole::Summary, Ok(leg)) => {
                            let mut entry = job_entry(role, &leg);
                            let summary = leg.text.trim();
                            if summary.is_empty() {
                                // The job ran and is a claim; the person's history still was not
                                // carried, and the entry says so rather than the app inventing one.
                                entry["error"] = json!("the summary job returned no text; the dropped turns were not carried");
                                summarized = None;
                            } else {
                                context = with_summary(&context, summary, plan.prompt_budget).0;
                            }
                            jobs.push(entry);
                            LegOutcome { finish_reason: leg.finish_reason, delivered_tokens: 0, requested_tokens, failed: false }
                        }
                        (LegRole::Summary, Err(e)) => {
                            summarized = None;
                            jobs.push(job_error(role, e.to_string()));
                            LegOutcome { finish_reason: String::new(), delivered_tokens: 0, requested_tokens, failed: true }
                        }
                        (LegRole::Answer | LegRole::Continue, Ok(leg)) => {
                            jobs.push(job_entry(role, &leg));
                            answer.push_str(&leg.text);
                            usage.prompt_tokens += leg.usage.prompt_tokens;
                            usage.completion_tokens += leg.usage.completion_tokens;
                            finish_reason = leg.finish_reason.clone();
                            if role == LegRole::Answer {
                                answer_misaka = leg.misaka;
                            }
                            LegOutcome {
                                finish_reason: leg.finish_reason,
                                delivered_tokens: usage.completion_tokens,
                                requested_tokens,
                                failed: false,
                            }
                        }
                        (LegRole::Answer, Err(e)) => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                        (LegRole::Continue, Err(e)) => {
                            jobs.push(job_error(role, e.to_string()));
                            break;
                        }
                    };

                    let Some(next) = planner.after(role, &outcome) else { break };
                    let leg = match next {
                        LegRole::Answer => {
                            let estimate = prompt_tokens_upper_bound(&context) + extra_prompt_tokens;
                            match answer_ceiling(estimate, n_ctx, max_output, requested_tokens) {
                                Ok(ceiling) => Some((context.clone(), ceiling, shape.clone())),
                                Err(e) => {
                                    let _ = tx.send(Err(e)).await;
                                    return;
                                }
                            }
                        }
                        LegRole::Continue => {
                            let remaining = requested_tokens.saturating_sub(usage.completion_tokens);
                            let budget = prompt_budget(n_ctx, extra_prompt_tokens, remaining);
                            match continue_leg_messages(&context, &answer, budget) {
                                Some(messages) => {
                                    let estimate = prompt_tokens_upper_bound(&messages) + extra_prompt_tokens;
                                    match answer_ceiling(estimate, n_ctx, max_output, remaining) {
                                        Ok(ceiling) => Some((messages, ceiling, shape.clone())),
                                        Err(e) => {
                                            jobs.push(job_error(next, e.to_string()));
                                            None
                                        }
                                    }
                                }
                                None => {
                                    jobs.push(job_error(
                                        next,
                                        "the answer so far does not fit beside the context on this class".into(),
                                    ));
                                    None
                                }
                            }
                        }
                        // The planner never schedules a summary after the first leg.
                        LegRole::Summary => None,
                    };
                    let Some((messages, ceiling, leg_shape)) = leg else { break };
                    match lane.send(&messages, ceiling, &leg_shape).await {
                        Ok(response) => {
                            role = next;
                            first = Leg { messages, shape: leg_shape, response };
                        }
                        Err(e) if next == LegRole::Answer => {
                            let _ = tx.send(Err(e)).await;
                            return;
                        }
                        Err(e) => {
                            jobs.push(job_error(next, e.to_string()));
                            break;
                        }
                    }
                }

                usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
                let kept_turns = context.iter().filter(|m| m.role != "system").count();
                let mut report = json!({
                    "n_ctx": n_ctx,
                    "prompt_tokens_estimate": prompt_tokens_upper_bound(&context) + extra_prompt_tokens,
                    "dropped_turns": original_turns.saturating_sub(kept_turns),
                });
                if let Some(cap) = max_output {
                    // ADR-0097 Decision 2: the per-job ceiling the gateway published, beside the window.
                    report["max_output_tokens"] = json!(cap);
                }
                if let Some(covered) = summarized {
                    report["summarized_turns"] = json!(covered);
                }
                let misaka = finalize_lane_misaka(answer_misaka, jobs, report);
                let _ = tx.send(Ok(StreamEvent::Done { usage, finish_reason, misaka: Some(misaka) })).await;
            }))
        })
    }
}

/// The refusal a `require_committed_format` request meets on a dormant network.
const COMMITTED_FORMAT_REFUSAL: &str = "misaka.require_committed_format: this chain has not armed palw_fp_decode_constraint (ADR-0096 \
     Decision 8), so a response_format can only be advisory here — the schema is rendered into the prompt, the run is \
     unconstrained, and the answer is checked after the fact. The request is refused before any job runs; drop \
     require_committed_format to accept an advisory answer marked as such (misaka.format.enforcement).";

/// Whether the request's `misaka` extension demands the committed format mode.
pub(crate) fn requires_committed_format(misaka: Option<&Value>) -> bool {
    misaka.and_then(|m| m.get("require_committed_format")).and_then(Value::as_bool).unwrap_or(false)
}

/// The shape fields one leg carries to the gateway (ADR-0096 Decisions 1–3), as the client sent
/// them. Empty for a summary leg: a summary is prose, not the answer's format, and not a tool
/// round-trip.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RequestShape {
    pub tools: Option<Value>,
    pub tool_choice: Option<Value>,
    pub response_format: Option<Value>,
    pub misaka: Option<Value>,
}

impl RequestShape {
    fn from_request(request: &GenerationRequest) -> Self {
        RequestShape {
            tools: request.tools.clone(),
            tool_choice: request.tool_choice.clone(),
            response_format: request.response_format.clone(),
            misaka: request.misaka.clone(),
        }
    }

    /// What the shape adds to the prompt: the gateway renders `tools` into the system turn as
    /// the model's own `<tools>` text and an advisory schema into the prompt, so both cost
    /// context the conversation does not get.
    fn prompt_tokens(&self) -> u64 {
        [&self.tools, &self.response_format].into_iter().flatten().map(|value| text_tokens_upper_bound(&value.to_string())).sum()
    }
}

/// The lane's request body: messages, a decode ceiling, the stream flag, and the shape fields
/// only when the client sent them (ADR-0096 Decisions 1–3). No sampling knob, ever — see the
/// module doc. A key sent as `null` is a different request from one not sent, so absent stays
/// absent.
pub(crate) fn lane_request_body(messages: &[ChatMessage], ceiling: u64, shape: &RequestShape) -> Value {
    let mut body = json!({
        "model": LANE_MODEL,
        "messages": messages,
        "max_tokens": ceiling,
        "stream": true,
    });
    for (key, value) in [
        ("tools", &shape.tools),
        ("tool_choice", &shape.tool_choice),
        ("response_format", &shape.response_format),
        ("misaka", &shape.misaka),
    ] {
        if let Some(value) = value {
            body[key] = value.clone();
        }
    }
    body
}

// ---------------------------------------------------------------------------------------------
// The decisions, pure: which legs, over which messages, at which ceiling
// ---------------------------------------------------------------------------------------------

/// The role of one job in a request's chain, as it is named in `misaka.jobs[].role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegRole {
    Summary,
    Answer,
    Continue,
}

impl LegRole {
    pub fn as_str(self) -> &'static str {
        match self {
            LegRole::Summary => "summary",
            LegRole::Answer => "answer",
            LegRole::Continue => "continue",
        }
    }
}

/// What a finished leg tells the planner.
#[derive(Clone, Debug, PartialEq)]
pub struct LegOutcome {
    pub finish_reason: String,
    /// Decode tokens delivered to the client so far, over every answer and continue leg.
    pub delivered_tokens: u64,
    /// What the request asked for (`max_tokens`).
    pub requested_tokens: u64,
    pub failed: bool,
}

/// **Which legs run, in what order** — the decision, apart from the HTTP.
///
/// A summary runs first only when the trim dropped MORE than `summarize_after_turns` turns; it
/// is followed by the answer whether it succeeded or not. A continue leg follows an answer (or a
/// continue) that the row cut short — `finish_reason == "length"` — while the request asked for
/// more tokens than have been delivered, up to `continue_max_legs` of them. A leg that failed is
/// never continued: a failed answer is the request's failure, and a failed continue leg ends the
/// chain with what was delivered.
#[derive(Clone, Debug)]
pub struct LegPlanner {
    limits: LegLimits,
    continues_run: u32,
}

impl LegPlanner {
    pub fn new(limits: LegLimits) -> Self {
        LegPlanner { limits, continues_run: 0 }
    }

    /// The first leg, given how many turns the trim dropped.
    pub fn first(&self, dropped_turns: usize) -> LegRole {
        if dropped_turns as u64 > self.limits.summarize_after_turns as u64 { LegRole::Summary } else { LegRole::Answer }
    }

    /// The leg after `role` finished with `outcome`, if any. Never a summary.
    pub fn after(&mut self, role: LegRole, outcome: &LegOutcome) -> Option<LegRole> {
        match role {
            LegRole::Summary => Some(LegRole::Answer),
            LegRole::Answer | LegRole::Continue => {
                if outcome.failed || outcome.finish_reason != "length" {
                    return None;
                }
                if outcome.delivered_tokens >= outcome.requested_tokens {
                    return None;
                }
                if self.continues_run >= self.limits.continue_max_legs {
                    return None;
                }
                self.continues_run += 1;
                Some(LegRole::Continue)
            }
        }
    }
}

/// The conversation as the answer leg will send it, and what was cut to get there.
#[derive(Clone, Debug, PartialEq)]
pub struct ContextPlan {
    /// The messages the answer's prompt is built from (system turns first, as the trim leaves them).
    pub kept: Vec<ChatMessage>,
    /// The non-system turns the trim dropped, oldest first.
    pub dropped: Vec<ChatMessage>,
    /// The prompt budget the kept turns fit — what a summary turn, once added, must fit too.
    pub prompt_budget: u64,
}

/// The prompt budget for a conversation whose answer may take `asked` tokens: the class's context
/// less the template's margin, the shape fields, and the answer's room (the ask, sized like an
/// answer — see [`DEFAULT_ANSWER_TOKENS`]). Unbounded when the class's context is unknown.
fn prompt_budget(n_ctx: Option<u64>, extra_prompt_tokens: u64, asked: u64) -> u64 {
    match n_ctx {
        Some(n_ctx) => n_ctx.saturating_sub(TEMPLATE_MARGIN_TOKENS + extra_prompt_tokens + asked.min(DEFAULT_ANSWER_TOKENS)),
        None => u64::MAX,
    }
}

/// **The trim, as one decision** (ADR-0096 Decision 5, step 1 — and the reservation step 2 needs).
///
/// Fits the conversation to the prompt budget with [`fit_messages_to_budget`] (system prompt and
/// the newest turn survive, oldest go first). When that drops more than
/// `limits.summarize_after_turns` turns, the fit is done again with the summary turn's room
/// reserved, so that the dropped set is exactly what the summary job will be shown: a summary
/// that had to make room for itself by dropping a kept turn would leave a hole between the
/// summary and the history, and nothing would have been said about that turn at all.
pub fn plan_context(
    messages: &[ChatMessage],
    n_ctx: Option<u64>,
    asked: u64,
    extra_prompt_tokens: u64,
    limits: LegLimits,
) -> ContextPlan {
    let budget = prompt_budget(n_ctx, extra_prompt_tokens, asked);
    let (mut kept, mut dropped_count) = fit_messages_to_budget(messages, budget);
    if dropped_count as u64 > limits.summarize_after_turns as u64 {
        let (with_reserve, more) = fit_messages_to_budget(messages, budget.saturating_sub(SUMMARY_RESERVE_TOKENS));
        kept = with_reserve;
        dropped_count = more;
    }
    let dropped = messages.iter().filter(|m| m.role != "system").take(dropped_count).cloned().collect();
    ContextPlan { kept, dropped, prompt_budget: budget }
}

/// **The summary job's messages**, and how many of the dropped turns they cover.
///
/// The dropped turns are shown as `role: content` lines in one user turn under
/// [`SUMMARY_INSTRUCTION`]. They may not all fit the class either — ten dropped turns of 300
/// tokens is six rows — so the newest are taken first, being the ones nearest the kept context,
/// until the budget is spent; the oldest beyond that are simply gone, and the count says so.
/// `None` when not even the newest dropped turn fits: a summary job is never sent a prompt the
/// worker will refuse.
pub fn summary_job_messages(dropped: &[ChatMessage], budget: u64) -> Option<(Vec<ChatMessage>, usize)> {
    let instruction = ChatMessage::new("system", SUMMARY_INSTRUCTION);
    let mut lines: Vec<String> = Vec::new();
    for turn in dropped.iter().rev() {
        let mut candidate = lines.clone();
        candidate.push(format!("{}: {}", turn.role, turn.content));
        let user = ChatMessage::new("user", candidate.iter().rev().cloned().collect::<Vec<_>>().join("\n"));
        if prompt_tokens_upper_bound(&[instruction.clone(), user]) > budget {
            break;
        }
        lines = candidate;
    }
    if lines.is_empty() {
        return None;
    }
    let covered = lines.len();
    lines.reverse();
    Some((vec![instruction, ChatMessage::new("user", lines.join("\n"))], covered))
}

/// **The summary, riding the next prompt** as a system-level turn placed after the original
/// system prompt (an instruction stays first; the summary is context). The result is fitted to
/// the budget once more in case the model overran its 120 words: [`fit_messages_to_budget`]
/// keeps every system turn, so the summary survives and history gives way. Returns the messages
/// and how many further turns that cost.
pub fn with_summary(kept: &[ChatMessage], summary: &str, budget: u64) -> (Vec<ChatMessage>, usize) {
    let leading_system = kept.iter().take_while(|m| m.role == "system").count();
    let mut messages = kept.to_vec();
    messages.insert(leading_system, ChatMessage::new("system", format!("{SUMMARY_PREFIX}{summary}")));
    fit_messages_to_budget(&messages, budget)
}

/// **A continue leg's messages**: the kept context, the answer so far as the assistant's turn,
/// and [`CONTINUE_INSTRUCTION`] as the user's. Fitted to the budget — the answer so far is up to
/// a row's worth of new text, so history gives way to it. `None` when even the answer so far does
/// not fit beside the instruction: a continuation of a text the model cannot see is not a
/// continuation.
pub fn continue_leg_messages(context: &[ChatMessage], answer_so_far: &str, budget: u64) -> Option<Vec<ChatMessage>> {
    let mut messages = context.to_vec();
    messages.push(ChatMessage::new("assistant", answer_so_far));
    messages.push(ChatMessage::new("user", CONTINUE_INSTRUCTION));
    let (kept, _) = fit_messages_to_budget(&messages, budget);
    let answer_survived = kept.len() >= 2 && kept[kept.len() - 2].role == "assistant" && kept[kept.len() - 2].content == answer_so_far;
    (answer_survived && prompt_tokens_upper_bound(&kept) <= budget).then_some(kept)
}

/// **The ceiling has to fit the class, not the app's default.**
///
/// A class is registered at a fixed context — 512 tokens for graph-v5@512 — and the worker checks
/// `prompt + the DECODE CEILING` against it, not `prompt + what is actually generated`. So a
/// request asking for the Studio's default 2048 is refused outright however short its answer
/// would have been, and a conversation with any history behind it never gets past the first
/// turn: "prompt 344 + decode ceiling 1024 exceeds max_context_tokens 512". Measured, on a chat
/// whose second message returned nothing at all.
///
/// The prompt is estimated rather than tokenized here — the class's tokenizer lives with the
/// worker — so a margin is left for the estimate being low and for the chat template's own
/// markers.
///
/// **And never more than one job decodes** (ADR-0097 Decision 2): `max_output` is the gateway's
/// published `max_output_tokens`, so a ceiling above it would only be clamped at the gateway. The
/// cap is per JOB: the request's own ask is what the continue legs compare delivery against, and
/// it is left as the request stated it.
fn answer_ceiling(prompt_estimate: u64, n_ctx: Option<u64>, max_output: Option<u64>, asked: u64) -> Result<u64> {
    let cap = |ceiling: u64| max_output.map_or(ceiling, |cap| ceiling.min(cap));
    let Some(n_ctx) = n_ctx else { return Ok(cap(asked)) };
    let used = prompt_estimate.saturating_add(TEMPLATE_MARGIN_TOKENS);
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
    Ok(cap(if asked <= room { asked } else { room.min(DEFAULT_ANSWER_TOKENS) }))
}

/// One `misaka.jobs[]` entry for a leg that ran.
fn job_entry(role: LegRole, leg: &LegResult) -> Value {
    let id = |key: &str| leg.misaka.as_ref().and_then(|m| m.get(key)).cloned().unwrap_or(Value::Null);
    if let Some(claim) = id("fp_claim_id").as_str() {
        tracing::info!(role = role.as_str(), claim, "free-prompt claim committed");
    }
    json!({
        "fp_job_id": id("fp_job_id"),
        "fp_claim_id": id("fp_claim_id"),
        "role": role.as_str(),
        "prompt_tokens": leg.usage.prompt_tokens,
        "decode_tokens": leg.usage.completion_tokens,
    })
}

/// One `misaka.jobs[]` entry for a leg that did not run, or did not finish.
fn job_error(role: LegRole, error: String) -> Value {
    tracing::warn!(role = role.as_str(), "lane leg failed: {error}");
    json!({
        "fp_job_id": null,
        "fp_claim_id": null,
        "role": role.as_str(),
        "prompt_tokens": 0,
        "decode_tokens": 0,
        "error": error,
    })
}

/// The answer's `misaka` object: the answer leg's own (job, claim, roots, derivation — it stays
/// the top-level one) with `jobs[]` and `context` added beside it.
fn finalize_lane_misaka(answer: Option<Value>, jobs: Vec<Value>, context: Value) -> Value {
    let mut misaka = match answer {
        Some(Value::Object(map)) => Value::Object(map),
        _ => json!({}),
    };
    misaka["jobs"] = Value::Array(jobs);
    misaka["context"] = context;
    misaka
}

// ---------------------------------------------------------------------------------------------
// Sampling at the entrance (ADR-0096 Decision 4): what was asked, printed beside what ran
// ---------------------------------------------------------------------------------------------

/// Every knob whose effective value asks for something other than the greedy decode the lane
/// replays, with the value asked for. Empty is a request the lane honours exactly as written.
pub fn sampling_divergence(params: &SamplingCommitment) -> Vec<(&'static str, Value)> {
    let mut out = Vec::new();
    if params.temperature != 0.0 {
        out.push(("temperature", json!(params.temperature)));
    }
    if params.top_p != 1.0 {
        out.push(("top_p", json!(params.top_p)));
    }
    if params.top_k != 0 {
        out.push(("top_k", json!(params.top_k)));
    }
    if params.min_p != 0.0 {
        out.push(("min_p", json!(params.min_p)));
    }
    if params.repeat_penalty != 1.0 {
        out.push(("repeat_penalty", json!(params.repeat_penalty)));
    }
    if let Some(seed) = params.seed {
        out.push(("seed", json!(seed)));
    }
    out
}

/// The `misaka.sampling` notice every answer that went to the lane carries: what was requested,
/// what was applied (the greedy decode and the zero seed, which is what a seat replays), why, and
/// which knobs have no rule on this lane at all.
pub fn sampling_notice(params: &SamplingCommitment, network: &str) -> Value {
    json!({
        "requested": {
            "temperature": params.temperature,
            "top_p": params.top_p,
            "top_k": params.top_k,
            "min_p": params.min_p,
            "repeat_penalty": params.repeat_penalty,
            "seed": params.seed,
        },
        "applied": { "temperature": 0, "seed": GREEDY_SEED_HEX },
        "reason": format!("palw_fp_decode_rules is not armed on {network}; the lane replays a greedy decode"),
        "not_a_rule_on_this_lane": NOT_A_RULE_ON_THIS_LANE,
    })
}

/// The refusal under `node.sampling_policy: refuse`, naming every knob and the way back.
pub fn sampling_refusal(divergence: &[(&'static str, Value)], network: &str) -> String {
    let asked: Vec<String> = divergence.iter().map(|(name, value)| format!("{name}={value}")).collect();
    format!(
        "node.sampling_policy is `refuse`, and this request — with the Studio's generation defaults filling what it did not send — resolves to sampling the free-prompt lane cannot honour: {}. \
         The lane replays a greedy decode while palw_fp_decode_rules is not armed on {network} (ADR-0082 Decision 11, \
         ADR-0096 Decision 4). Send temperature 0, top_p 1, top_k 0, min_p 0, repeat_penalty 1 and no seed — or set \
         node.sampling_policy to greedy_with_notice to have the knobs dropped and reported under misaka.sampling.",
        asked.join(", ")
    )
}

// ---------------------------------------------------------------------------------------------
// The HTTP half
// ---------------------------------------------------------------------------------------------

/// One leg, sent: what it was sent with (the retry re-sends the same messages at the ceiling
/// the worker names) and the gateway's open response.
struct Leg {
    messages: Vec<ChatMessage>,
    shape: RequestShape,
    response: reqwest::Response,
}

/// What one leg produced.
struct LegResult {
    text: String,
    usage: Usage,
    finish_reason: String,
    misaka: Option<Value>,
}

/// The gateway's chat entrance, with everything a leg needs to reach it.
#[derive(Clone)]
struct Lane {
    http: reqwest::Client,
    token: Option<String>,
    url: String,
    n_ctx: Option<u64>,
    /// The gateway's published `max_output_tokens` (ADR-0097 Decision 2), when it published one.
    max_output: Option<u64>,
    extra_prompt_tokens: u64,
}

impl Lane {
    /// The prompt budget of a summary job: the context less the template's margin and the
    /// summary's own ceiling.
    fn summary_budget(&self) -> u64 {
        self.n_ctx.map(|n| n.saturating_sub(TEMPLATE_MARGIN_TOKENS + SUMMARY_TOKENS)).unwrap_or(u64::MAX)
    }

    async fn send(&self, messages: &[ChatMessage], ceiling: u64, shape: &RequestShape) -> Result<reqwest::Response> {
        let body = lane_request_body(messages, ceiling, shape);
        let mut request = self.http.post(&self.url).json(&body);
        if let Some(token) = &self.token {
            request = request.header("x-pool-token", token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::Engine { backend: NAME, message: format!("the gateway did not accept the request: {e}") })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let text = response.text().await.unwrap_or_default();
        // The gateway's own error body is OpenAI-shaped; the sentence inside it is the one worth
        // repeating. A 4xx is the gateway refusing BY NAME — a temperature on a dormant fence, a
        // role it does not serve — and reaches the client as the 400 it is, not as an engine fault.
        // One reader for that body, here and in the stream and in the mining queue ([`LaneRefusal`]).
        let message = LaneRefusal::from_text(&text).message;
        if status.is_client_error() {
            return Err(Error::BadRequest { message: format!("the gateway refused the request ({status}): {message}") });
        }
        Err(Error::Engine { backend: NAME, message: format!("gateway returned {status}: {message}") })
    }

    /// Stream one leg to its end. Deltas and tool-call deltas go to `forward` as they arrive (an
    /// answer or continue leg); a summary leg forwards nothing. The gateway answers 200 and puts
    /// a refusal in the stream — a job over the class's context, a lane the chain does not
    /// certify — and silence would be the worst rendering of that. The worker sized the request
    /// for us in the act of refusing it, so there is one retry at the ceiling it named, and only
    /// when the numbers are there: a second refusal is a real answer.
    async fn run_leg(&self, leg: Leg, forward: Option<&Sender<Result<StreamEvent>>>) -> Result<LegResult> {
        use futures_util::StreamExt;
        let Leg { messages, shape, response } = leg;
        let fallback_prompt_tokens = prompt_tokens_upper_bound(&messages) + self.extra_prompt_tokens;
        let mut response = response;
        let mut retried = false;
        loop {
            let mut parser = SseParser::new(true);
            let mut byte_stream = response.bytes_stream();
            let mut text = String::new();
            let mut refusal: Option<LaneRefusal> = None;
            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk.map_err(|e| Error::Engine { backend: NAME, message: format!("stream broke: {e}") })?;
                for event in parser.push(&chunk) {
                    if let StreamEvent::Delta(delta) = &event {
                        text.push_str(delta);
                    }
                    if let Some(tx) = forward
                        && tx.send(Ok(event)).await.is_err()
                    {
                        // The client hung up. Dropping the response cancels the request.
                        return Err(Error::Cancelled);
                    }
                }
                if let Some((message, event)) = parser.take_error_event() {
                    refusal = Some(LaneRefusal::from_body(&event).unwrap_or(LaneRefusal { message, code: None, numbers: None }));
                    break;
                }
            }
            if let Some(refusal) = refusal {
                // ADR-0097 Decision 2: decided by the refusal's code and computed from its numbers;
                // the sentence is read only from a gateway that coded nothing. And never past what
                // one job decodes, which the gateway published beside its window.
                let room = refusal.retry_ceiling().map(|room| self.max_output.map_or(room, |cap| room.min(cap)));
                if let (false, Some(room)) = (retried, room) {
                    retried = true;
                    tracing::info!(
                        ceiling = room,
                        code = refusal.code.as_deref().unwrap_or("none"),
                        "retrying at the ceiling the refusal names"
                    );
                    response = self.send(&messages, room, &shape).await?;
                    continue;
                }
                return Err(Error::Engine { backend: NAME, message: refusal.message });
            }
            let (usage, finish_reason, misaka) = parser.finish_parts(fallback_prompt_tokens);
            return Ok(LegResult { text, usage, finish_reason, misaka });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The gateway's refusals, read by code (ADR-0097 Decision 2)
// ---------------------------------------------------------------------------------------------

/// The code the gateway puts in `error.code` when a job's prompt and ceiling do not fit the
/// class's window — OpenAI's own code for the same refusal.
pub const CONTEXT_LENGTH_EXCEEDED: &str = "context_length_exceeded";

/// **A refusal from the gateway, as the gateway coded it.**
///
/// ADR-0097 Decision 2: the gateway's error body is OpenAI's `{"error": {"message", "type"}}`,
/// plus `error.code` and `misaka.refusal` (the numbers) for the two bounds a request meets before
/// the chain. This app branches on the code and computes from the numbers. The sentence is read
/// only from a refusal that carries no code — a gateway from before ADR-0097, which is the case
/// [`ceiling_from_refusal`] was written for and is kept for.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneRefusal {
    /// `error.message` — what a person reads.
    pub message: String,
    /// `error.code`, when the gateway coded the refusal.
    pub code: Option<String>,
    /// `misaka.refusal`, when the gateway sent the numbers.
    pub numbers: Option<Value>,
}

impl LaneRefusal {
    /// From an error body — a 400's JSON or an SSE error event. `None` for a body with no `error`.
    pub fn from_body(body: &Value) -> Option<Self> {
        let error = body.get("error")?;
        let message = match error.get("message").and_then(Value::as_str) {
            Some(message) => message.to_string(),
            // A server (or a proxy in front of one) that put the sentence in `error` itself.
            None => error.as_str().map(str::to_string).unwrap_or_else(|| error.to_string()),
        };
        Some(LaneRefusal {
            message,
            code: error.get("code").and_then(Value::as_str).map(str::to_string),
            numbers: body.get("misaka").and_then(|m| m.get("refusal")).filter(|r| r.is_object()).cloned(),
        })
    }

    /// From a response's text: the body when it is JSON with an `error`; otherwise the text
    /// itself, cut at 400 characters (a proxy's HTML error page is not a sentence for a person).
    pub fn from_text(text: &str) -> Self {
        serde_json::from_str::<Value>(text).ok().and_then(|body| Self::from_body(&body)).unwrap_or_else(|| LaneRefusal {
            message: text.trim().chars().take(400).collect(),
            code: None,
            numbers: None,
        })
    }

    /// **The ceiling a retry of the same messages would fit at, if one would.**
    ///
    /// By the code when the gateway sent one: `context_length_exceeded` with its numbers is the
    /// arithmetic, and any other code is a refusal no ceiling fixes — a coded prompt-bytes refusal
    /// is never retried because its sentence happens to contain digits. By the sentence only when
    /// the refusal carries no code.
    pub fn retry_ceiling(&self) -> Option<u64> {
        match self.code.as_deref() {
            Some(CONTEXT_LENGTH_EXCEEDED) => {
                let number = |key: &str| self.numbers.as_ref()?.get(key)?.as_u64();
                match (number("prompt_tokens"), number("context_window")) {
                    (Some(prompt), Some(window)) => retry_ceiling_for(prompt, window),
                    // The code without its numbers: the sentence is the same refusal's.
                    _ => ceiling_from_refusal(&self.message),
                }
            }
            Some(_) => None,
            None => ceiling_from_refusal(&self.message),
        }
    }
}

/// One spelling of the retry arithmetic, whoever supplied the two numbers: the window less the
/// prompt, less one token of slack for a marker the prompt count did not include; nothing when
/// the prompt alone fills the window.
fn retry_ceiling_for(prompt_tokens: u64, context_window: u64) -> Option<u64> {
    context_window.checked_sub(prompt_tokens.saturating_add(1)).filter(|room| *room > 0)
}

/// **The ceiling the worker's own refusal implies — from the sentence**, for a gateway that sent
/// no code (before ADR-0097).
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
    retry_ceiling_for(prompt, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal carries the arithmetic that makes the retry exact. Written against the message
    /// the live worker actually sent, because a parser written against an imagined format is a
    /// parser that silently declines to fix anything.
    /// The fingerprint carries the address and a digest prefix of the token — never the token —
    /// and an empty token is no token, the same normalisation the constructor applies.
    #[test]
    fn the_fingerprint_names_the_slot_without_leaking_its_token() {
        let a = GatewayBackend::new("https://pool.example/pool/v1/slots/slot-06/fp/".into(), Some("token-06".into())).fingerprint();
        assert_eq!(a.kind, NAME);
        assert_eq!(
            a.url.as_deref(),
            Some("https://pool.example/pool/v1/slots/slot-06/fp"),
            "the trailing slash is trimmed as at construction"
        );
        let prefix = a.token_sha256_prefix.clone().expect("a prefix");
        assert_eq!(prefix.len(), 12);
        assert!(!serde_json::to_string(&a).expect("json").contains("token-06"), "the token itself never appears");
        assert_eq!(prefix, RuntimeFingerprint::token_prefix("token-06"));

        let rotated =
            GatewayBackend::new("https://pool.example/pool/v1/slots/slot-06/fp".into(), Some("token-07".into())).fingerprint();
        assert_ne!(a, rotated, "a new token is a new engine");
        let empty = GatewayBackend::new("https://pool.example/pool/v1/slots/slot-06/fp".into(), Some(String::new())).fingerprint();
        assert_eq!(empty.token_sha256_prefix, None);
        assert_eq!(empty, GatewayBackend::new("https://pool.example/pool/v1/slots/slot-06/fp".into(), None).fingerprint());
    }

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
        let jp = [ChatMessage::new("user", "東京の天気は")];
        assert!(prompt_tokens_upper_bound(&jp) >= 6 + 8, "one token per kana or kanji, plus the template's markers");
        let en = [ChatMessage::new("user", "weather in Tokyo")];
        assert!(prompt_tokens_upper_bound(&en) >= 4, "ascii is cheaper, but never free");
    }

    /// The lane's body: the shape fields and the extension go through as sent, absent stays
    /// absent, and no sampling knob is ever in it.
    #[test]
    fn the_lane_body_forwards_the_shape_fields_and_the_misaka_extension() {
        let messages = [ChatMessage::new("system", "be brief"), ChatMessage::new("user", "hi")];
        let plain = lane_request_body(&messages, 200, &RequestShape::default());
        assert_eq!(plain["model"], LANE_MODEL);
        assert_eq!(plain["max_tokens"], 200);
        assert_eq!(plain["stream"], true);
        assert_eq!(plain["messages"][1]["content"], "hi");
        for absent in ["tools", "tool_choice", "response_format", "misaka", "temperature", "seed", "top_p"] {
            assert!(plain.get(absent).is_none(), "`{absent}` must not be in the body");
        }

        let shape = RequestShape {
            tools: Some(json!([{"type":"function","function":{"name":"lookup"}}])),
            tool_choice: Some(json!({"type":"function","function":{"name":"lookup"}})),
            response_format: Some(json!({"type":"json_object"})),
            misaka: Some(json!({"require_committed_format": false})),
        };
        let body = lane_request_body(&messages, 200, &shape);
        assert_eq!(body["tools"][0]["function"]["name"], "lookup");
        assert_eq!(body["tool_choice"]["function"]["name"], "lookup");
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["misaka"]["require_committed_format"], false);
        assert!(shape.prompt_tokens() > 0, "tools and a schema cost prompt tokens the conversation does not get");
    }

    /// ADR-0096 Invariant 6's Studio half: the request is refused before any job is sent. The
    /// gateway address here is unreachable on purpose — had anything been sent, the error would
    /// name the connection, not the fence.
    #[tokio::test]
    async fn require_committed_format_is_refused_before_anything_is_sent() {
        let backend = GatewayBackend::new("http://127.0.0.1:1".into(), None);
        backend.set_facts_for_test(GatewayFacts { n_ctx: 512, ..Default::default() }).await;
        let mut request = GenerationRequest::plain("c", vec![ChatMessage::new("user", "{}")], None, Default::default());
        request.response_format = Some(json!({"type":"json_object"}));
        request.misaka = Some(json!({"require_committed_format": true}));
        match backend.generate(request).await {
            Err(Error::BadRequest { message }) => {
                assert!(message.contains("palw_fp_decode_constraint"), "the refusal names the fence: {message}");
                assert!(message.contains("advisory"), "and says what is available instead: {message}");
            }
            Err(other) => panic!("refused for the wrong reason (was something sent?): {other}"),
            Ok(_) => panic!("a dormant fence must refuse the committed mode"),
        }
        assert!(!requires_committed_format(Some(&json!({"require_committed_format": false}))));
        assert!(!requires_committed_format(None));
    }

    /// The planner's first decision: a summary only when the trim dropped MORE than the threshold.
    #[test]
    fn the_planner_runs_a_summary_only_past_the_threshold() {
        let planner = LegPlanner::new(LegLimits { summarize_after_turns: 4, continue_max_legs: 2 });
        assert_eq!(planner.first(0), LegRole::Answer);
        assert_eq!(planner.first(4), LegRole::Answer, "exactly the threshold is not past it");
        assert_eq!(planner.first(5), LegRole::Summary);
        let never = LegPlanner::new(LegLimits { summarize_after_turns: u32::MAX, continue_max_legs: 2 });
        assert_eq!(never.first(1_000), LegRole::Answer);
    }

    /// The whole chain, driven by synthetic outcomes: summary → answer → continue → continue →
    /// stop at the limit; and no continue at all when the answer stopped on its own, or when the
    /// client already got what it asked for.
    #[test]
    fn the_planner_continues_on_length_up_to_the_limit_and_only_when_more_was_asked() {
        let cut = |delivered: u64| LegOutcome {
            finish_reason: "length".into(),
            delivered_tokens: delivered,
            requested_tokens: 2048,
            failed: false,
        };
        let mut planner = LegPlanner::new(LegLimits { summarize_after_turns: 4, continue_max_legs: 2 });
        let mut order = vec![planner.first(7)];
        let mut role = order[0];
        let mut delivered = 0;
        while let Some(next) = planner.after(role, &if role == LegRole::Summary { cut(0) } else { cut(delivered) }) {
            delivered += 256;
            order.push(next);
            role = next;
        }
        assert_eq!(order, vec![LegRole::Summary, LegRole::Answer, LegRole::Continue, LegRole::Continue], "then the limit");

        let mut planner = LegPlanner::new(LegLimits::default());
        let stopped = LegOutcome { finish_reason: "stop".into(), delivered_tokens: 40, requested_tokens: 2048, failed: false };
        assert_eq!(planner.after(LegRole::Answer, &stopped), None, "an answer that ended is not continued");
        let satisfied = LegOutcome { finish_reason: "length".into(), delivered_tokens: 100, requested_tokens: 100, failed: false };
        assert_eq!(planner.after(LegRole::Answer, &satisfied), None, "the client asked for 100 and got 100");
        let tool_calls =
            LegOutcome { finish_reason: "tool_calls".into(), delivered_tokens: 30, requested_tokens: 2048, failed: false };
        assert_eq!(planner.after(LegRole::Answer, &tool_calls), None, "a tool call is the app's turn now");
        let mut none = LegPlanner::new(LegLimits { summarize_after_turns: 4, continue_max_legs: 0 });
        assert_eq!(none.after(LegRole::Answer, &cut(256)), None, "zero legs is zero legs");
    }

    /// The guard: a summary that failed is followed by the answer regardless; a continue leg that
    /// failed ends the chain with what was delivered, never with another leg.
    #[test]
    fn a_failed_summary_still_yields_the_answer_and_a_failed_continue_stops_the_chain() {
        let mut planner = LegPlanner::new(LegLimits::default());
        let failed = LegOutcome { finish_reason: String::new(), delivered_tokens: 0, requested_tokens: 2048, failed: true };
        assert_eq!(planner.after(LegRole::Summary, &failed), Some(LegRole::Answer));
        let cut = LegOutcome { finish_reason: "length".into(), delivered_tokens: 256, requested_tokens: 2048, failed: false };
        assert_eq!(planner.after(LegRole::Answer, &cut), Some(LegRole::Continue));
        let failed_cut = LegOutcome { failed: true, ..cut };
        assert_eq!(planner.after(LegRole::Continue, &failed_cut), None);
    }

    fn turns(n: usize, len: usize) -> Vec<ChatMessage> {
        let mut out = vec![ChatMessage::new("system", "日本語で答えてください。")];
        for i in 0..n {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            out.push(ChatMessage::new(role, format!("{i}:{}", "あ".repeat(len))));
        }
        out
    }

    /// The trim plan: what fits is kept untouched; what does not is dropped oldest-first and the
    /// dropped list is exactly the turns missing from `kept`; and past the threshold the reserve
    /// makes room for the summary before anything is summarized.
    #[test]
    fn the_context_plan_names_what_was_dropped_and_reserves_room_for_a_summary() {
        let limits = LegLimits { summarize_after_turns: 4, continue_max_legs: 2 };
        let short = turns(2, 10);
        let plan = plan_context(&short, Some(512), 2048, 0, limits);
        assert_eq!(plan.kept, short, "a conversation that fits is left alone");
        assert!(plan.dropped.is_empty());
        assert_eq!(plan.prompt_budget, 512 - 24 - 256);

        let long = turns(9, 60);
        let plan = plan_context(&long, Some(512), 2048, 0, limits);
        assert!(plan.dropped.len() > 4, "nine turns of 60 kana cannot fit a 232-token budget: {}", plan.dropped.len());
        assert_eq!(plan.kept[0].role, "system");
        assert_eq!(plan.kept.last(), long.last(), "the question survives");
        let kept_turns: Vec<_> = plan.kept.iter().filter(|m| m.role != "system").collect();
        let expected_dropped: Vec<_> = long.iter().filter(|m| m.role != "system").take(plan.dropped.len()).cloned().collect();
        assert_eq!(plan.dropped, expected_dropped, "dropped is the oldest turns, in order");
        assert_eq!(kept_turns.len() + plan.dropped.len(), 9);
        // On the 512 class the reserve leaves 32 prompt tokens, and the question alone is 69: the
        // trim keeps the question regardless (the ceiling shrinks to fit, later), so what is kept
        // is the irreducible minimum — the system turns and the question — and nothing else.
        assert_eq!(kept_turns.len(), 1, "the reserve is spent down to the question itself: {:?}", plan.kept);
        assert!(prompt_tokens_upper_bound(&plan.kept) > plan.prompt_budget - SUMMARY_RESERVE_TOKENS);

        // On a wider row the reserve is a real inequality: history is kept up to it and no further,
        // so the summary turn will fit beside what was kept without dropping anything more.
        let wide = turns(20, 40);
        let plan = plan_context(&wide, Some(1024), 2048, 0, limits);
        let kept_turns = plan.kept.iter().filter(|m| m.role != "system").count();
        assert!(plan.dropped.len() > 4 && kept_turns > 1, "dropped {} kept {kept_turns}", plan.dropped.len());
        let reserved = plan.prompt_budget - SUMMARY_RESERVE_TOKENS;
        assert!(prompt_tokens_upper_bound(&plan.kept) <= reserved, "the summary's room is reserved");
        let (without_reserve, dropped_without) = fit_messages_to_budget(&wide, plan.prompt_budget);
        assert!(without_reserve.len() > plan.kept.len() && dropped_without < plan.dropped.len(), "the reserve cost history");

        // Unknown context: nothing is trimmed, and the plan says the budget is unbounded.
        let plan = plan_context(&long, None, 2048, 0, limits);
        assert_eq!(plan.kept, long);
        assert_eq!(plan.prompt_budget, u64::MAX);
    }

    /// The summary job: the instruction, then the dropped turns as `role: content` lines in one
    /// user turn — the newest of them first to be admitted, in chronological order once they are
    /// — and `None` rather than a prompt the worker would refuse.
    #[test]
    fn summary_job_messages_hold_the_dropped_turns_that_fit_newest_first() {
        let dropped: Vec<ChatMessage> = turns(6, 30).into_iter().filter(|m| m.role != "system").collect();
        let (messages, covered) = summary_job_messages(&dropped, 512 - 24 - SUMMARY_TOKENS).expect("they fit");
        assert_eq!(covered, 6);
        assert_eq!(messages[0], ChatMessage::new("system", SUMMARY_INSTRUCTION));
        let body = &messages[1].content;
        assert!(body.starts_with("user: 0:"), "chronological order: {body}");
        assert!(body.contains("\nassistant: 5:"), "{body}");

        let (messages, covered) = summary_job_messages(&dropped, 120).expect("some fit");
        assert!(covered < 6 && covered > 0, "covered {covered}");
        assert!(messages[1].content.contains("assistant: 5:"), "the newest dropped turn is the first admitted");
        assert!(!messages[1].content.contains("user: 0:"), "the oldest is what goes");
        assert!(prompt_tokens_upper_bound(&messages) <= 120);

        assert_eq!(summary_job_messages(&dropped, 10), None, "not even one turn fits: no job");
        assert_eq!(summary_job_messages(&[], 1000), None, "nothing to summarize");
    }

    /// The summary rides as a system turn AFTER the original system prompt, and the result is
    /// fitted again so an overlong summary costs history rather than the request.
    #[test]
    fn the_summary_rides_as_a_system_turn_after_the_original_system_prompt() {
        let kept = vec![ChatMessage::new("system", "be brief"), ChatMessage::new("user", "and now?")];
        let (messages, dropped) = with_summary(&kept, "Alice asked for a plan; Bob chose option 2.", 1000);
        assert_eq!(dropped, 0);
        assert_eq!(messages[0].content, "be brief");
        assert_eq!(messages[1].role, "system");
        assert_eq!(messages[1].content, format!("{SUMMARY_PREFIX}Alice asked for a plan; Bob chose option 2."));
        assert_eq!(messages[2].content, "and now?");

        let kept = turns(4, 20);
        let (messages, dropped) = with_summary(&kept, &"あ".repeat(100), 160);
        assert!(dropped > 0, "an overlong summary costs history");
        assert!(messages.iter().any(|m| m.content.starts_with(SUMMARY_PREFIX)), "and the summary itself survives");
    }

    /// A continue leg carries the answer so far as the assistant's turn and the instruction as
    /// the user's — and is refused when the answer cannot be shown to the model that must go on.
    #[test]
    fn a_continue_leg_carries_the_answer_so_far_and_the_instruction() {
        let context = vec![ChatMessage::new("system", "be brief"), ChatMessage::new("user", "explain")];
        let messages = continue_leg_messages(&context, "First, the", 1000).expect("fits");
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2], ChatMessage::new("assistant", "First, the"));
        assert_eq!(messages[3], ChatMessage::new("user", CONTINUE_INSTRUCTION));

        let long_context = turns(6, 30);
        let messages = continue_leg_messages(&long_context, "First, the", 150).expect("history gives way");
        assert!(messages.len() < long_context.len() + 2, "older turns were dropped for the answer");
        assert_eq!(messages[messages.len() - 2].content, "First, the");

        assert_eq!(continue_leg_messages(&context, &"あ".repeat(400), 200), None, "the answer so far itself does not fit");
    }

    /// The ceiling rule: an ask that fits is honoured; one that does not is sized like an answer;
    /// no room at all is a refusal that names the numbers; no context is no rule.
    #[test]
    fn the_answer_ceiling_fits_the_class_and_names_a_full_context() {
        assert_eq!(answer_ceiling(50, Some(512), None, 100).expect("fits"), 100);
        assert_eq!(answer_ceiling(50, Some(512), None, 2048).expect("sized"), DEFAULT_ANSWER_TOKENS);
        assert_eq!(answer_ceiling(400, Some(512), None, 2048).expect("sized"), 512 - 424);
        assert_eq!(answer_ceiling(50, None, None, 2048).expect("no rule"), 2048);
        match answer_ceiling(500, Some(512), None, 10) {
            Err(Error::BadRequest { message }) => assert!(message.contains("512") && message.contains("524"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The notice prints what was asked beside what ran, and names the knobs that have no rule.
    #[test]
    fn the_sampling_notice_prints_what_was_asked_beside_what_ran() {
        let params = SamplingCommitment { temperature: 0.7, seed: Some(7), ..Default::default() };
        let notice = sampling_notice(&params, "testnet-11");
        assert_eq!(notice["requested"]["temperature"], 0.7);
        assert_eq!(notice["requested"]["seed"], 7);
        assert_eq!(notice["applied"]["temperature"], 0);
        assert_eq!(notice["applied"]["seed"], GREEDY_SEED_HEX);
        assert_eq!(notice["reason"], "palw_fp_decode_rules is not armed on testnet-11; the lane replays a greedy decode");
        assert_eq!(notice["not_a_rule_on_this_lane"], json!(["top_p", "top_k", "min_p", "repeat_penalty"]));
    }

    /// Identity values ask for nothing; everything else is named with its value, and the refusal
    /// under `refuse` says the way back.
    #[test]
    fn sampling_divergence_names_every_knob_off_identity_and_the_refusal_names_the_way_back() {
        let greedy =
            SamplingCommitment { temperature: 0.0, top_p: 1.0, top_k: 0, min_p: 0.0, repeat_penalty: 1.0, seed: None, max_tokens: 64 };
        assert!(sampling_divergence(&greedy).is_empty());
        let names: Vec<&str> = sampling_divergence(&SamplingCommitment::default()).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["temperature", "top_p", "top_k", "min_p", "repeat_penalty"], "the Studio's defaults are not greedy");
        let with_seed = SamplingCommitment { seed: Some(3), ..greedy };
        assert_eq!(sampling_divergence(&with_seed), vec![("seed", json!(3))]);

        let refusal = sampling_refusal(&sampling_divergence(&with_seed), "devnet");
        assert!(refusal.contains("seed=3") && refusal.contains("devnet") && refusal.contains("greedy_with_notice"), "{refusal}");
    }

    /// `misaka.jobs[]` entries carry the ids the leg's gateway object had, or nulls and an error.
    #[test]
    fn job_entries_carry_the_ids_and_a_failed_leg_says_why() {
        let leg = LegResult {
            text: "…".into(),
            usage: Usage { prompt_tokens: 51, completion_tokens: 160, total_tokens: 211 },
            finish_reason: "stop".into(),
            misaka: Some(json!({"fp_job_id": "aa", "fp_claim_id": "bb", "committed": true})),
        };
        let entry = job_entry(LegRole::Summary, &leg);
        assert_eq!(entry, json!({"fp_job_id":"aa","fp_claim_id":"bb","role":"summary","prompt_tokens":51,"decode_tokens":160}));
        let failed = job_error(LegRole::Continue, "connection refused".into());
        assert_eq!(failed["role"], "continue");
        assert_eq!(failed["fp_claim_id"], Value::Null);
        assert_eq!(failed["error"], "connection refused");

        let misaka = finalize_lane_misaka(Some(json!({"fp_claim_id": "bb"})), vec![entry, failed], json!({"n_ctx": 512}));
        assert_eq!(misaka["fp_claim_id"], "bb", "the answer leg's object stays the top-level one");
        assert_eq!(misaka["jobs"].as_array().map(Vec::len), Some(2));
        assert_eq!(misaka["context"]["n_ctx"], 512);
    }

    // -----------------------------------------------------------------------------------------
    // ADR-0097 Decision 2: the limits, read; the refusal, branched on by its code
    // -----------------------------------------------------------------------------------------

    /// The body `misaka-palw-gateway` serves for the worker's window refusal — the shape its
    /// `surface::refusal_body` builds and its test
    /// `a_context_refusal_carries_a_code_and_its_numbers_and_the_body_is_the_gateways_own` pins.
    fn gateway_window_refusal(message: &str, prompt_tokens: u64, context_window: u64) -> Value {
        json!({
            "error": { "message": message, "type": "invalid_request_error", "code": CONTEXT_LENGTH_EXCEEDED },
            "misaka": { "refusal": {
                "code": CONTEXT_LENGTH_EXCEEDED,
                "prompt_tokens": prompt_tokens,
                "decode_ceiling": 476,
                "context_window": context_window,
                "room_for_answer": context_window.saturating_sub(prompt_tokens),
            } },
        })
    }

    /// `/health` with the gateway's limits object, as `surface::limits_body` builds it.
    fn health_with_limits(context_window: u64, max_output_tokens: u64, committed: &str) -> Value {
        json!({
            "status": "ok",
            "class_id": "ab".repeat(64),
            "n_ctx": 4096,
            "runtime_manifest_hash": "00",
            "can_submit": false,
            "chain": { "fp_certified": true, "fp_decode_constraint_armed": true },
            "limits": {
                "schema": LIMITS_SCHEMA_V1,
                "context_window": context_window,
                "max_output_tokens": max_output_tokens,
                "default_output_tokens": 256,
                "max_prompt_bytes": 65_536,
                "tokenizer_id": "cd".repeat(64),
                "features": { "require_committed_format": committed },
            },
        })
    }

    /// **ADR-0097 invariant 7's Studio half.** The window is the limits' `context_window` — the
    /// bare `n_ctx` beside it deliberately disagrees, and loses — and the committed-format fence is
    /// the limits' word, not the chain flag beside it. A gateway without limits (or with another
    /// schema) is read the old way, field for field.
    #[test]
    fn the_health_limits_are_the_window_and_an_older_gateway_is_read_the_old_way() {
        let facts = facts_from_health(&health_with_limits(512, 511, "refused"));
        assert_eq!(facts.n_ctx, 512, "the limits' window, not the bare n_ctx beside it");
        let limits = facts.limits.clone().expect("limits read");
        assert_eq!(limits.max_output_tokens, 511);
        assert_eq!(limits.max_prompt_bytes, 65_536);
        assert_eq!(limits.tokenizer_id, "cd".repeat(64));
        assert!(!facts.fp_decode_constraint_armed, "the limits say refused; the chain flag beside them does not decide");
        assert!(facts.fp_certified);

        let served = facts_from_health(&health_with_limits(512, 511, "served"));
        assert!(served.fp_decode_constraint_armed);

        let mut older = health_with_limits(512, 511, "refused");
        older.as_object_mut().unwrap().remove("limits");
        let facts = facts_from_health(&older);
        assert_eq!(facts.limits, None);
        assert_eq!(facts.n_ctx, 4096, "no limits: the bare n_ctx, as before");
        assert!(facts.fp_decode_constraint_armed, "no limits: the chain flag, as before");

        let mut other_schema = health_with_limits(512, 511, "refused");
        other_schema["limits"]["schema"] = json!("misaka.palw.limits.v2");
        assert_eq!(facts_from_health(&other_schema).limits, None, "a schema this build does not know is not read as v1");
        let mut zero = health_with_limits(512, 0, "refused");
        zero["limits"]["max_output_tokens"] = json!(0);
        assert_eq!(facts_from_health(&zero).limits, None, "a zero ceiling is not a limit this lane can plan with");
    }

    /// The ceiling never asks one job for more than the gateway runs; the request's own ask is
    /// untouched (the continue legs compare delivery against it).
    #[test]
    fn the_answer_ceiling_never_asks_one_job_for_more_than_the_gateway_runs() {
        assert_eq!(answer_ceiling(50, Some(512), Some(64), 2048).expect("capped"), 64);
        assert_eq!(answer_ceiling(50, Some(512), Some(64), 40).expect("under the cap"), 40);
        assert_eq!(answer_ceiling(50, Some(512), Some(511), 100).expect("fits"), 100);
        assert_eq!(answer_ceiling(50, Some(512), Some(511), 2048).expect("sized like an answer"), DEFAULT_ANSWER_TOKENS);
        assert_eq!(answer_ceiling(50, None, Some(64), 2048).expect("no window, still the cap"), 64);
        assert_eq!(answer_ceiling(50, None, None, 2048).expect("nothing known"), 2048);
    }

    /// **ADR-0097 invariant 8's Studio half.** The retry is decided by the CODE and computed from
    /// its numbers: a coded refusal whose sentence names no numbers still retries at the right
    /// ceiling, and a coded refusal that is not the window never retries even when its sentence
    /// happens to parse. Only a refusal with no code — a gateway from before ADR-0097 — is read
    /// from its sentence.
    #[test]
    fn a_refusal_is_read_by_its_code_and_the_sentence_only_without_one() {
        let sentence = "the worker refused the job: prompt 51 + decode ceiling 476 exceeds max_context_tokens 512";
        let coded = LaneRefusal::from_body(&gateway_window_refusal(sentence, 51, 512)).expect("an error body");
        assert_eq!(coded.message, sentence, "error.message is the sentence a person reads");
        assert_eq!(coded.code.as_deref(), Some(CONTEXT_LENGTH_EXCEEDED));
        assert_eq!(coded.retry_ceiling(), Some(460));

        let reworded = LaneRefusal::from_body(&gateway_window_refusal("this job does not fit the class", 51, 512)).unwrap();
        assert_eq!(reworded.retry_ceiling(), Some(460), "the code and its numbers, not the sentence, decide");
        assert_eq!(ceiling_from_refusal(&reworded.message), None, "the sentence alone could not have");

        let bytes = LaneRefusal::from_body(&json!({
            "error": { "message": sentence, "type": "invalid_request_error", "code": "prompt_bytes_exceeded" },
            "misaka": { "refusal": { "code": "prompt_bytes_exceeded", "prompt_bytes": 70_000, "max_prompt_bytes": 65_536 } },
        }))
        .unwrap();
        assert_eq!(bytes.retry_ceiling(), None, "a coded refusal no ceiling fixes is never retried, whatever its sentence says");

        let older = LaneRefusal::from_body(&json!({ "error": { "message": sentence, "type": "invalid_request_error" } })).unwrap();
        assert_eq!(older.code, None);
        assert_eq!(older.retry_ceiling(), Some(460), "no code: the sentence, as before ADR-0097");

        let flat = LaneRefusal::from_body(&json!({ "error": sentence })).unwrap();
        assert_eq!(flat.message, sentence, "a sentence put in `error` itself is still the sentence");
        assert_eq!(LaneRefusal::from_body(&json!({ "choices": [] })), None);
        let proxy = LaneRefusal::from_text(&format!("<html>{}</html>", "x".repeat(1_000)));
        assert_eq!(proxy.message.chars().count(), 400, "a proxy's page is cut, not repeated whole");
        assert_eq!(LaneRefusal::from_text(&gateway_window_refusal(sentence, 51, 512).to_string()), coded);
    }

    /// A gateway on a real socket: `/health` publishes its limits, and the first job is refused in
    /// the stream with a coded window refusal whose SENTENCE NAMES NO NUMBERS. Every request body
    /// is recorded, so the test reads what the backend actually sent.
    async fn coded_refusal_gateway() -> (String, std::sync::Arc<std::sync::Mutex<Vec<Value>>>) {
        use axum::extract::State;
        use axum::routing::{get, post};
        type Seen = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;
        let seen: Seen = Default::default();
        let app = axum::Router::new()
            .route("/health", get(|| async { axum::Json(health_with_limits(512, 511, "refused")) }))
            .route(
                "/v1/chat/completions",
                post(|State(seen): State<Seen>, axum::Json(body): axum::Json<Value>| async move {
                    let first = {
                        let mut seen = seen.lock().unwrap();
                        seen.push(body);
                        seen.len() == 1
                    };
                    let events = if first {
                        // After the 200 head, as the gateway does for `stream: true`.
                        vec![gateway_window_refusal("the worker refused the job: it does not fit this class", 100, 512)]
                    } else {
                        vec![
                            json!({ "choices": [{ "index": 0, "delta": { "content": "ok" }, "finish_reason": null }] }),
                            json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] }),
                            json!({ "misaka": { "fp_job_id": "j1", "fp_claim_id": "c1", "committed": false },
                                    "usage": { "prompt_tokens": 100, "completion_tokens": 1, "total_tokens": 101 } }),
                        ]
                    };
                    let mut sse: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
                    sse.push_str("data: [DONE]\n\n");
                    ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], sse)
                }),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("binds");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    /// **End to end: the retry follows the code.** The refusal's sentence names no numbers, so a
    /// sentence parser would have given up and shown an empty reply; the backend retries once at
    /// `window − prompt − 1` from the numbers the code carries, and the answer arrives.
    #[tokio::test]
    async fn a_coded_refusal_is_retried_at_its_numbers_even_when_the_sentence_names_none() {
        use futures_util::StreamExt;
        let (url, seen) = coded_refusal_gateway().await;
        let backend = GatewayBackend::new(url, None);
        let loaded = backend
            .load(LoadRequest {
                model_id: "class".into(),
                model_path: std::path::PathBuf::new(),
                context_size: 4096,
                gpu_layers: None,
                threads: None,
                flash_attention: misaka_studio_core::settings::FlashAttention::default(),
                use_mmap: true,
                use_mlock: false,
                needs_default_chat_template: false,
                extra_args: Vec::new(),
            })
            .await
            .expect("the gateway is up");
        assert_eq!(loaded.context_size, 512, "the window the limits state, not the request's 4096 or the bare n_ctx");

        let request = GenerationRequest::plain("class", vec![ChatMessage::new("user", "hello")], None, Default::default());
        let mut stream = backend.generate(request).await.expect("the first job was sent");
        let mut text = String::new();
        let mut done = None;
        while let Some(event) = stream.next().await {
            match event.expect("no error reaches the client: the retry fitted") {
                StreamEvent::Delta(delta) => text.push_str(&delta),
                StreamEvent::Done { misaka, .. } => done = misaka,
                StreamEvent::ToolCallDelta(_) => {}
            }
        }
        assert_eq!(text, "ok");
        let misaka = done.expect("the answer's misaka object");
        assert_eq!(misaka["fp_claim_id"], "c1");
        assert_eq!(misaka["context"]["max_output_tokens"], 511, "the published per-job ceiling rides the context report");

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "one refusal, one retry, nothing more");
        let first = seen[0]["max_tokens"].as_u64().expect("a ceiling");
        assert!(first <= 511, "no job is asked for more than the gateway runs: {first}");
        assert_eq!(seen[1]["max_tokens"], 512 - 100 - 1, "the retry's ceiling is the code's arithmetic");
        assert_eq!(seen[1]["messages"], seen[0]["messages"], "the same messages, retried");
    }
}
