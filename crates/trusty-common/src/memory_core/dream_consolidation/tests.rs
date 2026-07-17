//! Tests for the tool-calling dream-consolidation pass (epic #2866).
//!
//! Why: The pass writes durable memory from untrusted model output; every
//! validation rule and every fail-open path needs direct coverage before the
//! feature can be enabled against a real palace.
//! What: Parse/validation unit tests, a scripted `ChatProvider` double, the
//! full mock-driven pass (summary + facts + tombstones), the recall-time
//! tombstone filter (default recall excludes, deep recall includes), and the
//! fail-open contract (disabled / no provider / provider error / no tool
//! call). No test touches the network; anything needing a live model would
//! be `#[ignore]` (none are needed for the PoC).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc::Sender;

use super::*;
use crate::ChatMessage;
use crate::chat::{ChatEvent, ChatProvider, ToolCall, ToolDef};
use crate::memory_core::dream::{DreamConfig, Dreamer};
use crate::memory_core::palace::{Palace, PalaceId, RoomType};
use crate::memory_core::retrieval::{
    PalaceHandle, recall, recall_deep, seed_shared_embedder_with_mock, shared_embedder,
};

// ─── Parse / validation unit tests ──────────────────────────────────────────

/// Why: lock the default config so the pass cannot silently ship enabled or
/// with a different model than Bob's 2026-07-16 decision.
#[test]
fn config_defaults_are_off_and_haiku() {
    let cfg = DreamConsolidationConfig::default();
    assert!(!cfg.enabled, "pass must be OFF by default");
    assert_eq!(cfg.model, "anthropic/claude-haiku-4-5");
    assert_eq!(cfg.max_batch_size, 8);
    assert_eq!(cfg.max_calls_per_cycle, 20);
}

/// Why: a well-formed payload must round-trip into the validated structs.
#[test]
fn parse_round_trips_valid_payload() {
    let raw = r#"{
        "summary": "  trusty-search is the hybrid search daemon. ",
        "inferences": ["both memories describe one daemon", "  "],
        "facts": [
            {"subject": "trusty-search", "predicate": "is-a", "object": "search daemon", "confidence": 0.9}
        ]
    }"#;
    let out = parse_emit_consolidation(raw).expect("valid payload parses");
    assert_eq!(out.summary, "trusty-search is the hybrid search daemon.");
    assert_eq!(out.inferences, vec!["both memories describe one daemon"]);
    assert_eq!(out.facts.len(), 1);
    assert_eq!(out.facts[0].subject, "trusty-search");
    assert_eq!(out.facts[0].predicate, "is-a");
    assert_eq!(out.facts[0].object, "search daemon");
    assert!((out.facts[0].confidence - 0.9).abs() < 1e-6);
    assert_eq!(out.facts_dropped, 0);
}

/// Why: malformed JSON must be a hard parse error, never a partial apply.
#[test]
fn parse_rejects_malformed_json() {
    let err = parse_emit_consolidation("{not json at all").expect_err("must fail");
    assert!(matches!(err, EmitParseError::Json(_)));
}

/// Why: `summary` is a required field; a payload without it must fail whole.
#[test]
fn parse_rejects_missing_summary() {
    let err = parse_emit_consolidation(r#"{"inferences": [], "facts": []}"#)
        .expect_err("missing summary must fail");
    assert!(matches!(err, EmitParseError::Json(_)));
}

/// Why: a fact missing its required `confidence` is a schema violation the
/// model made on the whole call — reject rather than guess.
#[test]
fn parse_rejects_fact_missing_confidence() {
    let raw = r#"{"summary": "s", "facts": [{"subject":"a","predicate":"b","object":"c"}]}"#;
    let err = parse_emit_consolidation(raw).expect_err("missing confidence must fail");
    assert!(matches!(err, EmitParseError::Json(_)));
}

/// Why: a whitespace-only summary has nothing to store; the cluster is
/// rejected outright.
#[test]
fn parse_rejects_blank_summary() {
    let err = parse_emit_consolidation(r#"{"summary": "   "}"#).expect_err("blank must fail");
    assert!(matches!(err, EmitParseError::EmptySummary));
}

/// Why: model confidences outside [0, 1] must be clamped, not rejected —
/// the fact itself is still usable.
#[test]
fn parse_clamps_out_of_range_confidence() {
    let raw = r#"{"summary": "s", "facts": [
        {"subject":"a","predicate":"p","object":"o","confidence": 1.7},
        {"subject":"x","predicate":"y","object":"z","confidence": -0.3}
    ]}"#;
    let out = parse_emit_consolidation(raw).expect("parses");
    assert_eq!(out.facts.len(), 2);
    assert_eq!(out.facts[0].confidence, 1.0);
    assert_eq!(out.facts[1].confidence, 0.0);
    assert_eq!(out.facts_dropped, 0);
}

