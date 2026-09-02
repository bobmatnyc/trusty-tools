//! Unit coverage for the #6652 measurement, prune predicate, and copy-then-swap.
//!
//! Why: the swap replaces a palace's whole knowledge graph in one `rename`.
//! Every branch that could leave the live file wrong has to be provably closed,
//! and "provably" means each one is triggered here rather than argued about.
//! What: prune-predicate tables, measurement counts against a hand-built
//! palace, the copy's row preservation, and one test per failure branch driven
//! through the [`CompactStep`] fault hook.
//! Test: this file is the test.

#![cfg(test)]

use super::copy_swap::{self, CompactPlan, CompactStep};
use super::stats::{HISTORY_KEY_PREFIX, KgRedbStats, history_close_ms, history_cutoff_ms};
use super::*;
use crate::memory_core::store::kg_store::{
    TRIPLES, TRIPLES_BY_PREDICATE, TripleValue, encode_triple_key, encode_value,
};
use chrono::Utc;
use redb::{ReadableDatabase, ReadableTableMetadata, TableDefinition, TableHandle};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

const DAY_MS: i64 = 86_400_000;

fn triple(subject: &str, predicate: &str, object: &str) -> Triple {
    Triple {
        subject: subject.into(),
        predicate: predicate.into(),
        object: object.into(),
        valid_from: Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    }
}

/// A palace with `active` live triples and `history` rows closed `age_days` ago.
///
/// Why: manufacturing history through the real `retract` path would tie every
/// row's close time to `now`, so the age gate could never be exercised. Writing
/// the `hist:` rows directly is the only way to place them in the past, and it
/// uses the exact key shape `close_active_row` writes.
fn palace_with_history(active: usize, history: usize, age_days: i64) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("kg.redb");
    {
        let kg = KgStoreRedb::open(&path).expect("open");
        for i in 0..active {
            kg.assert(&triple(&format!("s{i}"), "knows", &format!("o{i}")))
                .expect("assert");
        }
        let closed_ms = Utc::now().timestamp_millis() - age_days * DAY_MS;
        let db = kg.db();
        let wtx = db.begin_write().expect("begin");
        {
            let mut t = wtx.open_table(TRIPLES).expect("open triples");
            for i in 0..history {
                let core = encode_triple_key("hs", "was", &format!("old{i}"));
                let mut key = Vec::from(HISTORY_KEY_PREFIX);
                key.extend_from_slice(&core);
                key.extend_from_slice(&(closed_ms - 1000).to_be_bytes());
                let value = TripleValue {
                    object: format!("old{i}"),
                    valid_from_ms: closed_ms - 1000,
                    valid_to_ms: Some(closed_ms),
                    confidence: 1.0,
                    provenance: None,
                };
                let bytes = encode_value(&value).expect("encode");
                t.insert(key.as_slice(), bytes.as_slice()).expect("insert");
            }
        }
        wtx.commit().expect("commit");
    }
    (dir, path)
}

fn rows(path: &Path, def: TableDefinition<'static, &'static [u8], &'static [u8]>) -> u64 {
    let kg = KgStoreRedb::open(path).expect("open");
    let db = kg.db();
    let rtx = db.begin_read().expect("read");
    match rtx.open_table(def) {
        Ok(t) => t.len().expect("len"),
        Err(_) => 0,
    }
}

/// The palace's DATA, as the fail-closed tests compare it.
///
/// Why: raw bytes are the wrong comparison. redb writes its own allocator state
/// into the file when a `Database` is dropped, so an aborted compaction that
/// touched nothing still leaves a byte-different file. What must be identical
/// is the content: every table's row count, and the active/history split. A
/// rewrite that landed would change those; redb's bookkeeping does not.
/// Test: used by every `..._leaves_the_original_untouched` test below.
fn data_fingerprint(path: &Path) -> Vec<(String, u64)> {
    let s = KgRedbStats::measure(path, 90).expect("measure");
    let mut out: Vec<(String, u64)> = s.tables.iter().map(|t| (t.name.clone(), t.rows)).collect();
    out.push(("__active".into(), s.triples_active));
    out.push(("__history".into(), s.triples_history));
    out.sort();
    out
}

