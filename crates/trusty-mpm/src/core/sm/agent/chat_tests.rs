//! Acceptance tests for the SM chat turn (`SessionManagerAgent::chat`, SM-7).
//!
//! Why: SM-7's acceptance criteria are that the chat turn drives the FULL §7.5
//! assembly through a provider, records the round, and returns reply + cost —
//! and that the no-provider case degrades gracefully. These tests pin every one
//! of those behaviours deterministically with a mock resolver/provider and a
//! tempdir, with NO network and NO real model.
//! What: builds an agent via `with_runtime` (feature-aware) over a
//! [`MockResolver`], drives `chat`, and asserts the captured request + outcome.
//! Test: this is the test module.

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use super::super::mock::{CompleteGate, MockChatProvider, MockResolver};
use crate::core::sm::SmContextEngine;
use crate::core::sm::agent::{SessionManagerAgent, SmAgentError};
use crate::core::sm::config::SessionManagerConfig;

/// Build an enabled SM config with a tiny rolling window so compaction is easy
/// to trigger in tests.
fn enabled_config() -> SessionManagerConfig {
    SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    }
}

/// Build an agent over the mock resolver, feature-aware (no memory in tests).
///
/// Why: the `with_runtime` signature differs by the `sm-memory` feature (the
/// feature build takes an extra `Option<SmMemory>`); this helper hides that so
/// the tests read the same in both builds. Tests never wire a real palace —
/// recall coverage with a palace is exercised separately under the feature.
fn agent_with(
    cfg: SessionManagerConfig,
    resolver: Arc<MockResolver>,
    data_root: &std::path::Path,
) -> SessionManagerAgent {
    #[cfg(feature = "sm-memory")]
    {
        SessionManagerAgent::with_runtime(cfg, resolver, data_root.to_path_buf(), None)
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        SessionManagerAgent::with_runtime(cfg, resolver, data_root.to_path_buf())
    }
}

/// Why: the headline acceptance — an enabled agent with a (mock) provider drives
/// the full §7.5 turn: the assembled prompt carries the SM system prompt, the
/// current message reaches the model as the final user turn, and the reply +
/// cost come back.
/// What: builds the agent, calls `chat`, asserts the mock saw the system prompt
/// and the message, and that the outcome carries the reply + cost + a conv_id.
/// Test: this is the test.
#[tokio::test]
async fn chat_drives_full_turn_with_mock_provider() {
    let tmp = TempDir::new().unwrap();
    let provider = MockChatProvider::new("here is my plan", 0.0042);
    let resolver = Arc::new(MockResolver::with_provider(provider.clone()));
    let agent = agent_with(enabled_config(), resolver, tmp.path());

    let outcome = agent
        .chat("decompose the login feature", Some("conv-1"))
        .await
        .expect("chat turn succeeds with a provider");

    assert_eq!(outcome.reply, "here is my plan");
    assert_eq!(outcome.conv_id, "conv-1");
    assert!(
        (outcome.cost_usd - 0.0042).abs() < 1e-9,
        "per-call cost returned"
    );

    // The provider received the §7.5 assembly.
    let req = provider.last_request().expect("provider was called");
    // (1) The SM system prompt is delivered out-of-band in `system`.
    assert!(
        req.system.contains("# Session Manager (SM) -- trusty-mpm"),
        "system message must include the SM system prompt"
    );
    assert!(
        req.system.contains("# BASE_SM Framework Floor"),
        "system message must include the non-overridable floor"
    );
    // (5) The current operator message is the final user turn.
    let last = req.messages.last().expect("at least the current message");
    assert_eq!(last.role, "user");
    assert_eq!(last.content, "decompose the login feature");
    // Temperature comes from config (default 0.3).
    assert!((req.temperature - 0.3).abs() < 1e-6);
}