/// Why: a triple with any empty part is unassertable; it must be dropped and
/// counted without poisoning the rest of the payload.
#[test]
fn parse_drops_empty_triple_parts() {
    let raw = r#"{"summary": "s", "facts": [
        {"subject":"", "predicate":"p", "object":"o", "confidence": 0.5},
        {"subject":"a", "predicate":"  ", "object":"o", "confidence": 0.5},
        {"subject":"a", "predicate":"p", "object":"", "confidence": 0.5},
        {"subject":"keep", "predicate":"this", "object":"one", "confidence": 0.5}
    ]}"#;
    let out = parse_emit_consolidation(raw).expect("parses");
    assert_eq!(out.facts.len(), 1);
    assert_eq!(out.facts[0].subject, "keep");
    assert_eq!(out.facts_dropped, 3);
}

/// Why: NaN / infinite confidence cannot be meaningfully clamped or stored.
#[test]
fn parse_drops_non_finite_confidence() {
    // JSON has no NaN literal; serde_json parses numbers only, so exercise
    // the guard through an out-of-f32-range double that becomes +inf.
    let raw = r#"{"summary": "s", "facts": [
        {"subject":"a","predicate":"p","object":"o","confidence": 1e39}
    ]}"#;
    let out = parse_emit_consolidation(raw).expect("parses");
    assert!(out.facts.is_empty(), "non-finite confidence fact dropped");
    assert_eq!(out.facts_dropped, 1);
}

/// Why: the tool definition is the model-facing contract; lock its shape.
#[test]
fn tool_def_has_expected_shape() {
    let tool = emit_consolidation_tool();
    assert_eq!(tool.name, EMIT_CONSOLIDATION_TOOL);
    let params = tool.parameters;
    assert_eq!(params["type"], "object");
    let required: Vec<&str> = params["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["summary", "inferences", "facts"]);
    let fact_required = &params["properties"]["facts"]["items"]["required"];
    assert!(fact_required.as_array().is_some_and(|a| a.len() == 4));
}

// ─── Scripted ChatProvider double ────────────────────────────────────────────

/// Test double: replays a fixed event script on every `chat_stream` call.
///
/// Why: the pass must be testable without any network; scripting the exact
/// event sequence (tool call / delta / error) exercises every collector
/// branch deterministically.
/// What: sends each scripted event into `tx`, then returns `Ok` or (when
/// `fail_stream`) an error, counting invocations in `calls`.
/// Test: used by every `pass_*` test below.
struct ScriptedProvider {
    events: Vec<ChatEvent>,
    calls: Arc<AtomicUsize>,
    fail_stream: bool,
}

impl ScriptedProvider {
    fn new(events: Vec<ChatEvent>) -> Self {
        Self {
            events,
            calls: Arc::new(AtomicUsize::new(0)),
            fail_stream: false,
        }
    }
}

#[async_trait::async_trait]
impl ChatProvider for ScriptedProvider {
    fn name(&self) -> &str {
        "scripted"
    }
    fn model(&self) -> &str {
        "mock-model"
    }
    async fn chat_stream(
        &self,
        _messages: Vec<ChatMessage>,
        _tools: Vec<ToolDef>,
        tx: Sender<ChatEvent>,
    ) -> anyhow::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        for ev in self.events.clone() {
            let _ = tx.send(ev).await;
        }
        if self.fail_stream {
            anyhow::bail!("scripted stream failure");
        }
        Ok(())
    }
}

fn emit_tool_call(arguments: &str) -> ChatEvent {
    ChatEvent::ToolCall(ToolCall {
        id: "call-1".to_string(),
        name: EMIT_CONSOLIDATION_TOOL.to_string(),
        arguments: arguments.to_string(),
    })
}

// ─── Palace / config test fixtures ───────────────────────────────────────────

async fn open_test_handle(name: &str) -> Arc<PalaceHandle> {
    // Pre-seed the process-wide embedder with MockEmbedder so no model
    // download is attempted (issue #850 precedent from dream::tests).
    seed_shared_embedder_with_mock();
    let dir = tempfile::tempdir().expect("tempdir");
    let palace = Palace {
        id: PalaceId::new(name),
        name: name.into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.path().join(name),
    };
    std::fs::create_dir_all(&palace.data_dir).expect("create palace dir");
    let handle = PalaceHandle::open(&palace).expect("open palace");
    // Keep the tempdir alive for the test's duration.
    std::mem::forget(dir);
    handle
}

