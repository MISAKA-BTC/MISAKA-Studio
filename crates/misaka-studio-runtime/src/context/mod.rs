//! **The context manager: a conversation, fitted into a model's window.**
//!
//! A class registered at 512 tokens holds one question and one answer. Before this module the
//! Studio sent the whole conversation and, when it did not fit, dropped the oldest turns until it
//! did — so on a 512-token class the second question arrived with no trace of the first, and
//! "さっきの Q の座標を使って" had nothing to refer to. This module decides what the model sees,
//! in a fixed order of what matters:
//!
//! 1. **The system prompt and the question.** Never dropped; a question that does not fit on its
//!    own is refused with its numbers, not sent to be refused by the engine.
//! 2. **Pinned notes** — the conversation's standing facts ("the class is PALW-QWEN25-A16",
//!    "answer in Japanese"), which the person chose and which no amount of history should push out.
//!    At most half of what is left after the question.
//! 3. **Recent turns, whole.** A question and its answer together, newest first, as many as fit.
//! 4. **A memory of the older turns** in what remains: a summary from a local model when one is
//!    configured ([`summarizer`]), otherwise an extract — each question's first line and the
//!    sentence of its answer that states a result.
//!
//! The answer's share of the window is reserved before any of it ([`crate::backend::answer_room`]).
//! Everything is counted with the model's tokenizer when there is one ([`tokens`]).
//!
//! What was decided comes back as a [`ContextReport`] beside the reply, including the messages as
//! they were sent, because "the model forgot" and "the app did not send it" look the same from the
//! chat window and only one of them is fixed in Settings.

pub mod summarizer;
pub mod tokens;

use crate::backend::{ChatMessage, answer_room};
use serde::Serialize;
use tokens::{CounterSource, TokenCounter};

/// Below this, a memory block is a heading with nothing under it.
const MIN_MEMORY_TOKENS: u64 = 24;

/// What the manager is given.
pub struct PlanInputs<'a> {
    /// The conversation as the client sent it: system messages, turns, and the question last —
    /// or, when a cut-off reply is being continued, the question followed by the partial reply.
    pub messages: &'a [ChatMessage],
    /// The conversation's pinned notes, in the order the person pinned them.
    pub pinned: &'a [String],
    /// The model's window in tokens. `0` when unknown: nothing is fitted, only the pins are added.
    pub window: u64,
    /// The reply's requested length, from which its reserve is taken.
    pub max_tokens: u64,
    pub counter: &'a TokenCounter,
    /// Whether the engine continues a trailing assistant turn from inside it (llama.cpp's prefill).
    pub engine_continues_turns: bool,
    /// Counts are multiplied by this, in permille, before they are compared with the budget. 1000
    /// normally; more after an engine refused a prompt the counter under-counted, so the retry is
    /// planned in the engine's arithmetic rather than in the counter's.
    pub count_scale_permille: u64,
}

/// Where the memory of older turns came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    /// A local model summarised them.
    Summary,
    /// Extracted: each question's first line and the sentence of its answer that states a result.
    Extract,
}