/// Why: §7.5 step 2 — a prior round's content must appear as compressed context
/// or as a verbatim recent round on the NEXT turn, proving the context engine is
/// driving the assembly (not a stateless pass-through).
/// What: runs two turns on the same conv_id; asserts the second turn's request
/// carries the first turn's user message as a recent round.
/// Test: this is the test.
#[tokio::test]
async fn chat_carries_prior_round_into_next_turn() {
    let tmp = TempDir::new().unwrap();
    let provider = MockChatProvider::new("ack", 0.0);
    let resolver = Arc::new(MockResolver::with_provider(provider.clone()));
    let agent = agent_with(enabled_config(), resolver, tmp.path());

    agent.chat("first message", Some("c")).await.unwrap();
    agent.chat("second message", Some("c")).await.unwrap();

    let req = provider.last_request().unwrap();
    let contents: Vec<&str> = req.messages.iter().map(|m| m.content.as_str()).collect();
    assert!(
        contents.iter().any(|c| c.contains("first message")),
        "the prior round must be carried into the next turn's prompt, got {contents:?}"
    );
    // And the current message is still last.
    assert_eq!(req.messages.last().unwrap().content, "second message");
}

/// Why: the round must be RECORDED — a fresh agent over the same data_root +
/// conv_id must resume the prior round, proving `record` persisted it (§7.4).
/// What: turn 1 with agent A; build agent B over the same dir; turn 2 on B must
/// see turn-1 content.
/// Test: this is the test.
#[tokio::test]
async fn chat_records_round_to_persistent_store() {
    let tmp = TempDir::new().unwrap();
    let provider_a = MockChatProvider::new("a-reply", 0.0);
    let agent_a = agent_with(
        enabled_config(),
        Arc::new(MockResolver::with_provider(provider_a)),
        tmp.path(),
    );
    agent_a
        .chat("remember this", Some("persist"))
        .await
        .unwrap();

    // A new agent over the SAME data root resumes the persisted conversation.
    let provider_b = MockChatProvider::new("b-reply", 0.0);
    let provider_b_handle = provider_b.clone();
    let agent_b = agent_with(
        enabled_config(),
        Arc::new(MockResolver::with_provider(provider_b)),
        tmp.path(),
    );
    agent_b.chat("follow up", Some("persist")).await.unwrap();

    let req = provider_b_handle.last_request().unwrap();
    let joined: String = req
        .messages
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("remember this"),
        "persisted round must be resumed by a fresh agent, got: {joined}"
    );
}

/// Why: degraded mode (§5.3) — with no provider configured, `chat` returns a
/// graceful [`SmAgentError::Degraded`] carrying the DOC-13 "no inference" notice,
/// which the endpoint maps to the 503 fallback.
/// What: builds an agent over a degraded resolver and asserts the error variant
/// + message.
/// Test: this is the test.
#[tokio::test]
async fn chat_without_provider_is_degraded() {
    let tmp = TempDir::new().unwrap();
    let agent = agent_with(
        enabled_config(),
        Arc::new(MockResolver::degraded()),
        tmp.path(),
    );

    let err = agent.chat("hello", Some("c")).await.unwrap_err();
    match err {
        SmAgentError::Degraded(msg) => {
            assert!(msg.contains("no inference provider configured"));
        }
        other => panic!("expected Degraded, got {other:?}"),
    }
}

/// Why: the inert SM-1 agent (built via `new`, no runtime) must also degrade —
/// there is no provider — so the endpoint routes it straight to fallback.
/// What: builds via `new`, asserts `chat` returns `Degraded`.
/// Test: this is the test.
#[tokio::test]
async fn chat_without_runtime_is_degraded() {
    let agent = SessionManagerAgent::new(enabled_config());
    let err = agent.chat("hello", None).await.unwrap_err();
    assert!(matches!(err, SmAgentError::Degraded(_)));
}

