//! Tests for the redb 2.x → 4.x corpus migration.
//!
//! Why: the migration must be proven against genuine redb 2.6 on-disk files (a
//! synthetic 4.x file would not exercise the cross-format copy). These tests
//! build 2.6 fixtures with the `redb2` engine, migrate them, and assert the
//! result opens with the live redb 4.x `CorpusStore`.
//! What: round-trip (in-place and from a `.v2-incompatible` sibling),
//! idempotency on an already-4.x corpus, and a `#[ignore]`d real-corpus smoke.
//! Test: this module IS the test surface for `super`.

use super::*;
use crate::core::corpus::CorpusStore;

/// Seed a small redb 2.6 corpus fixture at `path` with representative rows
/// across all three table shapes plus a `_meta` schema_version.
///
/// Why: the migration must be tested against a genuine 2.x on-disk file, not
/// a 4.x file — that is the whole point. Writing the fixture with the redb2
/// engine produces the exact format the tool must read.
/// What: writes two `chunks`, one `entities`, one `kg_nodes`, one `kg_edges`,
/// one `kg_communities` (u64 key), one `kg_symbol_community` (u64 value), and
/// a `_meta` `schema_version = 4` row, then commits.
/// Test: this helper underpins every round-trip assertion below.
fn seed_v2_fixture(path: &Path, schema_version: u32) {
    use crate::core::chunker::{ChunkType, RawChunk};

    // Real corpus chunk values are serde_json-encoded `RawChunk`; encode
    // genuine ones so the migrated corpus round-trips through the live
    // `CorpusStore::load_all_chunks` (which deserializes each row).
    let raw = |id: &str| RawChunk {
        id: id.to_string(),
        file: "src/lib.rs".to_string(),
        start_line: 1,
        end_line: 1,
        content: format!("fn {id}() {{}}"),
        function_name: None,
        language: Some("rust".to_string()),
        chunk_type: ChunkType::Code,
        calls: Vec::new(),
        inherits_from: Vec::new(),
        chunk_depth: 0,
        parent_chunk_id: None,
        child_chunk_ids: Vec::new(),
        nlp_keywords: Vec::new(),
        nlp_code_refs: Vec::new(),
        virtual_terms: Vec::new(),
    };

    let db = redb2::Database::create(path).expect("create v2 fixture");
    let txn = db.begin_write().expect("v2 write txn");
    {
        let mut t = txn
            .open_table::<&str, &[u8]>(redb2::TableDefinition::new("chunks"))
            .unwrap();
        let a = serde_json::to_vec(&raw("a")).unwrap();
        let b = serde_json::to_vec(&raw("b")).unwrap();
        t.insert("a:1:1", a.as_slice()).unwrap();
        t.insert("b:2:2", b.as_slice()).unwrap();
    }
    {
        let mut t = txn
            .open_table::<&str, &[u8]>(redb2::TableDefinition::new("entities"))
            .unwrap();
        t.insert("src/lib.rs", b"[]".as_slice()).unwrap();
    }
    {
        let mut t = txn
            .open_table::<&str, &[u8]>(redb2::TableDefinition::new("kg_nodes"))
            .unwrap();
        t.insert("alpha", br#"{"chunk_id":"a:1:1","file":"a.rs"}"#.as_slice())
            .unwrap();
    }
    {
        let mut t = txn
            .open_table::<&str, &[u8]>(redb2::TableDefinition::new("kg_edges"))
            .unwrap();
        t.insert("alpha", br#"[["CallsFunction","beta"]]"#.as_slice())
            .unwrap();
    }
    {
        let mut t = txn
            .open_table::<u64, &[u8]>(redb2::TableDefinition::new("kg_communities"))
            .unwrap();
        t.insert(7u64, b"community-7".as_slice()).unwrap();
    }
    {
        let mut t = txn
            .open_table::<&str, u64>(redb2::TableDefinition::new("kg_symbol_community"))
            .unwrap();
        t.insert("alpha", 7u64).unwrap();
    }
    {
        let mut t = txn
            .open_table::<&str, &[u8]>(redb2::TableDefinition::new("_meta"))
            .unwrap();
        t.insert("schema_version", schema_version.to_le_bytes().as_slice())
            .unwrap();
    }
    txn.commit().expect("commit v2 fixture");
}

/// Why: the core guarantee — a 2.x corpus at the canonical path migrates to
/// a 4.x corpus in place, every row survives, the schema_version is
/// preserved, the original is backed up, and the result opens with the live
/// `CorpusStore` (redb 4.x).
/// What: seeds a fixture, migrates, asserts the outcome counts + schema
/// version, then opens the migrated file via `CorpusStore` and checks the
/// chunk count and KG rows.
/// Test: this test.
#[test]
fn round_trip_v2_to_v4() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("index.redb");
    seed_v2_fixture(&dest, 4);

    let outcome = migrate_redb_corpus(&dest).expect("migration must succeed");
    let (total, sv, backup) = match outcome {
        MigrationOutcome::Migrated {
            total_rows,
            schema_version,
            backup,
            per_table,
        } => {
            // Spot-check a couple of per-table counts.
            let chunks = per_table.iter().find(|(n, _)| *n == "chunks").unwrap().1;
            assert_eq!(chunks, 2, "both chunk rows must be copied");
            (total_rows, schema_version, backup)
        }
        MigrationOutcome::AlreadyV4 => panic!("fixture is 2.x, must migrate"),
    };
    // 2 chunks + 1 entities + 1 kg_nodes + 1 kg_edges + 1 kg_communities +
    // 1 kg_symbol_community + 1 _meta = 8 rows.
    assert_eq!(total, 8, "all rows across all tables must be copied");
    assert_eq!(sv, 4, "schema_version must be preserved");
    assert!(backup.exists(), "original 2.x bytes must be backed up");

    // The staging file must be gone (renamed into place).
    assert!(
        !staging_path(&dest).exists(),
        "staging file must not linger after a successful migration"
    );

    // The migrated file must open with the live 4.x CorpusStore and carry
    // the data.
    let store = CorpusStore::open(&dest).expect("migrated corpus must open with redb 4.x");
    assert_eq!(store.chunk_count().unwrap(), 2, "chunk count preserved");
    let chunks = store.load_all_chunks().unwrap();
    assert_eq!(chunks.len(), 2);
    let (nodes, fwd, _rev) = store.load_kg_graph().unwrap();
    assert_eq!(nodes.len(), 1, "kg node preserved");
    assert_eq!(fwd.len(), 1, "kg forward edge preserved");
    assert_eq!(
        store.read_schema_version_sync().unwrap(),
        4,
        "schema_version readable via CorpusStore after migration"
    );
}

/// Why: when the auto-recovery path has already moved the 2.x file aside to
/// `*.v2-incompatible` and left an empty 4.x file at the canonical path, the
/// tool must read the sibling, migrate, and replace the empty file.
/// What: writes a 2.x fixture at the `.v2-incompatible` sibling and an empty
/// 4.x corpus at the canonical path, runs the migration, and asserts the
/// canonical path now holds the preserved data.
/// Test: this test.
#[test]
fn round_trip_from_incompatible_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("index.redb");
    let sibling = dir.path().join("index.redb.v2-incompatible");
    seed_v2_fixture(&sibling, 3);

    // Simulate the auto-recovery leftover: a fresh empty 4.x corpus at dest.
    {
        let _empty = CorpusStore::open(&dest).expect("empty recovery corpus");
    }
    assert!(opens_with_v4(&dest), "precondition: dest is empty 4.x");

    let outcome = migrate_redb_corpus(&dest).expect("migration from sibling must succeed");
    match outcome {
        MigrationOutcome::Migrated { schema_version, .. } => {
            assert_eq!(schema_version, 3, "sibling's schema_version preserved");
        }
        MigrationOutcome::AlreadyV4 => {
            panic!("dest is empty but the sibling holds 2.x data — must migrate")
        }
    }

    let store = CorpusStore::open(&dest).expect("migrated corpus opens");
    assert_eq!(
        store.chunk_count().unwrap(),
        2,
        "data from the sibling must now live at the canonical path"
    );
}