/// A memory the caller supplies for the older turns (a model's summary).
#[derive(Clone, Debug)]
pub struct SuppliedMemory {
    pub text: String,
    pub source: MemorySource,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MemoryReport {
    pub source: MemorySource,
    /// History messages the memory stands in for.
    pub messages_covered: usize,
    pub tokens: u64,
    /// Why the memory is not the configured kind, when it is not (the summariser failed, say).
    pub note: Option<String>,
}

/// **What the model was sent, and why.**
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContextReport {
    /// `false` when the conversation fitted as it was: nothing was left out or rewritten.
    pub managed: bool,
    pub window: u64,
    /// Tokens held back for the answer.
    pub answer_reserve: u64,
    /// Tokens the prompt may take: the window less the reserve.
    pub prompt_budget: u64,
    /// Tokens the prompt as sent takes, template included, by `counter`.
    pub prompt_tokens: u64,
    pub counter: CounterSource,
    pub pinned_included: usize,
    /// Pinned notes that did not fit. Never silent: the UI names them.
    pub pinned_omitted: usize,
    /// History messages sent verbatim.
    pub recent_messages: usize,
    /// History messages not sent verbatim — covered by `memory` when it is present.
    pub older_messages: usize,
    pub memory: Option<MemoryReport>,
    /// A cut-off reply is being continued.
    pub continuation: bool,
    /// The question is larger than the budget by the counter's reckoning and was sent anyway,
    /// because the counter is an estimate and the engine is the authority.
    pub question_over_budget: bool,
    /// The messages exactly as sent, when the manager changed them.
    pub sent: Option<Vec<ChatMessage>>,
}

/// The plan before the memory is filled in: the older messages a memory would stand in for, and
/// how many tokens it may take. The caller decides where the memory comes from.
pub struct Draft {
    japanese: bool,
    base_system: String,
    pins: Vec<String>,
    pinned_omitted: usize,
    recent: Vec<ChatMessage>,
    question: Vec<ChatMessage>,
    pub older: Vec<ChatMessage>,
    pub memory_budget: u64,
    window: u64,
    reserve: u64,
    budget: u64,
    counter_source: CounterSource,
    fitted_as_sent: Option<Vec<ChatMessage>>,
    continuation: bool,
    question_over_budget: bool,
}

/// The plan: the messages to send and the report that says what they are.
#[derive(Clone, Debug)]
pub struct ContextPlan {
    pub messages: Vec<ChatMessage>,
    pub report: ContextReport,
}

/// Whether the conversation is Japanese enough to write the manager's own headings in Japanese.
fn looks_japanese(messages: &[ChatMessage], pinned: &[String]) -> bool {
    messages.iter().map(|m| m.content.as_str()).chain(pinned.iter().map(String::as_str)).any(|t| !t.is_ascii())
}

fn scaled(tokens: u64, scale_permille: u64) -> u64 {
    tokens.saturating_mul(scale_permille.max(1000)).div_ceil(1000)
}

/// The system message the manager writes: the client's own system prompt, then the pinned notes,
/// then the memory. `None` when there is nothing to say.
fn compose_system(base: &str, pins: &[String], memory: Option<&str>, japanese: bool) -> Option<ChatMessage> {
    let mut parts: Vec<String> = Vec::new();
    if !base.trim().is_empty() {
        parts.push(base.trim().to_string());
    }
    if !pins.is_empty() {
        let heading = if japanese { "固定メモ（常に守る前提）:" } else { "Pinned notes (standing facts):" };
        let lines: Vec<String> = pins.iter().map(|p| format!("- {}", p.trim())).collect();
        parts.push(format!("{heading}\n{}", lines.join("\n")));
    }
    if let Some(memory) = memory.filter(|m| !m.trim().is_empty()) {
        let heading =
            if japanese { "これまでの会話の要点（古い順）:" } else { "Earlier in this conversation (oldest first):" };
        parts.push(format!("{heading}\n{}", memory.trim()));
    }
    (!parts.is_empty()).then(|| ChatMessage::new("system", parts.join("\n\n")))
}

fn assemble(system: Option<ChatMessage>, recent: &[ChatMessage], question: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = system.into_iter().collect();
    out.extend(recent.iter().cloned());
    out.extend(question.iter().cloned());
    out
}

/// History as turns: each starts at a user message and carries the assistant replies after it.
fn turns(history: &[ChatMessage]) -> Vec<Vec<ChatMessage>> {
    let mut out: Vec<Vec<ChatMessage>> = Vec::new();
    for m in history {
        if m.role == "user" || out.is_empty() {
            out.push(vec![m.clone()]);
        } else if let Some(last) = out.last_mut() {
            last.push(m.clone());
        }
    }
    out
}

/// **Decide what fits.** Pure: the memory, when one is needed, is filled in by [`Draft::finish`].
///
/// `Err` is a sentence for the person: a question that on its own is larger than the window's
/// budget, by the model's own tokenizer. With an estimate the question is sent anyway — the
/// estimate over-counts prose, and the engine's refusal is the authority.
pub fn plan(inputs: &PlanInputs<'_>) -> Result<Draft, String> {
    let japanese = looks_japanese(inputs.messages, inputs.pinned);
    let pins: Vec<String> = inputs.pinned.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    let base_system = inputs
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.content.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let rest: Vec<ChatMessage> = inputs.messages.iter().filter(|m| m.role != "system").cloned().collect();
    let last_user = rest.iter().rposition(|m| m.role == "user");
    let (history, question) = match last_user {
        Some(i) => (rest[..i].to_vec(), rest[i..].to_vec()),
        None => (Vec::new(), rest.clone()),
    };
    let continuation = question.len() > 1 && question.last().is_some_and(|m| m.role == "assistant");
    // What must fit whole. A partial reply an engine cannot continue from inside is cut to its end
    // later (`continuation_as_instruction`), so only the question itself is required here —
    // counting the whole partial reply refused, as "the question alone is 426 tokens", a
    // continuation whose question was 140.
    let question_core: Vec<ChatMessage> =
        if continuation && !inputs.engine_continues_turns { question.iter().take(1).cloned().collect() } else { question.clone() };
    let scale = inputs.count_scale_permille;
    let count = |messages: &[ChatMessage]| scaled(inputs.counter.messages(messages), scale);

    let (reserve, budget) = match inputs.window {
        0 => (0, u64::MAX),
        window => {
            let reserve = answer_room(inputs.max_tokens, window);
            (reserve, window.saturating_sub(reserve))
        }
    };

    let mut draft = Draft {
        japanese,
        base_system: base_system.clone(),
        pins: Vec::new(),
        pinned_omitted: 0,
        recent: Vec::new(),
        question: question.clone(),
        older: Vec::new(),
        memory_budget: 0,
        window: inputs.window,
        reserve,
        budget,
        counter_source: inputs.counter.source().clone(),
        fitted_as_sent: None,
        continuation,
        question_over_budget: false,
    };

    // Everything, as sent: when it fits there is nothing to manage.
    let everything = assemble(compose_system(&base_system, &pins, None, japanese), &history, &question);
    if count(&everything) <= budget {
        draft.pins = pins;
        draft.recent = history;
        draft.fitted_as_sent = Some(everything);
        return Ok(draft);
    }

    // 1. The system prompt and the question.
    let required = assemble(compose_system(&base_system, &[], None, japanese), &[], &question_core);
    let required_tokens = count(&required);
    if required_tokens > budget {
        if inputs.counter.is_exact() && !(continuation && inputs.engine_continues_turns) {
            return Err(if japanese {
                format!(
                    "質問だけで {required_tokens} トークンあり、このモデルの窓 {window} トークンのうち質問に使える {budget} トークン（残り {reserve} は回答用）を超えています。質問を短くするか、いくつかに分けてください。",
                    window = inputs.window
                )
            } else {
                format!(
                    "The question alone is {required_tokens} tokens; this model's {window}-token window leaves {budget} for the prompt ({reserve} are held for the answer). Shorten the question or split it.",
                    window = inputs.window
                )
            });
        }
        draft.question_over_budget = true;
    }

    // 2. Pinned notes, up to half of what the question leaves.
    let pin_cap = budget.saturating_sub(required_tokens) / 2;
    let mut kept_pins: Vec<String> = Vec::new();
    for pin in &pins {
        let mut trial = kept_pins.clone();
        trial.push(pin.clone());
        let with_pins = assemble(compose_system(&base_system, &trial, None, japanese), &[], &question_core);
        let tokens = count(&with_pins);
        if tokens <= budget && tokens.saturating_sub(required_tokens) <= pin_cap {
            kept_pins = trial;
        } else {
            draft.pinned_omitted += 1;
        }
    }
    draft.pins = kept_pins;

    // A continuation keeps the question and the partial reply whole and spends nothing on history:
    // the reply's own text is the context it needs.
    if continuation {
        return Ok(draft);
    }

    // 3. Recent turns, whole, newest first — leaving a third of what is left for a memory while
    //    older turns remain. Without that room the newest turn took everything: on a 512-token
    //    class the third question kept the second answer verbatim and lost the first one, the one
    //    it referred to ("さっきの Q の座標"), because a memory was left 12 tokens.
    let history_turns = turns(&history);
    let after_pins =
        budget.saturating_sub(count(&assemble(compose_system(&base_system, &draft.pins, None, japanese), &[], &question)));
    let heading = count(&assemble(compose_system(&base_system, &draft.pins, Some("-"), japanese), &[], &question))
        .saturating_sub(budget.saturating_sub(after_pins));
    let memory_reserve = (MIN_MEMORY_TOKENS + heading).max(after_pins / 3);
    let mut kept_from = history_turns.len();
    for i in (0..history_turns.len()).rev() {
        let candidate: Vec<ChatMessage> = history_turns[i..].iter().flatten().cloned().collect();
        // Room for at least the shortest memory of the turn just before the ones kept whole: a
        // verbatim turn is not worth a memory that cannot say anything. When both do not fit, the
        // turn joins the memory instead — covering every turn briefly beats one turn in full.
        let memory_room = if i > 0 {
            let shortest =
                counter_text(inputs.counter, &memory_entry(&history_turns[i - 1], 0, SHORTEST_ANSWER_CHARS, japanese), scale);
            memory_reserve.max(heading + shortest)
        } else {
            0
        };
        let trial = assemble(compose_system(&base_system, &draft.pins, None, japanese), &candidate, &question);
        if count(&trial) + memory_room <= budget {
            kept_from = i;
        } else {
            break;
        }
    }
    draft.recent = history_turns[kept_from..].iter().flatten().cloned().collect();
    draft.older = history_turns[..kept_from].iter().flatten().cloned().collect();

    // 4. What is left is the memory's.
    if !draft.older.is_empty() {
        let heading_only = assemble(compose_system(&base_system, &draft.pins, Some("-"), japanese), &draft.recent, &question);
        // In the counter's own units: the memory is measured with `counter.text`, unscaled, so the
        // room left in scaled tokens is divided back out.
        let room = budget.saturating_sub(count(&heading_only)).saturating_sub(1);
        draft.memory_budget = room.saturating_mul(1000) / scale.max(1000);
    }
    Ok(draft)
}

impl Draft {
    pub fn japanese(&self) -> bool {
        self.japanese
    }

