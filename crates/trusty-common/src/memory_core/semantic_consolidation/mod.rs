//! Inference-backed semantic consolidation for the dream cycle.
//!
//! Why: The NLP-only dream passes (dedup, prune, closet) cannot handle
//! semantic equivalence: `"ts"` and `"trusty-search"` are the same concept,
//! three overlapping facts about a single topic should collapse to one crisp
//! triple, and paraphrases waste recall slots. An LLM can see the semantic
//! layer the vector index cannot.
//! What: Defines the `Inference` trait (abstraction over any chat model) with
//! `OpenRouterInference` (production) and `MockInference` (tests), plus the
//! `SemanticConsolidator` that clusters near-duplicate drawers and delegates
//! canonicalization to the configured provider. Gracefully degrades to a no-op
//! when no inference backend is available.
//! Test: `cargo test -p trusty-common --features memory-core
//!        semantic_consolidation::tests::`.

mod consolidator;
mod inference;
mod types;

pub use consolidator::SemanticConsolidator;
pub use inference::{Inference, MockInference, OllamaInference, OpenRouterInference};
pub use types::{
    CanonicalDrawer, ConsolidationAction, ConsolidationResult, SemanticConsolidationConfig,
    inference_available, parse_consolidation_actions, resolve_openrouter_api_key,
    validate_ollama_model,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_core::palace::Drawer;
    use std::sync::Arc;
    use uuid::Uuid;

    fn make_drawer(content: &str, importance: f32) -> Drawer {
        let room_id = Uuid::new_v4();
        let mut d = Drawer::new(room_id, content);
        d.importance = importance;
        d
    }

    /// Why: lock the default config values so accidental changes are caught.
    #[test]
    fn semantic_config_defaults() {
        let cfg = SemanticConsolidationConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.model, "anthropic/claude-haiku-4-5");
        assert!((cfg.similarity_threshold - 0.75).abs() < 1e-6);
        assert_eq!(cfg.max_batch_size, 8);
        assert_eq!(cfg.max_calls_per_cycle, 20);
    }

    /// Why: the JSON serialization of each action variant must round-trip
    /// cleanly through serde so LLM responses can be parsed unambiguously.
    #[test]
    fn consolidation_action_deserializes() {
        let alias_json = r#"{"action":"alias","from":"ts","to":"trusty-search"}"#;
        let action: ConsolidationAction = serde_json::from_str(alias_json).unwrap();
        assert_eq!(
            action,
            ConsolidationAction::Alias {
                from: "ts".into(),
                to: "trusty-search".into()
            }
        );

        let id = Uuid::new_v4();
        let merge_json = format!(
            r#"{{"action":"merge","canonical_content":"trusty-search is a hybrid search daemon","superseded_ids":["{id}"]}}"#
        );
        let action: ConsolidationAction = serde_json::from_str(&merge_json).unwrap();
        if let ConsolidationAction::Merge {
            canonical_content,
            superseded_ids,
        } = action
        {
            assert_eq!(canonical_content, "trusty-search is a hybrid search daemon");
            assert_eq!(superseded_ids, vec![id]);
        } else {
            panic!("expected Merge");
        }

        let flag_json =
            format!(r#"{{"action":"flag","drawer_id":"{id}","reason":"contradicts other entry"}}"#);
        let action: ConsolidationAction = serde_json::from_str(&flag_json).unwrap();
        assert_eq!(
            action,
            ConsolidationAction::Flag {
                drawer_id: id,
                reason: "contradicts other entry".into()
            }
        );
    }

    /// Why: the gate function must return false when no key is configured so
    /// the dream cycle skips LLM calls on unconfigured deployments.
    ///
    /// Audit finding (same class as #3607/#3608, found while fixing those
    /// issues): `inference_available("", false)` falls back to reading
    /// `OPENROUTER_API_KEY` from the process environment when neither an
    /// inline key nor a local model is configured, so this test's PASS/FAIL
    /// depends on that var being absent for its whole body — exactly like a
    /// writer of the var would. It raced `resolve_openrouter_api_key_falls_back_to_env`
    /// (and, cross-file, `memory_core::dream::tests`'s `OPENROUTER_API_KEY`
    /// mutators) because it held no lock at all. Join the same
    /// `dotenv_credential_env` group as every other test that reads or
    /// writes this var.
    #[test]
    #[serial_test::serial(dotenv_credential_env)]
    fn inference_available_false_without_key() {
        assert!(!inference_available("", false));
        assert!(!inference_available("   ", false));
    }

    /// Why: an inline key (not env var) must also enable inference.
    #[test]
    fn inference_available_true_with_inline_key() {
        assert!(inference_available("sk-test-key", false));
    }

    /// Why: local model flag alone is sufficient to enable inference.
    #[test]
    fn inference_available_true_with_local_model() {
        assert!(inference_available("", true));
    }

    /// Why: a non-empty configured value must always win over the
    /// environment, regardless of what `OPENROUTER_API_KEY` holds — no env
    /// mutation needed for this half of the contract.
    #[test]
    fn resolve_openrouter_api_key_prefers_configured_value() {
        assert_eq!(resolve_openrouter_api_key("sk-configured"), "sk-configured");
    }

    /// Why (code review follow-up on #2977): `build_consolidator_from_config`
    /// resolves an empty configured key against `OPENROUTER_API_KEY` in the
    /// process environment before picking a backend; `resolve_openrouter_api_key`
    /// must do the same so every caller (including
    /// `trusty-memory::dream_config_from_user_config`) agrees on the
    /// resolved key. `#[serial(dotenv_credential_env)]` + a local
    /// `EnvVarGuard` avoid racing other tests that read/write the same real
    /// env var — the named group joins the SAME lock used by
    /// `inference::credentials::{resolver,dotenv}::tests` and
    /// `memory_core::dream::tests` (audit finding, same class as
    /// #3607/#3608: a bare unnamed `#[serial]` here does NOT coordinate with
    /// those other files' named group, since serial_test treats each group
    /// name as an independent lock).
    #[test]
    #[serial_test::serial(dotenv_credential_env)]
    fn resolve_openrouter_api_key_falls_back_to_env() {
        let _guard = EnvVarGuard::set("OPENROUTER_API_KEY", "sk-from-env");
        assert_eq!(resolve_openrouter_api_key(""), "sk-from-env");
    }

    // ─── RAII env-var guard for tests (mirrors
    // trusty_common::memory_core::dream::tests::EnvVarGuard) ───────────────
    //
    // Safety: test-only; `#[serial_test::serial(dotenv_credential_env)]` on
    // every caller serialises access to the real process environment across
    // test threads (and across the other files sharing this named group).

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            // Safety: test-only; caller is `#[serial]`.
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // Safety: test-only; caller is `#[serial]`.
            match &self.previous {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    /// Why: `parse_consolidation_actions` must extract actions from raw JSON.
    #[test]
    fn parse_consolidation_actions_round_trips() {
        let id = Uuid::new_v4();
        let raw = format!(
            r#"[{{"action":"alias","from":"ts","to":"trusty-search"}},{{"action":"flag","drawer_id":"{id}","reason":"test"}}]"#
        );
        let actions = parse_consolidation_actions(&raw).unwrap();
        assert_eq!(actions.len(), 2);
    }

    /// Why: many models wrap their JSON in markdown fences; the parser must
    /// strip them.
    #[test]
    fn parse_handles_markdown_fence() {
        let raw = "```json\n[{\"action\":\"alias\",\"from\":\"a\",\"to\":\"b\"}]\n```";
        let actions = parse_consolidation_actions(raw).unwrap();
        assert_eq!(actions.len(), 1);
    }

    /// Why: if the model returns garbage, the dream cycle must not fail.
    #[test]
    fn parse_returns_empty_on_garbage() {
        let actions = parse_consolidation_actions("sorry, I cannot help with that").unwrap();
        assert!(actions.is_empty());
    }

    /// Why: same batch must produce the same cache key so cache hits work.
    #[test]
    fn batch_cache_key_is_deterministic() {
        let d1 = make_drawer("alpha content", 0.7);
        let d2 = make_drawer("beta content", 0.5);
        let batch = vec![d1.clone(), d2.clone()];
        let k1 = types::batch_cache_key(&batch);
        let k2 = types::batch_cache_key(&batch);
        assert_eq!(k1, k2);
    }

    /// Why (issue #2593): the exact failure — the Anthropic-style default
    /// model resolved against a local Ollama backend — must be rejected with
    /// an actionable message, not silently attempted and retried forever.
    #[test]
    fn validate_ollama_model_rejects_cloud_prefix() {
        let err = validate_ollama_model("anthropic/claude-haiku-4-5").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("anthropic/"), "message should name the model");
        assert!(msg.contains("Ollama"), "message should name the provider");
        assert!(
            msg.contains("semantic.model") || msg.contains("openrouter_api_key"),
            "message should name a config knob to fix it"
        );
    }

    /// Why: a real local model id (no vendor prefix) must pass validation so
    /// a correctly-configured local-model deployment keeps working.
    #[test]
    fn validate_ollama_model_accepts_local_tag() {
        assert!(validate_ollama_model("llama3.1").is_ok());
        assert!(validate_ollama_model("llama3.1:8b").is_ok());
        assert!(validate_ollama_model("qwen2.5:7b-instruct").is_ok());
    }

    /// Why (code review on #2977): several `CLOUD_VENDOR_PREFIXES` entries
    /// (`qwen/`, `mistralai/`, `nvidia/`, `meta-llama/`, `deepseek/`) are also
    /// real, pullable Ollama registry namespaces. A locally-pulled model
    /// referenced with its explicit `:tag` must NOT be rejected — otherwise
    /// the validator itself would produce the false-positive failure mode
    /// this issue is fixing.
    #[test]
    fn validate_ollama_model_accepts_tagged_vendor_namespaced_pull() {
        assert!(validate_ollama_model("qwen/qwen2.5:7b-instruct").is_ok());
        assert!(validate_ollama_model("mistralai/mistral-nemo:12b").is_ok());
        assert!(validate_ollama_model("meta-llama/llama-3.1:8b").is_ok());
    }

    /// Why: dropping the `:tag` suffix removes the only signal that
    /// distinguishes a deliberate local pull from a copy-pasted OpenRouter
    /// id, so the untagged form of a vendor-namespaced id must still be
    /// rejected.
    #[test]
    fn validate_ollama_model_rejects_untagged_vendor_namespaced_pull() {
        assert!(validate_ollama_model("qwen/qwen2.5-7b-instruct").is_err());
    }

    /// Why: two different batches must have different keys so the cache
    /// doesn't return stale actions.
    #[test]
    fn batch_cache_key_differs_for_different_content() {
        let d1 = make_drawer("alpha content", 0.7);
        let d2 = make_drawer("totally different", 0.5);
        let k1 = types::batch_cache_key(&[d1]);
        let k2 = types::batch_cache_key(&[d2]);
        assert_ne!(k1, k2);
    }

    /// Why: `SemanticConsolidator` must produce a `CanonicalDrawer` and
    /// populate `superseded_ids` when the mock returns a `Merge` action.
    #[tokio::test]
    async fn consolidator_merges_cluster() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        let mut d1 = make_drawer("ts is a search tool", 0.8);
        d1.id = id1;
        let mut d2 = make_drawer("trusty-search is a hybrid search daemon", 0.6);
        d2.id = id2;

        let actions = vec![ConsolidationAction::Merge {
            canonical_content: "trusty-search (ts) is a hybrid BM25+vector search daemon"
                .to_string(),
            superseded_ids: vec![id1, id2],
        }];

        let mock = Arc::new(MockInference::new(actions));
        let call_count = mock.call_count.clone();
        let cfg = SemanticConsolidationConfig {
            max_batch_size: 8,
            max_calls_per_cycle: 20,
            ..Default::default()
        };
        let consolidator = SemanticConsolidator::new(mock, cfg);

        let result = consolidator.consolidate(&[d1, d2]).await;

        assert_eq!(result.canonical_drawers.len(), 1);
        assert_eq!(
            result.canonical_drawers[0].content,
            "trusty-search (ts) is a hybrid BM25+vector search daemon"
        );
        assert!(result.superseded_ids.contains(&id1));
        assert!(result.superseded_ids.contains(&id2));
        assert_eq!(call_count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// Why: once a batch has been processed, subsequent identical calls must
    /// return cached actions without hitting the inference backend.
    #[tokio::test]
    async fn consolidator_caches_repeated_batches() {
        let d = make_drawer("trusty-memory is a palace storage engine", 0.7);
        let actions = vec![ConsolidationAction::Alias {
            from: "tm".to_string(),
            to: "trusty-memory".to_string(),
        }];

        let mock = Arc::new(MockInference::new(actions));
        let call_count = mock.call_count.clone();
        let consolidator = SemanticConsolidator::new(
            mock,
            SemanticConsolidationConfig {
                max_batch_size: 8,
                max_calls_per_cycle: 20,
                ..Default::default()
            },
        );

        let r1 = consolidator.consolidate(std::slice::from_ref(&d)).await;
        let r2 = consolidator.consolidate(std::slice::from_ref(&d)).await;

        assert_eq!(call_count.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(r1.cache_hits, 0);
        assert_eq!(r2.cache_hits, 1);
        assert_eq!(r1.aliases.len(), 1);
        assert_eq!(r2.aliases.len(), 1);
    }

    /// Why: the call budget must be honored so a palace with many drawers
    /// doesn't fire unlimited LLM calls in one cycle.
    #[tokio::test]
    async fn consolidator_respects_call_budget() {
        let drawers: Vec<Drawer> = (0..10)
            .map(|i| make_drawer(&format!("drawer content {i}"), 0.5))
            .collect();

        let mock = Arc::new(MockInference::no_op());
        let call_count = mock.call_count.clone();
        let consolidator = SemanticConsolidator::new(
            mock,
            SemanticConsolidationConfig {
                max_batch_size: 2,
                max_calls_per_cycle: 3,
                ..Default::default()
            },
        );

        let result = consolidator.consolidate(&drawers).await;

        assert_eq!(
            call_count.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "should stop at budget of 3 calls"
        );
        assert_eq!(result.llm_calls, 3);
    }

    /// Why: aliases must be collected from the mock and surfaced in the result.
    #[tokio::test]
    async fn consolidator_collects_aliases() {
        let d = make_drawer("ts stands for trusty-search", 0.5);
        let actions = vec![ConsolidationAction::Alias {
            from: "ts".into(),
            to: "trusty-search".into(),
        }];

        let mock = Arc::new(MockInference::new(actions));
        let consolidator = SemanticConsolidator::new(mock, SemanticConsolidationConfig::default());

        let result = consolidator.consolidate(&[d]).await;

        assert_eq!(
            result.aliases,
            vec![("ts".to_string(), "trusty-search".to_string())]
        );
    }

    /// Why: flagged drawers must be surfaced without being deleted.
    #[tokio::test]
    async fn consolidator_flags_contradictions() {
        let d = make_drawer("trusty-search uses PostgreSQL for storage", 0.7);
        let id = d.id;
        let actions = vec![ConsolidationAction::Flag {
            drawer_id: id,
            reason: "contradicts: trusty-search uses redb".into(),
        }];

        let mock = Arc::new(MockInference::new(actions));
        let consolidator = SemanticConsolidator::new(mock, SemanticConsolidationConfig::default());

        let result = consolidator.consolidate(&[d]).await;

        assert_eq!(result.flagged_ids.len(), 1);
        assert_eq!(result.flagged_ids[0].0, id);
        assert!(result.superseded_ids.is_empty());
        assert!(result.canonical_drawers.is_empty());
    }
}
