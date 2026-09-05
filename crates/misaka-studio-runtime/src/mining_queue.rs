//! **Mining runs behind the chat, not in front of it.**
//!
//! On this network the free-prompt lane makes the answer the work: one execution by the
//! deterministic runtime, under a bonded key, and the claim behind it is priced by what it ran.
//! That execution takes minutes — decode runs to its budget whatever the answer's length — and
//! for a while the Chat tab waited on it: seven minutes of "Mining your answer…", and every lane
//! condition (an unfunded slot, a full bond, a node mid-restart) surfaced as a chat error.
//!
//! Hash mining never worked like that. The miner runs on its own cadence, accumulates, submits,
//! and the person is told what landed. This is that shape for prompts: the Chat tab answers from
//! the engine that can answer now, and every prompt is *also* queued here, where one worker runs
//! it through the pool slot's gateway, commits the claim, and records what came back. The chat
//! is told — quietly, under the message — what became of it.
//!
//! Two things this deliberately is NOT:
//!
//! - It does not pretend the mined answer is the chat's answer. Two engines, two outputs. The
//!   mined one is kept on the job, and the UI may show it next to the chat's; misakascan shows
//!   the mined one, because that is the one the chain holds.
//! - It does not mine the conversation. A job is the latest user turn (plus the system prompt).
//!   The class holds 512 tokens and a history-bearing job hits that wall by the second message;
//!   one prompt per job is what the lane was registered for.
//!
//! Persistence is a single JSON file rewritten atomically. The queue is small — tens of jobs —
//! and a file the next start can read whole is worth more than an append log it has to fold.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, RwLock};

