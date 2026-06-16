//! Acceptance + unit tests for the §7.2 trigger and §7.5 assembly.
//!
//! Why: these are the SM-5 acceptance criteria — window eviction at >10,
//! evicted-content survival into the compressed block, the goal/session-id
//! golden, the token-budget safety valve, Haiku-default vs. override model
//! selection, per-record atomic persistence round-trip, and the exact §7.5
//! assembly order. All run deterministically: an injected mock provider (no real
//! LLM), injected timestamps (no wall clock), and a tempdir (no real home).
//! What: drives [`SmContextEngine`] with [`MockProvider`] against a tempdir.
//! Test: this is the test module.

use super::*;
use crate::core::sm::config::{SmInferenceConfig, SmRoundsConfig};
use crate::core::sm::context::compaction::COMPRESS_SUMMARY_SYSTEM_PROMPT;
use crate::core::sm::context::mock_provider::MockProvider;
use crate::core::sm::context::model::ToolTrace;
use chrono::{DateTime, TimeZone, Utc};
use tempfile::{TempDir, tempdir};

/// A deterministic timestamp offset by `n` seconds from a fixed base.
///
/// Why: rounds need distinct, reproducible timestamps without reading the clock.
/// What: returns `2026-01-01T00:00:00Z + n seconds`.
/// Test: used by the fixtures below.
fn ts(n: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("base ts")
        + chrono::Duration::seconds(n)
}

/// Default-ish inference config with small, test-friendly budgets unless a test
/// overrides them.
///
/// Why: the real defaults (24k budget) would never fire the token trigger in a
/// unit test; most tests want the round-count path with a generous token budget
/// and a generous compressed cap so re-summarisation doesn't interfere.
/// What: returns an [`SmInferenceConfig`] with a large token budget + compressed
/// cap; callers mutate fields for the budget/override tests.
/// Test: used by the fixtures below.
fn inference() -> SmInferenceConfig {
    SmInferenceConfig {
        // Effectively disable the safety valve + re-summarisation so the
        // round-count path is what most tests exercise; budget tests override.
        context_token_budget: 1_000_000,
        compressed_context_max_tokens: 1_000_000,
        ..SmInferenceConfig::default()
    }
}

/// Build an engine over a fresh tempdir; returns both so the dir outlives it.
///
/// Why: the engine borrows nothing from the dir, but the `TempDir` must stay
/// alive for the duration of the test or its files vanish; returning it keeps it
/// in scope.
/// What: opens an [`SmContextEngine`] for `conv_id` with the given config and a
/// `window`, rooted at a new tempdir.
/// Test: used by the tests below.
fn engine_with(window: u32, inf: &SmInferenceConfig) -> (SmContextEngine, TempDir) {
    let dir = tempdir().expect("tempdir");
    let rounds = SmRoundsConfig { window };
    let eng = SmContextEngine::open("conv-1", dir.path(), inf, &rounds).expect("open engine");
    (eng, dir)
}

/// Why: §7.2a — after adding window+1 rounds, exactly one oldest round is evicted
/// and the verbatim window holds exactly `window`.
/// What: window=10, fixed mock; records 11 rounds; asserts window_len == 10,
/// total_rounds == 11, and the last record reported one eviction.
/// Test: this is the test.
#[tokio::test]
async fn window_evicts_oldest_round() {
    let inf = inference();
    let (mut eng, _dir) = engine_with(10, &inf);
    let mock = MockProvider::fixed("summary");

    let mut last_evicted = 0;
    for i in 0..11 {
        last_evicted = eng
            .record(
                &mock,
                "claude-haiku",
                format!("u{i}"),
                format!("a{i}"),
                ts(i),
                Vec::new(),
            )
            .await
            .expect("record");
    }

    assert_eq!(eng.conversation().window_len(), 10, "window capped at 10");
    assert_eq!(eng.conversation().total_rounds, 11, "monotonic counter");
    assert_eq!(last_evicted, 1, "the 11th round evicted exactly one");
    assert_eq!(mock.requests().len(), 1, "exactly one compaction call");
}