/// Why: a non-degraded resolution failure (e.g. unknown provider) is a REAL
/// error, not a graceful notice — it must surface as [`SmAgentError::Inference`]
/// so the endpoint reports a genuine failure rather than the degraded notice.
/// What: builds an agent over a validation-error resolver; asserts the variant.
/// Test: this is the test.
#[tokio::test]
async fn chat_resolution_error_is_inference_error() {
    let tmp = TempDir::new().unwrap();
    let agent = agent_with(
        enabled_config(),
        Arc::new(MockResolver::validation()),
        tmp.path(),
    );
    let err = agent.chat("hello", Some("c")).await.unwrap_err();
    assert!(matches!(err, SmAgentError::Inference(_)));
}

/// Why: a `None`/empty conv_id must mint a fresh, non-empty conversation id so
/// the caller can continue the same rolling context on follow-ups.
/// What: calls `chat` with `None`, asserts the returned conv_id is non-empty.
/// Test: this is the test.
#[tokio::test]
async fn chat_mints_conv_id_when_absent() {
    let tmp = TempDir::new().unwrap();
    let agent = agent_with(
        enabled_config(),
        Arc::new(MockResolver::with_provider(MockChatProvider::new(
            "ok", 0.0,
        ))),
        tmp.path(),
    );
    let outcome = agent.chat("hi", None).await.unwrap();
    assert!(
        !outcome.conv_id.trim().is_empty(),
        "a conv_id must be minted"
    );
}

/// Why: the chat turn must work WITHOUT the `sm-memory` feature (recall skipped
/// gracefully). This compiles and runs in both builds: under no-feature it
/// proves the no-recall path; under the feature it proves a None-memory runtime
/// still works.
/// What: drives a turn and asserts success and that the system message contains
/// no "Relevant memory:" block (no recall wired in this test).
/// Test: this is the test.
#[tokio::test]
async fn chat_works_without_memory_recall() {
    let tmp = TempDir::new().unwrap();
    let provider = MockChatProvider::new("ok", 0.0);
    let resolver = Arc::new(MockResolver::with_provider(provider.clone()));
    let agent = agent_with(enabled_config(), resolver, tmp.path());

    let outcome = agent.chat("no recall here", Some("c")).await.unwrap();
    assert_eq!(outcome.reply, "ok");
    let req = provider.last_request().unwrap();
    assert!(
        !req.system.contains("Relevant memory:"),
        "no recall is wired, so no memory block should appear"
    );
}

/// Why: FINDING 1 (data integrity) — a chat turn whose reply was produced
/// (orchestration resolved) but whose compaction path can resolve NO provider for
/// EITHER tier must STILL persist the round verbatim. Dropping it would diverge the
/// stored conversation from the reply the caller already saw.
/// What: a resolver that succeeds for the first resolution (the orchestration
/// reply) and degrades for every later one (the compaction + fallback-orchestration
/// resolutions in `record_round`). The turn must return the reply; a fresh agent
/// over the same data root must then resume the round verbatim, proving it was
/// persisted via the no-compaction path.
/// Test: this is the test.
#[tokio::test]
async fn chat_records_round_when_no_provider_for_compaction() {
    let tmp = TempDir::new().unwrap();
    let provider = MockChatProvider::new("the plan", 0.0);
    // Only the first resolution (the orchestration reply) succeeds; the compaction
    // tier and its orchestration fallback both degrade.
    let resolver = Arc::new(MockResolver::provider_then_degraded(provider, 1));
    let agent = agent_with(enabled_config(), resolver, tmp.path());

    let outcome = agent
        .chat("decompose this", Some("conv-int"))
        .await
        .expect("turn succeeds: the reply was produced");
    assert_eq!(outcome.reply, "the plan");

    // The round must have been persisted VERBATIM despite no compaction provider.
    // A fresh agent (with a working provider) over the same data root resumes it.
    let provider_b = MockChatProvider::new("ack", 0.0);
    let provider_b_handle = provider_b.clone();
    let agent_b = agent_with(
        enabled_config(),
        Arc::new(MockResolver::with_provider(provider_b)),
        tmp.path(),
    );
    agent_b
        .chat("next", Some("conv-int"))
        .await
        .expect("follow-up turn");

    let req = provider_b_handle.last_request().expect("provider called");
    let joined: String = req
        .messages
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("decompose this"),
        "the round must be recorded verbatim even when compaction has no provider, got: {joined}"
    );
}

