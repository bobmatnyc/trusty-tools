//! Tests for `MemoryService::rename_palace` (#9544, PR-C2).
//!
//! Why: the rename touches every piece of per-palace daemon state — write
//! mutexes, chat stores, the BM25 lane, the name / last-used / pin caches — and
//! each one left under the old id is a separate way to serve stale state.
//! What: one test per acceptance row that has no route in from outside the
//! crate; the wire-level rows live in `tests/palace_rename_9544.rs`.
//! Test: this file IS the test module.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::palace_emptiness::PalaceNotEmpty;
use trusty_common::memory_core::registry::{PalaceRenameError, RenameOptions};
use trusty_common::memory_core::store::{PalaceStore, PalaceStoreError};

use super::rename::{count_refusal, RenameError};
use crate::service::MemoryService;
use crate::tools::dispatch_tool;
use crate::transport::rpc::{error_codes, JsonRpcResponse};
use crate::{AppState, DaemonEvent};

/// Bound on any single rename in these tests; a deadlock trips it.
const RENAME_BOUND: Duration = Duration::from_secs(10);

fn fixture() -> (AppState, tempfile::TempDir) {
    trusty_common::memory_core::retrieval::seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = AppState::new(tmp.path().to_path_buf());
    state.set_ready();
    (state, tmp)
}

fn make_palace(state: &AppState, id: &str) {
    state
        .registry
        .create_palace(
            &state.data_root,
            Palace {
                id: PalaceId::new(id),
                name: id.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: state.data_root.join(id),
            },
        )
        .expect("create palace");
}

/// `force`: the fixture notes share a prefix the Jaro-Winkler dedup gate merges.
async fn remember(state: &AppState, palace: &str, text: &str) -> Value {
    dispatch_tool(
        state,
        "memory_remember",
        json!({"palace": palace, "text": text, "force": true}),
    )
    .await
    .expect("memory_remember")
}

async fn rename(
    state: &AppState,
    old: &str,
    new: &str,
    replace_empty: bool,
) -> Result<Value, RenameError> {
    tokio::time::timeout(
        RENAME_BOUND,
        MemoryService::new(state.clone()).rename_palace(old, new, replace_empty),
    )
    .await
    .unwrap_or_else(|_| panic!("rename {old} -> {new} did not finish within {RENAME_BOUND:?}"))
}

/// The wire code and message a failed rename crosses as.
fn refusal(result: Result<Value, RenameError>) -> (i32, String) {
    let err = result.expect_err("the rename must be refused");
    (err.rpc_code(), err.to_string())
}

/// Note text long enough for the MCP content filter, distinct per `i`.
fn note(i: usize) -> String {
    format!("rename fixture note number {i} records a distinct durable fact about palace moves")
}

/// Why (#9544, A1): a rename that loses a drawer, triple, vector, room or wing
/// is data loss reported as success.
/// What: seeds three notes and two triples, renames, and asserts the before
/// and after counts the response reports are equal and non-trivial.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_preserves_counts() {
    let (state, _tmp) = fixture();
    make_palace(&state, "count-src");
    for i in 0..3 {
        remember(&state, "count-src", &note(i)).await;
    }
    for object in ["alpha", "beta"] {
        dispatch_tool(
            &state,
            "kg_assert",
            json!({"palace": "count-src", "subject": "rename", "predicate": "keeps", "object": object}),
        )
        .await
        .expect("kg_assert");
    }

    let out = rename(&state, "count-src", "count-dst", false)
        .await
        .expect("rename");
    let (before, after) = (&out["counts"]["before"], &out["counts"]["after"]);
    assert_eq!(before, after, "counts must survive the rename: {out}");
    assert_eq!(before["drawers"], 3, "{out}");
    assert!(before["triples"].as_u64() >= Some(2), "{out}");
    assert_eq!(before["vectors"], 3, "{out}");
    assert!(!state.data_root.join("count-src").exists());
}

/// Why (#9544, A5): after `a -> b`, both `a` and `b` resolve to one palace.
/// Locking each id's palace without deduplication takes that mutex twice and
/// the reverse rename waits on itself.
/// What: renames `a -> b`, then `b -> a`, each within [`RENAME_BOUND`].
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_reverse_does_not_deadlock() {
    let (state, _tmp) = fixture();
    make_palace(&state, "rev-a");
    rename(&state, "rev-a", "rev-b", false)
        .await
        .expect("a -> b");
    rename(&state, "rev-b", "rev-a", false)
        .await
        .expect("b -> a, the reverse rename");
    assert!(state.data_root.join("rev-a/palace.json").exists());
    assert!(!state.data_root.join("rev-b").exists());
}

