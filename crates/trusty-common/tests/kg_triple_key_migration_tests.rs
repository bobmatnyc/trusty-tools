//! End-to-end pre→post migration of the #4810 triple key.
//!
//! Why: the unit tests drive the format migration directly. This one goes
//! through the surfaces a real caller uses — a writer-intent `PalaceHandle`
//! open, which migrates a format-0 palace (#9274; the #4810 rewrite is its KG
//! step), then `KnowledgeGraph::open`, which translates `kg.db` to `kg.redb`
//! and hydrates the in-memory adjacency from the migrated rows — because that
//! hydration step is where the old key's damage was still visible after
//! storage was fixed.
//! What: writes a palace in the pre-#4810 key shape with no schema marker,
//! checks a direct `KnowledgeGraph` open refuses it un-migrated, migrates it
//! with a writer open, and asserts the facts are queryable, the adjacency
//! carries them, and a further multi-valued assert joins the set instead of
//! replacing it.
//! Test: `cargo test -p trusty-common --features memory-core --test
//! kg_triple_key_migration_tests -- --include-ignored`.

#![cfg(feature = "memory-core")]

use tempfile::tempdir;
use trusty_common::memory_core::palace::{Palace, PalaceId};
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::OpenIntent;
use trusty_common::memory_core::store::format_backup::{MANIFEST, backup_root};
use trusty_common::memory_core::store::kg::{KnowledgeGraph, Triple};
use trusty_common::memory_core::store::kg_store::{
    ACTIVE_SUBJECT_COUNTS, TRIPLES, TripleValue, encode_u64, encode_value,
};

/// The pre-#4810 key: `[subject_len: u16 BE][subject][predicate]`, no object.
fn legacy_triple_key(subject: &str, predicate: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(subject.len() as u16).to_be_bytes());
    out.extend_from_slice(subject.as_bytes());
    out.extend_from_slice(predicate.as_bytes());
    out
}

fn triple(subject: &str, predicate: &str, object: &str) -> Triple {
    Triple {
        subject: subject.into(),
        predicate: predicate.into(),
        object: object.into(),
        valid_from: chrono::Utc::now(),
        valid_to: None,
        confidence: 1.0,
        provenance: None,
    }
}

/// Why: proves the migration is complete end to end — storage rewrite,
/// adjacency hydration, and the write path that follows — rather than proving
/// each piece in isolation.
/// What: seeds three legacy-keyed facts under two subjects, opens the palace
/// through `KnowledgeGraph`, and checks reads, graph neighbours, and a
/// subsequent multi-valued assert.
/// Ignored by default: it writes a redb file and re-opens it, which is heavier
/// than a unit test and unnecessary on every inner-loop run.
#[tokio::test]
async fn legacy_palace_migrates_on_open_and_accepts_multi_valued_writes() {
    let root = tempdir().unwrap();
    let dir = root.path().join("palaces").join("legacy");
    std::fs::create_dir_all(&dir).unwrap();
    let redb_path = dir.join("kg.redb");

    // --- pre: a palace written by the pre-#4810 code ---------------------
    {
        let db = redb::Database::create(&redb_path).unwrap();
        let wtx = db.begin_write().unwrap();
        {
            let mut triples = wtx.open_table(TRIPLES).unwrap();
            // The old code maintained this counter alongside the rows, and the
            // migration preserves it rather than recomputing it — the rewrite
            // is one row in, one row out — so a faithful legacy palace has to
            // carry it too.
            let mut counts = wtx.open_table(ACTIVE_SUBJECT_COUNTS).unwrap();
            for (s, p, o) in [
                ("room:General", "contains", "drawer:a"),
                ("tga", "is_alias_for", "trusty-git-analytics"),
                ("alice", "knows", "bob"),
            ] {
                counts
                    .insert(s.as_bytes(), encode_u64(1).as_slice())
                    .unwrap();
                let value = TripleValue {
                    object: o.to_string(),
                    valid_from_ms: 1_700_000_000_000,
                    valid_to_ms: None,
                    confidence: 1.0,
                    provenance: None,
                };
                let bytes = encode_value(&value).unwrap();
                triples
                    .insert(legacy_triple_key(s, p).as_slice(), bytes.as_slice())
                    .unwrap();
            }
        }
        wtx.commit().unwrap();
        drop(db);
    }

    // --- #9274: never opened un-migrated; a writer open migrates ----------
    assert!(
        KnowledgeGraph::open(&dir.join("kg.db")).is_err(),
        "a format-0 KG must be refused, not opened un-migrated"
    );
    let palace = Palace {
        id: PalaceId::new("legacy"),
        name: "legacy".into(),
        description: None,
        created_at: chrono::Utc::now(),
        data_dir: dir.clone(),
    };
    drop(PalaceHandle::open_with_intent(&palace, OpenIntent::Writer).unwrap());

    // --- post: open through the public handle -----------------------------
    // Callers pass the legacy `kg.db` name; `KnowledgeGraph` resolves it to the
    // `kg.redb` file seeded above.
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).unwrap();

    let room = kg.query_active("room:General").await.unwrap();
    assert_eq!(room.len(), 1, "the migrated fact is queryable");
    assert_eq!(room[0].object, "drawer:a");
    assert_eq!(
        kg.query_active("tga").await.unwrap()[0].object,
        "trusty-git-analytics"
    );
    assert_eq!(kg.query_active("alice").await.unwrap()[0].object, "bob");

    // Hydration ran over the migrated rows, so the graph sees all three edges.
    assert_eq!(kg.edge_count(), 3);
    assert_eq!(kg.count_active_triples().unwrap(), 3);

    // Multi-valued predicate: the new member joins the room.
    kg.assert(triple("room:General", "contains", "drawer:b"))
        .await
        .unwrap();
    kg.assert(triple("room:General", "contains", "drawer:c"))
        .await
        .unwrap();
    let room = kg.query_active("room:General").await.unwrap();
    assert_eq!(room.len(), 3, "migrated palace accepts multi-valued writes");

    // Functional predicate: the newer alias still supersedes.
    kg.assert(triple("tga", "is_alias_for", "trusty-git-analytics-2"))
        .await
        .unwrap();
    let tga = kg.query_active("tga").await.unwrap();
    assert_eq!(tga.len(), 1);
    assert_eq!(tga[0].object, "trusty-git-analytics-2");

    // The pre-migration image is in the format-migration backup.
    let backups: Vec<_> = std::fs::read_dir(backup_root(root.path(), "legacy"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert!(backups[0].join(MANIFEST).is_file());
    assert!(
        backups[0].join("kg.redb").is_file(),
        "migration leaves a verified backup"
    );
}