// ── prune predicate ────────────────────────────────────────────────────────

#[test]
fn history_close_ms_requires_both_the_prefix_and_a_valid_to() {
    let closed = TripleValue {
        object: "o".into(),
        valid_from_ms: 1,
        valid_to_ms: Some(500),
        confidence: 1.0,
        provenance: None,
    };
    let open = TripleValue {
        valid_to_ms: None,
        ..closed.clone()
    };
    let hist_key = b"hist:whatever".as_slice();
    let live_key = b"whatever".as_slice();

    assert_eq!(history_close_ms(hist_key, &closed), Some(500));
    // A primary-key row carrying a valid_to is still the row a lookup lands on.
    assert_eq!(history_close_ms(live_key, &closed), None);
    // A hist: row with no valid_to is a shape nothing writes: unrecognised, not
    // dead.
    assert_eq!(history_close_ms(hist_key, &open), None);
    assert_eq!(history_close_ms(live_key, &open), None);
}

#[test]
fn history_cutoff_ms_is_days_before_now() {
    assert_eq!(history_cutoff_ms(10 * DAY_MS, 3), 7 * DAY_MS);
    assert_eq!(history_cutoff_ms(0, 1), -DAY_MS);
}

#[test]
fn dropped_names_match_the_definitions() {
    // The literal in `copy_tables` cannot call `TableHandle::name` in a const,
    // so this pins the two together.
    assert_eq!(
        super::copy_tables::DROPPED_TABLES,
        &[TRIPLES_BY_PREDICATE.name()]
    );
}

// ── measurement ────────────────────────────────────────────────────────────

#[test]
fn stats_counts_match_a_hand_built_palace() {
    let (_d, path) = palace_with_history(5, 9, 200);
    let s = KgRedbStats::measure(&path, 90).expect("measure");
    assert_eq!(s.triples_active, 5, "five live triples were asserted");
    assert_eq!(s.triples_history, 9, "nine hist: rows were written");
    assert_eq!(s.triples_history_stale, 9, "all nine are 200 days old");
    assert!(s.triples_history_stale_bytes > 0);
    assert_eq!(s.rows(TRIPLES.name()), 14, "5 active + 9 history");
    assert!(s.file_bytes > 0);
}

#[test]
fn stats_history_split_tracks_the_cutoff() {
    let (_d, path) = palace_with_history(2, 4, 30);
    let fresh = KgRedbStats::measure(&path, 90).expect("measure");
    assert_eq!(fresh.triples_history, 4);
    assert_eq!(
        fresh.triples_history_stale, 0,
        "30-day-old history is inside a 90-day retention window"
    );
    let aggressive = KgRedbStats::measure(&path, 7).expect("measure");
    assert_eq!(aggressive.triples_history_stale, 4);
}

#[test]
fn measure_writes_nothing_to_the_live_file() {
    let (_d, path) = palace_with_history(3, 3, 200);
    let before = std::fs::metadata(&path).expect("stat");
    let bytes = std::fs::read(&path).expect("read");
    let _ = KgRedbStats::measure(&path, 90).expect("measure");
    let after = std::fs::metadata(&path).expect("stat");
    assert_eq!(before.len(), after.len(), "measurement changed the size");
    assert_eq!(bytes, std::fs::read(&path).expect("read"), "bytes differ");
}

// ── the dead predicate index ───────────────────────────────────────────────

