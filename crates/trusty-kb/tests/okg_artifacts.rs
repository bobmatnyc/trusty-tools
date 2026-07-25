//! On-disk artifact contract for the OKG builder.
//!
//! Why: `_sources/registry.toml` and `_sources/<id>.jsonl` are durable, commit-
//! safe, hand-editable files. Their SHAPE is part of the contract — an operator
//! reviews the registry in a diff, and a crashed run's recovery depends on the
//! journal being line-oriented. The unit tests prove the BEHAVIOUR round-trips
//! through serde; this file pins the literal text, which serde alone would let
//! drift silently.
//!
//! What: runs one real ingest against a temp tree, then asserts the registry
//! TOML and the ledger JSONL read the way a human expects, and that a crashed
//! run (journal ahead of, or behind, the entities) converges on re-run.
//!
//! Test: `cargo test -p trusty-kb --test okg_artifacts`.

use trusty_kb::okg::ledger::Ledger;
use trusty_kb::okg::policy::DocStorePolicy;
use trusty_kb::okg::registry::{Locator, SourceRegistry, SourceSpec};
use trusty_kb::schema::Profile;
use trusty_kb::store::KbStore;

/// Build a temp KB tree plus a doc-store corpus beside it.
fn fixture() -> (tempfile::TempDir, KbStore, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = KbStore::new(tmp.path().join("kb"), Profile::default_profile());
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(&corpus).expect("corpus dir");
    (tmp, store, corpus)
}

/// A policy permitting the whole fixture tempdir.
fn policy(tmp: &tempfile::TempDir) -> DocStorePolicy {
    DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()])
}

/// Register a doc store pointed at `corpus`, with tombstoning on.
fn register(store: &KbStore, corpus: &std::path::Path) {
    let mut spec = SourceSpec::new(
        "field-notes",
        Some("notes"),
        Locator::DocStore {
            path: corpus.to_string_lossy().to_string(),
            extensions: vec!["md".into()],
            recursive: true,
        },
        "2026-07-24T12:00:00Z",
    );
    spec.tombstone_deleted = true;
    store.okg_register_source(spec).expect("register");
}

/// Why: the registry is reviewed in a pull request and edited by hand, so its
/// text must be legible TOML with the kind visible as the locator's table name.
/// What: asserts the header, the scalar fields, and the `[sources.locator.
/// doc_store]` sub-table are all present and in a readable order.
#[test]
fn registry_toml_is_human_readable() {
    let (_tmp, store, corpus) = fixture();
    register(&store, &corpus);

    let path = SourceRegistry::path(&store.root).expect("registry path");
    let text = std::fs::read_to_string(&path).expect("registry written");
    println!("--- {} ---\n{text}", path.display());

    assert!(text.starts_with("# OKG source registry"), "header:\n{text}");
    assert!(text.contains("id = \"field-notes\""), "id:\n{text}");
    assert!(
        text.contains("collection = \"notes\""),
        "collection:\n{text}"
    );
    assert!(text.contains("enabled = true"), "enabled:\n{text}");
    assert!(text.contains("tombstone_deleted = true"), "flag:\n{text}");
    assert!(
        text.contains("[sources.locator.doc_store]"),
        "the locator table name IS the source kind:\n{text}"
    );
    assert!(text.contains("recursive = true"), "locator field:\n{text}");
    // Reloading must produce exactly the same document.
    let reloaded = SourceRegistry::load(&store.root).expect("reload");
    reloaded.save(&store.root).expect("resave");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        text,
        "a load/save cycle must be byte-stable"
    );
}

/// Why: the journal is what a crashed run recovers from, so it must be one
/// self-describing JSON object per line — not a rewritten snapshot.
/// What: ingests two files, then asserts the ledger has exactly two parseable
/// lines carrying the item id, a real content hash, and the entity written.
#[test]
fn ledger_jsonl_is_one_line_per_item() {
    let (_tmp, store, corpus) = fixture();
    std::fs::write(corpus.join("alpha.md"), "alpha body").unwrap();
    std::fs::write(corpus.join("beta.md"), "beta body").unwrap();
    register(&store, &corpus);
    store
        .okg_ingest_docstore("field-notes", &policy(&_tmp), "2026-07-24T12:00:00Z")
        .expect("ingest");

    let path = Ledger::path_for(&store.root, "field-notes").expect("ledger path");
    let text = std::fs::read_to_string(&path).expect("ledger written");
    println!("--- {} ---\n{text}", path.display());

    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one line per item:\n{text}");
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("each line is valid JSON");
        assert!(v["item_id"].as_str().is_some_and(|s| s.ends_with(".md")));
        assert!(
            v["fingerprint"]
                .as_str()
                .is_some_and(|f| f.starts_with("sha256:")),
            "content-hashed, not mtime-based: {v}"
        );
        assert!(
            v["entity"]
                .as_str()
                .is_some_and(|e| e.starts_with("notes/"))
        );
        assert_eq!(v["status"], "ingested");
        assert_eq!(v["ingested_at"], "2026-07-24T12:00:00Z");
    }

    // The entities the journal names actually exist.
    let alpha = std::fs::read_to_string(store.entity_path("notes", "alpha").unwrap()).unwrap();
    println!("--- notes/alpha.md ---\n{alpha}");
    assert!(
        alpha.contains("source_id: field-notes"),
        "provenance:\n{alpha}"
    );
    assert!(
        alpha.contains("source_path: alpha.md"),
        "provenance:\n{alpha}"
    );
    assert!(alpha.contains("alpha body"));
}

/// Why: the crash-convergence claim needs a test that actually simulates a
/// crash. Both failure modes must converge on a plain re-run: a journal line
/// lost after its entity landed (re-run rewrites the identical entity), and a
/// torn final line (re-run re-ingests that one item).
/// What: truncates the journal mid-line, re-runs, and asserts the tree is whole
/// and a THIRD run is inert.
#[test]
fn crashed_run_converges_on_rerun() {
    let (_tmp, store, corpus) = fixture();
    for name in ["one.md", "two.md", "three.md"] {
        std::fs::write(corpus.join(name), format!("body of {name}")).unwrap();
    }
    register(&store, &corpus);
    store
        .okg_ingest_docstore("field-notes", &policy(&_tmp), "t0")
        .expect("ingest");

    // Simulate a kill -9 mid-append: drop the last full line and leave a torn
    // fragment behind it.
    let path = Ledger::path_for(&store.root, "field-notes").unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let mut kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let dropped = kept.pop().expect("at least one line");
    let torn = format!("{}\n{}", kept.join("\n"), &dropped[..dropped.len() / 2]);
    std::fs::write(&path, torn).unwrap();

    let recovery = store
        .okg_ingest_docstore("field-notes", &policy(&_tmp), "t1")
        .expect("recovery run");
    println!("recovery: {recovery:?}");
    assert_eq!(
        recovery.ingested, 1,
        "exactly the one item lost to the torn line is re-ingested"
    );
    assert_eq!(recovery.skipped, 2, "the intact records still skip");
    assert_eq!(
        recovery.tombstoned, 0,
        "recovery must not mistake a crash for a deletion"
    );
    assert!(recovery.errors.is_empty(), "errors: {:?}", recovery.errors);

    let settled = store
        .okg_ingest_docstore("field-notes", &policy(&_tmp), "t2")
        .expect("settled run");
    assert_eq!(
        (settled.ingested, settled.updated, settled.skipped),
        (0, 0, 3),
        "the run after recovery is inert — the state converged"
    );
    assert_eq!(settled.watermark.items, 3);
}