/// Why (#9544, A5): `a -> b` racing `b -> a` takes the same two mutexes in
/// opposite orders unless the order is fixed — the ABBA deadlock.
/// What: the test holds both palaces' mutexes, queues `a -> b` (waiting on
/// `a`), then `b -> a` (which, unordered, would wait on `b`), and releases
/// both at once. Unordered, each rename then holds one mutex and waits on the
/// other; ordered, they run one after the other. Both must finish within
/// [`RENAME_BOUND`] without a lock timeout.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_concurrent_opposite_renames_do_not_abba() {
    let (state, _tmp) = fixture();
    make_palace(&state, "abba-a");
    make_palace(&state, "abba-b");
    let held_a = state.palace_write_lock("abba-a").lock_owned().await;
    let held_b = state.palace_write_lock("abba-b").lock_owned().await;
    let spawn = |old: &'static str, new: &'static str| {
        let state = state.clone();
        tokio::spawn(async move { rename(&state, old, new, true).await })
    };
    let ab = spawn("abba-a", "abba-b");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let ba = spawn("abba-b", "abba-a");
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop((held_a, held_b));
    for (dir, joined) in [("a->b", ab.await), ("b->a", ba.await)] {
        if let Err(e) = joined.expect("join") {
            assert!(
                !e.to_string().contains("busy"),
                "{dir} timed out on a lock: {e}"
            );
        }
    }
}

/// Why (#9544, A6, ruling QD): a writer that took the old id's mutex Arc
/// before the rename must not write under it once the rename is done, while
/// writers arriving after the rename hold the new palace's mutex.
/// What: holds `stale-src`'s mutex, queues a `memory_remember` behind it,
/// renames the palace with the core primitive, takes the renamed palace's
/// mutex, then releases the old one. The queued writer must not finish while
/// the new mutex is held, and must land in the new palace once it is free.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_stale_writer_waits_on_the_renamed_palace_lock() {
    let (state, _tmp) = fixture();
    make_palace(&state, "stale-src");
    let old_lock = state.palace_write_lock("stale-src");
    let held_old = Arc::clone(&old_lock).lock_owned().await;
    let writer = {
        let state = state.clone();
        tokio::spawn(async move { remember(&state, "stale-src", &note(7)).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (registry, root) = (Arc::clone(&state.registry), state.data_root.clone());
    tokio::task::spawn_blocking(move || {
        registry.rename_palace(&root, "stale-src", "stale-dst", &RenameOptions::default())
    })
    .await
    .expect("join")
    .expect("core rename");
    let new_lock = state.palace_write_lock("stale-src");
    assert!(
        !Arc::ptr_eq(&new_lock, &old_lock),
        "the old id must now key the new palace"
    );
    let held_new = Arc::clone(&new_lock).lock_owned().await;
    drop(held_old);

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !writer.is_finished(),
        "#9544 QD: a writer holding the pre-rename mutex wrote while the renamed palace's \
         mutex was held"
    );
    drop(held_new);
    let out = tokio::time::timeout(RENAME_BOUND, writer)
        .await
        .expect("writer finishes once the new mutex is free")
        .expect("join");
    assert_eq!(out["status"], "stored", "{out}");
    let listed = dispatch_tool(&state, "memory_list", json!({"palace": "stale-dst"}))
        .await
        .expect("memory_list");
    assert_eq!(
        listed["drawers"].as_array().map(Vec::len),
        Some(1),
        "{listed}"
    );
}

/// Why (#9544, A7): chat sessions live in `chat_sessions.redb` under the palace
/// dir; a stale cached store would keep the moved file open under the old id.
/// What: creates a session through `src`, renames, and lists it through both
/// ids.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_moves_chat_sessions() {
    let (state, _tmp) = fixture();
    make_palace(&state, "chat-src");
    let id = state
        .session_store("chat-src")
        .expect("store")
        .create_session(Some("kept".to_string()))
        .expect("create session");

    rename(&state, "chat-src", "chat-dst", false)
        .await
        .expect("rename");
    for via in ["chat-src", "chat-dst"] {
        let sessions = state
            .session_store(via)
            .expect("store")
            .list_sessions()
            .expect("list");
        assert!(
            sessions.iter().any(|s| s.id == id),
            "session {id} missing through {via}: {sessions:?}"
        );
    }
    assert!(
        !state.data_root.join("chat-src").exists(),
        "the old dir was recreated"
    );
}

