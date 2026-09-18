//! #8246: a drawer edited in place must stop matching its OLD text.
//!
//! Why: the backfill established coverage by asking the index which drawer ids
//! it held. An edit keeps the id, so a corpus holding every id at pre-edit text
//! answered "all present", the pre-flight probe short-circuited to
//! `AlreadyIndexed`, and nothing re-indexed the drawer. The user saw a lexical
//! lane answering from text the palace no longer has — and, worse, NOT
//! answering for the text it does have. Neither the 300-second repair sweep nor
//! a daemon restart could repair it, because both re-enter the same probe.
//!
//! What: this test cannot be written against a mock. The claim is about what a
//! real corpus holds after a real in-place edit, so it drives the shipped write
//! path (`memory_remember`), edits the drawer the way the direct-mutation call
//! sites do (mutate under the drawer lock, persist via `kg.upsert_drawer`, no
//! BM25 enqueue), and then asks the real backfill to repair it.
//!
//! The assertions are on SEARCH RESULTS, not only on the report's status enum:
//! a change that flipped the enum without re-indexing would satisfy the enum
//! alone. Against the parent commit the first three assertions pass identically
//! and the palace still answers for `OLD_TOKEN` at the end.
//!
//! Test: this *is* the test file.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_memory::bm25_backfill::{backfill_state_palace, BackfillStatus};
use trusty_memory::bm25_lane::Bm25Lane;
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// Nonsense tokens so nothing but the drawer under test can match them.
const OLD_TOKEN: &str = "zqxjoldrunbook";
const NEW_TOKEN: &str = "zqxjnewrunbook";

/// Doc ids the corpus returns for `token`, polled until `expect_hits` matches.
///
/// Why: the live write path indexes through a bounded queue (#231), so the
/// corpus is eventually consistent after `memory_remember`. Polling is what
/// makes the PRECONDITION — the lane is live and holds the drawer — safe to
/// assert; without it the whole test could pass against a dark lane.
/// What: searches up to ~5 s for the desired emptiness/non-emptiness, then
/// returns whatever the last search saw so the caller can assert on it.
async fn corpus_hits(lane: &Bm25Lane, palace: &str, token: &str, expect_hits: bool) -> Vec<String> {
    let mut last = Vec::new();
    for _ in 0..100 {
        last = lane
            .search(palace, token, 10)
            .await
            .expect("bm25 search")
            .into_iter()
            .map(|h| h.doc_id)
            .collect();
        if last.is_empty() != expect_hits {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    last
}

/// Why: see the module doc — this is the defect #8246 records.
/// What: writes a drawer, proves the lane holds it and that the backfill calls
/// the palace covered, edits the drawer's body in place without touching BM25,
/// re-runs the backfill, and asserts the corpus moved to the new text.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread")]
async fn an_in_place_edit_is_re_indexed_by_the_next_backfill() {
    // The write path embeds; the mock keeps that off the ONNX download path.
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();

    let tmp = tempfile::tempdir().expect("tempdir");
    let data_root = tmp.path().to_path_buf();
    let palace = "editlane";

    let lane = Bm25Lane::with_limits(data_root.clone(), 3, None);
    let state = AppState::new(data_root.clone()).with_bm25_lane(Arc::clone(&lane));
    state.set_ready();
    assert!(
        state.bm25_lane().is_some(),
        "the lexical lane must be armed, or this test proves nothing"
    );
    state
        .registry
        .create_palace(
            &data_root,
            Palace {
                id: PalaceId::new(palace.to_string()),
                name: palace.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: data_root.join(palace),
            },
        )
        .expect("create palace");

    let payload = dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": palace,
            "text": format!("{OLD_TOKEN} rollback runbook for the staging deployment"),
            "force": true,
        }),
    )
    .await
    .expect("memory_remember");
    let drawer_id = payload["drawer_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the write was skipped, not stored: {payload}"))
        .to_string();

    // Precondition 1: the lane is LIVE and holds the drawer's original text.
    let indexed = corpus_hits(&lane, palace, OLD_TOKEN, true).await;
    assert!(
        indexed.contains(&drawer_id),
        "precondition: the drawer must be in the corpus before the edit. Got {indexed:?}"
    );

    let handle = state
        .registry
        .open_palace(&data_root, &PalaceId::new(palace.to_string()))
        .expect("open palace");

    // Precondition 2: with nothing edited, the backfill short-circuits. This is
    // the behaviour the fix must PRESERVE — otherwise the repair sweep becomes a
    // full re-index of every palace every five minutes.
    let baseline = backfill_state_palace(&state, &handle, palace, false).await;
    assert_eq!(
        baseline.status,
        BackfillStatus::AlreadyIndexed,
        "an unedited palace must still skip: {baseline:?}"
    );
    assert_eq!(baseline.indexed, 0, "a covered palace must not be re-fed");

    // The edit: mutate the drawer under the write lock and persist it the way
    // the direct-mutation call sites do. No `bm25_index_enqueue`, which is
    // precisely the gap — the id does not change, so nothing tells BM25.
    let uuid: uuid::Uuid = drawer_id.parse().expect("drawer id is a uuid");
    let updated = {
        let mut drawers = handle.drawers.write();
        let drawer = drawers
            .iter_mut()
            .find(|d| d.id == uuid)
            .expect("the drawer we just wrote");
        drawer.set_content(format!(
            "{NEW_TOKEN} rollback runbook for the staging deployment"
        ));
        drawer.clone()
    };
    handle
        .kg
        .upsert_drawer(&updated)
        .await
        .expect("persist the edited drawer");

    // The repair. On the parent commit this reports `AlreadyIndexed` with
    // `indexed == 0`, because every drawer id is still present.
    let repair = backfill_state_palace(&state, &handle, palace, false).await;
    assert_ne!(
        repair.status,
        BackfillStatus::AlreadyIndexed,
        "an edited drawer must not read as covered: {repair:?}"
    );
    assert_eq!(
        repair.indexed, 1,
        "the edited drawer must be re-indexed: {repair:?}"
    );
    assert!(
        repair.fully_indexed(),
        "and the run must establish coverage: {repair:?}"
    );

    // The load-bearing assertions: what the corpus actually answers.
    let new_hits = corpus_hits(&lane, palace, NEW_TOKEN, true).await;
    assert_eq!(
        new_hits,
        vec![drawer_id.clone()],
        "the edited text must be findable: {new_hits:?}"
    );
    let old_hits = corpus_hits(&lane, palace, OLD_TOKEN, false).await;
    assert!(
        old_hits.is_empty(),
        "#8246: the superseded text must stop matching. Got {old_hits:?}"
    );

    lane.shutdown().await;
}