/// Why (#6652): the dead index is reclaimed by the compaction NOT copying it,
/// not by an at-open `delete_table`. `delete_table` walks every page of the
/// table, and running that inline on every writer open would put a full-file
/// page walk on the daemon's startup path for a 342 MB palace — the #6366
/// symptom. The rewrite is already size-gated, off the request path, and
/// behind a verified backup, so it is where the drop belongs.
#[test]
fn the_compaction_drops_the_dead_predicate_index() {
    let (_d, path) = palace_with_history(3, 40, 400);
    let kg = KgStoreRedb::open(&path).expect("open");
    {
        // A palace that predates #6652 still carries the table on disk; a
        // fresh one never creates it, so seed it to model the real case.
        let db = kg.db();
        let wtx = db.begin_write().expect("begin");
        {
            let mut t = wtx.open_table(TRIPLES_BY_PREDICATE).expect("open");
            for i in 0..40u32 {
                t.insert(format!("p{i}").as_bytes(), b"".as_slice())
                    .expect("insert");
            }
        }
        wtx.commit().expect("commit");
    }
    drop(kg);
    let before = KgRedbStats::measure(&path, 90).expect("measure");
    assert_eq!(
        before.dead_predicate_index.as_ref().map(|t| t.rows),
        Some(40),
        "precondition: the seeded index is on disk"
    );

    compact(&path, 90);

    let after = KgRedbStats::measure(&path, 90).expect("measure");
    assert!(
        after.dead_predicate_index.is_none(),
        "the rewrite must not copy triples_by_predicate, got {:?}",
        after.dead_predicate_index
    );
}

// ── the copy-then-swap ─────────────────────────────────────────────────────

fn compact(path: &Path, cutoff_days: i64) -> copy_swap::CompactOutcome {
    let kg = KgStoreRedb::open(path).expect("open");
    let plan = CompactPlan {
        history_cutoff_ms: Some(history_cutoff_ms(
            Utc::now().timestamp_millis(),
            cutoff_days,
        )),
        keep_backup: true,
    };
    let prepared = copy_swap::prepare(&kg, plan, None).expect("prepare");
    prepared.commit(&kg, None).expect("commit")
}

#[test]
fn compaction_preserves_every_live_row() {
    let (_d, path) = palace_with_history(40, 200, 400);
    let before: Vec<String> = {
        let kg = KgStoreRedb::open(&path).expect("open");
        (0..40)
            .flat_map(|i| kg.query_active(&format!("s{i}")).expect("query"))
            .map(|t| format!("{}|{}|{}", t.subject, t.predicate, t.object))
            .collect()
    };
    let out = compact(&path, 90);
    assert_eq!(out.history_rows_pruned, 200);

    let kg = KgStoreRedb::open(&path).expect("reopen");
    let after: Vec<String> = (0..40)
        .flat_map(|i| kg.query_active(&format!("s{i}")).expect("query"))
        .map(|t| format!("{}|{}|{}", t.subject, t.predicate, t.object))
        .collect();
    assert_eq!(before.len(), 40);
    assert_eq!(
        before, after,
        "every active triple must survive the rewrite byte for byte"
    );
    assert_eq!(rows(&path, TRIPLES).min(40), 40, "40 active rows remain");
}

#[test]
fn compaction_prunes_only_stale_history() {
    let (_d, path) = palace_with_history(2, 6, 10);
    // A 90-day cutoff leaves 10-day-old history alone.
    let out = compact(&path, 90);
    assert_eq!(out.history_rows_pruned, 0);
    assert_eq!(rows(&path, TRIPLES), 8, "2 active + 6 history all survive");
}

#[test]
fn compaction_shrinks_the_file_and_keeps_live_rows() {
    // Enough history that the reclaimed pages exceed redb's own file
    // granularity; a handful of rows can round to no change.
    let (_d, path) = palace_with_history(10, 4_000, 400);
    let out = compact(&path, 90);
    assert_eq!(out.history_rows_pruned, 4_000);
    assert!(
        out.bytes_after < out.bytes_before,
        "file did not shrink: {} -> {}",
        out.bytes_before,
        out.bytes_after
    );
    assert_eq!(
        rows(&path, TRIPLES),
        10,
        "the ten live rows are all that is left"
    );
}

