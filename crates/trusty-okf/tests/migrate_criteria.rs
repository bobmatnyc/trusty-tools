//! Slice S6 of #10746: lossless trusty-kb to v0.2 migration (SPEC-OKF-01
//! §2.8, §2.9; §6.2 criteria 23, 25, 26, 67 and source preservation). The
//! fault-injection criteria (24, 106-108, 120, 127) and the
//! migrate-restore-diff test live in `src/bundle/s6_fault_tests.rs`.

mod s6_support;

use s6_support::*;
use serde_yaml::Value;
use trusty_okf::bundle::{Bundle, VerificationState};
use trusty_okf::format::{Concept, FindingKind, parse_concept};
use trusty_okf::state::{IndexState, IndexStateDir};

fn concept(root: &std::path::Path, rel: &str) -> Concept {
    let stem = rel.strip_suffix(".md").unwrap();
    parse_concept(id(stem), &read(root, rel)).unwrap().0
}

fn migrate(root: &std::path::Path) -> trusty_okf::bundle::MigrationReport {
    let mut b = Bundle::open(root).unwrap().0;
    let plan = b.plan_migration().unwrap();
    b.migrate(&plan, instant("2026-11-04T12:00:00Z")).unwrap()
}

/// Why: criterion 23 and §2.9 items 1-7 — on a tree with hand-owned,
/// ledger-owned and tombstoned concepts, a migration keeps the concept ID
/// set and every body byte for byte, maps each §2.8 key so its inverse
/// yields the old value, and keeps every source reference.
/// What: migrate the golden trusty-kb tree plus hand-owned concepts.
/// Test: self-contained.
#[test]
fn c23_migration_keeps_ids_and_bodies_on_the_trusty_kb_tree() {
    let (_t, root) = migration_tree();
    let before = tree(&root);
    let ids_before = Bundle::open(&root).unwrap().0.concept_ids().unwrap();
    let plan = Bundle::open(&root).unwrap().0.plan_migration().unwrap();
    assert!(plan.changes.len() >= 4, "{:?}", plan.changes);
    assert!(
        plan.findings
            .iter()
            .any(|f| f.kind == FindingKind::UnmappedLegacyValue && f.path.contains("date-only")),
        "{:?}",
        plan.findings
    );
    let report = migrate(&root);
    assert_eq!(report.changed, plan.changes.len());
    assert_eq!(
        Bundle::open(&root).unwrap().0.concept_ids().unwrap(),
        ids_before
    );
    for (rel, old) in before
        .iter()
        .filter(|(p, _)| p.ends_with(".md") && !p.contains('_'))
    {
        if rel == "index.md" || rel == "log.md" {
            continue;
        }
        let stem = rel.strip_suffix(".md").unwrap();
        let old = parse_concept(id(stem), std::str::from_utf8(old).unwrap())
            .unwrap()
            .0;
        let new = concept(&root, rel);
        assert_eq!(new.body, old.body, "{rel}: body");
        for key in [
            "trust",
            "source_id",
            "source_kind",
            "source_item_id",
            "source_status",
            "tombstoned",
            "created",
            "related",
            "x_flow",
            "from",
            "source_path",
        ] {
            assert_eq!(new.get(key), old.get(key), "{rel}: {key}");
        }
    }
    let hand = concept(&root, "notes/hand.md");
    assert_eq!(hand.get("updated"), None);
    let g = hand.get("generated").unwrap();
    assert_eq!(
        g.get("at").and_then(Value::as_str),
        Some("2026-07-20T10:00:00Z")
    );
    assert_eq!(
        g.get("by").and_then(Value::as_str),
        Some("process:trusty-kb-legacy")
    );
    let sources: Vec<&str> = hand
        .get("sources")
        .and_then(Value::as_sequence)
        .unwrap()
        .iter()
        .map(|s| s.get("resource").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(sources, ["https://example.com/a", "https://example.com/b"]);
    assert!(read(&root, "notes/hand.md").contains("x_flow: {a: 1, b: [2, 3]} # kept"));
    assert_eq!(
        concept(&root, "notes/date-only.md")
            .get("updated")
            .and_then(Value::as_str),
        Some("2026-07-20")
    );
    // Source preservation (§2.9 items 4, 10): registry and ledgers unchanged.
    for rel in [
        "_sources/registry.toml",
        "_sources/field-notes.jsonl",
        "_sources/sent-mail.jsonl",
        "index.md",
    ] {
        assert_eq!(tree(&root)[rel], before[rel], "{rel}");
    }
    assert!(
        !root
            .join(format!("_sources/migrations/{}", report.run_id))
            .exists(),
        "blobs removed"
    );
    assert!(
        root.join(format!("_sources/migrations/{}.jsonl", report.run_id))
            .is_file(),
        "manifest kept"
    );
}

/// Why: criterion 25 — a second migration plans zero changes and writes
/// zero bytes.
/// What: migrate twice; compare every file after the second run.
/// Test: self-contained.
#[test]
fn c25_second_migration_plans_nothing_and_writes_nothing() {
    let (_t, root) = migration_tree();
    migrate(&root);
    let once = tree(&root);
    let mut b = Bundle::open(&root).unwrap().0;
    let plan = b.plan_migration().unwrap();
    assert!(plan.changes.is_empty(), "{:?}", plan.changes);
    let report = b.migrate(&plan, instant("2026-11-05T12:00:00Z")).unwrap();
    assert_eq!(report.changed, 0);
    assert_eq!(tree(&root), once);
}

/// Why: criterion 26 — every `log.md`, `_state/conversion-log.md`, ledger
/// file and journal file is equal or only appended after a migration.
/// What: compare each such file before and after.
/// Test: self-contained.
#[test]
fn c26_logs_and_journals_are_equal_or_appended() {
    let (_t, root) = migration_tree();
    let before = tree(&root);
    migrate(&root);
    let after = tree(&root);
    for (rel, old) in &before {
        let journal = rel.starts_with("_sources/") || rel.starts_with("_state/") || rel == "log.md";
        if journal {
            assert!(after[rel].starts_with(old), "{rel} only appended");
        }
    }
    assert!(
        !after["_sources/revisions.jsonl"].is_empty(),
        "migrate lines appended"
    );
}

/// Why: criterion 67 — `migrate` leaves every concept's verification state
/// unchanged and moves changed files to Pending; an unchanged file keeps
/// its index state.
/// What: acknowledge every concept, migrate, read all three axes.
/// Test: self-contained.
#[test]
fn c67_migrate_keeps_verification_and_moves_changed_files_to_pending() {
    let (t, root) = migration_tree();
    // One concept already in v0.2 form, so it is not changed.
    std::fs::write(
        root.join("notes/v02.md"),
        "---\ntype: Note\n---\nAlready v0.2.\n",
    )
    .unwrap();
    let state = IndexStateDir::new(t.path().join("machine"));
    ack_all(&root, &state);
    let b = Bundle::open(&root).unwrap().0;
    let ids = b.concept_ids().unwrap();
    let verification: Vec<VerificationState> = ids
        .iter()
        .map(|i| {
            b.revision_state(i, &state, INDEX, &live())
                .unwrap()
                .verification
        })
        .collect();
    let plan = b.plan_migration().unwrap();
    let changed: Vec<String> = plan
        .changes
        .iter()
        .map(|c| c.id.as_str().to_string())
        .collect();
    migrate(&root);
    let b = Bundle::open(&root).unwrap().0;
    for (i, cid) in ids.iter().enumerate() {
        let r = b.revision_state(cid, &state, INDEX, &live()).unwrap();
        assert_eq!(r.verification, verification[i], "{cid}");
        let withdrawn = matches!(r.index, IndexState::WithdrawPending);
        if changed.contains(&cid.as_str().to_string()) {
            assert!(
                matches!(r.index, IndexState::Pending { .. }) || withdrawn,
                "{cid}: {:?}",
                r.index
            );
        } else {
            assert!(
                matches!(r.index, IndexState::Searchable) || withdrawn,
                "{cid}: {:?}",
                r.index
            );
        }
    }
    assert!(!changed.contains(&"notes/v02".to_string()));
}