/// Why: §7.3 — the evicted round's content must be folded into
/// `compressed_context`. With a `Fixed` mock we assert the canned summary lands;
/// the renderer-delivery is covered separately, so here we assert the engine
/// actually replaces the compressed block with the compaction response.
/// What: window=1; records 2 rounds; asserts the compressed block equals the mock
/// summary (proving the fold response was stored).
/// Test: this is the test.
#[tokio::test]
async fn evicted_content_lands_in_summary() {
    let inf = inference();
    let (mut eng, _dir) = engine_with(1, &inf);
    let mock = MockProvider::fixed("FOLDED: round about goal g-99");

    eng.record(&mock, "m", "first user", "first asst", ts(0), Vec::new())
        .await
        .expect("record 1");
    eng.record(&mock, "m", "second user", "second asst", ts(1), Vec::new())
        .await
        .expect("record 2");

    assert_eq!(
        eng.conversation().compressed_context,
        "FOLDED: round about goal g-99"
    );
    assert_eq!(eng.conversation().window_len(), 1);
}

/// Why: GOLDEN — a specific goal id and session id present in an evicted round
/// must survive into `compressed_context`. With an `Echo` mock (which returns the
/// delivered request content), the ids appear in the summary IFF the engine
/// actually delivered the evicted round to the compaction call.
/// What: window=1; round 1 carries `g-314` and `s-271` in its text + a tool
/// trace; records a 2nd round to evict round 1; asserts both ids appear in the
/// compressed block.
/// Test: this is the test.
#[tokio::test]
async fn golden_ids_survive_compaction() {
    let inf = inference();
    let (mut eng, _dir) = engine_with(1, &inf);
    let mock = MockProvider::echo("SUMMARY> ");

    eng.record(
        &mock,
        "m",
        "please advance goal g-314",
        "spawned session s-271 to do it",
        ts(0),
        vec![ToolTrace::new("session_new", "created s-271 for g-314")],
    )
    .await
    .expect("record 1");
    eng.record(&mock, "m", "status?", "in progress", ts(1), Vec::new())
        .await
        .expect("record 2");

    let summary = &eng.conversation().compressed_context;
    assert!(summary.contains("g-314"), "goal id survives: {summary}");
    assert!(summary.contains("s-271"), "session id survives: {summary}");
}

/// Why: §7.2b — a single round whose size exceeds the token budget must trigger
/// compaction even though the round-count is well under the window.
/// What: window=10 but token_budget tiny; records ONE huge round; asserts a
/// compaction call happened (the budget path), not the count path.
/// Test: this is the test.
#[tokio::test]
async fn token_budget_triggers_compaction() {
    let mut inf = inference();
    inf.context_token_budget = 50; // tiny safety valve
    inf.compressed_context_max_tokens = 1_000_000;
    let (mut eng, _dir) = engine_with(10, &inf);
    let mock = MockProvider::fixed("compacted");

    // One round, but huge: ~4000 chars → ~1000 tokens ≫ 50-token budget.
    // window=1 effect can't apply (count is 1 ≤ 10); only the budget can fire.
    // The engine keeps ≥1 verbatim round, so with a single round it folds nothing
    // — so add a SECOND small round first to give the loop a round to evict.
    eng.record(&mock, "m", "small", "small", ts(0), Vec::new())
        .await
        .expect("record small");
    let huge = "x".repeat(4_000);
    let evicted = eng
        .record(&mock, "m", huge, "ok", ts(1), Vec::new())
        .await
        .expect("record huge");

    assert!(evicted >= 1, "token-budget overflow evicted ≥1 round");
    assert!(!mock.requests().is_empty(), "budget path called compaction");
    assert!(
        eng.conversation().token_estimate <= 1_000_000,
        "estimate stays bounded"
    );
}

/// Why: §7.3 default resolution — when `compaction_model` is unset the engine is
/// handed the resolved `summary_model` (Haiku) by SM-7; SM-5 must pass through
/// exactly the model id it is given to the compaction call.
/// What: records to force one compaction with model "anthropic/claude-haiku";
/// asserts the mock's last request used that model.
/// Test: this is the test.
#[tokio::test]
async fn default_compaction_uses_summary_model() {
    let inf = inference();
    let (mut eng, _dir) = engine_with(1, &inf);
    let mock = MockProvider::fixed("s");

    // SM-7 resolves the Compaction tier → summary_model (Haiku) when
    // compaction_model is empty; the engine receives the resolved id.
    let haiku = "anthropic/claude-haiku";
    eng.record(&mock, haiku, "u0", "a0", ts(0), Vec::new())
        .await
        .expect("r0");
    eng.record(&mock, haiku, "u1", "a1", ts(1), Vec::new())
        .await
        .expect("r1");

    assert_eq!(mock.last_model().as_deref(), Some(haiku));
}