fn enabled_config() -> DreamConfig {
    DreamConfig {
        consolidation: DreamConsolidationConfig {
            enabled: true,
            ..DreamConsolidationConfig::default()
        },
        ..DreamConfig::default()
    }
}

async fn seed_two_sources(handle: &Arc<PalaceHandle>) -> (uuid::Uuid, uuid::Uuid) {
    let a = handle
        .remember(
            "trusty-search is a hybrid BM25 search daemon".into(),
            RoomType::General,
            vec!["search".into()],
            0.7,
        )
        .await
        .expect("remember a");
    let b = handle
        .remember(
            "trusty-search also does semantic vector search".into(),
            RoomType::General,
            vec!["vector".into()],
            0.6,
        )
        .await
        .expect("remember b");
    (a, b)
}

const VALID_ARGS: &str = r#"{
    "summary": "trusty-search is the team's hybrid BM25 plus vector search daemon.",
    "inferences": ["both memories describe one search daemon"],
    "facts": [
        {"subject": "trusty-search", "predicate": "is-a", "object": "search daemon", "confidence": 0.9}
    ]
}"#;

// ─── Full pass: summary + facts + tombstones ─────────────────────────────────

/// Why: the core PoC claim — a tool call becomes a summary drawer, KG facts
/// with dream provenance, and `superseded_by` tombstones on the sources
/// (which are NOT deleted).
#[tokio::test]
async fn pass_stores_summary_facts_and_tombstones() {
    let handle = open_test_handle("dc-full").await;
    let (src_a, src_b) = seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider::new(vec![
        emit_tool_call(VALID_ARGS),
        ChatEvent::Done,
    ]));
    let calls = provider.calls.clone();

    let stats = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider)).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1, "one cluster, one call");
    assert_eq!(stats.clusters_processed, 1);
    assert_eq!(stats.llm_calls, 1);
    assert_eq!(stats.summaries_created, 1);
    assert_eq!(stats.inferences_recorded, 1);
    assert_eq!(stats.facts_asserted, 1);
    assert_eq!(stats.sources_tombstoned, 2);
    assert_eq!(stats.errors, 0);
    assert_eq!(stats.no_tool_call, 0);

    // Sources are NOT deleted: 2 sources + 1 summary = 3 drawers.
    assert_eq!(handle.drawers.read().len(), 3, "sources retained");

    // Summary drawer is tagged and folds inferences + source back-links.
    let summary = handle
        .drawers
        .read()
        .iter()
        .find(|d| d.tags.iter().any(|t| t == DREAM_SUMMARY_TAG))
        .cloned()
        .expect("summary drawer exists");
    assert!(summary.content.contains("hybrid BM25 plus vector"));
    assert!(summary.content.contains("Inferences:"));
    assert!(
        summary
            .content
            .contains("both memories describe one search daemon")
    );
    assert!(summary.content.contains(&src_a.to_string()));
    assert!(summary.content.contains(&src_b.to_string()));

    // Fact triple carries the dream provenance and the model confidence.
    let facts = handle
        .kg
        .query_active("trusty-search")
        .await
        .expect("query facts");
    let fact = facts
        .iter()
        .find(|t| t.predicate == "is-a")
        .expect("is-a fact asserted");
    assert_eq!(fact.object, "search daemon");
    assert!((fact.confidence - 0.9).abs() < 1e-6);
    assert_eq!(
        fact.provenance.as_deref(),
        Some(DREAM_CONSOLIDATION_PROVENANCE)
    );

    // Each source carries an active superseded_by tombstone → the summary.
    for src in [src_a, src_b] {
        let triples = handle
            .kg
            .query_active(&format!("drawer:{src}"))
            .await
            .expect("query tombstones");
        let ts = triples
            .iter()
            .find(|t| t.predicate == SUPERSEDED_BY_PREDICATE)
            .expect("tombstone exists");
        assert_eq!(ts.object, format!("drawer:{}", summary.id));
        assert_eq!(
            ts.provenance.as_deref(),
            Some(DREAM_CONSOLIDATION_PROVENANCE)
        );
        assert!(ts.valid_to.is_none(), "tombstone must be active");
    }
}