/// Why (#9544, A8, ruling QG): a resident BM25 index keyed by either id keeps
/// the pre-move corpus, and a late flush must not recreate `<root>/<old>/bm25`.
/// What: indexes a note through the lane, renames, and asserts nothing stays
/// resident, the old dir is absent, and a lexical search through the old id
/// finds the note.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_drops_bm25_for_old_and_new_and_recreates_no_old_dir() {
    let (state, _tmp) = fixture();
    let lane = crate::bm25_lane::Bm25Lane::with_limits(state.data_root.clone(), 3, None);
    let state = state.with_bm25_lane(Arc::clone(&lane));
    make_palace(&state, "lex-src");
    lane.index("lex-src", "d1", "lexical corpus survives the rename")
        .await
        .expect("index");
    assert_eq!(lane.resident_count().await, 1);

    rename(&state, "lex-src", "lex-dst", false)
        .await
        .expect("rename");
    assert_eq!(
        lane.resident_count().await,
        0,
        "both ids' indexes must be dropped"
    );
    assert!(!state.data_root.join("lex-src").exists());
    let hits = lane.search("lex-src", "lexical", 5).await.expect("search");
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        !state.data_root.join("lex-src").exists(),
        "a search recreated the old dir"
    );
    lane.shutdown().await;
}

/// Why (#9544, A8, fail-open check): a dirty index that cannot be flushed
/// before the move is dropped; unless the new palace is queued for BM25 repair,
/// its unflushed documents are gone from the lexical lane for good.
/// What: makes `<root>/src/bm25` unwritable so every flush fails, indexes a
/// document, renames, and asserts the new id is in the repair set.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_queues_bm25_repair_when_an_unflushed_index_is_dropped() {
    use std::os::unix::fs::PermissionsExt as _;
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIP: running as root, so a read-only dir does not fail the flush");
        return;
    }
    let (state, _tmp) = fixture();
    let lane = crate::bm25_lane::Bm25Lane::with_limits(state.data_root.clone(), 3, None);
    let state = state.with_bm25_lane(Arc::clone(&lane));
    make_palace(&state, "dirty-src");
    let bm25_dir = lane.data_dir_for_palace("dirty-src");
    std::fs::create_dir_all(&bm25_dir).expect("bm25 dir");
    std::fs::set_permissions(&bm25_dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    lane.index("dirty-src", "d1", "never reaches disk")
        .await
        .expect("index");

    let result = rename(&state, "dirty-src", "dirty-dst", false).await;
    let moved = lane.data_dir_for_palace("dirty-dst");
    std::fs::set_permissions(&moved, std::fs::Permissions::from_mode(0o700)).expect("restore");
    result.expect("rename");
    assert!(
        state.bm25_dirty.contains("dirty-dst"),
        "the dropped documents must be queued for repair under the new id"
    );
    lane.shutdown().await;
}

/// Why (#9544, A7): the old id's cached name, last-used stamp, write mutex and
/// chat store would otherwise answer for a palace that moved.
/// What: seeds each cache under the old id (and the pin map), renames, and
/// asserts each is gone or remapped to the new id.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_clears_palace_names_last_used_write_locks_session_stores() {
    let (state, _tmp) = fixture();
    make_palace(&state, "cache-src");
    state
        .palace_names
        .insert("cache-src".into(), "label".into());
    state.palace_last_used.insert("cache-src".into(), 1);
    let _ = state.palace_write_lock("cache-src");
    let _ = state.session_store("cache-src").expect("store");
    let pin = std::path::PathBuf::from("/project/root");
    state
        .pin_project_map
        .insert("cache-src".into(), pin.clone());

    rename(&state, "cache-src", "cache-dst", false)
        .await
        .expect("rename");
    assert!(!state.palace_names.contains_key("cache-src"));
    assert!(!state.palace_last_used.contains_key("cache-src"));
    assert!(!state.palace_write_locks.contains_key("cache-src"));
    assert_eq!(
        state.session_stores.len(),
        0,
        "no chat store may stay cached"
    );
    assert!(!state.pin_project_map.contains_key("cache-src"));
    assert_eq!(
        state.pin_project_map.get("cache-dst").map(|p| p.clone()),
        Some(pin)
    );
}

/// Why (#9544, A9): dashboards keyed by palace id need to hear about the move.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_emits_palace_renamed_event() {
    let (state, _tmp) = fixture();
    make_palace(&state, "ev-src");
    let mut rx = state.events.subscribe();
    rename(&state, "ev-src", "ev-dst", false)
        .await
        .expect("rename");
    let mut seen = false;
    while let Ok(event) = rx.try_recv() {
        if let DaemonEvent::PalaceRenamed { old, new } = event {
            seen = old == "ev-src" && new == "ev-dst";
        }
    }
    assert!(seen, "PalaceRenamed {{ ev-src -> ev-dst }} was not emitted");
}

