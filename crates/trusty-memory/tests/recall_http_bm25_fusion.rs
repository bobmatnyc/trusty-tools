//! #5036: the HTTP recall path must fuse the BM25 lexical lane, not just the
//! vector lane.
//!
//! Why a real embedder and a real daemon: the thing under test is
//! *retrievability*, and a mock embedder cannot demonstrate it. A byte-hash
//! mock returns arbitrary neighbours, so "the vector lane missed this drawer"
//! would be an artefact of the mock rather than a property of dense retrieval.
//! An earlier end-to-end attempt on this code used `MockEmbedder` and proved
//! only that the plumbing was connected. So this test drives the real
//! `FastEmbedder` and a real `trusty-bm25-daemon`, and states its precondition
//! — that the vector lane genuinely does not return the target drawer — as an
//! assertion. If dense retrieval ever starts finding the target on its own,
//! this test fails loudly instead of passing for the wrong reason.
//!
//! Why a non-default palace: the lane resolves one socket per palace, but
//! `AppState::bm25_client` is built once against the DEFAULT palace. A test
//! run against `default` would pass without the socket-routing half of the fix
//! and hide it. `PALACE` is deliberately not the default.
//!
//! What: `#[ignore]`d — it needs the `trusty-bm25-daemon` binary and downloads
//! / loads the ONNX embedding model. Run with
//! `cargo test -p trusty-memory --test recall_http_bm25_fusion -- --include-ignored --nocapture`.
//!
//! Test: this IS the test module.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use trusty_common::memory_core::palace::PalaceId;
use trusty_common::memory_core::retrieval::recall_with_default_embedder;
use trusty_memory::bm25_backfill::backfill_state_palace;
use trusty_memory::service::MemoryService;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// A palace name that is NOT the default, so the per-palace socket routing is
/// on the critical path.
const PALACE: &str = "httpfuse";

/// A second non-default palace, so the deep test cannot inherit state — or a
/// daemon — from the shallow one.
const DEEP_PALACE: &str = "httpfusedeep";

/// The rare token that only the lexical lane can exploit.
///
/// Why: BM25 scores by inverse document frequency, so a token appearing in
/// exactly one drawer of the corpus dominates its ranking. A dense embedder
/// has no such mechanism — the token is out-of-vocabulary noise it splits into
/// meaningless subwords.
const RARE_TOKEN: &str = "QX7ZR4417";

/// The drawer only BM25 can find, and the decoys that crowd it out of the
/// vector lane.
///
/// Why the shape: the query is a decision-flavoured question, and the decoys
/// are all decision-flavoured. The target is about deep-sea biology and
/// carries the rare token. Dense retrieval ranks by topic, so the target sits
/// well outside the top of the vector list; BM25 ranks by term rarity, so it
/// sits at the top of the lexical list. The gap is deliberately large so the
/// precondition is not a coin flip.
const TARGET: &str =
    "Hydrothermal vent tubeworms host chemosynthetic bacteria in their trophosome, tagged QX7ZR4417";

const DECOYS: [&str; 6] = [
    "We decided to roll out the new billing service to ten percent of traffic first",
    "The team agreed the rollout schedule should slip a week to cover the migration",
    "Decision: adopt the staged rollout plan rather than a single cutover",
    "Rollout planning meeting concluded that we need a rollback path before launch",
    "We decided against a big-bang deployment because the rollback story was unclear",
    "The rollout decision was recorded and the launch checklist updated accordingly",
];

/// The query. Semantically it is about decisions and rollouts (the decoys);
/// lexically its rarest term appears only in [`TARGET`].
const QUERY: &str = "what did we decide about the QX7ZR4417 rollout";

/// How many results the recall asks for. Small enough that the six decoys can
/// fill it, so the target can only appear if the lexical lane put it there.
const TOP_K: usize = 3;

/// Resolve the freshly-built `trusty-bm25-daemon` binary.
///
/// Why: the supervisor discovers the daemon as a sibling of `current_exe()`,
/// which for an integration test is the test binary under `target/*/deps/`.
/// Pointing `TRUSTY_BM25_DAEMON_BIN` at the real build output sidesteps that.
/// What: honours the env var if already set, else walks up from the test
/// binary looking for the daemon next to it or one level up.
/// Test: this is the test bootstrap.
fn discover_daemon_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TRUSTY_BM25_DAEMON_BIN") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let mut p = exe.as_path();
    while let Some(parent) = p.parent() {
        for candidate in [
            parent.join("trusty-bm25-daemon"),
            parent.join("..").join("trusty-bm25-daemon"),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        p = parent;
    }
    None
}