/// Why: the recall-time filter is the user-visible half of tombstoning —
/// default recall must hide archived sources while deep recall (the
/// include-archived escape hatch) still reaches them.
#[tokio::test]
async fn recall_excludes_tombstoned_sources() {
    let handle = open_test_handle("dc-recall").await;
    let (src_a, src_b) = seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider::new(vec![
        emit_tool_call(VALID_ARGS),
        ChatEvent::Done,
    ]));
    let stats = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider)).await;
    assert_eq!(stats.sources_tombstoned, 2, "both sources tombstoned");

    let embedder = shared_embedder().await.expect("mock embedder");

    // Default recall: tombstoned sources are excluded.
    let results = recall(&handle, embedder.as_ref(), "search daemon", 10)
        .await
        .expect("recall");
    let ids: Vec<uuid::Uuid> = results.iter().map(|r| r.drawer.id).collect();
    assert!(
        !ids.contains(&src_a),
        "archived source A hidden from recall"
    );
    assert!(
        !ids.contains(&src_b),
        "archived source B hidden from recall"
    );
    let summary_id = handle
        .drawers
        .read()
        .iter()
        .find(|d| d.tags.iter().any(|t| t == DREAM_SUMMARY_TAG))
        .map(|d| d.id)
        .expect("summary drawer");
    assert!(ids.contains(&summary_id), "summary surfaces in recall");

    // Deep recall (include-archived path): sources are still reachable.
    let deep = recall_deep(&handle, embedder.as_ref(), "search daemon", 10)
        .await
        .expect("recall_deep");
    let deep_ids: Vec<uuid::Uuid> = deep.iter().map(|r| r.drawer.id).collect();
    assert!(deep_ids.contains(&src_a), "deep recall reaches archived A");
    assert!(deep_ids.contains(&src_b), "deep recall reaches archived B");
}

/// Why: an already-archived drawer must never be re-clustered — the second
/// pass run sees only the summary drawer and produces a fresh cluster from
/// it, not from the tombstoned sources.
#[tokio::test]
async fn pass_excludes_archived_from_reclustering() {
    let handle = open_test_handle("dc-recluster").await;
    seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider::new(vec![
        emit_tool_call(VALID_ARGS),
        ChatEvent::Done,
    ]));
    let first = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider)).await;
    assert_eq!(first.sources_tombstoned, 2);

    // Second run: only the summary drawer is live → exactly one 1-drawer
    // cluster, and the tombstone count comes from that summary alone.
    let provider2 = Arc::new(ScriptedProvider::new(vec![ChatEvent::Done]));
    let second = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider2)).await;
    assert_eq!(
        second.clusters_processed, 1,
        "archived sources must not be re-clustered"
    );
    assert_eq!(second.no_tool_call, 1);
    assert_eq!(second.sources_tombstoned, 0);
}

// ─── Fail-open contract ──────────────────────────────────────────────────────

/// Why: with the pass disabled the provider must never be touched, even when
/// one is injected (mirrors the legacy semantic pass contract).
#[tokio::test]
async fn pass_disabled_is_noop() {
    let handle = open_test_handle("dc-disabled").await;
    seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider::new(vec![
        emit_tool_call(VALID_ARGS),
        ChatEvent::Done,
    ]));
    let calls = provider.calls.clone();

    // Default config: consolidation.enabled == false.
    let stats =
        dream_consolidation_pass(&handle, &DreamConfig::default(), None, Some(provider)).await;

    assert_eq!(stats, DreamConsolidationStats::default(), "all-zero stats");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "provider never invoked");
    assert_eq!(handle.drawers.read().len(), 2, "palace untouched");
}

/// Why: enabled-but-unconfigured (no key, no local model) must be a silent
/// no-op — the fail-open gate for daemons without any inference backend.
#[tokio::test]
async fn pass_without_provider_is_noop() {
    let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");
    let handle = open_test_handle("dc-noprovider").await;
    seed_two_sources(&handle).await;

    let config = DreamConfig {
        local_model_enabled: false,
        openrouter_api_key: String::new(),
        ..enabled_config()
    };
    let stats = dream_consolidation_pass(&handle, &config, None, None).await;

    assert_eq!(stats, DreamConsolidationStats::default(), "all-zero stats");
    assert_eq!(handle.drawers.read().len(), 2, "palace untouched");
}