#[test]
fn compaction_swaps_the_live_handle_in_place() {
    let (_d, path) = palace_with_history(4, 100, 400);
    // The SAME handle the swap happens under must see the post-compaction
    // state — not just a freshly-opened one, which would pass trivially.
    let kg = KgStoreRedb::open(&path).expect("open");
    let plan = CompactPlan {
        history_cutoff_ms: Some(history_cutoff_ms(Utc::now().timestamp_millis(), 90)),
        keep_backup: true,
    };
    let prepared = copy_swap::prepare(&kg, plan, None).expect("prepare");
    prepared.commit(&kg, None).expect("commit");

    let db = kg.db();
    let rtx = db.begin_read().expect("read through the long-lived handle");
    let table = rtx.open_table(TRIPLES).expect("open triples");
    assert_eq!(
        table.len().expect("len"),
        4,
        "the pre-existing handle is still serving the pre-compaction inode"
    );
}

#[test]
fn a_backup_is_written_and_only_one_generation_is_kept() {
    let (_d, path) = palace_with_history(3, 50, 400);
    let out = compact(&path, 90);
    let backup = out.backup.expect("a backup path");
    assert!(backup.exists(), "backup missing at {}", backup.display());
    let first_len = std::fs::metadata(&backup).expect("stat").len();
    let out2 = compact(&path, 90);
    let backup2 = out2.backup.expect("a second backup");
    assert_eq!(backup, backup2, "the backup name must be stable");
    assert_ne!(
        first_len,
        std::fs::metadata(&backup2).expect("stat").len(),
        "the second run must replace the first backup, not keep both"
    );
}

// ── fail-closed branches ───────────────────────────────────────────────────

fn fail_at(step: CompactStep) -> copy_swap::CompactFaultHook {
    Arc::new(move |s| {
        if s == step {
            anyhow::bail!("injected failure at {s:?}");
        }
        Ok(())
    })
}

/// Every pre-rename failure must leave `kg.redb` byte-identical.
#[test]
fn a_crash_before_the_rename_leaves_the_original_untouched() {
    for step in [
        CompactStep::AfterBackup,
        CompactStep::AfterCopy,
        CompactStep::AfterFsync,
    ] {
        let (_d, path) = palace_with_history(5, 60, 400);
        let original = data_fingerprint(&path);
        let kg = KgStoreRedb::open(&path).expect("open");
        let plan = CompactPlan {
            history_cutoff_ms: Some(history_cutoff_ms(Utc::now().timestamp_millis(), 90)),
            keep_backup: true,
        };
        let err = copy_swap::prepare(&kg, plan, Some(&fail_at(step))).unwrap_err();
        assert!(
            format!("{err:#}").contains("injected failure"),
            "unexpected error at {step:?}: {err:#}"
        );
        drop(kg);
        assert_eq!(
            original,
            data_fingerprint(&path),
            "{step:?} changed the live file's contents"
        );
        assert!(
            !PathBuf::from(format!(
                "{}{}",
                path.display(),
                copy_swap::COMPACTING_SUFFIX
            ))
            .exists(),
            "{step:?} left a temp file behind"
        );
    }
}

#[test]
fn a_failure_at_the_rename_gate_leaves_the_original_untouched() {
    let (_d, path) = palace_with_history(5, 60, 400);
    let original = data_fingerprint(&path);
    let kg = KgStoreRedb::open(&path).expect("open");
    let plan = CompactPlan {
        history_cutoff_ms: Some(history_cutoff_ms(Utc::now().timestamp_millis(), 90)),
        keep_backup: true,
    };
    let prepared = copy_swap::prepare(&kg, plan, None).expect("prepare");
    let err = prepared
        .commit(&kg, Some(&fail_at(CompactStep::BeforeRename)))
        .unwrap_err();
    assert!(format!("{err:#}").contains("injected failure"), "{err:#}");
    drop(kg);
    assert_eq!(original, data_fingerprint(&path));
}