/// Arm the daemon locator and switch the lexical lane on.
///
/// Why: `with_bm25_client_from_env` reads `TRUSTY_BM25_DAEMON` at construction
/// time, so the variable has to be set before the `AppState` is built.
///
/// Why this panics instead of returning early: an absent daemon binary used to
/// make both tests `return`, and a test that returns is a PASS. "2 passed"
/// while nothing was exercised is precisely the vacuous green this module's
/// header spends its length warning about. Both tests are `#[ignore]`d, so
/// failing loudly costs CI nothing and costs a human one clear message.
/// What: sets the locator and the lane gate, or panics with the build command.
/// Test: this is the test bootstrap.
fn arm_lane() {
    let Some(binary) = discover_daemon_binary() else {
        panic!(
            "trusty-bm25-daemon binary not found — this test cannot run and must \
             not report success. Build it first \
             (`cargo build -p trusty-memory --bin trusty-bm25-daemon`) or set \
             TRUSTY_BM25_DAEMON_BIN=<path>."
        );
    };
    // SAFETY: test-only env mutation; this binary is the sole writer and does
    // it once, before any state is constructed.
    unsafe {
        std::env::set_var("TRUSTY_BM25_DAEMON_BIN", &binary);
        std::env::set_var("TRUSTY_BM25_DAEMON", "1");
        std::env::remove_var("TRUSTY_BM25_EXTERNAL");
    }
}

/// The drawer id of one recall row.
///
/// Why: `recall_entry_json` hoists the serialised `Drawer` to the top level
/// (#69), so the id arrives under the drawer's own field name rather than a
/// recall-specific one. Reading both spellings keeps this test from breaking
/// on a rename that does not change behaviour.
fn row_id(row: &serde_json::Value) -> Option<&str> {
    row.get("id")
        .or_else(|| row.get("drawer_id"))
        .and_then(|v| v.as_str())
}

/// Does this recall payload contain the drawer with `id`?
fn contains(payload: &serde_json::Value, id: &str) -> bool {
    payload
        .as_array()
        .map(|rows| rows.iter().any(|r| row_id(r) == Some(id)))
        .unwrap_or(false)
}