/// Why (#9544, A4): replacing an empty target must keep it recoverable.
/// What: without `replace_empty` the empty target is refused (-32006, naming
/// the flag); with it the target moves under `<root>/.trash` and the renamed
/// palace's `palace.json` carries the new id.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_replace_empty_trashes_target_under_dot_trash() {
    let (state, _tmp) = fixture();
    make_palace(&state, "rep-src");
    make_palace(&state, "rep-dst");
    let (code, msg) = refusal(rename(&state, "rep-src", "rep-dst", false).await);
    assert_eq!(code, error_codes::REFUSED, "{msg}");
    assert!(msg.contains("replace_empty"), "{msg}");

    let out = rename(&state, "rep-src", "rep-dst", true)
        .await
        .expect("rename");
    let trashed = out["trashed_target"].as_str().expect("trashed_target");
    assert!(
        std::path::Path::new(trashed).starts_with(state.data_root.join(".trash")),
        "{trashed}"
    );
    assert!(std::path::Path::new(trashed).exists());
    let moved = PalaceStore::load_palace(&state.data_root.join("rep-dst")).expect("load");
    assert_eq!(moved.id.as_str(), "rep-dst");
}

/// Why (#9544): the rename must judge a target's legacy kg.db with
/// `unaccounted_legacy_data`, which refuses unimported data, not with the
/// conservative default that refuses every kg.db.
/// What: a target with an unreadable kg.db is refused (-32006, legacy data); a
/// target whose kg.db holds no drawers or triples is replaced.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_target_with_legacy_kg_db_is_refused_by_unaccounted_legacy_data() {
    let (state, _tmp) = fixture();
    make_palace(&state, "leg-src");
    make_palace(&state, "leg-dst");
    let kg = state.data_root.join("leg-dst/kg.db");
    std::fs::write(&kg, b"not a sqlite database").expect("write kg.db");
    let (code, msg) = refusal(rename(&state, "leg-src", "leg-dst", true).await);
    assert_eq!(code, error_codes::REFUSED, "{msg}");
    assert!(msg.contains("legacy"), "{msg}");

    std::fs::remove_file(&kg).expect("remove kg.db");
    rusqlite::Connection::open(&kg)
        .and_then(|c| c.execute_batch("CREATE TABLE unrelated (x INTEGER);"))
        .expect("empty sqlite kg.db");
    rename(&state, "leg-src", "leg-dst", true)
        .await
        .expect("a kg.db with nothing unimported does not block the replace");
}

/// Why (#9544, ruling QA): a source in a newer on-disk format is a refusal the
/// operator clears by upgrading, not a daemon fault.
/// What: the trusty-common error (`Io` carrying `FormatTooNew`) and the
/// daemon's own pre-count refusal both map to -32006 and say to upgrade.
/// Test: itself.
#[test]
fn rename_format_too_new_source_maps_to_refused() {
    let too_new = || PalaceStoreError::FormatTooNew {
        palace: "p".into(),
        found: 99,
        supported: 1,
    };
    let core = RenameError::Palace(PalaceRenameError::Io {
        context: "check the source format".into(),
        source: Box::new(too_new()),
    });
    assert_eq!(core.rpc_code(), error_codes::REFUSED);
    assert!(core.to_string().contains("upgrade trusty-memory"), "{core}");

    let counted = count_refusal("p", anyhow::Error::new(too_new()).context("open palace p"));
    assert_eq!(counted.rpc_code(), error_codes::REFUSED);
    assert!(
        counted.to_string().contains("upgrade trusty-memory"),
        "{counted}"
    );
}

/// Why (#9544, ruling QB): `old` is joined onto the data root, so a path-shaped
/// id must be refused before any lock or palace check.
/// What: each shape is -32602 and no write mutex was created.
/// Test: itself.
#[tokio::test]
async fn rename_rejects_path_shaped_old() {
    let (state, _tmp) = fixture();
    for old in ["", "  ", "a/b", "a\\b", "..", "../x", "a\0b", ".hidden"] {
        let (code, msg) = refusal(rename(&state, old, "fine-new", false).await);
        assert_eq!(code, error_codes::INVALID_PARAMS, "{old:?}: {msg}");
    }
    assert!(
        state.palace_write_locks.is_empty(),
        "a refused id must not reach the locks"
    );
}

