//! Counting tokens the way the model will.
//!
//! On a 512-token class the count is the budget, and the estimate this replaces was wrong in the
//! direction that loses requests. Measured against Qwen2.5's own tokenizer (llama.cpp `/tokenize`,
//! 2026-09-17): a LaTeX line of 178 characters is 116 tokens and was estimated at 45; a table of
//! squares was 104 against 34; a 128-hex id 113 against 32. "A quarter of the ASCII" is right for
//! English prose and nothing else — Qwen's pre-tokenizer splits every digit into its own token and
//! keeps punctuation apart, and a formula is mostly both.
//!
//! So: the class's `tokenizer.json` when one is at hand ([`TokenCounter::from_tokenizer_file`]),
//! counting exactly; otherwise [`estimate_tokens`], an upper bound built from the same measurements.

use crate::backend::ChatMessage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Tokens the Qwen chat template adds around each message: `<|im_start|>`, the role, `\n`,
/// `<|im_end|>`, `\n`. Measured: two messages template to their contents + 13, four to + 23.
pub const TEMPLATE_TOKENS_PER_MESSAGE: u64 = 5;
/// `<|im_start|>assistant\n`, the generation prompt after the last message.
pub const TEMPLATE_GENERATION_PROMPT: u64 = 3;
/// What a template may insert when the conversation has no system message of its own. Qwen2.5's
/// template writes "You are Qwen, created by Alibaba Cloud. You are a helpful assistant." — 20
/// tokens with its markers, measured; the free-prompt gateway writes nothing for a plain chat.
/// Counted anyway, because the manager cannot see which engine's template will run.
pub const TEMPLATE_DEFAULT_SYSTEM: u64 = 24;

/// Where a count came from, reported beside it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CounterSource {
    /// The model's own tokenizer: the count is the count.
    Tokenizer { path: PathBuf },
    /// No tokenizer at hand: an upper bound that over-counts prose to be safe on formulas.
    Estimate,
}

#[derive(Clone)]
pub struct TokenCounter {
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,
    source: CounterSource,
}

impl std::fmt::Debug for TokenCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenCounter").field("source", &self.source).finish()
    }
}

impl TokenCounter {
    pub fn estimate() -> Self {
        TokenCounter { tokenizer: None, source: CounterSource::Estimate }
    }

    /// Load a Hugging Face `tokenizer.json`. Blocking — 7 MB of JSON for Qwen — so callers on an
    /// async runtime use `spawn_blocking`.
    pub fn from_tokenizer_file(path: &Path) -> Result<Self, String> {
        let tokenizer = tokenizers::Tokenizer::from_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(TokenCounter { tokenizer: Some(Arc::new(tokenizer)), source: CounterSource::Tokenizer { path: path.to_path_buf() } })
    }

    pub fn source(&self) -> &CounterSource {
        &self.source
    }

    pub fn is_exact(&self) -> bool {
        self.tokenizer.is_some()
    }

    /// Tokens in `text`, with no special tokens added.
    pub fn text(&self, text: &str) -> u64 {
        if text.is_empty() {
            return 0;
        }
        match &self.tokenizer {
            // A tokenizer that fails on some input has not told us the count; the estimate is the
            // safe answer, not zero.
            Some(tokenizer) => tokenizer.encode_fast(text, false).map(|e| e.len() as u64).unwrap_or_else(|_| estimate_tokens(text)),
            None => estimate_tokens(text),
        }
    }

    /// Tokens the whole templated prompt takes: every message's content, the template's markers
    /// around each, the generation prompt, and a default system line when there is no system
    /// message to displace it.
    pub fn messages(&self, messages: &[ChatMessage]) -> u64 {
        let contents: u64 = messages.iter().map(|m| self.text(&m.content)).sum();
        let markers = TEMPLATE_TOKENS_PER_MESSAGE * messages.len() as u64 + TEMPLATE_GENERATION_PROMPT;
        let default_system = if messages.iter().any(|m| m.role == "system") { 0 } else { TEMPLATE_DEFAULT_SYSTEM };
        contents + markers + default_system
    }
}

