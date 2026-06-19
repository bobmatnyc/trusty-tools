//! Acceptance tests for the action-capable SM chat loop (#1283).
//!
//! Why: Phase 2's headline behaviours must be pinned deterministically with NO
//! network and NO real session: a turn where the model calls one verb then
//! answers must actually execute the verb (the mock control records it), list it
//! in `actions_taken`, and return the final text; the max-iteration guard must
//! terminate a model that always calls a verb; plain prose must be the final
//! answer with no verb executed; and a degraded provider must surface the
//! degraded error. These tests use a SCRIPTED mock provider (a per-call reply
//! queue) + the recording `MockSessionControl` + the `MockResolver`.
//! What: drives [`SessionManagerAgent::chat_with_actions`] over a tempdir and
//! asserts the outcome + the mock control's recorded calls.
//! Test: this is the test module.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tempfile::TempDir;

use crate::core::sm::agent::SessionManagerAgent;
use crate::core::sm::agent::delegate::mock_control::MockSessionControl;
use crate::core::sm::config::{SessionManagerConfig, SmInferenceConfig};
use crate::core::sm::control::SessionControl;
use crate::core::sm::providers::{
    LlmProvider, LlmRequest, LlmResponse, ProviderKind, ResolvedCall, SmLlmError, SmModelTier,
    TierResolver, resolve_tier_model,
};

/// A scripted, recording mock provider: returns a queued reply per `complete`.
///
/// Why: the action loop calls the provider once per iteration; a test must script
/// a DIFFERENT reply per call (e.g. a verb-call, then a final answer) and assert
/// how many calls happened. `MockChatProvider` only returns a fixed reply, so the
/// loop tests need this per-call queue.
/// What: holds a `VecDeque` of canned reply texts; each `complete` pops the front
/// (or repeats the last when exhausted, so an "always verb" script never starves
/// the max-iter guard). Records the call count.
struct ScriptedProvider {
    replies: Mutex<VecDeque<String>>,
    last: Mutex<String>,
    calls: Mutex<usize>,
}

impl ScriptedProvider {
    fn new(replies: impl IntoIterator<Item = &'static str>) -> Self {
        let q: VecDeque<String> = replies.into_iter().map(str::to_string).collect();
        let last = q.back().cloned().unwrap_or_default();
        Self {
            replies: Mutex::new(q),
            last: Mutex::new(last),
            calls: Mutex::new(0),
        }
    }

    fn call_count(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        *self.calls.lock().unwrap() += 1;
        let text = {
            let mut q = self.replies.lock().unwrap();
            match q.pop_front() {
                Some(t) => {
                    *self.last.lock().unwrap() = t.clone();
                    t
                }
                None => self.last.lock().unwrap().clone(),
            }
        };
        Ok(LlmResponse {
            text,
            model: req.model,
            input_tokens: 1,
            output_tokens: 1,
            latency_ms: 1,
            cost_usd: 0.001,
        })
    }
}

/// A resolver that hands back a shared [`ScriptedProvider`] for every tier.
struct ScriptedResolver {
    provider: Arc<ScriptedProvider>,
}

#[async_trait]
impl TierResolver for ScriptedResolver {
    async fn resolve(
        &self,
        cfg: &SmInferenceConfig,
        tier: SmModelTier,
    ) -> Result<ResolvedCall, SmLlmError> {
        let model = resolve_tier_model(cfg, tier)?;
        Ok(ResolvedCall {
            provider: self.provider.clone(),
            model,
            kind: ProviderKind::Anthropic,
        })
    }
}

/// A resolver that always reports degraded (no provider).
struct DegradedResolver;

#[async_trait]
impl TierResolver for DegradedResolver {
    async fn resolve(
        &self,
        _cfg: &SmInferenceConfig,
        _tier: SmModelTier,
    ) -> Result<ResolvedCall, SmLlmError> {
        Err(SmLlmError::Degraded("mock: no provider".to_string()))
    }
}

fn enabled_config() -> SessionManagerConfig {
    SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    }
}

/// Build an action-capable agent over a scripted provider + a tempdir.
fn agent_with(
    replies: impl IntoIterator<Item = &'static str>,
    data_root: &std::path::Path,
) -> (SessionManagerAgent, Arc<ScriptedProvider>) {
    let provider = Arc::new(ScriptedProvider::new(replies));
    let resolver = Arc::new(ScriptedResolver {
        provider: provider.clone(),
    });
    let agent = SessionManagerAgent::for_test(enabled_config(), resolver, data_root.to_path_buf());
    (agent, provider)
}