    /// Whether a memory would be used: older turns exist and there is room to say something.
    pub fn wants_memory(&self) -> bool {
        !self.older.is_empty() && self.memory_budget >= MIN_MEMORY_TOKENS
    }

    /// Fill in the memory and produce the plan. `supplied` is a summary from a model, used when it
    /// fits (trimmed line by line when it does not); otherwise the older turns are extracted.
    pub fn finish(self, supplied: Option<SuppliedMemory>, counter: &TokenCounter, note: Option<String>) -> ContextPlan {
        let Draft {
            japanese,
            base_system,
            pins,
            pinned_omitted,
            recent,
            question,
            older,
            memory_budget,
            window,
            reserve,
            budget,
            counter_source,
            fitted_as_sent,
            continuation,
            question_over_budget,
        } = self;

        if let Some(messages) = fitted_as_sent {
            let prompt_tokens = counter.messages(&messages);
            let report = ContextReport {
                managed: false,
                window,
                answer_reserve: reserve,
                prompt_budget: budget,
                prompt_tokens,
                counter: counter_source,
                pinned_included: pins.len(),
                pinned_omitted: 0,
                recent_messages: recent.len(),
                older_messages: 0,
                memory: None,
                continuation,
                question_over_budget: false,
                sent: None,
            };
            return ContextPlan { messages, report };
        }

        let mut memory_report = None;
        let mut memory_text: Option<String> = None;
        if !older.is_empty() && memory_budget >= MIN_MEMORY_TOKENS {
            let from_supplied = supplied.as_ref().and_then(|m| fit_lines(&m.text, memory_budget, counter).map(|t| (t, m.source)));
            let (text, source, note) = match from_supplied {
                Some((text, source)) => (text, source, note),
                None => {
                    let note = note.or_else(|| {
                        supplied
                            .as_ref()
                            .map(|_| "the summary did not fit the room left, so the turns were extracted instead".to_string())
                    });
                    (extract_memory(&older, memory_budget, counter, japanese), MemorySource::Extract, note)
                }
            };
            if !text.trim().is_empty() {
                memory_report = Some(MemoryReport { source, messages_covered: older.len(), tokens: counter.text(&text), note });
                memory_text = Some(text);
            }
        }

        let messages = assemble(compose_system(&base_system, &pins, memory_text.as_deref(), japanese), &recent, &question);
        let prompt_tokens = counter.messages(&messages);
        let report = ContextReport {
            managed: true,
            window,
            answer_reserve: reserve,
            prompt_budget: budget,
            prompt_tokens,
            counter: counter_source,
            pinned_included: pins.len(),
            pinned_omitted,
            recent_messages: recent.len(),
            older_messages: older.len(),
            memory: memory_report,
            continuation,
            question_over_budget,
            sent: Some(messages.clone()),
        };
        ContextPlan { messages, report }
    }
}

/// The longest prefix of `text`'s lines that fits `budget` tokens, or `None` when not even one does.
fn fit_lines(text: &str, budget: u64, counter: &TokenCounter) -> Option<String> {
    let mut out = String::new();
    for line in text.trim().lines() {
        let candidate = if out.is_empty() { line.to_string() } else { format!("{out}\n{line}") };
        if counter.text(&candidate) > budget {
            break;
        }
        out = candidate;
    }
    (!out.trim().is_empty()).then_some(out)
}

/// Collapse whitespace and cut at `max_chars`, marking the cut.
fn squash(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut cut: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The sentences of a text, split on Japanese and Western full stops and on line breaks.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        current.push(c);
        if matches!(c, '。' | '！' | '？' | '\n') || ((c == '.' || c == '!' || c == '?') && current.len() > 1) {
            let s = current.trim().to_string();
            if !s.is_empty() {
                out.push(s);
            }
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

/// The sentence of an answer most worth remembering: the last one that states a result, or the
/// first one when none does.
fn key_sentence(answer: &str) -> String {
    const MARKERS: &[&str] = &[
        "よって",
        "したがって",
        "ゆえに",
        "結論",
        "答え",
        "結果",
        "求める",
        "となる",
        "である",
        "∴",
        "=",
        "therefore",
        "Therefore",
        "answer",
        "Answer",
        "result",
        "so the",
        "thus",
        "Thus",
    ];
    let all = sentences(answer);
    all.iter().rev().find(|s| MARKERS.iter().any(|m| s.contains(m))).or_else(|| all.first()).cloned().unwrap_or_default()
}

/// The shortest answer an extracted memory entry is cut to.
const SHORTEST_ANSWER_CHARS: usize = 40;

fn counter_text(counter: &TokenCounter, text: &str, scale_permille: u64) -> u64 {
    scaled(counter.text(text), scale_permille)
}

/// One turn as a memory line: its question's first line and its answer's key sentence, each cut to
/// its cap. A question cap of zero leaves the question out — the result is what a later question
/// refers to, and it is the last thing to give up.
fn memory_entry(turn: &[ChatMessage], q_cap: usize, a_cap: usize, japanese: bool) -> String {
    let (q_label, a_label) = if japanese { ("質問", "回答") } else { ("Q", "A") };
    let question = turn.iter().find(|m| m.role == "user").map(|m| question_gist(&m.content)).unwrap_or_default();
    let answer: String = turn.iter().filter(|m| m.role == "assistant").map(|m| m.content.as_str()).collect::<Vec<_>>().join("\n");
    let key = key_sentence(&answer);
    match (q_cap, key.is_empty()) {
        (0, false) => format!("- {}", squash(&key, a_cap)),
        (0, true) => format!("- {}", squash(&question, a_cap)),
        (_, true) => format!("- {q_label}: {}", squash(&question, q_cap)),
        (_, false) => format!("- {q_label}: {}\n  {a_label}: {}", squash(&question, q_cap), squash(&key, a_cap)),
    }
}

/// The sentence of a question worth remembering: one that carries a formula, else the last one —
/// a problem states its setting first and its ask last, and the first line alone was measured
/// losing the very formula the next question referred to.
fn question_gist(question: &str) -> String {
    let all = sentences(question);
    all.iter().find(|s| s.contains('=')).or_else(|| all.last()).cloned().unwrap_or_default()
}

/// **A memory of older turns without a model.** Newest turns get room first; each is its
/// question's first line and its answer's key sentence, cut to fit, listed oldest first. When the
/// room is tight the questions go first and the results stay.
pub fn extract_memory(older: &[ChatMessage], budget: u64, counter: &TokenCounter, japanese: bool) -> String {
    let history_turns = turns(older);
    // Each cap level, newest turns first while they fit. The first level that covers every turn
    // wins; failing that, the one that covers the most — a memory of three results beats a fuller
    // memory of one.
    let mut best: Vec<String> = Vec::new();
    for (q_cap, a_cap) in [(80, 140), (48, 90), (28, 56), (0, 64), (0, SHORTEST_ANSWER_CHARS)] {
        let mut kept: Vec<String> = Vec::new();
        for turn in history_turns.iter().rev() {
            let line = memory_entry(turn, q_cap, a_cap, japanese);
            let mut trial = kept.clone();
            trial.insert(0, line);
            if counter.text(&trial.join("\n")) <= budget {
                kept = trial;
            } else {
                break;
            }
        }
        if kept.len() == history_turns.len() {
            return kept.join("\n");
        }
        if kept.len() > best.len() {
            best = kept;
        }
    }
    best.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage::new(role, content)
    }

    fn inputs<'a>(messages: &'a [ChatMessage], pinned: &'a [String], window: u64, counter: &'a TokenCounter) -> PlanInputs<'a> {
        PlanInputs { messages, pinned, window, max_tokens: 2048, counter, engine_continues_turns: false, count_scale_permille: 1000 }
    }

