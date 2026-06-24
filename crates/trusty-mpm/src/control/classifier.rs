//! Optional OpenRouter activity classifier gate (§8.3 of SPEC-SESSCTL-01, WI-5).
//!
//! Why: §8.3 makes the OpenRouter LLM classifier an explicit *fallback*, not the
//! default. AC-9 requires that with the classifier disabled (and no key) NO
//! outbound call to openrouter.ai is ever made. The gate that decides whether to
//! call the classifier is the single security/cost-relevant decision point, so
//! it is isolated here as a pure function that can be exhaustively unit-tested
//! without any network, env mutation, or LLM dependency.
//! What: [`ClassifierGate`] captures the three §8.3 preconditions (config flag,
//! key presence, SM-agent connection) and [`ClassifierGate::should_classify`]
//! returns `true` only when all three permit it. No HTTP client lives here —
//! WI-5 ships the gate; the actual OpenRouter call is wired by a later ticket
//! once the gate proves the classifier is permitted.
//! Test: `classifier_gate_*` in the inline `tests` module.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, warn};

use crate::control::config::ObservabilityConfig;
use crate::control::event::ActivityKind;
use crate::core::sm::providers::{ChatMessage, LlmProvider, LlmRequest};

/// The three §8.3 preconditions that gate the optional OpenRouter classifier.
///
/// Why: §8.3 lists three independent conditions, ALL of which must hold before
/// the daemon may call OpenRouter. Modeling them as one value with one decision
/// method keeps the policy in a single auditable place and prevents a caller
/// from checking only a subset.
/// What: `config_enabled` mirrors `[control_plane.observability].llm_classifier`;
/// `api_key_present` reflects whether `OPENROUTER_API_KEY` is set in the
/// environment; `sm_agent_connected` is `true` when a parent SM agent is
/// attached (in which case the SM provides inference and the daemon must NOT
/// call OpenRouter).
/// Test: `classifier_gate_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifierGate {
    /// `[control_plane.observability].llm_classifier == true`.
    pub config_enabled: bool,
    /// `OPENROUTER_API_KEY` is present in the environment.
    pub api_key_present: bool,
    /// An SM agent is currently connected (it provides inference; §8.3 #3).
    pub sm_agent_connected: bool,
}

impl ClassifierGate {
    /// Build a gate from the observability config and the two runtime signals.
    ///
    /// Why: callers hold a [`ObservabilityConfig`] and two booleans; this
    /// constructor keeps the field-mapping in one spot so the gate's three
    /// inputs always come from the same authoritative sources.
    /// What: copies `config.llm_classifier` into `config_enabled` and stores the
    /// two runtime signals verbatim.
    /// Test: `classifier_gate_from_config`.
    pub fn from_config(
        config: &ObservabilityConfig,
        api_key_present: bool,
        sm_agent_connected: bool,
    ) -> Self {
        Self {
            config_enabled: config.llm_classifier,
            api_key_present,
            sm_agent_connected,
        }
    }

    /// Decide whether the OpenRouter classifier may be invoked (§8.3).
    ///
    /// Why: this is the AC-9 enforcement point — if it returns `false` no
    /// outbound OpenRouter call is permitted, guaranteeing the default path
    /// (classifier off, no key) never touches the network.
    /// What: returns `true` only when ALL of: the config flag is on, the API key
    /// is present, AND no SM agent is connected (an attached SM provides the
    /// inference itself, per §8.3 condition 3).
    /// Test: `classifier_gate_all_off`, `classifier_gate_requires_all_three`,
    /// `classifier_gate_sm_connected_blocks`.
    pub fn should_classify(&self) -> bool {
        self.config_enabled && self.api_key_present && !self.sm_agent_connected
    }
}

// ─── Activity classifier seam (§8.3, #1648) ────────────────────────────────────