/// A crash after the rename but before the install: the FILE is correct, and a
/// reopen picks it up. This is the safe direction to fail toward.
#[test]
fn a_crash_between_rename_and_install_recovers_on_reopen() {
    let (_d, path) = palace_with_history(6, 300, 400);
    {
        let kg = KgStoreRedb::open(&path).expect("open");
        let plan = CompactPlan {
            history_cutoff_ms: Some(history_cutoff_ms(Utc::now().timestamp_millis(), 90)),
            keep_backup: true,
        };
        let prepared = copy_swap::prepare(&kg, plan, None).expect("prepare");
        let err = prepared
            .commit(&kg, Some(&fail_at(CompactStep::AfterRename)))
            .unwrap_err();
        assert!(format!("{err:#}").contains("injected failure"), "{err:#}");
    }
    // Fresh process equivalent: every handle dropped, reopen from disk.
    assert_eq!(
        rows(&path, TRIPLES),
        6,
        "the renamed file is the compacted one and reopening finds it"
    );
}

#[test]
fn a_write_during_the_copy_aborts_the_swap() {
    let (_d, path) = palace_with_history(4, 40, 400);
    let kg = KgStoreRedb::open(&path).expect("open");
    let plan = CompactPlan {
        history_cutoff_ms: Some(history_cutoff_ms(Utc::now().timestamp_millis(), 90)),
        keep_backup: true,
    };
    let prepared = copy_swap::prepare(&kg, plan, None).expect("prepare");
    // The write the re-check exists to catch.
    kg.assert(&triple("late", "arrived", "yes"))
        .expect("assert");
    let err = prepared.commit(&kg, None).unwrap_err();
    assert!(
        format!("{err:#}").contains("changed during the rewrite"),
        "expected a fingerprint abort, got: {err:#}"
    );
    // The injected row is still there and the history was not pruned.
    let after = KgStoreRedb::open(&path).expect("reopen");
    assert_eq!(after.query_active("late").expect("query").len(), 1);
    assert_eq!(rows(&path, TRIPLES), 45, "4 active + 40 history + 1 late");
}

#[test]
fn a_stale_compacting_file_is_removed_and_the_run_still_succeeds() {
    let (_d, path) = palace_with_history(3, 80, 400);
    let stale = PathBuf::from(format!(
        "{}{}",
        path.display(),
        copy_swap::COMPACTING_SUFFIX
    ));
    std::fs::write(&stale, b"leftover from a killed process").expect("seed");
    let out = compact(&path, 90);
    assert_eq!(out.history_rows_pruned, 80);
    assert!(!stale.exists(), "the temp file must be gone after a commit");
}

#[test]
fn a_backup_write_failure_aborts_before_the_copy_starts() {
    let (_d, path) = palace_with_history(3, 20, 400);
    let original = data_fingerprint(&path);
    // A directory where the backup file must go makes the copy fail.
    let backup = PathBuf::from(format!("{}{}", path.display(), copy_swap::BACKUP_SUFFIX));
    std::fs::create_dir(&backup).expect("seed a directory in the backup's place");
    let kg = KgStoreRedb::open(&path).expect("open");
    let err = copy_swap::prepare(&kg, CompactPlan::default(), None).unwrap_err();
    assert!(
        format!("{err:#}").contains("back up") || format!("{err:#}").contains("remove previous"),
        "expected a backup failure, got: {err:#}"
    );
    drop(kg);
    assert_eq!(original, data_fingerprint(&path));
    let tmp = PathBuf::from(format!(
        "{}{}",
        path.display(),
        copy_swap::COMPACTING_SUFFIX
    ));
    assert!(!tmp.exists(), "the copy must not have started");
}