/// Why: §7.3 override — when `compaction_model` IS set it supersedes
/// summary_model for the compaction call; the engine must use whatever resolved
/// model it is handed.
/// What: forces a compaction with an override model id; asserts the mock saw it.
/// Test: this is the test.
#[tokio::test]
async fn compaction_model_override_is_honored() {
    let inf = inference();
    let (mut eng, _dir) = engine_with(1, &inf);
    let mock = MockProvider::fixed("s");

    let override_model = "openrouter/meta-llama/llama-3.1-8b-instruct:free";
    eng.record(&mock, override_model, "u0", "a0", ts(0), Vec::new())
        .await
        .expect("r0");
    eng.record(&mock, override_model, "u1", "a1", ts(1), Vec::new())
        .await
        .expect("r1");

    assert_eq!(mock.last_model().as_deref(), Some(override_model));
}

/// Why: §7.4 — after each `record` the state file must exist and a fresh engine
/// opened against the same root must reconstruct an identical conversation.
/// What: records two rounds; asserts the file exists after each; then opens a new
/// engine and asserts its conversation equals the original's.
/// Test: this is the test.
#[tokio::test]
async fn state_file_written_each_record() {
    let inf = inference();
    let dir = tempdir().expect("tempdir");
    let rounds = SmRoundsConfig { window: 10 };
    let mut eng = SmContextEngine::open("conv-x", dir.path(), &inf, &rounds).expect("open");
    let mock = MockProvider::fixed("s");
    let store = ConversationStore::new(dir.path());

    eng.record(&mock, "m", "u0", "a0", ts(0), Vec::new())
        .await
        .expect("r0");
    assert!(store.path_for("conv-x").exists(), "file after first record");

    eng.record(&mock, "m", "u1", "a1", ts(1), Vec::new())
        .await
        .expect("r1");
    assert!(
        store.path_for("conv-x").exists(),
        "file after second record"
    );

    // Round-trip: a fresh engine reconstructs an identical conversation.
    let resumed = SmContextEngine::open("conv-x", dir.path(), &inf, &rounds).expect("resume");
    assert_eq!(resumed.conversation(), eng.conversation());
}

/// Why: a brand-new conv_id with no state file opens empty (not an error).
/// What: opens a fresh engine and asserts an empty conversation.
/// Test: this is the test.
#[tokio::test]
async fn new_conversation_starts_empty() {
    let inf = inference();
    let (eng, _dir) = engine_with(10, &inf);
    assert_eq!(eng.conversation(), &SmConversation::new());
}

/// Why: an engine opened against a root with a persisted conversation resumes it
/// intact (§7.4 restart survival).
/// What: records via one engine, drops it, opens a second engine on the same root
/// and conv_id, asserts the resumed conversation matches.
/// Test: this is the test.
#[tokio::test]
async fn engine_resumes_persisted_conversation() {
    let inf = inference();
    let dir = tempdir().expect("tempdir");
    let rounds = SmRoundsConfig { window: 10 };
    let mock = MockProvider::fixed("s");

    let mut eng = SmContextEngine::open("c", dir.path(), &inf, &rounds).expect("open");
    eng.record(&mock, "m", "u0", "a0", ts(0), Vec::new())
        .await
        .expect("r0");
    let snapshot = eng.conversation().clone();
    drop(eng);

    let resumed = SmContextEngine::open("c", dir.path(), &inf, &rounds).expect("resume");
    assert_eq!(resumed.conversation(), &snapshot);
}

