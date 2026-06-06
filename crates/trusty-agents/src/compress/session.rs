//! Session-level conversation history compression (#448).
//!
//! Why: Long-running multi-turn conversations accumulate history that pushes
//! against the model's context window and burns tokens on every request.
//! Distinct from `src/compress/mod.rs` (deterministic prompt-token shaving)
//! and `src/compress/history.rs` (sliding-window eviction): this module
//! summarizes *old* turns with a cheap LLM call so semantic content survives
//! while turn count shrinks.
//! What: `SessionCompressor` holds threshold/keep-recent/model knobs and
//! exposes `should_compress()` + async `compress()`. On compression the old
//! prefix is replaced with a single synthetic system "[CONVERSATION SUMMARY]"
//! message; the most recent N turns are kept verbatim.
//! Test: `should_compress_*`, `compress_replaces_old_with_summary`,
//! `compress_below_threshold_is_noop`.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Simple role/content pair used by the session compressor.
///
/// Why: Decouples this module from async-openai's typed message enum so it
/// can be exercised by unit tests and (eventually) plugged into different
/// LLM backends. Conversion helpers bridge to `HistoryMessage` at the wiring
/// site.
/// What: `role` is one of "system" | "user" | "assistant"; `content` is plain
/// text.
/// Test: All compressor tests construct these directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: content.into(),
        }
    }
}

/// Trait covering the minimal LLM surface SessionCompressor needs.
///
/// Why: Lets unit tests inject a deterministic mock without spinning up an
/// HTTP client or an OpenRouter session. Real callers wrap whatever live
/// client they already use.
/// What: Async `complete(system, user, model)` returning the assistant text.
/// Implementations are free to ignore `model` (mock) or honor it.
/// Test: `MockLlm` in tests below; eval framework reuses the same trait.
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, system: &str, user: &str, model: Option<&str>) -> Result<String>;
}

/// Configuration + behavior for collapsing old turns into a summary.
///
/// Why: The thresholds need to be tunable per agent so chatty personas can
/// keep more context than terse engineering agents.
/// What: `threshold` is the total-turn count at which compression triggers;
/// `keep_recent` is the count of trailing turns preserved verbatim;
/// `summary_model` overrides which model performs the summarization call
/// (commonly a cheap one like Haiku).
/// Test: `should_compress_*` for the trigger, `compress_*` for the action.
#[derive(Debug, Clone)]
pub struct SessionCompressor {
    /// Total turns before compression triggers.
    pub threshold: usize,
    /// Turns to keep verbatim (most recent).
    pub keep_recent: usize,
    /// Model to use for summarization (can differ from the main agent model).
    pub summary_model: Option<String>,
}

impl Default for SessionCompressor {
    fn default() -> Self {
        Self {
            threshold: 40,
            keep_recent: 10,
            summary_model: None,
        }
    }
}

impl SessionCompressor {
    /// Why: Lets callers cheaply skip the (async) compress() call when the
    /// history is still small.
    /// What: True iff `history.len() >= self.threshold`.
    /// Test: `should_compress_at_threshold`, `should_compress_below_threshold`,
    /// `should_compress_above_threshold`.
    pub fn should_compress(&self, history: &[ChatMessage]) -> bool {
        history.len() >= self.threshold
    }

    /// Why: Replaces the old-history prefix with one compact summary so the
    /// model retains semantic context while turn count drops to `keep_recent + 1`.
    /// What: Splits at `len - keep_recent`, asks `llm_client` to summarize
    /// the old prefix as plain text, then returns `[summary_system,
    /// ...recent]`. When `keep_recent >= history.len()` or `history.len() < 2`
    /// the input is returned unchanged.
    /// Test: `compress_replaces_old_with_summary`,
    /// `compress_keeps_recent_verbatim`, `compress_with_keep_all_is_noop`.
    pub async fn compress(
        &self,
        history: Vec<ChatMessage>,
        llm_client: &dyn LlmClient,
    ) -> Result<Vec<ChatMessage>> {
        if history.len() < 2 || self.keep_recent >= history.len() {
            return Ok(history);
        }

        let split_at = history.len() - self.keep_recent;
        let (old, recent) = history.split_at(split_at);

        let rendered = render_history_for_summary(old);
        let prompt = format!(
            "Summarize the following conversation history concisely, preserving key decisions, facts, and context:\n\n{rendered}"
        );

        let summary = llm_client
            .complete(
                "You are a conversation summarizer. Output a short, factual prose summary.",
                &prompt,
                self.summary_model.as_deref(),
            )
            .await?;

        let mut out = Vec::with_capacity(recent.len() + 1);
        out.push(ChatMessage::system(format!(
            "[CONVERSATION SUMMARY]\n{}",
            summary.trim()
        )));
        out.extend(recent.iter().cloned());
        Ok(out)
    }
}

/// Render an old-history slice into a single text block for the summary prompt.
///
/// Why: The summarizer LLM consumes a plain transcript; we don't want to
/// re-serialize the typed messages with all their tool-call noise.
/// What: One line per turn formatted as `"<role>: <content>"`. Empty content
/// is preserved so the summarizer sees the actual turn count.
/// Test: Indirectly via `compress_*` tests below.
fn render_history_for_summary(old: &[ChatMessage]) -> String {
    let mut buf = String::new();
    for m in old {
        buf.push_str(&m.role);
        buf.push_str(": ");
        buf.push_str(&m.content);
        buf.push('\n');
    }
    buf
}