/// Why (#9544): renaming an alias would move the palace it points at under a
/// name the caller did not mean.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_refuses_an_alias_source() {
    let (state, _tmp) = fixture();
    make_palace(&state, "al-target");
    trusty_common::palace_alias::PalaceAliasStore::register_alias(
        &state.data_root,
        "al-alias",
        "al-target",
    )
    .expect("register alias");
    let (code, msg) = refusal(rename(&state, "al-alias", "al-new", false).await);
    assert_eq!(code, error_codes::REFUSED, "{msg}");
    assert!(msg.contains("alias"), "{msg}");
}

/// Why (#9544): a missing source is "not found", not a conflict or a fault.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_refuses_a_missing_source() {
    let (state, _tmp) = fixture();
    let (code, msg) = refusal(rename(&state, "no-such", "no-such-new", false).await);
    assert_eq!(code, error_codes::NOT_FOUND, "{msg}");
}

/// Why (#9544): a non-empty target must never be replaced, with or without
/// `replace_empty`.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_refuses_a_nonempty_target() {
    let (state, _tmp) = fixture();
    make_palace(&state, "ne-src");
    make_palace(&state, "ne-dst");
    remember(&state, "ne-dst", &note(1)).await;
    let (code, msg) = refusal(rename(&state, "ne-src", "ne-dst", true).await);
    assert_eq!(code, error_codes::REFUSED, "{msg}");
    assert!(
        state.data_root.join("ne-src/palace.json").exists(),
        "nothing may move"
    );
}

/// Why (#9544): a target that is not a palace id is refused as a conflict.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_refuses_a_bad_slug() {
    let (state, _tmp) = fixture();
    make_palace(&state, "slug-src");
    let (code, msg) = refusal(rename(&state, "slug-src", "Bad_Slug", false).await);
    assert_eq!(code, error_codes::REFUSED, "{msg}");
}

/// Why (#9544): renaming a palace onto itself is refused, not a no-op success.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_refuses_new_equal_to_old() {
    let (state, _tmp) = fixture();
    make_palace(&state, "same-id");
    let (code, msg) = refusal(rename(&state, "same-id", "same-id", false).await);
    assert_eq!(code, error_codes::REFUSED, "{msg}");
}

/// Why (#9544, rulings QA-QC): `palace_rename`'s refusals reach the wire
/// through `from_anyhow`; one row of the table mapped wrong reads as a
/// different next move to the caller.
/// What: every row, through `JsonRpcResponse::from_anyhow`, one of them under
/// an extra `context` layer.
/// Test: itself.
#[test]
fn from_anyhow_maps_palace_rename_errors() {
    use error_codes::{INTERNAL_ERROR, INVALID_PARAMS, NOT_FOUND, REFUSED};
    let core = RenameError::Palace;
    let cases: Vec<(RenameError, i32)> = vec![
        (RenameError::InvalidParams("bad".into()), INVALID_PARAMS),
        (core(PalaceRenameError::NotFound("p".into())), NOT_FOUND),
        (
            core(PalaceRenameError::SourceIsAlias {
                alias: "a".into(),
                target: "t".into(),
            }),
            REFUSED,
        ),
        (
            core(PalaceRenameError::TargetIsAlias {
                new: "n".into(),
                target: "t".into(),
            }),
            REFUSED,
        ),
        (
            core(PalaceRenameError::TargetExists { new: "n".into() }),
            REFUSED,
        ),
        (
            core(PalaceRenameError::TargetNotEmpty {
                new: "n".into(),
                reason: PalaceNotEmpty::Unconfirmed("x".into()),
            }),
            REFUSED,
        ),
        (
            core(PalaceRenameError::InvalidTarget {
                new: "n".into(),
                reason: "r".into(),
            }),
            REFUSED,
        ),
        (
            core(PalaceRenameError::Busy {
                palace: "p".into(),
                detail: "d".into(),
            }),
            REFUSED,
        ),
        (RenameError::Refused("write lock busy".into()), REFUSED),
        (
            core(PalaceRenameError::Io {
                context: "c".into(),
                source: Box::new(PalaceStoreError::FormatTooNew {
                    palace: "p".into(),
                    found: 9,
                    supported: 1,
                }),
            }),
            REFUSED,
        ),
        (
            core(PalaceRenameError::Io {
                context: "c".into(),
                source: Box::new(std::io::Error::other("disk")),
            }),
            INTERNAL_ERROR,
        ),
        (RenameError::Internal("join".into()), INTERNAL_ERROR),
    ];
    for (i, (err, code)) in cases.into_iter().enumerate() {
        let label = err.to_string();
        let mut e = anyhow::Error::new(err);
        if i == 1 {
            e = e.context("dispatch palace_rename");
        }
        let resp = JsonRpcResponse::from_anyhow(json!(1), e);
        assert_eq!(resp.error.expect("error").code, code, "row {i}: {label}");
    }
}