/// Why: the headline acceptance — the model calls one verb (`sessions.list`),
/// then answers; the verb must actually execute (mock records the call),
/// `actions_taken` must list it, and `reply` must be the final text.
/// What: scripts a verb-call then a final answer; asserts all three.
/// Test: this is the test.
#[tokio::test]
async fn loop_executes_verb_then_answers() {
    let tmp = TempDir::new().unwrap();
    let (agent, provider) = agent_with(
        [
            r#"{"action":"sessions.list","args":{}}"#,
            r#"{"action":"final","message":"you have no sessions"}"#,
        ],
        tmp.path(),
    );
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());

    let outcome = agent
        .chat_with_actions("show my sessions", Some("c1"), &control)
        .await
        .expect("action turn succeeds");

    assert_eq!(outcome.reply, "you have no sessions");
    assert_eq!(outcome.conv_id, "c1");
    assert_eq!(outcome.actions_taken, vec!["sessions.list".to_string()]);
    // Two provider calls: one to decide the verb, one to produce the final answer.
    assert_eq!(provider.call_count(), 2);
    // Cost is summed across both calls.
    assert!((outcome.cost_usd - 0.002).abs() < 1e-9);
}

/// Why: a `sessions.send` verb must reach the control with its id + text, and the
/// audit label must carry the session id so the operator sees the target.
/// What: scripts a send then a final; asserts the mock recorded the send and the
/// label includes the id.
/// Test: this is the test.
#[tokio::test]
async fn loop_send_records_target_and_label() {
    let tmp = TempDir::new().unwrap();
    let (agent, _p) = agent_with(
        [
            r#"{"action":"sessions.send","args":{"session_id":"abc123","text":"go"}}"#,
            r#"{"action":"final","message":"sent"}"#,
        ],
        tmp.path(),
    );
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();

    let outcome = agent
        .chat_with_actions("nudge it", None, &control)
        .await
        .expect("action turn succeeds");

    assert_eq!(mock.sends(), vec![("abc123".to_string(), "go".to_string())]);
    assert_eq!(
        outcome.actions_taken,
        vec!["sessions.send abc123".to_string()]
    );
}

/// Why: the max-iteration guard must terminate a model that ALWAYS emits a verb-
/// call — the loop must not spin unbounded inside the HTTP handler.
/// What: scripts a single always-repeated verb-call (the queue repeats its last);
/// asserts the loop terminates with a non-empty reply and a bounded call count.
/// Test: this is the test.
#[tokio::test]
async fn max_iterations_guard_terminates() {
    let tmp = TempDir::new().unwrap();
    // Only ever a verb-call: the ScriptedProvider repeats it forever.
    let (agent, provider) = agent_with([r#"{"action":"sessions.list","args":{}}"#], tmp.path());
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());

    let outcome = agent
        .chat_with_actions("loop please", Some("c2"), &control)
        .await
        .expect("guard terminates the loop");

    // Bounded by MAX_ACTION_ITERATIONS (8): never unbounded.
    assert!(
        provider.call_count() <= 8,
        "call count {} exceeded the guard",
        provider.call_count()
    );
    assert!(!outcome.reply.is_empty(), "guard must yield a final reply");
}

/// Why: plain prose (no action JSON) must be treated as the final answer with NO
/// verb executed — the always-safe terminal.
/// What: scripts a prose reply; asserts no verb ran and the reply is the prose.
/// Test: this is the test.
#[tokio::test]
async fn prose_reply_is_final_no_verb() {
    let tmp = TempDir::new().unwrap();
    let (agent, provider) = agent_with(["Everything is healthy, nothing to do."], tmp.path());
    let mock = Arc::new(MockSessionControl::default());
    let control: Arc<dyn SessionControl> = mock.clone();

    let outcome = agent
        .chat_with_actions("status?", Some("c3"), &control)
        .await
        .expect("prose answer succeeds");

    assert_eq!(outcome.reply, "Everything is healthy, nothing to do.");
    assert!(outcome.actions_taken.is_empty(), "no verb should run");
    assert!(mock.sends().is_empty());
    assert_eq!(provider.call_count(), 1);
}

/// Why: a degraded provider (no credentials) must surface as the degraded error
/// so the endpoint can map it to a 503.
/// What: builds an agent over a degraded resolver; asserts `Degraded`.
/// Test: this is the test.
#[tokio::test]
async fn degraded_provider_surfaces_degraded() {
    use crate::core::sm::SmAgentError;
    let tmp = TempDir::new().unwrap();
    let resolver = Arc::new(DegradedResolver);
    let agent = SessionManagerAgent::for_test(enabled_config(), resolver, tmp.path().to_path_buf());
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());

    let err = agent
        .chat_with_actions("do a thing", None, &control)
        .await
        .expect_err("degraded provider errors");
    assert!(matches!(err, SmAgentError::Degraded(_)));
}

/// Why: the inert agent (no runtime) must report degraded immediately — there is
/// no provider.
/// What: builds via `new` and asserts `chat_with_actions` is degraded.
/// Test: this is the test.
#[tokio::test]
async fn inert_agent_is_degraded() {
    use crate::core::sm::SmAgentError;
    let agent = SessionManagerAgent::new(enabled_config());
    let control: Arc<dyn SessionControl> = Arc::new(MockSessionControl::default());
    let err = agent
        .chat_with_actions("hi", None, &control)
        .await
        .expect_err("inert agent is degraded");
    assert!(matches!(err, SmAgentError::Degraded(_)));
}
