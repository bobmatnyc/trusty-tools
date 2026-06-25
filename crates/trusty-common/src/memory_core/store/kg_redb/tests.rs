#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    // `super` is the file-level of tests.rs (kg_redb::tests);
    // `super::super` is kg_redb itself, which re-exports KgStoreRedb,
    // BatchWriteOp, BatchOpResult, Triple, and Drawer.
    use super::super::*;
    use chrono::Utc;
    use std::path::PathBuf;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn open_kg() -> (tempfile::TempDir, KgStoreRedb) {
        let dir = tempdir().unwrap();
        let kg = KgStoreRedb::open(&dir.path().join("kg.redb")).unwrap();
        (dir, kg)
    }

    fn t(subject: &str, predicate: &str, object: &str) -> Triple {
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

    #[test]
    fn open_then_reopen_persists_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        {
            let kg = KgStoreRedb::open(&path).unwrap();
            kg.assert(&t("alice", "knows", "bob")).unwrap();
        }
        let kg = KgStoreRedb::open(&path).unwrap();
        let active = kg.query_active("alice").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].object, "bob");
    }

    #[test]
    fn assert_then_query_returns_triple() {
        let (_d, kg) = open_kg();
        kg.assert(&t("alice", "works_at", "Acme Corp")).unwrap();
        let active = kg.query_active("alice").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].object, "Acme Corp");
    }

    #[test]
    fn assert_supersedes_prior() {
        let (_d, kg) = open_kg();
        kg.assert(&t("alice", "works_at", "Acme")).unwrap();
        kg.assert(&t("alice", "works_at", "Beta")).unwrap();
        let active = kg.query_active("alice").unwrap();
        assert_eq!(active.len(), 1, "exactly one active row");
        assert_eq!(active[0].object, "Beta");

        // dump_all should include both — history + current.
        let all = kg.dump_all_triples().unwrap();
        assert_eq!(all.len(), 2);
        let objects: Vec<_> = all.iter().map(|x| x.object.as_str()).collect();
        assert!(objects.contains(&"Acme"));
        assert!(objects.contains(&"Beta"));
    }

    #[test]
    fn retract_closes_active_interval() {
        let (_d, kg) = open_kg();
        kg.assert(&t("tga", "is_alias_for", "trusty-git-analytics"))
            .unwrap();
        assert_eq!(kg.query_active("tga").unwrap().len(), 1);

        let closed = kg.retract("tga", "is_alias_for").unwrap();
        assert_eq!(closed, 1);
        assert!(kg.query_active("tga").unwrap().is_empty());

        // Second retract no-op.
        let again = kg.retract("tga", "is_alias_for").unwrap();
        assert_eq!(again, 0);

        // History row preserved.
        let all = kg.dump_all_triples().unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].valid_to.is_some());
    }

    #[test]
    fn list_subjects_returns_distinct_active_subjects() {
        let (_d, kg) = open_kg();
        assert!(kg.list_subjects(50).unwrap().is_empty());

        kg.assert(&t("bob", "knows", "alice")).unwrap();
        kg.assert(&t("alice", "knows", "bob")).unwrap();
        kg.assert(&t("alice", "knows", "carol")).unwrap(); // supersedes prior

        let subjects = kg.list_subjects(50).unwrap();
        assert_eq!(subjects, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn list_subjects_with_counts_returns_grouped_counts() {
        let (_d, kg) = open_kg();
        assert!(kg.list_subjects_with_counts(50).unwrap().is_empty());

        for (subj, pred) in [
            ("alice", "knows"),
            ("alice", "likes"),
            ("alice", "owns"),
            ("bob", "knows"),
        ] {
            kg.assert(&t(subj, pred, "thing")).unwrap();
        }

        let rows = kg.list_subjects_with_counts(50).unwrap();
        assert_eq!(rows, vec![("alice".to_string(), 3), ("bob".to_string(), 1)]);
    }

    #[test]
    fn count_active_triples_returns_live_only() {
        let (_d, kg) = open_kg();
        assert_eq!(kg.count_active_triples(), 0);

        kg.assert(&t("alice", "works_at", "Acme")).unwrap();
        assert_eq!(kg.count_active_triples(), 1);

        kg.assert(&t("alice", "works_at", "Beta")).unwrap();
        assert_eq!(kg.count_active_triples(), 1);

        kg.assert(&t("bob", "works_at", "Gamma")).unwrap();
        assert_eq!(kg.count_active_triples(), 2);

        kg.retract("alice", "works_at").unwrap();
        assert_eq!(kg.count_active_triples(), 1);
    }

    #[test]
    fn list_active_returns_ordered_window() {
        let (_d, kg) = open_kg();
        for i in 0..3 {
            kg.assert(&Triple {
                subject: format!("subj-{i}"),
                predicate: "rel".into(),
                object: format!("obj-{i}"),
                valid_from: Utc::now() + chrono::Duration::milliseconds(i * 10),
                valid_to: None,
                confidence: 1.0,
                provenance: None,
            })
            .unwrap();
        }

        let all = kg.list_active(10, 0).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].subject, "subj-2");
        assert_eq!(all[2].subject, "subj-0");

        let window = kg.list_active(2, 1).unwrap();
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].subject, "subj-1");
        assert_eq!(window[1].subject, "subj-0");
    }

    #[test]
    fn upsert_drawer_then_load_drawers_round_trips() {
        let (_d, kg) = open_kg();
        let room_id = Uuid::new_v4();
        let mut d = Drawer::new(room_id, "the cold-start drawer");
        d.importance = 0.83;
        d.tags = vec!["alpha".into(), "beta".into()];
        d.source_file = Some(PathBuf::from("/tmp/source.md"));
        kg.upsert_drawer(&d).unwrap();

        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, d.id);
        assert_eq!(loaded[0].room_id, room_id);
        assert_eq!(loaded[0].content, "the cold-start drawer");
        assert!((loaded[0].importance - 0.83).abs() < 1e-5);
        assert_eq!(loaded[0].tags, vec!["alpha".to_string(), "beta".into()]);
        assert_eq!(loaded[0].source_file, Some(PathBuf::from("/tmp/source.md")));
    }

    #[test]
    fn drawer_type_round_trips_through_redb() {
        // Issue #61: drawer_type + expires_at must survive a write/read.
        use crate::memory_core::palace::DrawerType;
        let (_d, kg) = open_kg();
        let room_id = Uuid::new_v4();
        let drawer =
            Drawer::new(room_id, "session event content").with_type(DrawerType::SessionEvent);
        kg.upsert_drawer(&drawer).unwrap();
        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].drawer_type, DrawerType::SessionEvent);
        assert!(
            loaded[0].expires_at.is_some(),
            "session events must carry a TTL"
        );
    }

    #[test]
    fn drawer_completed_at_round_trips_through_redb() {
        // spec-001: a Task drawer's type and optional completed_at timestamp
        // must survive a write/read through the new on-disk field.
        use crate::memory_core::palace::DrawerType;
        let (_d, kg) = open_kg();
        let mut drawer =
            Drawer::new(Uuid::new_v4(), "ship v2 milestone").with_type(DrawerType::Task);
        let done = chrono::Utc::now();
        drawer.completed_at = Some(done);
        kg.upsert_drawer(&drawer).unwrap();

        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].drawer_type, DrawerType::Task);
        assert!(loaded[0].expires_at.is_none(), "tasks never expire");
        let got = loaded[0].completed_at.expect("completed_at persisted");
        // redb stores millisecond precision; compare at that granularity.
        assert_eq!(got.timestamp_millis(), done.timestamp_millis());
    }

    #[test]
    fn load_drawer_ids_matches_load_drawers() {
        let (_d, kg) = open_kg();
        let room = Uuid::new_v4();
        let d1 = Drawer::new(room, "one");
        let d2 = Drawer::new(room, "two");
        kg.upsert_drawer(&d1).unwrap();
        kg.upsert_drawer(&d2).unwrap();
        let ids = kg.load_drawer_ids().unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&d1.id));
        assert!(ids.contains(&d2.id));
    }

    #[test]
    fn delete_drawer_removes_row() {
        let (_d, kg) = open_kg();
        let d = Drawer::new(Uuid::new_v4(), "to be deleted");
        kg.upsert_drawer(&d).unwrap();
        kg.delete_drawer(d.id).unwrap();
        assert!(kg.load_drawers().unwrap().is_empty());
    }

    #[test]
    fn upsert_drawer_replaces_existing_row() {
        let (_d, kg) = open_kg();
        let mut d = Drawer::new(Uuid::new_v4(), "original");
        kg.upsert_drawer(&d).unwrap();
        d.content = "updated".into();
        d.importance = 0.95;
        kg.upsert_drawer(&d).unwrap();
        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "updated");
        assert!((loaded[0].importance - 0.95).abs() < 1e-5);
    }

    /// Why: Production opens the same palace from multiple registries (test
    /// setup + `AppState`, foreground + dreamer). redb forbids two `Database`
    /// handles to one file; the cache must hand back the live handle so
    /// concurrent opens of the same path succeed.
    #[test]
    fn multiple_handles_to_same_path_share_database() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let a = KgStoreRedb::open(&path).unwrap();
        let b = KgStoreRedb::open(&path).unwrap();
        // Writes through one are visible through the other.
        a.assert(&t("alice", "knows", "bob")).unwrap();
        let active = b.query_active("alice").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].object, "bob");
    }

    #[test]
    fn checkpoint_is_noop() {
        let (_d, kg) = open_kg();
        kg.checkpoint().unwrap();
        kg.checkpoint().unwrap();
    }

    /// Why: `apply_batch` is the heart of the write-coalescing path —
    /// asserting multiple triples in one transaction must produce the
    /// same end state as calling `assert` N times.
    /// What: Submits a 5-op batch (4 asserts + 1 retract) and verifies
    /// the active set matches the expected result.
    /// Test ID: apply_batch_groups_asserts_into_single_commit.
    #[test]
    fn apply_batch_groups_asserts_into_single_commit() {
        let (_d, kg) = open_kg();
        let ops = vec![
            BatchWriteOp::Assert(t("a", "p1", "v1")),
            BatchWriteOp::Assert(t("a", "p2", "v2")),
            BatchWriteOp::Assert(t("b", "p1", "v3")),
            BatchWriteOp::Assert(t("a", "p1", "v1b")), // supersedes a/p1
            BatchWriteOp::Retract {
                subject: "a".to_string(),
                predicate: "p2".to_string(),
            },
        ];
        let results = kg.apply_batch(&ops).unwrap();
        assert_eq!(results.len(), 5);
        assert!(matches!(results[0], BatchOpResult::Asserted));
        assert!(matches!(results[3], BatchOpResult::Asserted));
        assert_eq!(results[4], BatchOpResult::Retracted(1));

        // Active state: a/p1 = v1b (latest), a/p2 retracted, b/p1 = v3.
        let a_active = kg.query_active("a").unwrap();
        assert_eq!(a_active.len(), 1);
        assert_eq!(a_active[0].predicate, "p1");
        assert_eq!(a_active[0].object, "v1b");

        let b_active = kg.query_active("b").unwrap();
        assert_eq!(b_active.len(), 1);
        assert_eq!(b_active[0].object, "v3");
    }

    /// Why: Empty batches must be safe — the writer may flush a coalesce
    /// window with zero queued ops if the caller dropped its sender
    /// between recv and drain.
    /// What: `apply_batch(&[])` returns `Ok(vec![])` and does not open a
    /// transaction (so write-locks are not contended for nothing).
    /// Test ID: apply_batch_empty_is_noop.
    #[test]
    fn apply_batch_empty_is_noop() {
        let (_d, kg) = open_kg();
        let results = kg.apply_batch(&[]).unwrap();
        assert!(results.is_empty());
    }

    /// Why: Drawer upserts must coexist with triple ops in the same
    /// transaction so a `remember` + `kg_assert` burst can be coalesced.
    /// What: Mixed batch with a drawer and a triple; both visible after.
    /// Test ID: apply_batch_mixes_drawer_and_triple_ops.
    #[test]
    fn apply_batch_mixes_drawer_and_triple_ops() {
        use crate::memory_core::palace::Drawer;
        let (_d, kg) = open_kg();
        let drawer = Drawer::new(Uuid::new_v4(), "hello world".to_string());
        let drawer_id = drawer.id;
        let ops = vec![
            BatchWriteOp::UpsertDrawer(drawer),
            BatchWriteOp::Assert(t("alice", "wrote", "drawer-1")),
        ];
        let results = kg.apply_batch(&ops).unwrap();
        assert_eq!(results.len(), 2);
        assert!(matches!(results[0], BatchOpResult::DrawerUpserted));
        assert!(matches!(results[1], BatchOpResult::Asserted));

        let drawer_ids = kg.load_drawer_ids().unwrap();
        assert!(drawer_ids.contains(&drawer_id));
        assert_eq!(kg.query_active("alice").unwrap().len(), 1);
    }

    // -- Issue #59 / #1152: cross-process lock + snapshot fallback -------------
    // `KgStoreRedb::open` uses `OpenIntent::ReadOnlyClient` so that when another
    // process holds the redb exclusive lock, we fall back to a read-only snapshot
    // (issue #59 behaviour). Writes against that snapshot are rejected via
    // `READ_ONLY_ERROR_MSG`. The issue #1152 guard against accidentally writing
    // to a snapshot is enforced at the storage layer (every write method checks
    // `is_read_only`) and at the daemon level (`single_instance_check` in
    // main.rs). The `OpenIntent::Writer` variant is available for callers that
    // need a loud Err on lock contention, but the default storage open path
    // preserves the snapshot fallback for read-only use cases.

    /// Hold the live redb file with a direct `Database::create` (bypassing
    /// the in-process `db_cache`) so the next `KgStoreRedb::open` triggers
    /// the lock-contention path. The returned `Database` must be kept
    /// alive for the duration of the test so the file lock is held.
    ///
    /// Why: Centralises the lock-from-another-handle pattern.
    /// What: Creates a redb file at `path` via the raw `redb` API; the
    /// returned handle owns the exclusive flock.
    /// Test: Indirect — lock-contention tests below.
    fn lock_redb_file(path: &std::path::Path) -> redb::Database {
        redb::Database::create(path).expect("first lock-holder open")
    }

    /// Why (issue #59 / #1152): `KgStoreRedb::open` uses
    /// `OpenIntent::ReadOnlyClient` — when a cross-process lock conflict occurs
    /// (another daemon holds the file), the caller gets a read-only snapshot
    /// handle rather than an error. Writes are rejected via `READ_ONLY_ERROR_MSG`
    /// so silent divergence is impossible.
    /// What: Seeds a palace file, drops the seeding store so the cache entry
    /// expires, locks the file via raw `Database::create`, then asserts the
    /// second `KgStoreRedb::open` SUCCEEDS in snapshot (read-only) mode.
    /// Test: this test.
    #[test]
    fn open_on_locked_file_returns_snapshot_handle() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        // Touch the file so it has the redb header.
        drop(KgStoreRedb::open(&path).unwrap());
        let _live = lock_redb_file(&path);

        let result = KgStoreRedb::open(&path);
        assert!(
            result.is_ok(),
            "ReadOnlyClient open on locked file must succeed via snapshot fallback"
        );
        let snap = result.expect("should be Ok");
        assert!(
            snap.is_read_only(),
            "snapshot handle must report is_read_only()"
        );
    }

    /// Why (issue #1487): the HTTP daemon opens with `OpenIntent::Writer`.
    /// When a second live instance already holds the redb write lock, the
    /// Writer open MUST fail loud (after the bounded handoff window) and MUST
    /// NOT degrade to a read-only snapshot — otherwise the daemon would
    /// silently reject every write for its lifetime (the original bug).
    /// What: Seeds a palace file, drops the seeding store so the cache entry
    /// expires, holds the file lock with a raw `Database::create`, then calls
    /// `KgStoreRedb::open_with_intent(.., Writer)`. The call must return `Err`
    /// whose message names the lock conflict — never an `Ok` snapshot handle.
    /// Test: this test.
    #[test]
    fn writer_intent_open_fails_loud_on_locked_file() {
        use crate::memory_core::store::concurrent_open::OpenIntent;
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        // Touch the file so it has the redb header, then expire the cache.
        drop(KgStoreRedb::open(&path).unwrap());
        let _live = lock_redb_file(&path);

        let result = KgStoreRedb::open_with_intent(&path, OpenIntent::Writer);
        // Match rather than `unwrap_err()` so we don't require KgStoreRedb: Debug.
        let err = match result {
            Ok(_) => {
                panic!("Writer open on a locked file must fail loud, not return a snapshot handle")
            }
            Err(e) => e,
        };
        let msg = format!("{err:#}");
        assert!(
            msg.contains("still locked") || msg.contains("write access"),
            "Writer error must name the lock conflict; got: {msg}"
        );
    }

    /// Why: Cached in-process handles to the same canonical path must be
    /// usable concurrently — multiple tasks holding cloned `KgStoreRedb`
    /// handles must each be able to issue reads simultaneously without
    /// blocking each other. Validates the cache + `Arc<KgDbState>`
    /// sharing.
    /// What: Opens the same path three times in the same process (all
    /// served from the cache), then issues `query_active` concurrently
    /// on three threads. All three must succeed and observe the same row.
    /// Test: this test.
    #[test]
    fn concurrent_readers_share_cached_state() {
        use std::thread;

        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let primary = KgStoreRedb::open(&path).unwrap();
        primary.assert(&t("alice", "knows", "bob")).unwrap();

        let a = KgStoreRedb::open(&path).unwrap();
        let b = KgStoreRedb::open(&path).unwrap();
        let c = KgStoreRedb::open(&path).unwrap();

        let handles: Vec<_> = [a, b, c]
            .into_iter()
            .map(|store| {
                thread::spawn(move || {
                    let active = store.query_active("alice").unwrap();
                    assert_eq!(active.len(), 1);
                    assert_eq!(active[0].object, "bob");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("reader thread panicked");
        }
    }
}