/// Bridge: convert `crate::session::HistoryMessage` ↔ `ChatMessage`.
///
/// Why: The harness stores history as `HistoryMessage`; the compressor
/// operates on its own simple shape so it stays unit-testable. These small
/// conversion helpers keep the wiring site (`src/ctrl/mod.rs`) trivial.
/// What: Field-for-field clones; no validation beyond what the caller
/// already enforces (roles are pre-tagged at append time).
/// Test: `bridge_history_message_round_trip`.
pub fn from_history(messages: &[crate::session::HistoryMessage]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|h| ChatMessage {
            role: h.role.clone(),
            content: h.content.clone(),
        })
        .collect()
}

pub fn to_history(messages: Vec<ChatMessage>) -> Vec<crate::session::HistoryMessage> {
    messages
        .into_iter()
        .map(|m| crate::session::HistoryMessage {
            role: m.role,
            content: m.content,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock LLM that returns a canned summary and counts invocations.
    struct MockLlm {
        response: String,
        calls: Arc<AtomicUsize>,
    }

    impl MockLlm {
        fn new(response: &str) -> (Self, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    response: response.to_string(),
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn complete(
            &self,
            _system: &str,
            _user: &str,
            _model: Option<&str>,
        ) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        }
    }

    fn make_history(n: usize) -> Vec<ChatMessage> {
        let mut h = Vec::with_capacity(n);
        for i in 0..n {
            if i % 2 == 0 {
                h.push(ChatMessage::user(format!("user turn {i}")));
            } else {
                h.push(ChatMessage::assistant(format!("assistant turn {i}")));
            }
        }
        h
    }

    #[test]
    fn should_compress_below_threshold() {
        let c = SessionCompressor {
            threshold: 10,
            keep_recent: 3,
            summary_model: None,
        };
        assert!(!c.should_compress(&make_history(9)));
    }

    #[test]
    fn should_compress_at_threshold() {
        let c = SessionCompressor {
            threshold: 10,
            keep_recent: 3,
            summary_model: None,
        };
        assert!(c.should_compress(&make_history(10)));
    }

    #[test]
    fn should_compress_above_threshold() {
        let c = SessionCompressor {
            threshold: 10,
            keep_recent: 3,
            summary_model: None,
        };
        assert!(c.should_compress(&make_history(50)));
    }

    #[tokio::test]
    async fn compress_replaces_old_with_summary() {
        let c = SessionCompressor {
            threshold: 5,
            keep_recent: 2,
            summary_model: Some("haiku".to_string()),
        };
        let history = make_history(10);
        let (mock, calls) = MockLlm::new("Discussed widget design and chose option B.");

        let result = c.compress(history.clone(), &mock).await.unwrap();

        // First message is the synthetic summary.
        assert_eq!(result[0].role, "system");
        assert!(result[0].content.starts_with("[CONVERSATION SUMMARY]"));
        assert!(result[0].content.contains("widget design"));

        // keep_recent=2 → exactly 2 trailing messages preserved verbatim.
        assert_eq!(result.len(), 3, "expected 1 summary + 2 recent");
        assert_eq!(result[1], history[8]);
        assert_eq!(result[2], history[9]);

        // The LLM was called exactly once.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn compress_keeps_recent_verbatim() {
        let c = SessionCompressor {
            threshold: 5,
            keep_recent: 4,
            summary_model: None,
        };
        let history = make_history(12);
        let (mock, _) = MockLlm::new("summary");
        let result = c.compress(history.clone(), &mock).await.unwrap();
        assert_eq!(result.len(), 5);
        // Last 4 entries are byte-identical to the original tail.
        for (i, recent) in result[1..].iter().enumerate() {
            assert_eq!(recent, &history[history.len() - 4 + i]);
        }
    }

    #[tokio::test]
    async fn compress_with_keep_all_is_noop() {
        let c = SessionCompressor {
            threshold: 1,
            keep_recent: 100,
            summary_model: None,
        };
        let history = make_history(5);
        let (mock, calls) = MockLlm::new("unused");
        let result = c.compress(history.clone(), &mock).await.unwrap();
        assert_eq!(result, history);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn compress_empty_history_is_noop() {
        let c = SessionCompressor::default();
        let (mock, calls) = MockLlm::new("unused");
        let result = c.compress(vec![], &mock).await.unwrap();
        assert!(result.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn bridge_history_message_round_trip() {
        let original = vec![
            crate::session::HistoryMessage::user("hello"),
            crate::session::HistoryMessage::assistant("hi"),
        ];
        let bridged = from_history(&original);
        assert_eq!(bridged.len(), 2);
        assert_eq!(bridged[0].role, "user");
        let restored = to_history(bridged);
        assert_eq!(restored, original);
    }

    #[test]
    fn default_compressor_has_documented_values() {
        let c = SessionCompressor::default();
        assert_eq!(c.threshold, 40);
        assert_eq!(c.keep_recent, 10);
        assert!(c.summary_model.is_none());
    }
}
