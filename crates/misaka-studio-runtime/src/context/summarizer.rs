//! **Summarising older turns with a local model.**
//!
//! Opt-in (`context.summarizer_model`). The chat engine on a 512-token class is the lane itself,
//! and a summary run there would be a mined job of its own inside the same 512 tokens — so the
//! summary comes from a GGUF on a local `llama-server`, started beside the chat engine with a
//! window of its own and kept for the life of the app.
//!
//! Summaries are rolling and cached by the history they cover: the summary of turns 1..k+1 is the
//! summary of 1..k with turn k+1 folded in, and a key is a hash chain over the messages, so a
//! conversation that grows by a turn costs one fold rather than a re-read of everything.

use super::tokens::estimate_tokens;
use crate::backend::llamacpp::{LlamaCppBackend, accelerator_tag};
use crate::backend::{ChatMessage, GenerationRequest, InferenceBackend, LoadRequest, StreamEvent};
use futures_util::StreamExt;
use misaka_studio_core::HardwareSnapshot;
use misaka_studio_core::model::LocalModel;
use misaka_studio_core::provenance::SamplingCommitment;
use misaka_studio_core::settings::Settings;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The summariser's own window. A turn on a 512-token class is at most 512 tokens, so 4K folds
/// several at a time with room for the previous summary.
pub const SUMMARIZER_CONTEXT: u32 = 4096;
/// Transcript tokens folded per request, by estimate — the rest of the window is the instruction,
/// the previous summary and the output.
const FOLD_TOKENS: u64 = 2400;
/// A message longer than this is cut before it is folded; a summary does not need the tail of a
/// 2,000-line answer.
const MESSAGE_CHARS: usize = 1600;
const CACHE_ENTRIES: usize = 512;
const GENERATION_TIMEOUT: Duration = Duration::from_secs(120);

struct Loaded {
    model_id: String,
    backend: Arc<LlamaCppBackend>,
}

#[derive(Default)]
struct Cache {
    map: HashMap<[u8; 32], String>,
    order: VecDeque<[u8; 32]>,
}

