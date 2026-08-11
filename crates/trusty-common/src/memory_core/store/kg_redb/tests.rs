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
        // #4810: `is_alias_for` is functional, so a second object closes the
        // first. Before #4810 every predicate behaved this way.
        let (_d, kg) = open_kg();
        kg.assert(&t("alice", "is_alias_for", "Acme")).unwrap();
        kg.assert(&t("alice", "is_alias_for", "Beta")).unwrap();
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
        assert_eq!(kg.count_active_triples().unwrap(), 0);

        kg.assert(&t("alice", "is_alias_for", "Acme")).unwrap();
        assert_eq!(kg.count_active_triples().unwrap(), 1);

        // #4810: functional predicate — the supersede keeps the count at 1.
        kg.assert(&t("alice", "is_alias_for", "Beta")).unwrap();
        assert_eq!(kg.count_active_triples().unwrap(), 1);

        kg.assert(&t("bob", "is_alias_for", "Gamma")).unwrap();
        assert_eq!(kg.count_active_triples().unwrap(), 2);

        kg.retract("alice", "is_alias_for").unwrap();
        assert_eq!(kg.count_active_triples().unwrap(), 1);
    }

    /// Why (#5384): a failed read used to come back as `0`, and `kg_query`
    /// turns a whole-graph `0` into `graph_state: "graph_empty"` — so a broken
    /// storage read claimed the graph held nothing while three live triples
    /// sat in it. Dropping ACTIVE_SUBJECT_COUNTS is the cheapest way to make
    /// `open_table` fail for real; before the fix this assertion read
    /// `assert_ne!(kg.count_active_triples(), 0)` and failed with `0 == 0`.
    #[test]
    fn count_active_triples_surfaces_read_failure() {
        let (_d, kg) = open_kg();
        kg.assert(&t("alice", "is_alias_for", "Acme")).unwrap();
        kg.assert(&t("bob", "is_alias_for", "Beta")).unwrap();
        kg.assert(&t("carol", "is_alias_for", "Gamma")).unwrap();
        assert_eq!(kg.count_active_triples().unwrap(), 3);

        let wtx = kg.db().begin_write().unwrap();
        assert!(
            wtx.delete_table(crate::memory_core::store::kg_store::ACTIVE_SUBJECT_COUNTS)
                .unwrap(),
            "the count table must have existed to be dropped"
        );
        wtx.commit().unwrap();

        let err = kg
            .count_active_triples()
            .expect_err("a failed count read must not be reported as a count");
        assert!(
            format!("{err:#}").contains("active_subject_counts"),
            "error names the table it could not read: {err:#}"
        );
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
    fn pre_task_drawer_row_migrates_completed_at_to_none() {
        // spec-001 migration regression: a drawer row written *before* the
        // `completed_at_ms` field existed (the #61-era `PreTaskDrawerRecord`
        // layout) must still decode. Postcard is positional, so decoding such
        // bytes as the current `DrawerRecord` fails and the reader falls back
        // through `PreTaskDrawerRecord` — see the fallback chain in
        // `read_ops.rs::load_drawers`. The migrated row must keep its
        // drawer_type / expires_at and default `completed_at` to `None`.
        use crate::memory_core::palace::DrawerType;
        use crate::memory_core::store::kg_store::{DRAWERS, encode_value};
        use redb::Database;

        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let id = Uuid::new_v4();

        // Write the legacy-shaped bytes directly, then drop the raw handle so
        // `KgStoreRedb::open` can acquire its own (redb forbids two in-process
        // handles to one file).
        {
            let old = super::super::types::PreTaskDrawerRecord {
                room_id: Uuid::new_v4().to_string(),
                content: "pre-spec-001 task row".to_string(),
                importance: 0.6,
                tags: vec!["legacy".to_string()],
                source_file: None,
                created_at_ms: 1_700_000_000_000,
                drawer_type: Some("Task".to_string()),
                expires_at_ms: None,
            };
            let bytes = encode_value(&old).expect("encode legacy record");
            let db = Database::create(&path).expect("create redb");
            let wtx = db.begin_write().expect("begin write");
            {
                let mut table = wtx.open_table(DRAWERS).expect("open drawers");
                table
                    .insert(id.as_bytes().as_slice(), bytes.as_slice())
                    .expect("insert legacy drawer");
            }
            wtx.commit().expect("commit");
        }

        let kg = KgStoreRedb::open(&path).expect("reopen via KgStoreRedb");
        let loaded = kg.load_drawers().expect("load drawers");
        assert_eq!(loaded.len(), 1, "legacy row must decode, not be skipped");
        assert_eq!(loaded[0].id, id);
        assert_eq!(loaded[0].content, "pre-spec-001 task row");
        assert_eq!(
            loaded[0].drawer_type,
            DrawerType::Task,
            "drawer_type preserved through migration"
        );
        assert!(loaded[0].expires_at.is_none());
        assert!(
            loaded[0].completed_at.is_none(),
            "missing completed_at_ms field must migrate to None"
        );
    }

    /// #4884: a `fact_key` written through the normal upsert path must come
    /// back off disk intact, and a drawer that claims no slot must come back
    /// claiming none.
    #[test]
    fn drawer_fact_key_round_trips_through_redb() {
        let (_d, kg) = open_kg();
        let mut slotted = Drawer::new(Uuid::new_v4(), "PR #4818 is in flight");
        slotted.fact_key = Some("pr:4818/state".to_string());
        let slotted_id = slotted.id;
        let plain = Drawer::new(Uuid::new_v4(), "no slot claimed");
        let plain_id = plain.id;

        kg.upsert_drawer(&slotted).unwrap();
        kg.upsert_drawer(&plain).unwrap();

        let loaded = kg.load_drawers().unwrap();
        let got_slotted = loaded.iter().find(|d| d.id == slotted_id).expect("slotted");
        let got_plain = loaded.iter().find(|d| d.id == plain_id).expect("plain");
        assert_eq!(got_slotted.fact_key.as_deref(), Some("pr:4818/state"));
        assert_eq!(got_plain.fact_key, None);
    }

    /// #4884 migration regression: a row written BEFORE `fact_key` existed —
    /// the spec-001-era `PreFactKeyDrawerRecord` layout — must still decode.
    /// Postcard is positional, so those bytes fail as the current
    /// `DrawerRecord`; the reader falls back through `PreFactKeyDrawerRecord`.
    /// The link matters: without it the walk would skip to
    /// `PreTaskDrawerRecord` and silently drop `completed_at`, marking a
    /// finished task open again. No existing row is rewritten to add the field.
    #[test]
    fn pre_fact_key_drawer_row_migrates_fact_key_to_none() {
        use crate::memory_core::palace::DrawerType;
        use crate::memory_core::store::kg_store::{DRAWERS, encode_value};
        use redb::Database;

        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let id = Uuid::new_v4();
        let completed_ms = 1_720_000_000_000i64;

        {
            let old = super::super::types::PreFactKeyDrawerRecord {
                room_id: Uuid::new_v4().to_string(),
                content: "pre-#4884 task row".to_string(),
                importance: 0.6,
                tags: vec!["legacy".to_string()],
                source_file: None,
                created_at_ms: 1_700_000_000_000,
                drawer_type: Some("Task".to_string()),
                expires_at_ms: None,
                completed_at_ms: Some(completed_ms),
            };
            let bytes = encode_value(&old).expect("encode pre-#4884 record");
            let db = Database::create(&path).expect("create redb");
            let wtx = db.begin_write().expect("begin write");
            {
                let mut table = wtx.open_table(DRAWERS).expect("open drawers");
                table
                    .insert(id.as_bytes().as_slice(), bytes.as_slice())
                    .expect("insert pre-#4884 drawer");
            }
            wtx.commit().expect("commit");
        }

        let kg = KgStoreRedb::open(&path).expect("reopen via KgStoreRedb");
        let loaded = kg.load_drawers().expect("load drawers");
        assert_eq!(loaded.len(), 1, "pre-#4884 row must decode, not be skipped");
        assert_eq!(loaded[0].id, id);
        assert_eq!(loaded[0].content, "pre-#4884 task row");
        assert_eq!(loaded[0].drawer_type, DrawerType::Task);
        assert!(
            loaded[0].fact_key.is_none(),
            "missing fact_key field must migrate to None"
        );
        assert_eq!(
            loaded[0].completed_at.map(|d| d.timestamp_millis()),
            Some(completed_ms),
            "completed_at must survive — that is why this fallback shape exists"
        );
    }

    /// #4884: the index must gain an entry on write and lose it on delete. A
    /// surviving entry would make the ADR-0028 D5 occupancy check report a slot
    /// as taken by a drawer that no longer exists.
    #[test]
    fn fact_key_index_tracks_upsert_and_delete() {
        let (_d, kg) = open_kg();
        assert_eq!(
            kg.drawer_id_for_fact_key("pr:4818/state").unwrap(),
            None,
            "an untouched slot is free"
        );

        let mut drawer = Drawer::new(Uuid::new_v4(), "PR #4818 is in flight");
        drawer.fact_key = Some("pr:4818/state".to_string());
        let id = drawer.id;
        kg.upsert_drawer(&drawer).unwrap();
        assert_eq!(
            kg.drawer_id_for_fact_key("pr:4818/state").unwrap(),
            Some(id)
        );

        kg.delete_drawer(id).unwrap();
        assert_eq!(
            kg.drawer_id_for_fact_key("pr:4818/state").unwrap(),
            None,
            "deleting the occupant must free the slot"
        );
    }

    /// #4884: writing a slot another drawer holds moves the index onto the new
    /// drawer — "one slot, one live fact" (ADR-0028 D5).
    #[test]
    fn fact_key_index_follows_the_slot_on_reassignment() {
        let (_d, kg) = open_kg();
        let room = Uuid::new_v4();

        let mut first = Drawer::new(room, "PR #4818 at head d3963848");
        first.fact_key = Some("pr:4818/state".to_string());
        let first_id = first.id;
        kg.upsert_drawer(&first).unwrap();

        let mut second = Drawer::new(room, "PR #4818 merged at head 59ae50d8");
        second.fact_key = Some("pr:4818/state".to_string());
        let second_id = second.id;
        kg.upsert_drawer(&second).unwrap();

        assert_eq!(
            kg.drawer_id_for_fact_key("pr:4818/state").unwrap(),
            Some(second_id),
            "the newest writer owns the slot"
        );
        // Storage does not retire the displaced drawer — that is the Tier C
        // write path's job. It must still be readable (ADR-0028 D6: demoted,
        // never deleted).
        let loaded = kg.load_drawers().unwrap();
        assert!(loaded.iter().any(|d| d.id == first_id));
        assert_eq!(loaded.len(), 2);
    }

    /// #4884: re-upserting a drawer with its `fact_key` cleared must release
    /// the slot. Leaving the entry behind would point the index at a drawer
    /// that no longer claims the key.
    #[test]
    fn clearing_a_fact_key_drops_the_index_entry() {
        let (_d, kg) = open_kg();
        let mut drawer = Drawer::new(Uuid::new_v4(), "temporarily slotted");
        drawer.fact_key = Some("ws:tm-03/resume".to_string());
        kg.upsert_drawer(&drawer).unwrap();
        assert!(
            kg.drawer_id_for_fact_key("ws:tm-03/resume")
                .unwrap()
                .is_some()
        );

        drawer.fact_key = None;
        kg.upsert_drawer(&drawer).unwrap();
        assert_eq!(
            kg.drawer_id_for_fact_key("ws:tm-03/resume").unwrap(),
            None,
            "clearing the key must free the slot"
        );
        // The drawer itself survives; only its slot claim was dropped.
        let loaded = kg.load_drawers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].fact_key, None);
    }

    /// #4884: the ownership guard. A drawer that lost its slot to a newer
    /// writer must not evict that newer writer's index entry when it is later
    /// deleted — an unguarded removal would report a slot as free while a live
    /// drawer occupies it, and the next Tier C write would then fail to retire
    /// the real occupant.
    #[test]
    fn deleting_a_drawer_that_lost_its_slot_leaves_the_new_owner_indexed() {
        let (_d, kg) = open_kg();
        let room = Uuid::new_v4();

        let mut first = Drawer::new(room, "stale state");
        first.fact_key = Some("pr:4818/state".to_string());
        let first_id = first.id;
        kg.upsert_drawer(&first).unwrap();

        let mut second = Drawer::new(room, "current state");
        second.fact_key = Some("pr:4818/state".to_string());
        let second_id = second.id;
        kg.upsert_drawer(&second).unwrap();

        kg.delete_drawer(first_id).unwrap();

        assert_eq!(
            kg.drawer_id_for_fact_key("pr:4818/state").unwrap(),
            Some(second_id),
            "deleting the displaced drawer must not free the live owner's slot"
        );
    }

    /// #4884: `apply_batch` shares the same helpers as the single-op path, so a
    /// batched upsert-then-delete must leave the index in the same state a pair
    /// of individual calls would.
    #[test]
    fn batched_drawer_ops_maintain_the_fact_key_index() {
        let (_d, kg) = open_kg();
        let room = Uuid::new_v4();
        let mut kept = Drawer::new(room, "kept");
        kept.fact_key = Some("daemon:trusty-search/install-state".to_string());
        let kept_id = kept.id;
        let mut dropped = Drawer::new(room, "dropped");
        dropped.fact_key = Some("pr:4818/state".to_string());
        let dropped_id = dropped.id;

        kg.apply_batch(&[
            BatchWriteOp::UpsertDrawer(kept),
            BatchWriteOp::UpsertDrawer(dropped),
            BatchWriteOp::DeleteDrawer(dropped_id),
        ])
        .unwrap();

        assert_eq!(
            kg.drawer_id_for_fact_key("daemon:trusty-search/install-state")
                .unwrap(),
            Some(kept_id)
        );
        assert_eq!(kg.drawer_id_for_fact_key("pr:4818/state").unwrap(), None);
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
            BatchWriteOp::Assert(t("a", "is_alias_for", "v1")),
            BatchWriteOp::Assert(t("a", "p2", "v2")),
            BatchWriteOp::Assert(t("b", "is_alias_for", "v3")),
            // #4810: supersedes only because `is_alias_for` is functional.
            BatchWriteOp::Assert(t("a", "is_alias_for", "v1b")),
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

        // Active state: a/is_alias_for = v1b (latest), a/p2 retracted,
        // b/is_alias_for = v3.
        let a_active = kg.query_active("a").unwrap();
        assert_eq!(a_active.len(), 1);
        assert_eq!(a_active[0].predicate, "is_alias_for");
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
    // ---------------------------------------------------------------------
    // #4810 — the object joins the TRIPLES key
    // ---------------------------------------------------------------------

    /// Encode a key in the PRE-#4810 shape: `[subject_len][subject][predicate]`,
    /// with no length prefix on the predicate and no object.
    ///
    /// Why: the migration tests must be able to write a palace that looks
    /// exactly like one written by the old code. Nothing in production emits
    /// this shape any more, which is precisely why the test has to.
    fn legacy_triple_key(subject: &str, predicate: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(subject.len() as u16).to_be_bytes());
        out.extend_from_slice(subject.as_bytes());
        out.extend_from_slice(predicate.as_bytes());
        out
    }

    /// Write `rows` into a fresh redb file using the pre-#4810 key shape and
    /// leave no `KG_SCHEMA` marker, so the next `KgStoreRedb::open` sees a
    /// palace that predates the fix.
    fn seed_legacy_palace(path: &std::path::Path, rows: &[(&str, &str, &str, Option<i64>)]) {
        use crate::memory_core::store::kg_store::{
            ACTIVE_SUBJECT_COUNTS, TRIPLES, TripleValue, encode_u64, encode_value,
        };
        let db = redb::Database::create(path).expect("create legacy palace");
        let wtx = db.begin_write().unwrap();
        {
            let mut triples = wtx.open_table(TRIPLES).unwrap();
            let mut counts = wtx.open_table(ACTIVE_SUBJECT_COUNTS).unwrap();
            let mut per_subject: std::collections::BTreeMap<&str, u64> =
                std::collections::BTreeMap::new();
            for (s, p, o, valid_to_ms) in rows {
                let value = TripleValue {
                    object: (*o).to_string(),
                    valid_from_ms: 1_700_000_000_000,
                    valid_to_ms: *valid_to_ms,
                    confidence: 1.0,
                    provenance: None,
                };
                let core = legacy_triple_key(s, p);
                let key = if valid_to_ms.is_some() {
                    let mut k = Vec::new();
                    k.extend_from_slice(b"hist:");
                    k.extend_from_slice(&core);
                    k.extend_from_slice(&value.valid_from_ms.to_be_bytes());
                    k
                } else {
                    *per_subject.entry(s).or_insert(0) += 1;
                    core
                };
                let bytes = encode_value(&value).unwrap();
                triples.insert(key.as_slice(), bytes.as_slice()).unwrap();
            }
            for (s, n) in per_subject {
                counts
                    .insert(s.as_bytes(), encode_u64(n).as_slice())
                    .unwrap();
            }
        }
        wtx.commit().unwrap();
        drop(db);
    }

    /// Assert `backup` is a readable redb image that still holds the row at the
    /// PRE-migration key — the property that makes it a recovery point, and one
    /// a byte-count comparison cannot check (the live file grows during open).
    fn assert_backup_holds_legacy_row(backup: &std::path::Path, subject: &str, predicate: &str) {
        use crate::memory_core::store::kg_store::TRIPLES;
        use redb::ReadableDatabase;
        assert!(backup.is_file(), "backup must be a regular file");
        let db = redb::Database::create(backup).expect("open backup as redb");
        let rtx = db.begin_read().unwrap();
        let triples = rtx.open_table(TRIPLES).unwrap();
        let key = legacy_triple_key(subject, predicate);
        assert!(
            triples.get(key.as_slice()).unwrap().is_some(),
            "the backup must still carry the pre-migration row"
        );
        drop(rtx);
        drop(db);
    }

    /// Why (#4810): the defect. `room:General --contains--> drawer:N` keyed on
    /// `(subject, predicate)` alone, so each new member closed the last one and
    /// a room of any size reported exactly one drawer. This test FAILS on
    /// `e2ca949a3`.
    /// What: three `contains` asserts under one subject; all three must be
    /// active, and the active counter must agree.
    #[test]
    fn assert_multiple_objects_for_multivalued_predicate_all_survive() {
        let (_d, kg) = open_kg();
        kg.assert(&t("room:General", "contains", "drawer:a"))
            .unwrap();
        kg.assert(&t("room:General", "contains", "drawer:b"))
            .unwrap();
        kg.assert(&t("room:General", "contains", "drawer:c"))
            .unwrap();

        let active = kg.query_active("room:General").unwrap();
        assert_eq!(active.len(), 3, "every member of the room stays active");
        let mut objects: Vec<_> = active.iter().map(|x| x.object.as_str()).collect();
        objects.sort_unstable();
        assert_eq!(objects, vec!["drawer:a", "drawer:b", "drawer:c"]);
        assert_eq!(kg.count_active_triples().unwrap(), 3);

        // Nothing was demoted to history — no object was superseded.
        let all = kg.dump_all_triples().unwrap();
        assert_eq!(all.len(), 3, "no history rows for a multi-valued predicate");
    }

    /// Why (#4810): the other half of the split — a functional predicate must
    /// keep its one-active-object rule, or `is_alias_for` would accumulate
    /// every alias a subject ever had and prompt-fact injection would grow
    /// without bound.
    /// What: two `is_alias_for` asserts under one subject leave one active row
    /// carrying the newer object, plus one history row.
    #[test]
    fn assert_functional_predicate_still_supersedes() {
        let (_d, kg) = open_kg();
        kg.assert(&t("tga", "is_alias_for", "trusty-git-analytics"))
            .unwrap();
        kg.assert(&t("tga", "is_alias_for", "trusty-git-analytics-v2"))
            .unwrap();

        let active = kg.query_active("tga").unwrap();
        assert_eq!(active.len(), 1, "functional predicate holds one object");
        assert_eq!(active[0].object, "trusty-git-analytics-v2");
        assert_eq!(kg.count_active_triples().unwrap(), 1);

        let all = kg.dump_all_triples().unwrap();
        assert_eq!(all.len(), 2, "the superseded object is kept as history");
    }

    /// Why (#4810): re-asserting the SAME triple must stay a re-affirmation —
    /// a new interval over one row — not an accumulation of duplicates.
    #[test]
    fn reasserting_the_same_triple_reaffirms_rather_than_duplicates() {
        let (_d, kg) = open_kg();
        kg.assert(&t("room:General", "contains", "drawer:a"))
            .unwrap();
        kg.assert(&t("room:General", "contains", "drawer:a"))
            .unwrap();

        let active = kg.query_active("room:General").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(kg.count_active_triples().unwrap(), 1);
        assert_eq!(kg.dump_all_triples().unwrap().len(), 2, "one history row");
    }

    /// Why (#4810): `retract(subject, predicate)` kept its two-argument shape,
    /// so its meaning had to widen from "the active row" to "every active
    /// row" — otherwise retracting a multi-valued pair would leave the other
    /// objects live and the caller with no way to reach them.
    #[test]
    fn retract_closes_every_object_at_the_pair() {
        let (_d, kg) = open_kg();
        for object in ["drawer:a", "drawer:b", "drawer:c"] {
            kg.assert(&t("room:General", "contains", object)).unwrap();
        }
        assert_eq!(kg.retract("room:General", "contains").unwrap(), 3);
        assert!(kg.query_active("room:General").unwrap().is_empty());
        assert_eq!(kg.count_active_triples().unwrap(), 0);
        // Second retract is a no-op.
        assert_eq!(kg.retract("room:General", "contains").unwrap(), 0);
    }

    /// Why (#5396): this is the whole reason the three-argument shape exists.
    /// A test that only checked the bad object was gone would pass against
    /// two-argument `retract`, which takes the entire pair down with it — so
    /// the surviving siblings are the assertion that distinguishes the two.
    #[test]
    fn retract_triple_closes_one_object_and_leaves_siblings_active() {
        let (_d, kg) = open_kg();
        for object in ["drawer:a", "drawer:b", "drawer:c"] {
            kg.assert(&t("room:General", "contains", object)).unwrap();
        }

        assert_eq!(
            kg.retract_triple("room:General", "contains", "drawer:b")
                .unwrap(),
            1
        );

        let mut objects: Vec<String> = kg
            .query_active("room:General")
            .unwrap()
            .into_iter()
            .map(|x| x.object)
            .collect();
        objects.sort();
        assert_eq!(
            objects,
            vec!["drawer:a".to_string(), "drawer:c".to_string()],
            "the good siblings must survive"
        );
        assert_eq!(kg.count_active_triples().unwrap(), 2);

        // The retracted object is closed, not erased.
        let closed: Vec<_> = kg
            .dump_all_triples()
            .unwrap()
            .into_iter()
            .filter(|x| x.object == "drawer:b")
            .collect();
        assert_eq!(closed.len(), 1);
        assert!(closed[0].valid_to.is_some(), "history row carries valid_to");
    }

    /// Why (#5396): a caller cleaning object-side noise scans for candidates
    /// and retracts them one at a time. An object that is already gone (or was
    /// never there) is the normal outcome of a re-run, not an error.
    #[test]
    fn retract_triple_on_an_absent_object_is_a_noop() {
        let (_d, kg) = open_kg();
        kg.assert(&t("room:General", "contains", "drawer:a"))
            .unwrap();

        assert_eq!(
            kg.retract_triple("room:General", "contains", "drawer:zzz")
                .unwrap(),
            0,
            "unknown object"
        );
        assert_eq!(
            kg.retract_triple("room:Other", "contains", "drawer:a")
                .unwrap(),
            0,
            "unknown subject"
        );
        assert_eq!(
            kg.retract_triple("room:General", "mentions", "drawer:a")
                .unwrap(),
            0,
            "unknown predicate"
        );
        assert_eq!(kg.query_active("room:General").unwrap().len(), 1);
        assert_eq!(kg.count_active_triples().unwrap(), 1);
    }

    /// Why (#5396): `retract_triple` addresses one row by its full key, so a
    /// functional predicate gets no special "close every object" treatment —
    /// naming the wrong object must leave the right one alone even there.
    #[test]
    fn retract_triple_on_a_functional_predicate_closes_only_the_named_object() {
        let (_d, kg) = open_kg();
        kg.assert(&t("tga", "is_alias_for", "trusty-git-analytics"))
            .unwrap();

        assert_eq!(
            kg.retract_triple("tga", "is_alias_for", "something-else")
                .unwrap(),
            0
        );
        assert_eq!(kg.query_active("tga").unwrap().len(), 1);

        assert_eq!(
            kg.retract_triple("tga", "is_alias_for", "trusty-git-analytics")
                .unwrap(),
            1
        );
        assert!(kg.query_active("tga").unwrap().is_empty());
        assert_eq!(kg.count_active_triples().unwrap(), 0);
    }

    /// Why (#5396): the last object at a pair is the boundary case for the
    /// active-subject counter — it must reach zero and stay there, so a second
    /// call finds nothing rather than driving the count negative.
    #[test]
    fn retract_triple_on_the_only_object_clears_the_active_count() {
        let (_d, kg) = open_kg();
        kg.assert(&t("room:General", "contains", "drawer:a"))
            .unwrap();

        assert_eq!(
            kg.retract_triple("room:General", "contains", "drawer:a")
                .unwrap(),
            1
        );
        assert!(kg.query_active("room:General").unwrap().is_empty());
        assert_eq!(kg.count_active_triples().unwrap(), 0);

        assert_eq!(
            kg.retract_triple("room:General", "contains", "drawer:a")
                .unwrap(),
            0,
            "second call is a no-op"
        );
        assert_eq!(kg.count_active_triples().unwrap(), 0);
    }

    /// Why (#4810): `delete_by_subject` collects pairs, and one pair can now
    /// span several rows. Without the dedup it would call `retract` once per
    /// row and double-count what the first call already closed.
    #[test]
    fn cascade_delete_closes_every_object_of_a_multivalued_pair() {
        let (_d, kg) = open_kg();
        for object in ["a", "b", "c"] {
            kg.assert(&t("drawer:x", "mentions", object)).unwrap();
        }
        kg.assert(&t("drawer:x", "is_alias_for", "y")).unwrap();
        assert_eq!(kg.delete_by_subject("drawer:x").unwrap(), 4);
        assert!(kg.query_active("drawer:x").unwrap().is_empty());
        assert_eq!(kg.count_active_triples().unwrap(), 0);
    }

    /// Why (#4810): the write path reads the `(subject, predicate)` range and
    /// then mutates it. If two writers could interleave that read-modify-write,
    /// one object would be lost. redb serialises write transactions, and this
    /// pins that guarantee to the behaviour that depends on it.
    /// What: eight threads assert a distinct object under one multi-valued
    /// pair through separate (cache-shared) handles; all eight must survive.
    #[test]
    fn concurrent_asserts_to_one_pair_lose_no_object() {
        use std::thread;
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let primary = KgStoreRedb::open(&path).unwrap();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = KgStoreRedb::open(&path).unwrap();
                thread::spawn(move || {
                    store
                        .assert(&t("room:General", "contains", &format!("drawer:{i}")))
                        .expect("concurrent assert");
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer thread panicked");
        }

        let active = primary.query_active("room:General").unwrap();
        assert_eq!(active.len(), 8, "no concurrent assert overwrote another");
        assert_eq!(primary.count_active_triples().unwrap(), 8);
    }

    /// Why (#5396): `retract_triple` reads the `(subject, predicate)` range,
    /// closes the one row whose object matches, then folds a NEGATIVE delta into
    /// the subject's active counter. Those are two read-modify-writes over rows
    /// that every concurrent retract at the same pair contends for, and
    /// `adjust_active_count` clamps with `saturating_sub` — so a lost decrement
    /// never goes negative and panics. It leaves a phantom count standing over
    /// zero live rows, silently and permanently.
    /// `concurrent_asserts_to_one_pair_lose_no_object` pins only the additive
    /// side of that counter, and closing rows takes a different path than
    /// inserting them.
    /// What: eight threads each retract a DIFFERENT object of one eight-valued
    /// pair through separate (cache-shared) handles. Every call must close
    /// exactly its own row, every row must end up in history, and the counter
    /// must reach zero rather than drift.
    #[test]
    fn concurrent_retract_triples_at_one_pair_close_every_object() {
        use std::thread;
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let primary = KgStoreRedb::open(&path).unwrap();
        for i in 0..8 {
            primary
                .assert(&t("room:General", "contains", &format!("drawer:{i}")))
                .unwrap();
        }
        assert_eq!(
            primary.count_active_triples().unwrap(),
            8,
            "seeded eight active"
        );

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let store = KgStoreRedb::open(&path).unwrap();
                thread::spawn(move || {
                    store
                        .retract_triple("room:General", "contains", &format!("drawer:{i}"))
                        .expect("concurrent retract_triple")
                })
            })
            .collect();
        let closed: Vec<usize> = handles
            .into_iter()
            .map(|h| h.join().expect("retract thread panicked"))
            .collect();

        assert_eq!(
            closed,
            vec![1; 8],
            "each retract closed exactly its own row — no double-close, no lost update"
        );
        assert!(
            primary.query_active("room:General").unwrap().is_empty(),
            "every object closed"
        );
        assert_eq!(
            primary.count_active_triples().unwrap(),
            0,
            "the counter absorbed all eight decrements"
        );

        // Every row was closed in place, not dropped: eight history rows remain.
        let all = primary.dump_all_triples().unwrap();
        assert_eq!(all.len(), 8, "no retraction lost its history row");
        assert!(
            all.iter().all(|tr| tr.valid_to.is_some()),
            "no row was left active"
        );
    }

    /// Why (#5396): the cleanup pass this method exists for runs against a live
    /// palace, so a retract of one object races an assert of another at the same
    /// multi-valued pair. Both fold a delta into the same counter row, in
    /// opposite directions — the interleaving where one side's read-modify-write
    /// overwrites the other's is exactly what leaves the count disagreeing with
    /// the rows it counts.
    /// What: eight threads retract the eight seeded objects while eight more
    /// assert eight fresh ones. `contains` is multi-valued, so an assert
    /// supersedes nothing and the two object sets stay disjoint — which makes
    /// the outcome interleaving-independent: the seeded eight end closed, the
    /// fresh eight end active, and the counter reads eight.
    #[test]
    fn concurrent_retract_triple_and_assert_at_one_pair_agree_on_the_count() {
        use std::thread;
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let primary = KgStoreRedb::open(&path).unwrap();
        for i in 0..8 {
            primary
                .assert(&t("room:General", "contains", &format!("seeded:{i}")))
                .unwrap();
        }

        let mut handles = Vec::new();
        for i in 0..8 {
            let store = KgStoreRedb::open(&path).unwrap();
            handles.push(thread::spawn(move || {
                store
                    .retract_triple("room:General", "contains", &format!("seeded:{i}"))
                    .expect("concurrent retract_triple");
            }));
            let store = KgStoreRedb::open(&path).unwrap();
            handles.push(thread::spawn(move || {
                store
                    .assert(&t("room:General", "contains", &format!("fresh:{i}")))
                    .expect("concurrent assert");
            }));
        }
        for h in handles {
            h.join().expect("writer thread panicked");
        }

        let mut active: Vec<String> = primary
            .query_active("room:General")
            .unwrap()
            .into_iter()
            .map(|tr| tr.object)
            .collect();
        active.sort();
        let expected: Vec<String> = (0..8).map(|i| format!("fresh:{i}")).collect();
        assert_eq!(
            active, expected,
            "the asserted objects survived and the retracted ones did not"
        );
        assert_eq!(
            primary.count_active_triples().unwrap(),
            8,
            "the counter agrees with the rows after eight closes and eight opens"
        );
    }

    /// Why (#4810): a fresh palace has nothing to rewrite but must still record
    /// which key shape it uses, or every open would re-scan every triple.
    /// What: a new palace carries the marker; a second open is a no-op and
    /// leaves no backup behind.
    #[test]
    fn migration_stamps_schema_and_is_idempotent() {
        use crate::memory_core::store::kg_store::{
            KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION, KgSchemaMarker,
            decode_value,
        };
        use redb::ReadableDatabase;

        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        let read_marker = |p: &std::path::Path| -> Option<u32> {
            let db = redb::Database::create(p).unwrap();
            let rtx = db.begin_read().unwrap();
            let table = rtx.open_table(KG_SCHEMA).unwrap();
            let out = table.get(KG_SCHEMA_TRIPLE_KEY).unwrap().map(|g| {
                decode_value::<KgSchemaMarker>(g.value())
                    .unwrap()
                    .schema_version
            });
            drop(rtx);
            drop(db);
            out
        };

        {
            let kg = KgStoreRedb::open(&path).unwrap();
            kg.assert(&t("alice", "knows", "bob")).unwrap();
        }
        assert_eq!(read_marker(&path), Some(KG_TRIPLE_KEY_SCHEMA_VERSION));

        // An already-migrated palace is not backed up and not rewritten.
        {
            let kg = KgStoreRedb::open(&path).unwrap();
            assert_eq!(kg.query_active("alice").unwrap().len(), 1);
        }
        let backup = dir.path().join("kg.redb.pre-4810.bak");
        assert!(
            !backup.exists(),
            "no backup for a palace with nothing to do"
        );
    }

    /// Why (#4810): the whole point of the migration — rows written under the
    /// old key must be readable, and readable as the facts they were, after
    /// one open. History rows carry the same key and must move with them.
    /// What: seeds a legacy palace with two active rows and one closed row —
    /// note that the legacy key physically CANNOT hold two objects for one
    /// pair, which is the defect — opens it, and checks every row is
    /// queryable at its new key and the history survived.
    #[test]
    fn migration_rewrites_legacy_keys_and_preserves_history() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        seed_legacy_palace(
            &path,
            &[
                ("room:General", "contains", "drawer:a", None),
                ("alice", "knows", "bob", None),
                ("alice", "knows", "carol", Some(1_700_000_100_000)),
            ],
        );

        let kg = KgStoreRedb::open(&path).unwrap();
        let room = kg.query_active("room:General").unwrap();
        assert_eq!(room.len(), 1);
        assert_eq!(room[0].object, "drawer:a");

        let alice = kg.query_active("alice").unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].object, "bob");

        // The closed row moved with the active ones.
        let all = kg.dump_all_triples().unwrap();
        assert_eq!(all.len(), 3);
        assert!(
            all.iter()
                .any(|x| x.object == "carol" && x.valid_to.is_some())
        );

        // A new member now joins instead of replacing.
        kg.assert(&t("room:General", "contains", "drawer:b"))
            .unwrap();
        assert_eq!(kg.query_active("room:General").unwrap().len(), 2);

        // The pre-migration image was preserved.
        let backup = dir.path().join("kg.redb.pre-4810.bak");
        assert_backup_holds_legacy_row(&backup, "room:General", "contains");
    }

    /// Why (#4810), the fail-open check: the migration rewrites every triple in
    /// the palace, so its failure branch is the one that must be reviewable.
    /// A failed attempt must commit nothing, must not stop the palace opening,
    /// and must be retried on the next open rather than latched off.
    /// What: makes the backup step fail by putting a DIRECTORY where the `.bak`
    /// file belongs — `fs::copy` cannot write over it — then asserts the open
    /// succeeds, the legacy rows are still on disk untouched, and removing the
    /// blocker lets the next open migrate them for real.
    #[test]
    fn migration_failure_leaves_the_palace_openable_and_retries() {
        use crate::memory_core::store::kg_store::TRIPLES;
        use redb::ReadableDatabase;

        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        seed_legacy_palace(
            &path,
            &[
                ("room:General", "contains", "drawer:a", None),
                ("room:Other", "contains", "drawer:b", None),
            ],
        );

        // Block the backup: a directory at the backup path makes `fs::copy`
        // fail, so the migration aborts before it opens a write transaction.
        let backup = dir.path().join("kg.redb.pre-4810.bak");
        std::fs::create_dir(&backup).unwrap();

        {
            let kg = KgStoreRedb::open(&path).unwrap();
            assert!(
                !kg.is_read_only(),
                "a failed migration must not degrade the handle"
            );
            // Un-migrated rows do not decode under the new key, so queries are
            // empty. That is the documented degraded state — not data loss.
            assert!(kg.query_active("room:General").unwrap().is_empty());
        }

        // Nothing was committed: both legacy rows are still there, byte-shaped
        // exactly as seeded.
        {
            let db = redb::Database::create(&path).unwrap();
            let rtx = db.begin_read().unwrap();
            let triples = rtx.open_table(TRIPLES).unwrap();
            let legacy = legacy_triple_key("room:General", "contains");
            assert!(
                triples.get(legacy.as_slice()).unwrap().is_some(),
                "the failed migration must not have removed the legacy row"
            );
            drop(rtx);
            drop(db);
        }

        // Unblock and reopen: the migration retries rather than staying off.
        std::fs::remove_dir(&backup).unwrap();
        let kg = KgStoreRedb::open(&path).unwrap();
        assert_eq!(kg.query_active("room:General").unwrap().len(), 1);
        assert_eq!(
            kg.query_active("room:Other").unwrap().len(),
            1,
            "retry migrated every row, not just the first"
        );
        assert_backup_holds_legacy_row(&backup, "room:General", "contains");
    }

    /// Why (#4810): "do not overwrite a `.bak` that verifies good" has a
    /// mirror obligation — a `.bak` that does NOT verify must be replaced, or
    /// a truncated leftover would be trusted as the recovery point.
    #[test]
    fn migration_replaces_a_backup_that_does_not_verify() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("kg.redb");
        seed_legacy_palace(&path, &[("alice", "knows", "bob", None)]);

        let backup = dir.path().join("kg.redb.pre-4810.bak");
        std::fs::write(&backup, b"truncated").unwrap();

        let kg = KgStoreRedb::open(&path).unwrap();
        assert_eq!(kg.query_active("alice").unwrap().len(), 1);
        // The short leftover was replaced by a real pre-migration image.
        assert_backup_holds_legacy_row(&backup, "alice", "knows");
    }
}