/// **An upper bound on Qwen2.5's token count, without the tokenizer.**
///
/// Built on the pre-tokenizer's rules and checked against the tokenizer on 16 samples (see the
/// tests, whose `actual` column is llama.cpp's `/tokenize` over Qwen2.5-1.5B-Instruct):
///
/// * an ASCII digit is a token — Qwen splits numbers digit by digit;
/// * a run of ASCII letters is at most a token per three letters;
/// * ASCII punctuation is a token a character;
/// * one space before a word joins the word; any other run of blanks, and any run of newlines, is
///   a token;
/// * kana is a token a character, CJK ideographs and Hangul one and a half, other non-ASCII
///   letters one, symbols one and a half, anything outside the BMP three.
///
/// It over-counts prose by 1.5–2× — the price of not under-counting formulas. The one measured
/// under-count is a string of rare kanji (33 actual, 31 estimated), which a byte-level BPE spends
/// two or three tokens on; that is what the tokenizer file is for.
pub fn estimate_tokens(text: &str) -> u64 {
    let chars: Vec<char> = text.chars().collect();
    let mut halves: u64 = 0; // counted in half-tokens so 1.5 stays an integer
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii() {
            if c.is_ascii_digit() {
                halves += 2;
                i += 1;
            } else if c.is_ascii_alphabetic() {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                halves += 2 * (i - start).div_ceil(3) as u64;
            } else if c == ' ' || c == '\t' {
                let start = i;
                while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
                    i += 1;
                }
                let joins_next = i - start == 1 && chars.get(i).is_some_and(|n| n.is_alphanumeric());
                if !joins_next {
                    halves += 2;
                }
            } else if c == '\n' || c == '\r' {
                while i < chars.len() && (chars[i] == '\n' || chars[i] == '\r') {
                    i += 1;
                }
                halves += 2;
            } else {
                halves += 2;
                i += 1;
            }
        } else {
            let o = c as u32;
            halves += if (0x3040..=0x30ff).contains(&o) || (0x31f0..=0x31ff).contains(&o) || (0xff66..=0xff9f).contains(&o) {
                2
            } else if (0x4e00..=0x9fff).contains(&o)
                || (0x3400..=0x4dbf).contains(&o)
                || (0xf900..=0xfaff).contains(&o)
                || (0xac00..=0xd7af).contains(&o)
            {
                3
            } else if o > 0xffff {
                6
            } else if c.is_alphabetic() {
                2
            } else {
                3
            };
            i += 1;
        }
    }
    halves.div_ceil(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(text, actual)` where `actual` is Qwen2.5-1.5B-Instruct's tokenizer, through llama.cpp's
    /// `/tokenize` with no special tokens (2026-09-17). The estimate must not fall below any of
    /// them but the rare-kanji row, which is pinned as the known exception.
    const MEASURED: &[(&str, u64)] = &[
        (
            "原点 O(0, 0) を中心とする半径 1 の円に, 円外の点 P(x0, y0) から 2 本の接線を引く。\n(1) 2 つの接点の中点を Q とするとき, 点 Q の座標 (x1, y1) を, 点 P の座標 (x0, y0) を用いて表せ。また, OP･OQ=1 であることを示せ。\n(2) 点 P が直線 x+y=2 上を動くとき, 点 Q の軌跡を求めよ。",
            140,
        ),
        (
            "$l_1: y - y_0 = \\frac{y_0 - 0}{x_0 - 1}(x - 1)$\n$l_2: x_1 = \\frac{x_0 + 1}{y_0 - 0}(y - 1)$ \\[ x = \\frac{x_0 + y_0}{2}, \\quad y = \\frac{y_0 - 2\\sqrt{x_0^2 + y_0^2 - 1}}{|x_0|} \\]",
            116,
        ),
        ("1² = 1, 1³ = 1\n12² = 144, 12³ = 1728\n45² = 2025, 45³ = 97533\n46³ = 1.048576e+06 0x1a7457f100d9fb0f3406d882b4b5bcd7", 104),
        (
            "The answer to the question above was cut off. Do not repeat what was already written and add no preamble: output only the continuation.",
            27,
        ),
        ("fn main() {\n    let v: Vec<u64> = (1..=10).map(|x| x * x).collect();\n    println!(\"{:?}\", v);\n}\n", 39),
        (
            "日本語で答えてください。これらの接線の方程式をそれぞれ変形すると、接点の座標を求めるためには、直線の方程式と交差する点を解く必要があります。",
            43,
        ),
        ("テスト🙂✅ → ⟦SEAM⟧ ・「」『』【】", 16),
        (
            "4277d84f7d91528cc04aa366d51ee1c2e4f7902c4f6b16a213dead1c7e227977db732f18ed6183db3d944d44726ebd3feff7b15c48f9dba11cd526684f35f1b7",
            113,
        ),
        ("a    b\n\n\n\n    c\t\td", 8),
        ("我们需要找到圆上的点，然后计算从该点到圆的切线与中点的坐标。这个问题并不成立。", 26),
        ("원점을 중심으로 하는 반지름 1인 원에 원 밖의 점 P에서 두 개의 접선을 긋는다.", 32),
        (
            "## 解答\n\n1. **接線の方程式**: `y = mx + c`\n2. 詳細は [ドキュメント](https://github.com/MISAKA-BTC/MISAKA-Studio/blob/main/README.md) を参照。\n\n| 項目 | 値 |\n|---|---|\n| 窓 | 512 |",
            81,
        ),
        (
            "これまでの会話の要点（古い順）:\n- 質問: 円外の点 P から引いた 2 本の接線の接点の中点 Q の座標を求めよ。\n  回答: Q = (x0/(x0²+y0²), y0/(x0²+y0²))、OP・OQ=1。",
            82,
        ),
        (
            "PALW is proof of adjudicable LLM work. A block is won by verified inference in one of the chain-registered classes, and every seat re-executes the job it is shown before it signs a receipt.",
            44,
        ),
        (
            "{\"model\":\"misaka-palw-fp-v3\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":256,\"stream\":true}",
            35,
        ),
    ];

    #[test]
    fn the_estimate_is_an_upper_bound_on_every_measured_sample() {
        for (text, actual) in MEASURED {
            let estimate = estimate_tokens(text);
            assert!(estimate >= *actual, "under-counted: estimate {estimate} < actual {actual} for {text:?}");
            // And not absurdly: the budget is small, and a 3× over-count would throw most of it away.
            assert!(estimate <= actual * 5 / 2, "over-counted past 2.5×: estimate {estimate} vs actual {actual} for {text:?}");
        }
    }

    /// The exception, pinned so it is a known limit and not a surprise: rare kanji cost a
    /// byte-level BPE more than the estimate allows. The tokenizer file is the fix.
    #[test]
    fn rare_kanji_are_the_known_under_count() {
        let text = "齟齬・鬱屈・躊躇・薔薇・檸檬・醤油・顰蹙・贔屓";
        assert!(estimate_tokens(text) < 33, "if this now holds, move it into MEASURED");
    }

    #[test]
    fn a_conversation_counts_its_template() {
        let counter = TokenCounter::estimate();
        let with_system = [ChatMessage::new("system", ""), ChatMessage::new("user", "")];
        assert_eq!(counter.messages(&with_system), 2 * TEMPLATE_TOKENS_PER_MESSAGE + TEMPLATE_GENERATION_PROMPT);
        let without = [ChatMessage::new("user", "")];
        assert_eq!(counter.messages(&without), TEMPLATE_TOKENS_PER_MESSAGE + TEMPLATE_GENERATION_PROMPT + TEMPLATE_DEFAULT_SYSTEM);
    }

    /// With the real tokenizer the count is exact: the same samples, the same numbers llama.cpp
    /// printed. Runs when `MISAKA_TEST_QWEN25_TOKENIZER` names Qwen2.5's `tokenizer.json`.
    #[test]
    fn the_tokenizer_file_counts_what_llama_cpp_counts() {
        let Ok(path) = std::env::var("MISAKA_TEST_QWEN25_TOKENIZER") else {
            eprintln!("skipping: set MISAKA_TEST_QWEN25_TOKENIZER to Qwen2.5's tokenizer.json");
            return;
        };
        let counter = TokenCounter::from_tokenizer_file(Path::new(&path)).expect("loads");
        assert!(counter.is_exact());
        for (text, actual) in MEASURED {
            assert_eq!(counter.text(text), *actual, "{text:?}");
        }
        assert_eq!(counter.text("齟齬・鬱屈・躊躇・薔薇・檸檬・醤油・顰蹙・贔屓"), 33);
    }
}
