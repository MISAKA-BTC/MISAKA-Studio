//! **A reply that repeats itself, caught while it is still streaming.**
//!
//! The chat backends this Studio talks to range from a full sampler (llama.cpp) to a fixed
//! `greedy-argmax-lowest-id` decoder with no anti-repetition mechanism at all (`misaka-palw-serve`,
//! confirmed at `/health`: `"sampler": "greedy-argmax-lowest-id"`) and no `stop` support either
//! (`misaka-palw-gateway`'s free-prompt surface refuses `stop` by name — ADR-0096 Decision 4 —
//! because that lane runs every job to its declared, priced budget on purpose). For that backend a
//! model that starts looping has no way to escape on its own, and nothing upstream will cut it off:
//! it runs to `max_tokens`, which on a hard prompt can be tens of seconds of degenerate output.
//!
//! [`ui/src/lib/history.ts`] already detects this pattern — after the fact, to decide whether a
//! finished reply is fit to feed back as context (a looping reply sent back as history reproduces
//! the same loop on the next turn, measured 2026-09-17). This module is the same two tests, ported
//! to Rust, run WHILE the reply streams so the app can act during the reply that is looping rather
//! than only the one after it: `crate::state` stops reading the backend's stream (which cancels the
//! underlying HTTP request — the engine stops burning cycles on a reply nobody wants) and reports
//! `finish_reason: "repetition"` instead of running out the clock on `max_tokens`.
//!
//! Deterministic and backend-agnostic on purpose: it is a pure function of the text already
//! generated, so it changes nothing about what any backend computes or verifies — it only decides,
//! from the outside, when this Studio has seen enough of an answer to stop asking for more of it.
//! Never applied to the free-prompt/mining lane (`api/prompt_mining.rs` calls the gateway directly
//! and never goes through here) — a committed job's execution is priced and verified as a whole, and
//! this module never touches that path.

/// Whitespace-collapsed, trimmed — the same normalisation `history.ts`'s `compact` applies before
/// either test, so punctuation and line breaks around a repeated phrase do not hide it.
fn compact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = false;
    for ch in text.trim().chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out
}

/// The length of text compared when looking for a repeated tail.
const TAIL: usize = 120;

/// Whether the last stretch of `text` already appeared earlier in it — a long-period loop, such as
/// a repeated paragraph.
fn has_repeated_tail(text: &str) -> bool {
    let flat = compact(text);
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() < TAIL * 2 {
        return false;
    }
    let tail: String = chars[chars.len() - TAIL..].iter().collect();
    let head: String = chars[..chars.len() - TAIL].iter().collect();
    head.contains(&tail)
}

/// Shortest and longest loop period looked for directly at the end of the text.
const SHORT_PERIOD_MIN: usize = 8;
const SHORT_PERIOD_MAX: usize = 300;

/// Whether `text` ends in the same block three times running — a loop too short for the tail test
/// (a repeated sentence or clause, rather than a whole repeated paragraph).
fn short_loop_period(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let max = SHORT_PERIOD_MAX.min(chars.len() / 3);
    for period in SHORT_PERIOD_MIN..=max {
        let last = &chars[chars.len() - period..];
        // A run of symbols or digits is formatting (an underline, a zero-filled array), not a
        // model saying the same thing again — the same guard `history.ts` applies.
        if !last.iter().any(|c| c.is_alphabetic()) {
            continue;
        }
        let mid = &chars[chars.len() - 2 * period..chars.len() - period];
        let first = &chars[chars.len() - 3 * period..chars.len() - 2 * period];
        if mid == last && first == last {
            return Some(period);
        }
    }
    None
}

/// Whether a reply loops, by either test — the live counterpart of `history.ts`'s `loops`.
///
/// Cheap enough to call on every delta: both tests are linear scans over the tail of a string that
/// stays well under a few thousand characters for any answer this catches early.
pub fn loops(text: &str) -> bool {
    let trimmed = text.trim_end();
    has_repeated_tail(trimmed) || short_loop_period(trimmed).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_paragraph_is_a_loop() {
        let sentence = "同じ入力なら誰が実行しても同じ出力になることを維持したまま繰り返し抑制を実現する方法を考える。";
        let text = sentence.repeat(3);
        assert!(loops(&text));
        assert!(!loops(sentence), "one copy is not a loop");
    }

    #[test]
    fn a_short_repeated_clause_is_caught_before_the_tail_test_would_fire() {
        let text = "答えは θ = π/2 です。θ = π/2 です。θ = π/2 です。";
        assert!(loops(text));
        assert!(text.chars().count() < TAIL * 2, "short enough that has_repeated_tail alone would miss it");
    }

    #[test]
    fn ordinary_prose_does_not_loop() {
        let text = "放物線 Q 上の 2 点 B, C における接線が点 A で交わるとき、θ = ∠CAB とおく。\
                    (1) では b, c を p, a を用いて表す。(2) では θ = π/2 のときの p を a で表す。";
        assert!(!loops(text));
    }

    #[test]
    fn a_repeated_rule_or_a_zero_filled_run_is_formatting_not_a_loop() {
        assert!(!loops(&"-".repeat(200)), "an underline is not prose looping");
        assert!(!loops(&"0".repeat(200)), "a zero-filled run is not prose looping");
    }

    #[test]
    fn a_short_period_needs_three_full_repeats_not_two() {
        let period = "abcdefgh"; // 8 chars, at SHORT_PERIOD_MIN
        let twice = period.repeat(2);
        assert!(!loops(&twice), "two repeats is not yet the three this test requires");
        let thrice = period.repeat(3);
        assert!(loops(&thrice));
    }

    #[test]
    fn text_shorter_than_one_short_period_times_three_never_loops() {
        assert!(!loops("ab"));
        assert!(!loops(""));
    }
}