    const Q1: &str = "原点 O を中心とする半径 1 の円に円外の点 P(x0, y0) から 2 本の接線を引く。2 つの接点の中点 Q の座標を求めよ。";
    const A1: &str = "接点を A, B とすると、直線 AB は極線 x0 x + y0 y = 1 である。Q は OP と AB の交点なので、よって Q = (x0/(x0²+y0²), y0/(x0²+y0²)) となる。";
    const Q2: &str = "では OP・OQ=1 を示してください。";
    const A2: &str = "OP = √(x0²+y0²)、OQ = 1/√(x0²+y0²) なので、したがって OP・OQ = 1 である。";
    const Q3: &str = "P が直線 x+y=2 上を動くとき、Q の軌跡は？";

    fn conversation() -> Vec<ChatMessage> {
        vec![
            msg("system", "日本語で答えてください。"),
            msg("user", Q1),
            msg("assistant", A1),
            msg("user", Q2),
            msg("assistant", A2),
            msg("user", Q3),
        ]
    }

    /// A conversation that fits is sent as it is, with its pins, and says it was not managed.
    #[test]
    fn a_conversation_that_fits_is_sent_whole() {
        let counter = TokenCounter::estimate();
        let messages = conversation();
        let pins = vec!["P は円の外".to_string()];
        let draft = plan(&inputs(&messages, &pins, 32_768, &counter)).unwrap();
        let plan = draft.finish(None, &counter, None);
        assert!(!plan.report.managed);
        assert_eq!(plan.messages.len(), messages.len());
        assert!(plan.messages[0].content.contains("P は円の外"), "the pin rides in the system message");
        assert!(plan.report.sent.is_none());
    }

