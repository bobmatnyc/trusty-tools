//! Send-time conversation compression + context-window trimming.
//!
//! Why: Long multi-turn conversations drift toward the model's hard context
//! window and re-pay full input rates for stale history. Compressing/trimming
//! at send-time (#69, #135) keeps stored history untouched while cutting tokens
//! on the wire; a TOML-disabled agent sees exactly the messages it would have
//! before these hooks existed.
//! What: `trim_messages_with_manager` applies a `ContextManager`'s soft token
//! budget to a typed message vector; `apply_compression` runs the sliding-window
//! history compressor and (optionally) the deterministic task compressor.
//! Test: `apply_compression_*` unit tests below; trimming is covered by the
//! `ContextManager`'s own tests.

use async_openai::types::ChatCompletionRequestMessage;

use crate::agents::AgentCompressConfig;
use crate::compress::history::{HistoryConfig, Turn, compress_history, history_token_count};
use crate::compress::{CompressConfig, compress as compress_text};
use crate::context::ContextManager;
use crate::session::HistoryMessage;

/// Apply a `ContextManager`'s soft token budget to a typed message vector
/// BEFORE issuing a chat completion (#69).
///
/// Why: Long multi-turn conversations drift toward the model's hard context
/// window; proactively trimming at ~50% leaves headroom for caching and the
/// assistant's response. We operate on the typed `ChatCompletionRequestMessage`
/// by round-tripping through `serde_json::Value` so we don't need a custom
/// size accounting for every message variant.
/// What: Serializes each message to a Value, calls
/// `ContextManager::trim_to_budget` with `protected_count = 1` (the system
/// message), and deserializes survivors back. On any serde failure the
/// original vector is returned unchanged (fail-open).
/// Test: Exercised via the manager's own unit tests; integration covered by
/// the workflow smoke tests.
pub fn trim_messages_with_manager(
    messages: Vec<ChatCompletionRequestMessage>,
    manager: &ContextManager,
    model: &str,
) -> Vec<ChatCompletionRequestMessage> {
    let json_msgs: Vec<serde_json::Value> = match messages
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(v) => v,
        Err(_) => return messages,
    };
    let original_len = json_msgs.len();
    let (trimmed, evicted) = manager.trim_to_budget(json_msgs, model, 1);
    if evicted == 0 {
        return messages;
    }
    tracing::debug!(
        model = %model,
        evicted,
        before = original_len,
        after = trimmed.len(),
        "context manager: trimmed messages"
    );
    let parsed: Result<Vec<ChatCompletionRequestMessage>, _> = trimmed
        .into_iter()
        .map(serde_json::from_value::<ChatCompletionRequestMessage>)
        .collect();
    parsed.unwrap_or(messages)
}

/// Apply compression to a conversation history and task text before sending.
///
/// Why: #135 — wires the existing `compress` module into the actual LLM call
/// path. Compressing at send-time keeps stored history untouched while
/// cutting tokens on the wire. A TOML-disabled agent sees the exact same
/// messages it would have before this hook existed.
/// What: If `cfg.enabled` is false this is a no-op passthrough. Otherwise:
/// runs the pinned sliding-window over `history` using the configured token
/// budget, and — when `cfg.compress_task` is true — runs the task string
/// through the deterministic prompt compressor. Any internal failure logs a
/// WARN and returns the original inputs (fail-open). Metrics are emitted at
/// DEBUG level.
/// Test: `apply_compression_disabled_passthrough`,
/// `apply_compression_compresses_history`,
/// `apply_compression_compresses_task_when_flag_set`.
pub fn apply_compression(
    history: Vec<HistoryMessage>,
    task: String,
    cfg: &AgentCompressConfig,
) -> (Vec<HistoryMessage>, String) {
    if !cfg.enabled {
        return (history, task);
    }

    // History window compression.
    let compressed_history = compress_history_messages(&history, cfg);

    // Task compression (optional).
    let compressed_task = if cfg.compress_task && !task.is_empty() {
        let result = std::panic::catch_unwind(|| compress_text(&task, &CompressConfig::default()));
        match result {
            Ok(r) => {
                tracing::debug!(
                    orig_chars = r.original_len,
                    compressed_chars = r.compressed_len,
                    reduction_pct = r.reduction_pct,
                    "[compress] task: {} → {} chars ({:.1}% reduction)",
                    r.original_len,
                    r.compressed_len,
                    r.reduction_pct
                );
                r.text
            }
            Err(_) => {
                tracing::warn!("compress: task compression panicked; using original");
                task
            }
        }
    } else {
        task
    };

    (compressed_history, compressed_task)
}

