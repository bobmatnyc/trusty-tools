//! A pre-#4810 (format-0) palace under a format-2 binary (#9275, #9918).
//!
//! Why: the #4810 triple-key rewrite is the KG step of the 0 → 1 migration
//! (#9274). #9918: a format-2 binary chains 0 → 1 → 2 at the first
//! writer-intent open, so a legacy-key palace is migrated through the surface
//! a real caller uses, and never opened un-migrated or half-rewritten.
//! What: writes a palace in the pre-#4810 key shape with no schema marker,
//! checks a direct `KnowledgeGraph` open still refuses it unchanged, then that
//! a writer-intent `PalaceHandle` open migrates it behind one backup with every
//! fact readable under the #4810 key.
//! Test: `cargo test -p trusty-memory --test
//! kg_triple_key_migration_tests -- --include-ignored`.

use tempfile::tempdir;
use trusty_memory::engine::palace::{Palace, PalaceId};
use trusty_memory::engine::retrieval::PalaceHandle;
use trusty_memory::engine::store::OpenIntent;
use trusty_memory::engine::store::format_backup::backup_root;
use trusty_memory::engine::store::kg::KnowledgeGraph;
use trusty_memory::engine::store::kg_store::{
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

/// Why: #9918 — a palace two formats behind is migrated, not refused.
/// What: seeds legacy-keyed facts with no schema marker; a direct KG open
/// refuses them with no byte changed, and the writer open migrates them.
#[tokio::test]
async fn a_legacy_key_palace_is_migrated_by_the_first_writer_open() {
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

    // --- #9918: a direct KG open refuses; the writer open migrates -------
    let before = std::fs::read(&redb_path).unwrap();
    assert!(
        KnowledgeGraph::open(&dir.join("kg.db")).is_err(),
        "a format-0 KG must be refused, not opened un-migrated"
    );
    assert_eq!(
        before,
        std::fs::read(&redb_path).unwrap(),
        "kg.redb changed"
    );
    assert!(
        !backup_root(root.path(), "legacy").exists(),
        "a refused palace starts no backup"
    );
    let palace = Palace::new(PalaceId::new("legacy"), "legacy", dir.clone());
    let handle = PalaceHandle::open_with_intent(&palace, OpenIntent::Writer)
        .expect("a format-0 palace is migrated under writer intent");
    let alice = handle.kg.query_active("alice").await.unwrap();
    assert_eq!(alice.len(), 1, "{alice:?}");
    assert_eq!(alice[0].object, "bob");
    assert!(backup_root(root.path(), "legacy").exists(), "no backup");
}