    /// The 512 case: the question and the system prompt survive, the older turns become a memory
    /// that still carries the result the question refers to, and the whole prompt fits.
    #[test]
    fn at_512_the_older_turns_become_a_memory_that_keeps_the_result() {
        let counter = TokenCounter::estimate();
        let messages = conversation();
        let draft = plan(&inputs(&messages, &[], 512, &counter)).unwrap();
        assert!(draft.wants_memory(), "older turns and room for them");
        let plan = draft.finish(None, &counter, None);
        let r = &plan.report;
        assert!(r.managed);
        assert_eq!((r.window, r.answer_reserve, r.prompt_budget), (512, 256, 256));
        assert!(r.prompt_tokens <= r.prompt_budget, "{r:?}");
        assert_eq!(plan.messages.last().unwrap().content, Q3, "the question is last and whole");
        let system = &plan.messages[0].content;
        assert!(system.starts_with("日本語で答えてください。"), "the client's system prompt leads");
        assert!(system.contains("これまでの会話の要点"), "{system}");
        let memory = r.memory.as_ref().expect("a memory");
        assert_eq!(memory.source, MemorySource::Extract);
        assert!(memory.messages_covered + r.recent_messages == 4, "{r:?}");
        assert!(system.contains("OP・OQ = 1") || system.contains("Q = (x0/"), "the key results survive: {system}");
        assert_eq!(r.sent.as_ref(), Some(&plan.messages));
    }