/// Run `compress_history` against a `HistoryMessage` slice and log metrics.
///
/// Why: Isolates the `HistoryMessage` <-> `Turn` round-trip and the
/// panic-guarded compressor call from `apply_compression`'s control flow.
/// What: Maps to `Turn`s, runs the sliding-window compressor with a fixed
/// `keep_last_n` and the configured token budget, logs a reduction metric,
/// and maps survivors back to `HistoryMessage`. Fail-open on panic.
/// Test: Covered via `apply_compression_compresses_history`.
fn compress_history_messages(
    history: &[HistoryMessage],
    cfg: &AgentCompressConfig,
) -> Vec<HistoryMessage> {
    let turns: Vec<Turn> = history
        .iter()
        .map(|h| Turn {
            role: h.role.clone(),
            content: h.content.clone(),
        })
        .collect();

    let history_cfg = HistoryConfig {
        keep_last_n: 6,
        token_budget: Some(cfg.token_budget),
        compress_turns: false,
        compress_config: CompressConfig::default(),
    };

    let orig_tokens = history_token_count(&turns);
    let orig_len = turns.len();
    let result = std::panic::catch_unwind(|| compress_history(&turns, &history_cfg));
    let compressed = match result {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("compress: history compression panicked; using original");
            return history.to_vec();
        }
    };
    let new_tokens = history_token_count(&compressed);
    let new_len = compressed.len();

    let ratio = if orig_len > 0 {
        (1.0 - new_len as f64 / orig_len as f64) * 100.0
    } else {
        0.0
    };
    tracing::debug!(
        orig_msgs = orig_len,
        compressed_msgs = new_len,
        orig_tokens,
        new_tokens,
        "[compress] history: {} → {} messages ({:.1}% reduction)",
        orig_len,
        new_len,
        ratio
    );

    compressed
        .into_iter()
        .map(|t| HistoryMessage {
            role: t.role,
            content: t.content,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(role: &str, content: &str) -> HistoryMessage {
        HistoryMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn apply_compression_disabled_passthrough() {
        // When compress.enabled is false, messages and task pass through unchanged.
        let cfg = AgentCompressConfig {
            enabled: false,
            token_budget: 1000,
            compress_task: true, // even with compress_task on, disabled wins
            ..AgentCompressConfig::default()
        };
        let hist = vec![hm("user", "one"), hm("assistant", "two")];
        let task = "Write a program that adds two integers and prints the result.".to_string();
        let (h, t) = apply_compression(hist.clone(), task.clone(), &cfg);
        assert_eq!(h, hist);
        assert_eq!(t, task);
    }

    #[test]
    fn apply_compression_compresses_history() {
        // With a tight token budget, middle turns should be evicted.
        let mut hist: Vec<HistoryMessage> = Vec::new();
        for i in 0..20 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            hist.push(hm(
                role,
                &format!("Turn {i} content with plenty of descriptive words for scoring"),
            ));
        }
        let cfg = AgentCompressConfig {
            enabled: true,
            token_budget: 100,
            compress_task: false,
            ..AgentCompressConfig::default()
        };
        let (h, t) = apply_compression(hist.clone(), "task".to_string(), &cfg);
        assert!(
            h.len() < hist.len(),
            "expected history to shrink, got {} -> {}",
            hist.len(),
            h.len()
        );
        // Turn 0 is always pinned.
        assert_eq!(h[0].content, hist[0].content);
        // Task is untouched when compress_task=false.
        assert_eq!(t, "task");
    }

    #[test]
    fn apply_compression_compresses_task_when_flag_set() {
        // With compress_task=true, a verbose task should shrink.
        let cfg = AgentCompressConfig {
            enabled: true,
            token_budget: 32_000,
            compress_task: true,
            ..AgentCompressConfig::default()
        };
        let verbose = "This is a very long paragraph about the system. \
            Furthermore, it contains many words that should be removed by the stop word filter. \
            Moreover, there are also discourse markers in this text that add no real value. \
            Additionally, the system is indeed quite verbose by design to test the pipeline."
            .to_string();
        let (_h, t) = apply_compression(vec![], verbose.clone(), &cfg);
        assert!(
            t.len() < verbose.len(),
            "expected task to shrink: {} -> {}",
            verbose.len(),
            t.len()
        );
    }

    #[test]
    fn apply_compression_empty_history_task_untouched_when_flag_off() {
        let cfg = AgentCompressConfig {
            enabled: true,
            token_budget: 1000,
            compress_task: false,
            ..AgentCompressConfig::default()
        };
        let task = "hello world".to_string();
        let (h, t) = apply_compression(vec![], task.clone(), &cfg);
        assert!(h.is_empty());
        assert_eq!(t, task);
    }
}