#[test]
fn unknown_table_aborts_the_compaction() {
    let (_d, path) = palace_with_history(3, 10, 400);
    const MYSTERY: TableDefinition<'static, &'static [u8], &'static [u8]> =
        TableDefinition::new("a_table_from_the_future");
    let kg = KgStoreRedb::open(&path).expect("open");
    {
        let db = kg.db();
        let wtx = db.begin_write().expect("begin");
        {
            let mut t = wtx.open_table(MYSTERY).expect("open");
            t.insert(b"k".as_slice(), b"v".as_slice()).expect("insert");
        }
        wtx.commit().expect("commit");
    }
    let original = data_fingerprint(&path);
    let err = copy_swap::prepare(&kg, CompactPlan::default(), None).unwrap_err();
    assert!(
        format!("{err:#}").contains("a_table_from_the_future"),
        "expected the unknown table to be named, got: {err:#}"
    );
    drop(kg);
    assert_eq!(original, data_fingerprint(&path));
}

/// Why (#6652, code-critic BLOCK): `handle.write_mutex` does not gate every
/// writer. `KnowledgeGraph::assert` routes through `KgWriter`, whose actor
/// commits on a `spawn_blocking` thread holding only the shared
/// `Arc<KgStoreRedb>` — a grep for `write_mutex` under `store/` returns
/// nothing. A commit that lands between `commit`'s fingerprint re-check and
/// its `rename` therefore writes to the old inode, the rename unlinks it, and
/// the caller is told the write succeeded. Silent loss, fail-open.
///
/// What: fires a real `KnowledgeGraph::assert` (the `KgWriter` path, not a raw
/// `KgStoreRedb::assert`) from inside the `BeforeRename` hook — the exact
/// window — and requires that the write either survives the swap or the swap
/// aborts. Never a silent drop.
/// Test: this is the test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kg_writer_commit_inside_the_swap_window_is_never_dropped() {
    use crate::memory_core::store::kg::KnowledgeGraph;

    let (_d, path) = palace_with_history(4, 40, 400);
    let kg = KnowledgeGraph::open(&path).expect("open KnowledgeGraph");
    let store = kg.redb_store().clone();
    let plan = CompactPlan {
        history_cutoff_ms: Some(history_cutoff_ms(Utc::now().timestamp_millis(), 90)),
        keep_backup: true,
    };
    let prepared = copy_swap::prepare(&store, plan, None).expect("prepare");

    // The hook runs on the compaction thread after the re-check and before the
    // rename. It launches the writer and gives it time to reach its commit,
    // which is what makes the race deterministic rather than incidental.
    let kg_for_hook = std::sync::Arc::new(kg);
    let writer_kg = std::sync::Arc::clone(&kg_for_hook);
    let handle = tokio::runtime::Handle::current();
    let launched = Arc::new(std::sync::Mutex::new(None::<tokio::task::JoinHandle<()>>));
    let launched_slot = Arc::clone(&launched);
    let hook: copy_swap::CompactFaultHook = Arc::new(move |step| {
        if step == CompactStep::BeforeRename {
            let kg = std::sync::Arc::clone(&writer_kg);
            let task = handle.spawn(async move {
                kg.assert(triple("late", "arrived", "yes"))
                    .await
                    .expect("the KgWriter commit must not fail");
            });
            *launched_slot.lock().expect("slot") = Some(task);
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Ok(())
    });

    let store_for_commit = store.clone();
    let swap = tokio::task::spawn_blocking(move || prepared.commit(&store_for_commit, Some(&hook)))
        .await
        .expect("join swap");

    let writer_task = launched.lock().expect("slot").take();
    if let Some(task) = writer_task {
        task.await.expect("join writer");
    }

    // Either outcome is acceptable; losing the write is not.
    let survived = kg_for_hook
        .redb_store()
        .query_active("late")
        .expect("query")
        .len();
    assert_eq!(
        survived,
        1,
        "a KgWriter commit inside the swap window was silently dropped \
         (swap result: {:?})",
        swap.as_ref()
            .map(|o| o.bytes_after)
            .map_err(|e| format!("{e:#}"))
    );
}