    /// Pins are kept ahead of history, and the ones that cannot fit are counted, not dropped quietly.
    #[test]
    fn pins_come_before_history_and_the_ones_that_do_not_fit_are_counted() {
        let counter = TokenCounter::estimate();
        let messages = conversation();
        let pins = vec!["答えは分数で書く".to_string(), "長い前提".repeat(120)];
        let plan = plan(&inputs(&messages, &pins, 512, &counter)).unwrap().finish(None, &counter, None);
        assert_eq!(plan.report.pinned_included, 1);
        assert_eq!(plan.report.pinned_omitted, 1);
        assert!(plan.messages[0].content.contains("答えは分数で書く"));
        assert!(plan.report.prompt_tokens <= plan.report.prompt_budget);
    }

    /// A model's summary is used when it fits, trimmed line by line when it does not.
    #[test]
    fn a_supplied_summary_is_used_and_trimmed_to_the_room() {
        let counter = TokenCounter::estimate();
        let messages = conversation();
        let draft = plan(&inputs(&messages, &[], 512, &counter)).unwrap();
        let budget = draft.memory_budget;
        let long = (0..40).map(|i| format!("- 事実 {i}: Q = (x0/(x0²+y0²), y0/(x0²+y0²))")).collect::<Vec<_>>().join("\n");
        let plan = draft.finish(Some(SuppliedMemory { text: long, source: MemorySource::Summary }), &counter, None);
        let memory = plan.report.memory.as_ref().unwrap();
        assert_eq!(memory.source, MemorySource::Summary);
        assert!(memory.tokens <= budget);
        assert!(plan.report.prompt_tokens <= plan.report.prompt_budget);
    }