/// Why: #1309 (data integrity) — two concurrent chat turns for the SAME conv_id
/// must NOT lose a round. The context engine opens fresh per turn and persists by
/// atomic whole-file replace with no merge, so before the per-conv_id serialization
/// both turns would load the same state, append their round, and save — the second
/// save clobbering the first (total_rounds == 1, one message lost). This test
/// reliably reproduces that interleave and asserts BOTH rounds survive post-fix.
/// What: one shared agent (its conv_locks are shared across the two spawned
/// clones). A gate parks turn A inside its provider call — so A has OPENED the
/// engine and is mid-turn — while turn B runs; the paused clock lets B settle
/// (pre-fix it saves and clobbers; post-fix it blocks on the per-conv lock) before
/// A is released. After both finish, a fresh engine over the same data root must
/// show `total_rounds == 2` with BOTH messages persisted.
/// Test: this is the test (the #1309 regression guard).
#[tokio::test(start_paused = true)]
async fn concurrent_turns_same_conv_id_persist_both_rounds() {
    let tmp = TempDir::new().unwrap();
    let gate = CompleteGate::new();
    // One shared provider (gate + request log shared via Arc across clones) so the
    // FIRST complete() — turn A's orchestration call — parks on the gate.
    let provider = MockChatProvider::new("reply", 0.0).gated(gate.clone());
    let resolver = Arc::new(MockResolver::with_provider(provider));
    let agent = agent_with(enabled_config(), resolver, tmp.path());

    // Two concurrent turns on the SAME conv_id. Clones share the agent's conv_locks
    // (an Arc), mirroring the daemon's single shared `Arc<SessionManagerAgent>`.
    let a = {
        let ag = agent.clone();
        tokio::spawn(async move { ag.chat("turn A", Some("race")).await })
    };
    let b = {
        let ag = agent.clone();
        tokio::spawn(async move { ag.chat("turn B", Some("race")).await })
    };

    // Let both spawned turns run until they settle: A parks at its gated provider
    // call (having opened the engine), B either finishes (pre-fix, no lock) or
    // blocks on the per-conv lock (post-fix). With the paused clock, this sleep
    // returns only once the runtime is otherwise idle, so both have settled.
    tokio::time::sleep(Duration::from_secs(1)).await;
    // Release A's provider so A records + saves; post-fix, B then proceeds.
    gate.release();

    let ra = a.await.unwrap().expect("turn A succeeds");
    let rb = b.await.unwrap().expect("turn B succeeds");
    assert_eq!(ra.conv_id, "race");
    assert_eq!(rb.conv_id, "race");

    // Both rounds must be persisted. Pre-fix the second save clobbers the first
    // (total_rounds == 1, one message lost); post-fix both survive.
    let cfg = enabled_config();
    let engine =
        SmContextEngine::open("race", tmp.path(), &cfg.inference, &cfg.rounds).expect("reopen");
    let conv = engine.conversation();
    assert_eq!(
        conv.total_rounds, 2,
        "both concurrent same-conv_id turns must persist their round (no loss)"
    );
    let users: Vec<&str> = conv.recent_rounds.iter().map(|r| r.user.as_str()).collect();
    assert!(
        users.contains(&"turn A") && users.contains(&"turn B"),
        "both turns' rounds must survive, got {users:?}"
    );
}