/// Why: a provider stream error must be swallowed (counted, logged), never
/// propagated — and must leave the palace unmutated.
#[tokio::test]
async fn pass_swallows_provider_error() {
    let handle = open_test_handle("dc-error").await;
    seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider::new(vec![ChatEvent::Error(
        "upstream 500".to_string(),
    )]));
    let stats = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider)).await;

    assert_eq!(stats.errors, 1, "error counted");
    assert_eq!(stats.summaries_created, 0);
    assert_eq!(stats.sources_tombstoned, 0);
    assert_eq!(handle.drawers.read().len(), 2, "palace untouched");
}

/// Why: `chat_stream` returning `Err` (transport failure) is the other
/// provider failure shape; same swallow contract.
#[tokio::test]
async fn pass_swallows_stream_err_return() {
    let handle = open_test_handle("dc-streamerr").await;
    seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider {
        events: vec![],
        calls: Arc::new(AtomicUsize::new(0)),
        fail_stream: true,
    });
    let stats = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider)).await;

    assert_eq!(stats.errors, 1);
    assert_eq!(handle.drawers.read().len(), 2, "palace untouched");
}

/// Why: a model reply with no tool call is a defined no-op for the cluster —
/// logged at debug and counted, never an error.
#[tokio::test]
async fn pass_counts_no_tool_call_as_noop() {
    let handle = open_test_handle("dc-notool").await;
    seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider::new(vec![
        ChatEvent::Delta("I think these are related.".to_string()),
        ChatEvent::Done,
    ]));
    let stats = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider)).await;

    assert_eq!(stats.no_tool_call, 1);
    assert_eq!(stats.errors, 0);
    assert_eq!(stats.summaries_created, 0);
    assert_eq!(handle.drawers.read().len(), 2, "palace untouched");
}

/// Why: malformed tool-call arguments must skip the cluster without any
/// partial mutation.
#[tokio::test]
async fn pass_skips_cluster_on_malformed_arguments() {
    let handle = open_test_handle("dc-malformed").await;
    seed_two_sources(&handle).await;

    let provider = Arc::new(ScriptedProvider::new(vec![
        emit_tool_call("{definitely not json"),
        ChatEvent::Done,
    ]));
    let stats = dream_consolidation_pass(&handle, &enabled_config(), None, Some(provider)).await;

    assert_eq!(stats.errors, 1);
    assert_eq!(stats.summaries_created, 0);
    assert_eq!(stats.facts_asserted, 0);
    assert_eq!(stats.sources_tombstoned, 0);
    assert_eq!(handle.drawers.read().len(), 2, "palace untouched");
}

/// Why: the per-cycle call cap is the cost control; with batch size 1 and a
/// budget of 1, only one of three drawers may reach the model.
#[tokio::test]
async fn pass_respects_call_budget() {
    let handle = open_test_handle("dc-budget").await;
    seed_two_sources(&handle).await;
    handle
        .remember(
            "a third memory about the search daemon".into(),
            RoomType::General,
            vec![],
            0.5,
        )
        .await
        .expect("remember c");

    let provider = Arc::new(ScriptedProvider::new(vec![ChatEvent::Done]));
    let calls = provider.calls.clone();
    let config = DreamConfig {
        consolidation: DreamConsolidationConfig {
            enabled: true,
            max_batch_size: 1,
            max_calls_per_cycle: 1,
            ..DreamConsolidationConfig::default()
        },
        ..DreamConfig::default()
    };
    let stats = dream_consolidation_pass(&handle, &config, None, Some(provider)).await;

    assert_eq!(stats.llm_calls, 1, "budget capped at 1 call");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Why: the dream cycle must complete exactly as today when the pass is
/// enabled but no provider is available — the fail-open contract at the
/// cycle level (never error, never wedge).
#[tokio::test]
async fn dream_cycle_completes_with_pass_enabled_no_provider() {
    let _guard = EnvVarGuard::remove("OPENROUTER_API_KEY");
    let handle = open_test_handle("dc-cycle").await;
    seed_two_sources(&handle).await;

    let dreamer = Dreamer::new(DreamConfig {
        local_model_enabled: false,
        openrouter_api_key: String::new(),
        ..enabled_config()
    });
    let stats = dreamer
        .dream_cycle(&handle)
        .await
        .expect("cycle completes despite enabled pass with no provider");

    assert_eq!(
        stats.llm_consolidation,
        DreamConsolidationStats::default(),
        "pass no-oped"
    );
}

// ─── RAII env-var guard (mirrors dream::tests) ───────────────────────────────

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        // Safety: test-only; #[tokio::test] runs on a current-thread runtime.
        unsafe { std::env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // Safety: test-only; single-threaded test execution.
        match &self.previous {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