/// How many finished jobs the file keeps. Queued and running jobs are never dropped.
const KEEP_FINISHED: usize = 200;
/// Transient failures (gateway unreachable, node restarting) retry this many times before the job
/// is marked failed — with a backoff that reaches ten minutes, that is over an hour of patience.
const MAX_TRANSIENT_ATTEMPTS: u32 = 8;
/// One free-prompt execution legitimately runs for minutes; the pool proxy allows fifteen.
const JOB_TIMEOUT: Duration = Duration::from_secs(900);
/// The answer ceiling asked of the gateway. The lane decodes to its ceiling whatever the answer's
/// length, so this is the latency knob as much as the length knob — and the gateway clamps it to
/// what the class's context leaves anyway.
const ANSWER_TOKENS: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Waiting for the worker (or for its retry backoff to elapse).
    Queued,
    /// Handed to the gateway; the worker is inside the request.
    Running,
    /// The gateway answered with a job block: the claim is committed and the slot's submitter
    /// carries it to the chain.
    Committed,
    /// The lane said no, about this job: a 4xx with a reason. Not retried.
    Refused,
    /// The machine said no too many times: unreachable, timeouts, restarts. Retried out.
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Msg {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MiningJob {
    pub id: String,
    /// Where the prompt came from, so the chat can find its badge. Absent for prompts queued
    /// from elsewhere (the Network tab, the API).
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
    /// The person's text — what the badge is under.
    pub prompt: String,
    /// What is sent: the system prompt (if any) and the user turn. Not the history.
    pub messages: Vec<Msg>,
    pub created_ms: u64,
    pub status: JobStatus,
    pub attempts: u32,
    /// Earliest time the worker may pick this up again after a transient failure.
    #[serde(default)]
    pub not_before_ms: u64,
    pub started_ms: Option<u64>,
    pub finished_ms: Option<u64>,
    pub fp_job_id: Option<String>,
    pub claim_id: Option<String>,
    /// The mined answer — the worker's, not the chat engine's.
    pub answer: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    /// The lane's own words for a refusal or the last transient failure. Passed through, not
    /// translated: it is about the chain and the slot, not about this app.
    pub error: Option<String>,
    /// The gateway this job went (or will go) to. Recorded so a slot change mid-queue is visible.
    pub gateway_url: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Counts {
    pub queued: usize,
    pub running: usize,
    pub committed: usize,
    pub refused: usize,
    pub failed: usize,
}

pub struct MiningQueue {
    path: PathBuf,
    jobs: RwLock<Vec<MiningJob>>,
    wake: Notify,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn uid() -> String {
    format!("{:x}{:x}", now_ms(), rand_u32())
}

fn rand_u32() -> u32 {
    // Enough to keep two jobs created in the same millisecond apart; not a security boundary.
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    t.wrapping_mul(2654435761)
}

impl MiningQueue {
    pub async fn open(path: PathBuf) -> Arc<Self> {
        let jobs = match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice::<Vec<MiningJob>>(&bytes) {
                Ok(mut jobs) => {
                    // A job that was running when the process died did not finish: the gateway
                    // may have completed it, but nobody read the answer, so it runs again. The
                    // lane treats a repeated prompt as a new job; the cost is one execution.
                    for job in jobs.iter_mut().filter(|j| j.status == JobStatus::Running) {
                        job.status = JobStatus::Queued;
                        job.started_ms = None;
                    }
                    jobs
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), "mining queue unreadable, starting empty: {e}");
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Arc::new(MiningQueue { path, jobs: RwLock::new(jobs), wake: Notify::new() })
    }

    pub async fn list(&self) -> Vec<MiningJob> {
        let mut jobs = self.jobs.read().await.clone();
        jobs.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        jobs
    }

    pub async fn counts(&self) -> Counts {
        let jobs = self.jobs.read().await;
        let n = |s: JobStatus| jobs.iter().filter(|j| j.status == s).count();
        Counts {
            queued: n(JobStatus::Queued),
            running: n(JobStatus::Running),
            committed: n(JobStatus::Committed),
            refused: n(JobStatus::Refused),
            failed: n(JobStatus::Failed),
        }
    }

    pub async fn enqueue(
        &self,
        prompt: String,
        system_prompt: Option<String>,
        conversation_id: Option<String>,
        message_id: Option<String>,
        gateway_url: String,
    ) -> MiningJob {
        let mut messages = Vec::new();
        if let Some(system) = system_prompt.filter(|s| !s.trim().is_empty()) {
            messages.push(Msg { role: "system".into(), content: system });
        }
        messages.push(Msg { role: "user".into(), content: prompt.clone() });
        // **The same prompt, still waiting, is the same job.** The lane is deterministic: two
        // identical jobs produce two identical answers and two claims, six minutes and a fee
        // apart, for one question. A prompt that is already queued (not yet running — a running
        // one is the gateway's, and a finished one was a different moment) is answered by the job
        // that is going to run anyway; the new message's badge simply points at it.
        {
            let jobs = self.jobs.read().await;
            if let Some(existing) = jobs.iter().find(|j| {
                j.status == JobStatus::Queued
                    && j.messages.len() == messages.len()
                    && j.messages.iter().zip(&messages).all(|(a, b)| a.role == b.role && a.content == b.content)
            }) {
                return existing.clone();
            }
        }
        let job = MiningJob {
            id: uid(),
            conversation_id,
            message_id,
            prompt,
            messages,
            created_ms: now_ms(),
            status: JobStatus::Queued,
            attempts: 0,
            not_before_ms: 0,
            started_ms: None,
            finished_ms: None,
            fp_job_id: None,
            claim_id: None,
            answer: None,
            prompt_tokens: None,
            completion_tokens: None,
            error: None,
            gateway_url,
        };
        {
            let mut jobs = self.jobs.write().await;
            jobs.push(job.clone());
            trim(&mut jobs);
        }
        self.persist().await;
        self.wake.notify_one();
        job
    }

    /// Drop a job that has not started. A running job is the gateway's now; it finishes.
    pub async fn remove(&self, id: &str) -> bool {
        let removed = {
            let mut jobs = self.jobs.write().await;
            let before = jobs.len();
            jobs.retain(|j| !(j.id == id && j.status != JobStatus::Running));
            jobs.len() != before
        };
        if removed {
            self.persist().await;
        }
        removed
    }

    /// Put a refused or failed job back in the queue — after fixing what refused it.
    pub async fn retry(&self, id: &str) -> bool {
        let found = {
            let mut jobs = self.jobs.write().await;
            match jobs.iter_mut().find(|j| j.id == id && matches!(j.status, JobStatus::Refused | JobStatus::Failed)) {
                Some(job) => {
                    job.status = JobStatus::Queued;
                    job.attempts = 0;
                    job.not_before_ms = 0;
                    job.error = None;
                    true
                }
                None => false,
            }
        };
        if found {
            self.persist().await;
            self.wake.notify_one();
        }
        found
    }

    async fn persist(&self) {
        let jobs = self.jobs.read().await.clone();
        if let Err(e) = write_atomic(&self.path, &jobs).await {
            tracing::warn!(path = %self.path.display(), "could not persist the mining queue: {e}");
        }
    }

    async fn update<F: FnOnce(&mut MiningJob)>(&self, id: &str, f: F) {
        {
            let mut jobs = self.jobs.write().await;
            if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
                f(job);
            }
        }
        self.persist().await;
    }

    /// The next job the worker may run: oldest queued whose backoff has elapsed.
    async fn next_ready(&self) -> Option<MiningJob> {
        let now = now_ms();
        let jobs = self.jobs.read().await;
        jobs.iter().filter(|j| j.status == JobStatus::Queued && j.not_before_ms <= now).min_by_key(|j| j.created_ms).cloned()
    }

    /// **The worker.** One at a time — the gateway has one job slot, and two requests in flight
    /// would only queue there instead of here, invisibly.
    pub fn spawn_worker(self: &Arc<Self>, state: Arc<AppState>) {
        let queue = self.clone();
        tokio::spawn(async move {
            loop {
                let Some(job) = queue.next_ready().await else {
                    // Nothing ready: sleep until something is enqueued, or a backoff may have
                    // elapsed. The tick is coarse on purpose; this is a miner, not a UI.
                    tokio::select! {
                        _ = queue.wake.notified() => {}
                        _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                    }
                    continue;
                };
                queue
                    .update(&job.id, |j| {
                        j.status = JobStatus::Running;
                        j.started_ms = Some(now_ms());
                        j.attempts += 1;
                    })
                    .await;
                let (url, token) = {
                    let settings = state.settings.read().await;
                    // The slot the person holds NOW. A job queued under a slot they have since
                    // left is still theirs; the new slot's bond carries it.
                    let url = settings.node.palw_gateway_url.clone().unwrap_or_else(|| job.gateway_url.clone());
                    (url.trim_end_matches('/').to_string(), settings.node.pool_slot_token.clone())
                };
                match run_job(&job, &url, token.as_deref()).await {
                    Ok(done) => {
                        tracing::info!(job = %job.id, claim = ?done.claim_id, "free-prompt job committed from the queue");
                        queue
                            .update(&job.id, |j| {
                                j.status = JobStatus::Committed;
                                j.finished_ms = Some(now_ms());
                                j.gateway_url = url.clone();
                                j.fp_job_id = done.fp_job_id;
                                j.claim_id = done.claim_id;
                                j.answer = Some(done.answer);
                                j.prompt_tokens = done.prompt_tokens;
                                j.completion_tokens = done.completion_tokens;
                                j.error = None;
                            })
                            .await;
                    }
                    Err(Outcome::Refused(why)) => {
                        tracing::warn!(job = %job.id, "free-prompt job refused: {why}");
                        queue
                            .update(&job.id, |j| {
                                j.status = JobStatus::Refused;
                                j.finished_ms = Some(now_ms());
                                j.error = Some(why);
                            })
                            .await;
                    }
                    Err(Outcome::Transient(why)) => {
                        let attempts = job.attempts + 1;
                        let give_up = attempts >= MAX_TRANSIENT_ATTEMPTS;
                        let backoff_s = (60u64 * attempts as u64).min(600);
                        tracing::warn!(job = %job.id, attempts, "free-prompt job did not run ({why}); {}", if give_up { "giving up" } else { "will retry" });
                        queue
                            .update(&job.id, |j| {
                                j.error = Some(why);
                                if give_up {
                                    j.status = JobStatus::Failed;
                                    j.finished_ms = Some(now_ms());
                                } else {
                                    j.status = JobStatus::Queued;
                                    j.not_before_ms = now_ms() + backoff_s * 1000;
                                }
                            })
                            .await;
                    }
                }
            }
        });
    }
}

fn trim(jobs: &mut Vec<MiningJob>) {
    let finished = jobs.iter().filter(|j| matches!(j.status, JobStatus::Committed | JobStatus::Refused | JobStatus::Failed)).count();
    if finished <= KEEP_FINISHED {
        return;
    }
    // Oldest finished go first; the queue's live entries are never candidates.
    let mut finished_idx: Vec<usize> = jobs
        .iter()
        .enumerate()
        .filter(|(_, j)| matches!(j.status, JobStatus::Committed | JobStatus::Refused | JobStatus::Failed))
        .map(|(i, _)| i)
        .collect();
    finished_idx.sort_by_key(|&i| jobs[i].created_ms);
    let drop: std::collections::HashSet<usize> = finished_idx.into_iter().take(finished - KEEP_FINISHED).collect();
    let mut i = 0;
    jobs.retain(|_| {
        let keep = !drop.contains(&i);
        i += 1;
        keep
    });
}

async fn write_atomic(path: &Path, jobs: &[MiningJob]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(jobs).map_err(std::io::Error::other)?;
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

struct Done {
    answer: String,
    fp_job_id: Option<String>,
    claim_id: Option<String>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

enum Outcome {
    /// The lane refused this job for a reason about the job or the slot. Kept verbatim.
    Refused(String),
    /// The machine was not there. Worth retrying.
    Transient(String),
}

/// One request to the slot's gateway, non-streaming: the queue wants the job block at the end,
/// not the tokens as they come. Mirrors the chat engine's request so the lane sees one shape.
async fn run_job(job: &MiningJob, url: &str, token: Option<&str>) -> std::result::Result<Done, Outcome> {
    let http = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(JOB_TIMEOUT)
        .user_agent(concat!("misaka-studio/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Outcome::Transient(format!("http client: {e}")))?;
    let messages: Vec<serde_json::Value> =
        job.messages.iter().map(|m| serde_json::json!({ "role": m.role, "content": m.content })).collect();
    let mut ceiling = ANSWER_TOKENS;
    for pass in 0..2 {
        let body = serde_json::json!({
            "model": "misaka-palw-fp-v3",
            "messages": messages,
            "max_tokens": ceiling,
            "stream": false,
        });
        let mut request = http.post(format!("{url}/v1/chat/completions")).json(&body);
        if let Some(token) = token {
            request = request.header("x-pool-token", token);
        }
        let response = request.send().await.map_err(|e| Outcome::Transient(format!("the gateway did not accept the request: {e}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| Outcome::Transient(format!("the gateway's answer was cut off: {e}")))?;
        if !status.is_success() {
            let message = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())))
                .unwrap_or_else(|| text.trim().chars().take(400).collect());
            // The worker's refusal for an ask that does not fit the class carries the numbers that
            // make the retry exact; one retry with those numbers, then it is the lane's answer.
            if pass == 0 && let Some(room) = crate::backend::gateway::ceiling_from_refusal(&message) {
                ceiling = room.min(ANSWER_TOKENS).max(1);
                continue;
            }
            let transient = status.is_server_error() || status.as_u16() == 429 || message.contains("Connection refused") || message.contains("could not be asked");
            return Err(if transient { Outcome::Transient(format!("{status}: {message}")) } else { Outcome::Refused(format!("{status}: {message}")) });
        }
        let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| Outcome::Transient(format!("the gateway's answer was not JSON: {e}")))?;
        let answer = body
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string();
        let misaka = body.get("misaka");
        let field = |key: &str| misaka.and_then(|m| m.get(key)).and_then(|v| v.as_str()).map(str::to_string);
        if misaka.is_none() {
            // An answer with no job block is a chat, not a job — it mined nothing.
            return Err(Outcome::Refused("the gateway answered without a job block: that reply was a chat, not a claim".into()));
        }
        let usage = |key: &str| body.get("usage").and_then(|u| u.get(key)).and_then(|v| v.as_u64());
        return Ok(Done {
            answer,
            fp_job_id: field("fp_job_id"),
            claim_id: field("fp_claim_id").or_else(|| field("claim_id")),
            prompt_tokens: usage("prompt_tokens"),
            completion_tokens: usage("completion_tokens"),
        });
    }
    Err(Outcome::Refused("the gateway refused the retry sized from its own numbers".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_job_survives_a_restart_and_a_running_one_is_requeued() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mining-queue.json");
        let queue = MiningQueue::open(path.clone()).await;
        let job = queue.enqueue("hello".into(), Some("be brief".into()), Some("c1".into()), Some("m1".into()), "http://gw".into()).await;
        assert_eq!(job.messages.len(), 2, "system prompt and the one user turn — never the history");
        queue.update(&job.id, |j| j.status = JobStatus::Running).await;

        let reopened = MiningQueue::open(path).await;
        let jobs = reopened.list().await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Queued, "a job that was running when the process died runs again");
    }

    #[tokio::test]
    async fn only_finished_jobs_are_trimmed_and_the_live_ones_stay() {
        // 210 jobs, five of them live (every 50th): 205 finished against a keep of 200, so the five
        // oldest finished ones — j1..j5, never the live j0 — are the ones that go.
        let mut jobs: Vec<MiningJob> = (0..(KEEP_FINISHED + 10))
            .map(|i| MiningJob {
                id: format!("j{i}"),
                conversation_id: None,
                message_id: None,
                prompt: String::new(),
                messages: vec![],
                created_ms: i as u64,
                status: if i % 50 == 0 { JobStatus::Queued } else { JobStatus::Committed },
                attempts: 0,
                not_before_ms: 0,
                started_ms: None,
                finished_ms: None,
                fp_job_id: None,
                claim_id: None,
                answer: None,
                prompt_tokens: None,
                completion_tokens: None,
                error: None,
                gateway_url: String::new(),
            })
            .collect();
        let live = jobs.iter().filter(|j| j.status == JobStatus::Queued).count();
        trim(&mut jobs);
        assert_eq!(jobs.iter().filter(|j| j.status == JobStatus::Queued).count(), live, "queued jobs are never trimmed");
        assert_eq!(jobs.iter().filter(|j| j.status == JobStatus::Committed).count(), KEEP_FINISHED);
        assert!(!jobs.iter().any(|j| j.id == "j1"), "the oldest finished job went first");
    }

    #[tokio::test]
    async fn the_same_prompt_still_waiting_is_the_same_job() {
        let dir = tempfile::tempdir().expect("tempdir");
        let queue = MiningQueue::open(dir.path().join("q.json")).await;
        let first = queue.enqueue("小林って誰".into(), None, Some("c1".into()), Some("m1".into()), "http://gw".into()).await;
        let again = queue.enqueue("小林って誰".into(), None, Some("c1".into()), Some("m2".into()), "http://gw".into()).await;
        assert_eq!(again.id, first.id, "a queued twin is answered by the job already waiting");
        assert_eq!(queue.list().await.len(), 1);

        // Once it runs it is the gateway's; a new ask is a new job — and so is one with a different
        // system prompt, which is a different job to the lane.
        queue.update(&first.id, |j| j.status = JobStatus::Running).await;
        let third = queue.enqueue("小林って誰".into(), None, None, None, "http://gw".into()).await;
        assert_ne!(third.id, first.id);
        let fourth = queue.enqueue("小林って誰".into(), Some("日本語で".into()), None, None, "http://gw".into()).await;
        assert_ne!(fourth.id, third.id, "the system prompt is part of the job");
        assert_eq!(queue.list().await.len(), 3);
    }

    #[tokio::test]
    async fn retry_reopens_a_refused_job_and_remove_leaves_a_running_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let queue = MiningQueue::open(dir.path().join("q.json")).await;
        let a = queue.enqueue("a".into(), None, None, None, "http://gw".into()).await;
        let b = queue.enqueue("b".into(), None, None, None, "http://gw".into()).await;
        queue.update(&a.id, |j| { j.status = JobStatus::Refused; j.error = Some("bond full".into()); }).await;
        queue.update(&b.id, |j| j.status = JobStatus::Running).await;

        assert!(queue.retry(&a.id).await);
        assert_eq!(queue.list().await.iter().find(|j| j.id == a.id).map(|j| j.status), Some(JobStatus::Queued));
        assert!(!queue.remove(&b.id).await, "a running job belongs to the gateway until it finishes");
        assert!(queue.remove(&a.id).await);
    }
}