/// Why: §7.6 — when the compressed block exceeds its cap the engine re-summarises
/// it, replacing it with the shorter pass output.
/// What: window=1, tiny compressed cap; the `Echo` fold makes the block large, so
/// the engine should run a re-summarise pass (a SECOND provider call for the
/// eviction). Assert there were two calls (fold + resummarise) and the block is
/// the resummarise output.
/// Test: this is the test.
#[tokio::test]
async fn oversized_summary_is_resummarised() {
    let mut inf = inference();
    inf.context_token_budget = 1_000_000;
    inf.compressed_context_max_tokens = 1; // any non-empty block exceeds 1 token
    let (mut eng, _dir) = engine_with(1, &inf);
    let mock = MockProvider::echo("BLK> ");

    eng.record(&mock, "m", "first with content", "reply", ts(0), Vec::new())
        .await
        .expect("r0");
    eng.record(&mock, "m", "second", "reply2", ts(1), Vec::new())
        .await
        .expect("r1");

    // One eviction → one fold + one resummarise = 2 calls.
    assert_eq!(mock.requests().len(), 2, "fold then resummarise");
    // The last call used the resummarise system prompt.
    let reqs = mock.requests();
    assert_eq!(reqs[1].system, COMPRESS_SUMMARY_SYSTEM_PROMPT);
}

/// Why: §7.5 — the working prompt must be assembled in EXACTLY the order
/// system → compressed → recall → recent rounds → current message.
/// What: builds an engine, injects a compressed block + one recent round, then
/// assembles with a system prompt, a recall string, and a current message;
/// asserts the precise role/content sequence.
/// Test: this is the test.
#[tokio::test]
async fn assembly_order_is_exact() {
    let inf = inference();
    let mock = MockProvider::fixed("s");

    // window=1 → after 2 records there is a compressed block ("s") and exactly
    // one verbatim round ("recent-*") left in the window.
    let (mut eng, _dir) = engine_with(1, &inf);
    eng.record(&mock, "m", "old-u", "old-a", ts(0), Vec::new())
        .await
        .expect("r0");
    eng.record(&mock, "m", "recent-u", "recent-a", ts(1), Vec::new())
        .await
        .expect("r1");

    let msgs = eng.assemble_working_prompt("SYSTEM", Some("RECALL"), "CURRENT");

    // system + compressed + recall + (user, assistant) for one round + current.
    assert_eq!(msgs.len(), 6, "exact §7.5 message count");
    // system, compressed, recall, [user(recent), assistant(recent)], current(user)
    assert_eq!(msgs[0].role, "system");
    assert_eq!(msgs[0].content, "SYSTEM");
    assert_eq!(msgs[1].role, "system");
    assert!(msgs[1].content.starts_with("Earlier in this conversation:"));
    assert_eq!(msgs[2].role, "system");
    assert!(msgs[2].content.starts_with("Relevant memory:"));
    assert_eq!(msgs[3].role, "user");
    assert_eq!(msgs[3].content, "recent-u");
    assert_eq!(msgs[4].role, "assistant");
    assert_eq!(msgs[4].content, "recent-a");
    assert_eq!(msgs[5].role, "user");
    assert_eq!(msgs[5].content, "CURRENT");
}

/// Why: §7.5 — empty optional blocks (no compressed context, no recall) must be
/// omitted, never emitted as blank messages.
/// What: a fresh engine (empty compressed block) assembled with `None` recall
/// yields only system + current message.
/// Test: this is the test.
#[tokio::test]
async fn assembly_skips_empty_blocks() {
    let inf = inference();
    let (eng, _dir) = engine_with(10, &inf);
    let msgs = eng.assemble_working_prompt("SYSTEM", None, "CURRENT");
    assert_eq!(msgs.len(), 2, "only system + current");
    assert_eq!(msgs[0].role, "system");
    assert_eq!(msgs[0].content, "SYSTEM");
    assert_eq!(msgs[1].role, "user");
    assert_eq!(msgs[1].content, "CURRENT");
}

/// Why: the token estimate must track content (compressed block + rounds) so the
/// safety valve is meaningful.
/// What: records a couple of rounds with no compaction and asserts the estimate
/// is non-zero and equals chars/4 of the held content.
/// Test: this is the test.
#[tokio::test]
async fn token_estimate_tracks_content() {
    let inf = inference();
    let (mut eng, _dir) = engine_with(10, &inf);
    let mock = MockProvider::fixed("s");
    eng.record(&mock, "m", "abcd", "efgh", ts(0), Vec::new())
        .await
        .expect("r0");
    // 8 chars / 4 = 2 tokens, no compressed block yet.
    assert_eq!(eng.conversation().token_estimate, 2);
}