    /// With the model's tokenizer the counter is exact, so a question too big for the window is
    /// refused here with its numbers; with an estimate it is sent and the engine decides.
    #[test]
    fn a_question_larger_than_the_window_is_refused_only_on_an_exact_count() {
        let counter = TokenCounter::estimate();
        let huge = vec![msg("user", &"長い質問".repeat(200))];
        let draft = plan(&inputs(&huge, &[], 512, &counter)).expect("an estimate never refuses");
        assert!(draft.finish(None, &counter, None).report.question_over_budget);
    }

    /// With the model's own tokenizer, a continuation whose partial reply is longer than the budget
    /// is not refused as "the question is too long": the reply is cut to its end afterwards, and
    /// only the question has to fit.
    #[test]
    fn a_long_partial_reply_does_not_count_against_the_question() {
        let counter = TokenCounter::estimate();
        let messages = vec![msg("system", "日本語で答えてください。"), msg("user", Q1), msg("assistant", &A1.repeat(12))];
        let draft = plan(&inputs(&messages, &[], 512, &counter)).expect("planned");
        let plan = draft.finish(None, &counter, None);
        assert!(plan.report.continuation);
        assert!(!plan.report.question_over_budget, "the question fits; the reply is the part that gets cut");
    }

    /// A continuation keeps the question and the partial reply together and adds no history.
    #[test]
    fn a_continuation_keeps_its_question_and_reply_and_skips_history() {
        let counter = TokenCounter::estimate();
        let mut messages = conversation();
        messages.push(msg("assistant", "直線 x+y=2 上の点を P(t, 2-t) とおくと、"));
        let plan = plan(&inputs(&messages, &[], 512, &counter)).unwrap().finish(None, &counter, None);
        assert!(plan.report.continuation);
        let roles: Vec<_> = plan.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["system", "user", "assistant"]);
        assert_eq!(plan.report.older_messages, 0);
    }

    /// A refusal that showed the counter ran low re-plans in the engine's arithmetic: the same
    /// conversation under a scale keeps less.
    #[test]
    fn a_count_scale_leaves_more_margin() {
        let counter = TokenCounter::estimate();
        let messages = conversation();
        let normal = plan(&inputs(&messages, &[], 512, &counter)).unwrap().finish(None, &counter, None);
        let mut scaled_inputs = inputs(&messages, &[], 512, &counter);
        scaled_inputs.count_scale_permille = 1500;
        let tight = plan(&scaled_inputs).unwrap().finish(None, &counter, None);
        assert!(tight.report.prompt_tokens * 1500 / 1000 <= tight.report.prompt_budget + 1, "{:?}", tight.report);
        assert!(tight.report.prompt_tokens <= normal.report.prompt_tokens);
    }

    /// Measured on a real 512-token run: the first line of the question was kept and the formula
    /// the next turn needed, later in the same question, was cut off.
    #[test]
    fn the_question_gist_keeps_its_formula_or_its_ask() {
        let with_formula = "原点 O を中心とする半径 1 の円に、円外の点 P(x0, y0) から 2 本の接線を引く。2 つの接点の中点 Q の座標は Q = (x0/(x0²+y0²), y0/(x0²+y0²)) になることを説明してください。";
        assert!(question_gist(with_formula).contains("Q = (x0/"), "{}", question_gist(with_formula));
        let ask_last = "円外の点 P から 2 本の接線を引く。接点の中点 Q の軌跡を求めよ。";
        assert_eq!(question_gist(ask_last), "接点の中点 Q の軌跡を求めよ。");
    }

    #[test]
    fn the_key_sentence_is_the_one_that_states_a_result() {
        assert!(key_sentence(A1).contains("よって Q ="));
        assert_eq!(key_sentence("Hello there. Nothing to conclude"), "Hello there.");
        assert_eq!(key_sentence(""), "");
    }

    /// A memory covers as many turns as it can before it keeps any one of them in full.
    #[test]
    fn an_extract_prefers_covering_every_turn() {
        let counter = TokenCounter::estimate();
        let older = &conversation()[1..5];
        let text = extract_memory(older, 120, &counter, true);
        assert_eq!(text.lines().filter(|l| l.starts_with("- ")).count(), 2, "both turns: {text}");
        assert!(text.contains("Q = (x0/"), "the first result: {text}");
        assert!(text.contains("OP・OQ = 1"), "the second result: {text}");
    }

    #[test]
    fn an_extract_fits_its_budget_or_is_empty() {
        let counter = TokenCounter::estimate();
        let older = &conversation()[1..5];
        for budget in [8u64, 24, 60, 200] {
            let text = extract_memory(older, budget, &counter, true);
            assert!(counter.text(&text) <= budget, "budget {budget}: {text}");
        }
    }
}