/// Why: the per-conv_id serialization must NOT over-serialize — turns for
/// DIFFERENT conv_ids must run concurrently (distinct locks), or the fix would
/// throttle unrelated conversations to one-at-a-time.
/// What: gates turn A (conv "x") inside its provider call; turn B (conv "y") must
/// still complete WITHOUT waiting on A's lock (a different id → a different lock).
/// After releasing A, both persist their own single round.
/// Test: this is the test.
#[tokio::test(start_paused = true)]
async fn different_conv_ids_do_not_serialize() {
    let tmp = TempDir::new().unwrap();
    let gate = CompleteGate::new();
    let provider = MockChatProvider::new("reply", 0.0).gated(gate.clone());
    let resolver = Arc::new(MockResolver::with_provider(provider));
    let agent = agent_with(enabled_config(), resolver, tmp.path());

    let a = {
        let ag = agent.clone();
        tokio::spawn(async move { ag.chat("for x", Some("x")).await })
    };
    let b = {
        let ag = agent.clone();
        tokio::spawn(async move { ag.chat("for y", Some("y")).await })
    };

    // A parks on the gate (conv "x"); B (conv "y") must finish independently since
    // it holds a DIFFERENT lock. If the fix over-serialized on a single global
    // lock, B would block behind A here and this turn would never settle.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let rb = b
        .await
        .unwrap()
        .expect("turn B (conv y) finishes without waiting on A");
    assert_eq!(rb.conv_id, "y");

    // Now release A and let it finish too.
    gate.release();
    let ra = a.await.unwrap().expect("turn A (conv x) succeeds");
    assert_eq!(ra.conv_id, "x");

    let cfg = enabled_config();
    for (id, msg) in [("x", "for x"), ("y", "for y")] {
        let engine =
            SmContextEngine::open(id, tmp.path(), &cfg.inference, &cfg.rounds).expect("reopen");
        let conv = engine.conversation();
        assert_eq!(conv.total_rounds, 1, "conv {id} persists its single round");
        assert_eq!(conv.recent_rounds.back().unwrap().user, msg);
    }
}

/// Why: §7.5 step 3 — when the SM memory palace IS wired (feature build) and
/// holds a relevant fact, the chat turn must inject it into the working prompt as
/// the "Relevant memory:" block. This proves the SM-4 recall is actually
/// composed into the SM-5 assembly by the SM-7 turn.
/// What: opens a real `SmMemory` over a tempdir (mock embedder, no ONNX),
/// remembers a distinctive fact, builds an agent wired with that memory, drives a
/// turn whose message matches the fact, and asserts the provider's system message
/// carries the recalled content.
/// Test: this is the test (feature-gated).
#[cfg(feature = "sm-memory")]
#[tokio::test]
async fn chat_includes_recall_when_memory_present() {
    use crate::core::sm::config::SmMemoryConfig;
    use crate::core::sm::memory::SmMemory;
    use trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock;

    seed_shared_embedder_with_mock();
    let tmp = TempDir::new().unwrap();
    let mem = SmMemory::open(tmp.path().join("palace"), &SmMemoryConfig::default())
        .expect("open SM memory");
    mem.remember("project trusty-tools requires SKIP_UI_BUILD=1 for cargo publish")
        .await
        .expect("remember a fact");

    let provider = MockChatProvider::new("noted", 0.0);
    let resolver = Arc::new(MockResolver::with_provider(provider.clone()));
    let agent = SessionManagerAgent::with_runtime(
        enabled_config(),
        resolver,
        tmp.path().to_path_buf(),
        Some(mem),
    );

    agent
        .chat("how do I cargo publish trusty-tools?", Some("c"))
        .await
        .expect("chat with recall succeeds");

    let req = provider.last_request().unwrap();
    assert!(
        req.system.contains("Relevant memory:"),
        "a recall block must be injected when memory holds a relevant fact"
    );
    assert!(
        req.system.contains("SKIP_UI_BUILD"),
        "the recalled fact content must appear in the working prompt, got: {}",
        req.system
    );
}