/// Sampling temperature for the §8.3 classifier call (deterministic-leaning).
///
/// Why: classification wants a low-variance label, not creative prose, so the
/// temperature is pinned low. A named constant keeps the value referenceable
/// and prevents an accidental high-temperature edit from making labels flaky.
/// What: `0.0` — fully deterministic decoding for the single-label task.
/// Test: covered transitively by `openrouter_classifier_*` tests.
const CLASSIFIER_TEMPERATURE: f32 = 0.0;

/// Maximum tokens the classifier may generate (one short label word).
///
/// Why: the classifier returns a single label token; capping `max_tokens` keeps
/// the (already-gated) call cheap and bounds cost per §8.3's fallback intent.
/// What: `16` tokens — ample for any of the label keywords below.
/// Test: covered transitively by `openrouter_classifier_*` tests.
const CLASSIFIER_MAX_TOKENS: u32 = 16;

/// The §8.3 classifier system prompt.
///
/// Why: §8.3 invokes the LLM only for activity the regex layer could not
/// resolve; the prompt must constrain the model to emit exactly one of the
/// known [`ActivityKind`] labels so the response maps deterministically.
/// What: instructs the model to answer with a single lowercase label keyword.
/// Test: `openrouter_classifier_returns_label_on_gate_on`.
const CLASSIFIER_SYSTEM_PROMPT: &str = "You are a session-activity classifier. \
Given raw output from a coding-agent session, reply with EXACTLY ONE lowercase \
label from this set and nothing else: awaiting-input, tool-use, error, \
test-result, git-op, auth-prompt, session-complete, rate-limited, unknown.";

/// An injectable seam that classifies raw session output into an [`ActivityKind`].
///
/// Why: §8.3's OpenRouter call is the single cost/network-relevant action in the
/// observability path. Modeling it as a trait lets the actor depend on an
/// abstraction — so the default path injects nothing (zero network, AC-9) and
/// tests inject a mock returning a fixed label, with NO real HTTP and NO key.
/// What: one async `classify` method mapping `raw_output` to an `ActivityKind`
/// (or an error string the caller logs and swallows — classification is
/// fail-open and must never break supervision). Implementors are `Send + Sync`
/// so they live behind `Arc<dyn ActivityClassifier>`.
/// Test: `openrouter_classifier_returns_label_on_gate_on`,
/// `actor_classifier_*` in `actor_tests.rs`.
#[async_trait]
pub trait ActivityClassifier: Send + Sync {
    /// Classify `raw_output` into an [`ActivityKind`].
    ///
    /// Why: the actor calls this only when the §8.3 gate is open AND the regex
    /// layer returned no confident match; it surfaces the label as an
    /// `ActivityParsed` event.
    /// What: returns `Ok(kind)` on success or `Err(reason)` on any failure; the
    /// caller MUST treat `Err` as fail-open (log + continue), never as fatal.
    /// Test: `openrouter_classifier_returns_label_on_gate_on`.
    async fn classify(&self, raw_output: &str) -> Result<ActivityKind, String>;
}

/// OpenRouter-backed [`ActivityClassifier`] (§8.3) reusing the SM `LlmProvider`.
///
/// Why: WI-5 must wire the REAL OpenRouter call without writing a second HTTP
/// client — the SM already vendors a cost/latency-aware `LlmProvider` over
/// OpenRouter's chat-completions endpoint (`core::sm::providers::OpenRouterProvider`).
/// Holding the provider behind `Arc<dyn LlmProvider>` reuses that client and
/// keeps this classifier testable against the same mock seam the SM uses.
/// What: builds a single-turn classification request, calls `provider.complete`,
/// and maps the returned text to an [`ActivityKind`] via [`label_to_activity_kind`].
/// Test: `openrouter_classifier_returns_label_on_gate_on`,
/// `openrouter_classifier_maps_unknown_to_none_err`.
pub struct OpenRouterClassifier {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl OpenRouterClassifier {
    /// Build a classifier over an already-resolved LLM provider.
    ///
    /// Why: the daemon resolves the provider once (from `OPENROUTER_API_KEY` +
    /// the configured model) and injects it; tests inject a mock provider.
    /// What: stores the `Arc<dyn LlmProvider>` and the bare model id used per call.
    /// Test: `openrouter_classifier_returns_label_on_gate_on`.
    pub fn new(provider: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }
}

#[async_trait]
impl ActivityClassifier for OpenRouterClassifier {
    async fn classify(&self, raw_output: &str) -> Result<ActivityKind, String> {
        let req = LlmRequest {
            model: self.model.clone(),
            system: CLASSIFIER_SYSTEM_PROMPT.to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: raw_output.to_string(),
            }],
            temperature: CLASSIFIER_TEMPERATURE,
            max_tokens: CLASSIFIER_MAX_TOKENS,
        };
        let resp = self
            .provider
            .complete(req)
            .await
            .map_err(|e| format!("openrouter classify failed: {e}"))?;
        match label_to_activity_kind(&resp.text) {
            Some(kind) => {
                debug!(label = %resp.text.trim(), "§8.3 classifier resolved a label");
                Ok(kind)
            }
            None => Err(format!(
                "classifier returned unmapped label {:?}",
                resp.text.trim()
            )),
        }
    }
}