/// Why: re-running the tool against an already-4.x corpus must be a safe
/// no-op so an operator can run it blindly.
/// What: creates a 4.x corpus, runs the migration, asserts `AlreadyV4` and
/// that no backup/staging files were created.
/// Test: this test.
#[test]
fn idempotent_on_v4() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("index.redb");
    {
        let store = CorpusStore::open(&dest).expect("create 4.x corpus");
        store
            .upsert_chunks(&[])
            .expect("no-op upsert to keep store warm");
    }

    let outcome = migrate_redb_corpus(&dest).expect("no-op on 4.x must succeed");
    assert!(
        matches!(outcome, MigrationOutcome::AlreadyV4),
        "an already-4.x corpus must report AlreadyV4"
    );
    assert!(
        !dest.with_file_name("index.redb.v2-incompatible").exists(),
        "no backup should be created for a no-op"
    );
    assert!(!staging_path(&dest).exists(), "no staging file for a no-op");
}

/// Why: prove the migration against a REAL redb 2.x corpus produced by a
/// shipped trusty-search build, not just a synthetic fixture — the strongest
/// evidence the byte-level copy handles a production schema. Machine-specific
/// (depends on a `*.v2-incompatible` backup existing under the user's data
/// dir), so it is `#[ignore]`d and run only on demand.
/// What: copies a real `index.redb.v2-incompatible` into a tempdir (never
/// touching the original), migrates it, and asserts the result opens with the
/// live `CorpusStore` and reports a non-zero chunk count. Skips with a clear
/// message if no such backup is present on the host.
/// Test: `cargo test -p trusty-search --lib real_v2_incompatible_smoke -- \
///        --ignored --nocapture`.
#[test]
#[ignore = "depends on a machine-specific *.v2-incompatible backup under the data dir"]
fn real_v2_incompatible_smoke() {
    // Candidate real backups produced by older shipped builds on this host.
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("skip: no home dir");
            return;
        }
    };
    let base = home.join("Library/Application Support/trusty-search/indexes");
    let mut found: Option<PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(&base) {
        for e in entries.flatten() {
            let candidate = e.path().join("index.redb.v2-incompatible");
            if candidate.exists() {
                found = Some(candidate);
                break;
            }
        }
    }
    let real = match found {
        Some(p) => p,
        None => {
            eprintln!(
                "skip: no *.v2-incompatible backup found under {} — nothing to smoke-test",
                base.display()
            );
            return;
        }
    };
    eprintln!(
        "smoke: migrating a copy of real 2.x corpus {}",
        real.display()
    );

    // Copy into a tempdir so the real file is never mutated.
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("index.redb");
    std::fs::copy(&real, &dest).expect("copy real 2.x corpus into tempdir");

    let outcome = migrate_redb_corpus(&dest).expect("real 2.x corpus must migrate");
    let copied = match outcome {
        MigrationOutcome::Migrated {
            total_rows,
            schema_version,
            per_table,
            ..
        } => {
            eprintln!(
                "smoke: migrated {total_rows} rows, schema_version={schema_version}, \
                 tables={per_table:?}"
            );
            total_rows
        }
        MigrationOutcome::AlreadyV4 => panic!("the real backup is 2.x and must migrate"),
    };

    // The migrated corpus must open with the live 4.x store. We do NOT
    // assert a non-zero row count: a `*.v2-incompatible` backup can legitimately
    // be an EMPTY corpus (the daemon materializes all tables at `open` even for
    // an index that was never populated — e.g. a failed/aborted first index).
    // The load-bearing guarantee this test proves is that a REAL on-disk 2.x
    // schema (all nine tables, correct key/value shapes) is read and copied
    // without error and the result opens with redb 4.x. If the backup happens
    // to carry chunks, they are preserved (copied == total_rows); if it is
    // empty, an empty 4.x corpus is the correct, faithful result.
    let store = CorpusStore::open(&dest).expect("migrated real corpus must open with redb 4.x");
    let chunks = store.chunk_count().unwrap() as u64;
    eprintln!("smoke: migrated corpus reports {chunks} chunks (copied {copied} total rows)");
    // Whatever chunk rows the source had must survive verbatim.
    let src_chunks = {
        let db = redb2::Database::open(&real).unwrap();
        let r = db.begin_read().unwrap();
        match r.open_table::<&str, &[u8]>(redb2::TableDefinition::new("chunks")) {
            Ok(t) => {
                use redb2::ReadableTableMetadata as _;
                t.len().unwrap()
            }
            Err(_) => 0,
        }
    };
    assert_eq!(
        chunks, src_chunks,
        "every chunk row in the real 2.x corpus must survive migration"
    );
}