/// Render a payload as `score  content-prefix` lines, for failure output.
fn summarize(payload: &serde_json::Value) -> String {
    payload
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|r| {
                    format!(
                        "  {:>6.3}  L{}  {}",
                        r.get("score").and_then(|v| v.as_f64()).unwrap_or(-1.0),
                        r.get("layer").and_then(|v| v.as_u64()).unwrap_or(9),
                        r.get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .chars()
                            .take(64)
                            .collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| format!("{payload}"))
}

/// A seeded palace with every precondition already asserted.
///
/// Why: the two tests below differ only in the `deep` flag, and an earlier cut
/// of this file spelled the setup out twice — the deep copy silently dropping
/// four of the five preconditions, so it could have passed because dense
/// retrieval found the target rather than because fusion did. One fixture makes
/// that class of drift impossible: whatever the shallow test proves about its
/// starting state, the deep test proves too.
/// What: enables the lane, creates `palace`, writes the decoys and the target,
/// backfills the palace's own BM25 daemon, and asserts all five preconditions —
/// lane enabled, index verifiably complete, target stored, BM25 ranks it, and
/// dense retrieval does NOT.
/// Test: used by both tests below.
struct Fixture {
    _tmp: tempfile::TempDir,
    state: AppState,
    target_id: String,
}

impl Fixture {
    async fn seeded(palace: &str) -> Self {
        arm_lane();

        let tmp = tempfile::tempdir().expect("tempdir");
        let state = AppState::new(tmp.path().to_path_buf()).with_bm25_client_from_env();
        state.set_ready();
        assert!(
            state.bm25_client.is_some(),
            "precondition: the lexical lane must be enabled, or this test proves nothing"
        );

        let cwd = tmp.path().to_string_lossy().to_string();
        dispatch_tool(
            &state,
            "palace_create",
            json!({ "name": palace, "force": true, "cwd": cwd }),
        )
        .await
        .expect("palace_create");

        for text in DECOYS.iter().chain(std::iter::once(&TARGET)) {
            dispatch_tool(
                &state,
                "memory_remember",
                json!({ "palace": palace, "text": text, "force": true }),
            )
            .await
            .expect("memory_remember");
        }

        let handle = state
            .registry
            .open_palace(&state.data_root, &PalaceId::new(palace.to_string()))
            .expect("open palace");

        let target_id = {
            let drawers = handle.drawers.read();
            drawers
                .iter()
                .find(|d| d.content.contains(RARE_TOKEN))
                .map(|d| d.id.to_string())
                .expect("the target drawer must be stored")
        };

        // The lexical index must actually hold the corpus. Fusing against an
        // index nothing populated is the silent-no-op shape this assertion
        // exists to rule out — every downstream assertion would still "pass"
        // on an empty index if the expectation were merely "does not error".
        let report = backfill_state_palace(&state, &handle, palace, false).await;
        assert!(
            report.fully_indexed(),
            "precondition: the palace's BM25 index must be verifiably complete, \
             or the fusion below has nothing to fuse: {report:?}"
        );

        // Give the daemon's coalescing window time to make the writes
        // searchable, then confirm the lexical lane finds the target on its own.
        let socket = trusty_common::bm25_client::socket_path_for_palace(palace);
        let bm25 = trusty_common::bm25_client::Bm25Client::new(socket);
        let mut lexical_hit = false;
        for _ in 0..50 {
            let hits = bm25.search(QUERY, TOP_K).await.expect("bm25 search");
            if hits.iter().any(|h| h.doc_id == target_id) {
                lexical_hit = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            lexical_hit,
            "precondition: BM25 must rank the target for this query, or the corpus \
             or the query is wrong and the tests below are meaningless"
        );

        // The pre-#5036 behaviour, computed rather than assumed: this is the
        // exact call `MemoryService::recall` used to make, and nothing else.
        let vector_only = recall_with_default_embedder(&handle, QUERY, TOP_K)
            .await
            .expect("vector-only recall");
        let vector_ids: Vec<String> = vector_only
            .iter()
            .map(|r| r.drawer.id.to_string())
            .collect();
        assert!(
            !vector_ids.contains(&target_id),
            "precondition: dense retrieval must MISS the target, or these tests cannot \
             distinguish a fused result from a vector one. Vector top-{TOP_K} was:\n{}",
            vector_only
                .iter()
                .map(|r| format!(
                    "  {:>6.3}  L{}  {}",
                    r.score,
                    r.layer,
                    r.drawer.content.chars().take(64).collect::<String>()
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );

        Self {
            _tmp: tmp,
            state,
            target_id,
        }
    }
}

/// Assert the fused payload returns `target_id` above the relevance floor.
///
/// Why: appearing in the payload is not enough. `prompt_context` drops every
/// drawer under `DEFAULT_RELEVANCE_FLOOR`, so a promoted hit scored on a
/// rank-only RRF scale (max ~0.033) would be in the response and out of the
/// injection — a fix that changes the JSON and nothing a user sees.
/// What: asserts presence, then that the target's score clears the floor.
/// Test: used by both tests below.
fn assert_fused_and_above_floor(fused: &serde_json::Value, target_id: &str, label: &str) {
    assert!(
        contains(fused, target_id),
        "#5036: {label} must return the drawer only the lexical lane can find. \
         Fused top-{TOP_K} was:\n{}",
        summarize(fused)
    );
    let score = fused
        .as_array()
        .and_then(|rows| rows.iter().find(|r| row_id(r) == Some(target_id)))
        .and_then(|r| r.get("score").and_then(|v| v.as_f64()))
        .expect("the target row carries a score");
    assert!(
        score as f32 >= trusty_common::memory_core::retrieval::DEFAULT_RELEVANCE_FLOOR,
        "a promoted lexical hit must clear the relevance floor ({}), or \
         prompt_context discards it again; got {score}",
        trusty_common::memory_core::retrieval::DEFAULT_RELEVANCE_FLOOR
    );
}

/// Why: this is #5036 itself. Against the parent commit, `MemoryService::recall`
/// calls `recall_with_default_embedder` and nothing else, so the fused payload
/// is identical to the vector-only payload and the target drawer — the one a
/// lexical lane exists to find — is absent from both.
///
/// What: seeds a non-default palace with six decoys and one target through the
/// real MCP write path, backfills the palace's own BM25 daemon and asserts the
/// index is genuinely covered (a fusion against an empty index would pass
/// vacuously), asserts the vector lane alone does NOT return the target, then
/// asserts the HTTP recall path DOES.
///
/// Test: this test itself.
#[ignore = "needs the trusty-bm25-daemon binary and the real ONNX embedder; run with --include-ignored"]
#[tokio::test(flavor = "multi_thread")]
async fn http_recall_returns_a_lexical_match_the_vector_lane_misses() {
    let fixture = Fixture::seeded(PALACE).await;
    let (state, target_id) = (fixture.state.clone(), fixture.target_id.clone());

    // The fix: the same query through the HTTP service path.
    let fused = MemoryService::new(state.clone())
        .recall(PALACE, QUERY, TOP_K, false)
        .await
        .expect("http recall");
    assert_fused_and_above_floor(&fused, &target_id, "the HTTP recall path");

    if let Some(sup) = state.bm25_supervisor.as_ref() {
        sup.shutdown().await;
    }
}

/// Why: `deep` is a separate branch of the same function, and the issue records
/// `handle_memory_recall_deep` as having been left behind by #156 in exactly the
/// same way. A fix covering only the shallow branch reintroduces the defect for
/// any caller passing `deep=true`.
/// What: the same fixture — so the same five preconditions hold — through
/// `recall(.., deep = true)`.
/// Test: this test itself.
#[ignore = "needs the trusty-bm25-daemon binary and the real ONNX embedder; run with --include-ignored"]
#[tokio::test(flavor = "multi_thread")]
async fn deep_http_recall_also_fuses_the_lexical_lane() {
    let fixture = Fixture::seeded(DEEP_PALACE).await;
    let (state, target_id) = (fixture.state.clone(), fixture.target_id.clone());

    let fused = MemoryService::new(state.clone())
        .recall(DEEP_PALACE, QUERY, TOP_K, true)
        .await
        .expect("deep http recall");
    assert_fused_and_above_floor(&fused, &target_id, "deep recall");

    if let Some(sup) = state.bm25_supervisor.as_ref() {
        sup.shutdown().await;
    }
}