/// Map a classifier label string to an [`ActivityKind`] (§8.3).
///
/// Why: the LLM replies with a single label keyword; the actor needs a typed
/// `ActivityKind` to emit `ActivityParsed`. Centralising the mapping keeps the
/// label vocabulary in one place alongside the system prompt that produces it.
/// What: trims + lowercases the label and matches the known keyword set,
/// returning `None` for an unrecognised or `unknown` label (the caller treats
/// `None` as a no-op — fail-open). Label variants that carry data
/// (`ToolUse`/`Error`/`TestResult`/`GitOp`) use empty/zero placeholders since
/// the single-label classifier does not extract structured sub-fields.
/// Test: `label_to_activity_kind_maps_known`, `label_to_activity_kind_unknown_none`.
pub fn label_to_activity_kind(label: &str) -> Option<ActivityKind> {
    match label.trim().to_ascii_lowercase().as_str() {
        "awaiting-input" | "awaiting_input" => Some(ActivityKind::AwaitingInput),
        "tool-use" | "tool_use" => Some(ActivityKind::ToolUse {
            name: String::new(),
        }),
        "error" => Some(ActivityKind::Error {
            excerpt: String::new(),
        }),
        "test-result" | "test_result" => Some(ActivityKind::TestResult {
            passed: 0,
            failed: 0,
        }),
        "git-op" | "git_op" => Some(ActivityKind::GitOp {
            verb: String::new(),
        }),
        "auth-prompt" | "auth_prompt" => Some(ActivityKind::AuthPrompt),
        "session-complete" | "session_complete" => Some(ActivityKind::SessionComplete),
        "rate-limited" | "rate_limited" => Some(ActivityKind::RateLimited),
        other => {
            // `unknown` and anything unrecognised → no confident label. The
            // caller logs and continues (fail-open, §8.3).
            if other != "unknown" {
                warn!(label = %other, "§8.3 classifier returned an unmapped label");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(
        config_enabled: bool,
        api_key_present: bool,
        sm_agent_connected: bool,
    ) -> ClassifierGate {
        ClassifierGate {
            config_enabled,
            api_key_present,
            sm_agent_connected,
        }
    }

    #[test]
    fn classifier_gate_all_off() {
        // Default path: classifier disabled, no key, no SM → never classify.
        assert!(!gate(false, false, false).should_classify());
    }

    #[test]
    fn classifier_gate_requires_all_three() {
        // Only when config-on AND key-present AND no-SM does the gate open.
        assert!(gate(true, true, false).should_classify());

        // Each single missing precondition closes the gate.
        assert!(
            !gate(false, true, false).should_classify(),
            "config off blocks"
        );
        assert!(!gate(true, false, false).should_classify(), "no key blocks");
    }

    #[test]
    fn classifier_gate_sm_connected_blocks() {
        // Even with config-on and key-present, an attached SM agent blocks the
        // classifier (the SM provides inference; §8.3 condition 3).
        assert!(!gate(true, true, true).should_classify());
    }

    #[test]
    fn classifier_gate_from_config() {
        let on = ObservabilityConfig {
            llm_classifier: true,
        };
        let g = ClassifierGate::from_config(&on, true, false);
        assert!(g.config_enabled);
        assert!(g.api_key_present);
        assert!(!g.sm_agent_connected);
        assert!(g.should_classify());

        let off = ObservabilityConfig::default();
        let g2 = ClassifierGate::from_config(&off, true, false);
        assert!(!g2.config_enabled);
        assert!(!g2.should_classify());
    }

    // ── §8.3 classifier label mapping + OpenRouter seam tests ──────────────

    use crate::core::sm::providers::{LlmProvider, LlmResponse, error::SmLlmError};
    use std::sync::Arc;

    /// A mock `LlmProvider` that returns a fixed text body without any network.
    ///
    /// Why: the §8.3 classifier must be testable with NO real HTTP and NO
    /// `OPENROUTER_API_KEY`; injecting this mock exercises the full
    /// `OpenRouterClassifier::classify` path against a controlled response.
    /// What: `complete` ignores the request and returns `text` (or an injected
    /// error) with zero token usage.
    /// Test: `openrouter_classifier_*`.
    struct MockProvider {
        reply: Result<String, SmLlmError>,
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _req: crate::core::sm::providers::LlmRequest,
        ) -> Result<LlmResponse, SmLlmError> {
            match &self.reply {
                Ok(text) => Ok(LlmResponse {
                    text: text.clone(),
                    model: "mock-model".into(),
                    input_tokens: 0,
                    output_tokens: 0,
                    latency_ms: 0,
                    cost_usd: 0.0,
                }),
                Err(_) => Err(SmLlmError::RateLimited),
            }
        }
    }

    #[test]
    fn label_to_activity_kind_maps_known() {
        assert_eq!(
            label_to_activity_kind("awaiting-input"),
            Some(ActivityKind::AwaitingInput)
        );
        assert_eq!(
            label_to_activity_kind("  AUTH-PROMPT \n"),
            Some(ActivityKind::AuthPrompt)
        );
        assert_eq!(
            label_to_activity_kind("rate_limited"),
            Some(ActivityKind::RateLimited)
        );
        assert!(matches!(
            label_to_activity_kind("tool-use"),
            Some(ActivityKind::ToolUse { .. })
        ));
    }

    #[test]
    fn label_to_activity_kind_unknown_none() {
        assert_eq!(label_to_activity_kind("unknown"), None);
        assert_eq!(label_to_activity_kind("definitely-not-a-label"), None);
        assert_eq!(label_to_activity_kind(""), None);
    }

    #[tokio::test]
    async fn openrouter_classifier_returns_label_on_gate_on() {
        // Gate-on path: a mock provider stands in for OpenRouter — NO network,
        // NO key. The classifier must map the returned label to an ActivityKind.
        let provider = Arc::new(MockProvider {
            reply: Ok("rate-limited".to_string()),
        });
        let classifier = OpenRouterClassifier::new(provider, "openai/gpt-4o-mini");
        let kind = classifier
            .classify("HTTP 429 Too Many Requests from the API")
            .await
            .expect("mock provider must classify");
        assert_eq!(kind, ActivityKind::RateLimited);
    }

    #[tokio::test]
    async fn openrouter_classifier_maps_unknown_to_err() {
        // An unmapped label is an Err so the actor treats it as a fail-open
        // no-op (it never emits a bogus ActivityParsed).
        let provider = Arc::new(MockProvider {
            reply: Ok("???".to_string()),
        });
        let classifier = OpenRouterClassifier::new(provider, "m");
        assert!(classifier.classify("weird output").await.is_err());
    }

    #[tokio::test]
    async fn openrouter_classifier_provider_error_is_err() {
        // A provider/transport error surfaces as Err (fail-open at the caller).
        let provider = Arc::new(MockProvider {
            reply: Err(SmLlmError::RateLimited),
        });
        let classifier = OpenRouterClassifier::new(provider, "m");
        assert!(classifier.classify("anything").await.is_err());
    }
}