impl Cache {
    fn get(&self, key: &[u8; 32]) -> Option<String> {
        self.map.get(key).cloned()
    }
    fn put(&mut self, key: [u8; 32], value: String) {
        if self.map.insert(key, value).is_none() {
            self.order.push_back(key);
            while self.order.len() > CACHE_ENTRIES {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
}

#[derive(Default)]
pub struct Summarizer {
    engine: tokio::sync::Mutex<Option<Loaded>>,
    cache: Mutex<Cache>,
}

/// The hash chain over a history: `keys[i]` names the first `i + 1` messages.
pub fn prefix_keys(messages: &[ChatMessage]) -> Vec<[u8; 32]> {
    let mut previous = [0u8; 32];
    messages
        .iter()
        .map(|m| {
            let mut hasher = Sha256::new();
            hasher.update(previous);
            hasher.update(m.role.as_bytes());
            hasher.update([0]);
            hasher.update(m.content.as_bytes());
            hasher.update([0]);
            previous = hasher.finalize().into();
            previous
        })
        .collect()
}

/// The instruction for one fold: the previous summary, the new messages, the length.
pub fn fold_prompt(previous: Option<&str>, messages: &[ChatMessage], target_tokens: u64, japanese: bool) -> Vec<ChatMessage> {
    let transcript = messages
        .iter()
        .map(|m| {
            let who = match (m.role.as_str(), japanese) {
                ("user", true) => "ユーザー",
                ("assistant", true) => "アシスタント",
                ("user", false) => "User",
                ("assistant", false) => "Assistant",
                (other, _) => other,
            };
            let content: String = m.content.chars().take(MESSAGE_CHARS).collect();
            format!("{who}: {content}")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    if japanese {
        vec![
            ChatMessage::new(
                "system",
                format!(
                    "あなたは会話の記録係です。後で続きの質問に答えるときに必要な事実だけを、短い箇条書き（各行「- 」で始める）で残してください。\
                     数式・数値・結論・決めたこと・ユーザーが示した条件を優先し、挨拶や説明の繰り返しは書きません。全体で {target_tokens} トークン以内、日本語で。箇条書きだけを出力してください。"
                ),
            ),
            ChatMessage::new(
                "user",
                format!("これまでの要約:\n{}\n\n新しいやりとり:\n{transcript}\n\n更新した要約:", previous.unwrap_or("（なし）")),
            ),
        ]
    } else {
        vec![
            ChatMessage::new(
                "system",
                format!(
                    "You keep the record of a conversation. Write only the facts needed to answer follow-up questions later, as short bullet \
                     lines starting with \"- \". Prefer formulas, numbers, conclusions, decisions and constraints the user stated; leave out \
                     greetings and repeated explanation. At most {target_tokens} tokens in total. Output only the bullets."
                ),
            ),
            ChatMessage::new(
                "user",
                format!("Summary so far:\n{}\n\nNew exchange:\n{transcript}\n\nUpdated summary:", previous.unwrap_or("(none)")),
            ),
        ]
    }
}

/// Keep the bullet lines of a model's output; a model that ignores the format still has its
/// non-empty lines kept, as bullets.
pub fn clean_summary(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.ends_with(':') && !l.ends_with('：'))
        .map(|l| {
            let body = l.trim_start_matches(['-', '*', '・', '•']).trim();
            format!("- {body}")
        })
        .filter(|l| l.len() > 2)
        .collect();
    lines.join("\n")
}

impl Summarizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop the summariser's engine. Called when the setting changes; the next summary starts it
    /// again with the new model.
    pub async fn shutdown(&self) {
        if let Some(loaded) = self.engine.lock().await.take() {
            let _ = loaded.backend.unload().await;
        }
    }

    /// The summary of `older`, from the cache where it can be and by folding where it cannot.
    pub async fn summarize(
        &self,
        model: &LocalModel,
        settings: &Settings,
        hardware: &HardwareSnapshot,
        older: &[ChatMessage],
        target_tokens: u64,
        japanese: bool,
    ) -> Result<String, String> {
        if older.is_empty() {
            return Ok(String::new());
        }
        let keys = prefix_keys(older);
        let (mut done, mut summary) = {
            let cache = self.cache.lock().expect("summary cache");
            match (0..keys.len()).rev().find_map(|i| cache.get(&keys[i]).map(|s| (i + 1, s))) {
                Some((covered, text)) => (covered, Some(text)),
                None => (0, None),
            }
        };
        if done == older.len() {
            return Ok(summary.unwrap_or_default());
        }

        let backend = self.engine_for(model, settings, hardware).await?;
        while done < older.len() {
            // One fold: as many whole messages as the fold allows, at least one.
            let mut end = done;
            let mut tokens = 0u64;
            while end < older.len() {
                let cost = estimate_tokens(&older[end].content.chars().take(MESSAGE_CHARS).collect::<String>());
                if end > done && tokens + cost > FOLD_TOKENS {
                    break;
                }
                tokens += cost;
                end += 1;
            }
            let prompt = fold_prompt(summary.as_deref(), &older[done..end], target_tokens, japanese);
            let text = clean_summary(&generate(&backend, &model.id, prompt, target_tokens).await?);
            if text.is_empty() {
                return Err("the summariser returned nothing".to_string());
            }
            self.cache.lock().expect("summary cache").put(keys[end - 1], text.clone());
            summary = Some(text);
            done = end;
        }
        Ok(summary.unwrap_or_default())
    }

    async fn engine_for(
        &self,
        model: &LocalModel,
        settings: &Settings,
        hardware: &HardwareSnapshot,
    ) -> Result<Arc<LlamaCppBackend>, String> {
        let mut guard = self.engine.lock().await;
        if let Some(loaded) = guard.as_ref()
            && loaded.model_id == model.id
            && loaded.backend.loaded().await.is_some()
        {
            return Ok(loaded.backend.clone());
        }
        if let Some(old) = guard.take() {
            let _ = old.backend.unload().await;
        }
        let backend = Arc::new(LlamaCppBackend::new(
            settings.backend.llama_server_path.clone(),
            accelerator_tag(hardware),
            Duration::from_secs(settings.backend.startup_timeout_secs),
        ));
        if let crate::backend::Availability::Unavailable { reason, remedy } = backend.availability().await {
            return Err(format!("the summariser needs llama.cpp: {reason}. {remedy}"));
        }
        let devices = backend.devices().await;
        let gpu_layers =
            crate::state::plan_gpu_layers(model, hardware, devices.as_deref(), SUMMARIZER_CONTEXT as u64, settings.backend.gpu_layers);
        backend
            .load(LoadRequest {
                model_id: model.id.clone(),
                model_path: model.path.clone(),
                context_size: SUMMARIZER_CONTEXT,
                gpu_layers,
                threads: settings.backend.threads,
                flash_attention: settings.backend.flash_attention,
                use_mmap: settings.backend.use_mmap,
                use_mlock: false,
                needs_default_chat_template: !model.has_chat_template,
                extra_args: Vec::new(),
            })
            .await
            .map_err(|e| format!("the summariser could not load {}: {e}", model.id))?;
        tracing::info!(model = %model.id, "summariser engine ready");
        *guard = Some(Loaded { model_id: model.id.clone(), backend: backend.clone() });
        Ok(backend)
    }
}

async fn generate(
    backend: &LlamaCppBackend,
    model_id: &str,
    messages: Vec<ChatMessage>,
    target_tokens: u64,
) -> Result<String, String> {
    let request = GenerationRequest {
        model: model_id.to_string(),
        messages,
        prompt: None,
        params: SamplingCommitment {
            temperature: 0.2,
            top_p: 0.9,
            top_k: 40,
            min_p: 0.05,
            repeat_penalty: 1.05,
            max_tokens: target_tokens + 48,
            seed: Some(0),
        },
        stop: Vec::new(),
        prompt_tokens: None,
        // A summary is the output, not a reasoning trace ahead of one.
        disable_thinking: true,
    };
    let run = async {
        let mut stream = backend.generate(request).await.map_err(|e| e.to_string())?;
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            match event.map_err(|e| e.to_string())? {
                StreamEvent::Delta(d) => text.push_str(&d),
                StreamEvent::Done { .. } => break,
            }
        }
        Ok::<String, String>(text)
    };
    tokio::time::timeout(GENERATION_TIMEOUT, run)
        .await
        .map_err(|_| format!("the summariser did not answer within {}s", GENERATION_TIMEOUT.as_secs()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_growing_history_shares_its_prefix_keys() {
        let a = vec![ChatMessage::new("user", "q1"), ChatMessage::new("assistant", "a1")];
        let mut b = a.clone();
        b.push(ChatMessage::new("user", "q2"));
        let (ka, kb) = (prefix_keys(&a), prefix_keys(&b));
        assert_eq!(ka[..], kb[..2], "the first turns name the same prefix");
        assert_ne!(kb[1], kb[2]);
        // Order and role are part of the key.
        let swapped = vec![ChatMessage::new("assistant", "q1"), ChatMessage::new("user", "a1")];
        assert_ne!(prefix_keys(&swapped)[1], ka[1]);
    }

    #[test]
    fn the_fold_prompt_carries_the_previous_summary_and_the_new_turns() {
        let turns = vec![ChatMessage::new("user", "Q の座標は？"), ChatMessage::new("assistant", "Q = (x0/r², y0/r²)")];
        let prompt = fold_prompt(Some("- P は円外"), &turns, 120, true);
        assert_eq!(prompt.len(), 2);
        assert!(prompt[0].content.contains("120 トークン以内"));
        assert!(prompt[1].content.contains("- P は円外"));
        assert!(prompt[1].content.contains("アシスタント: Q = (x0/r², y0/r²)"));
    }

    #[test]
    fn a_summary_is_cleaned_to_bullets() {
        let raw = "要約:\n- Q = (x0/r², y0/r²)\n* OP・OQ = 1\n\n結論として軌跡は円";
        assert_eq!(clean_summary(raw), "- Q = (x0/r², y0/r²)\n- OP・OQ = 1\n- 結論として軌跡は円");
    }

    #[test]
    fn the_cache_forgets_its_oldest_entries_past_its_cap() {
        let mut cache = Cache::default();
        for i in 0..(CACHE_ENTRIES + 3) {
            let mut key = [0u8; 32];
            key[..8].copy_from_slice(&(i as u64).to_le_bytes());
            cache.put(key, format!("{i}"));
        }
        assert_eq!(cache.map.len(), CACHE_ENTRIES);
        assert!(cache.get(&[0u8; 32]).is_none(), "the first entry went first");
    }
}
