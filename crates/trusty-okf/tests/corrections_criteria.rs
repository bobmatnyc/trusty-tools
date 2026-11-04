//! Slice S6 of #10746: the correction lifecycle — supersede, deprecate,
//! tombstone (SPEC-OKF-01 §2.5, §4.8, §2.10 "Events with no hash"; §6.2
//! criteria 27, 38, 39, 40, 42, 143). Criterion 37 needs trusty-common and
//! lives in `crates/trusty-agents/tests/okf_tombstone.rs` (#10746 S11).

mod s6_support;

use s6_support::*;
use serde_yaml::Value;
use trusty_okf::OkfError;
use trusty_okf::build::ingest::{concept_id_for, sweep_deleted};
use trusty_okf::build::ledger::{ItemStatus, Ledger};
use trusty_okf::bundle::{Bundle, PutOutcome, VerificationState};
use trusty_okf::format::{Concept, Verification, WriteMode, content_hash};
use trusty_okf::state::{IndexJournal, IndexRecord, IndexState, IndexStateDir};

fn note(name: &str, body: &str) -> Concept {
    let mut m = serde_yaml::Mapping::new();
    m.insert("type".into(), "Note".into());
    Concept::new(id(name), m, body.to_string())
}

fn verify(b: &mut Bundle, name: &str, at: &str) {
    let h = content_hash(&b.get(&id(name)).unwrap());
    let v = Verification {
        by: process(),
        at: instant(at),
        stale_after: None,
    };
    b.verify(&id(name), v, &h).unwrap();
}

fn conflict_reason<T: std::fmt::Debug>(got: Result<T, OkfError>) -> String {
    match got {
        Err(OkfError::Conflict { reason, .. }) => reason,
        other => panic!("expected Conflict, got {other:?}"),
    }
}

/// Why: criterion 40 (3c A) — after `supersede`, the old file still exists
/// with `status: deprecated` and `superseded_by`, and the new concept has
/// `supersedes`; a verification of the old concept stays, and `log.md`
/// gains one entry.
/// What: put and verify `old`, supersede it by `new`.
/// Test: self-contained.
#[test]
fn c40_supersede_keeps_the_old_file_and_links_both_ways() {
    let (_t, root) = fresh();
    let mut b = Bundle::open(&root).unwrap().0;
    let t = instant("2026-11-04T10:00:00Z");
    b.put(
        note("old", "Old claim.\n"),
        process(),
        WriteMode::Lenient,
        t,
    )
    .unwrap();
    verify(&mut b, "old", "2026-11-04T10:05:00Z");
    b.supersede(
        &id("old"),
        note("new", "Corrected claim.\n"),
        "wrong figure",
        process(),
        t,
    )
    .unwrap();
    let old = b.get(&id("old")).unwrap();
    assert_eq!(
        old.get("status").and_then(Value::as_str),
        Some("deprecated")
    );
    assert_eq!(
        old.get("superseded_by").and_then(Value::as_str),
        Some("/new.md")
    );
    assert_eq!(old.body, "Old claim.\n");
    let new = b.get(&id("new")).unwrap();
    assert_eq!(
        new.get("supersedes").and_then(Value::as_str),
        Some("/old.md")
    );
    assert!(matches!(
        b.verification_state(&id("old")).unwrap(),
        VerificationState::Verified { events: 1, .. }
    ));
    let log = read(&root, "log.md");
    assert_eq!(
        log.matches("**Superseded** `/old.md` by `/new.md`").count(),
        1,
        "{log}"
    );
    // A second supersede of the same concept is refused, no file changed.
    let before = tree(&root);
    let got = b.supersede(&id("old"), note("newer", "x\n"), "again", process(), t);
    assert_eq!(conflict_reason(got), "superseded");
    assert_eq!(tree(&root), before);
}

/// Why: §2.5 rule 1 — `deprecate` is a lifecycle-only write: the concept
/// keeps its path, body and verification, `log.md` gains one entry, and a
/// second call writes nothing.
/// What: put, verify, deprecate twice.
/// Test: self-contained.
#[test]
fn deprecate_is_lifecycle_only_and_logged() {
    let (_t, root) = fresh();
    let mut b = Bundle::open(&root).unwrap().0;
    let t = instant("2026-11-04T10:00:00Z");
    b.put(note("x", "Body.\n"), process(), WriteMode::Lenient, t)
        .unwrap();
    verify(&mut b, "x", "2026-11-04T10:05:00Z");
    let (out, _) = b.deprecate(&id("x"), "obsolete", process(), t).unwrap();
    assert_eq!(
        out,
        PutOutcome::Updated {
            content_changed: false,
            verifications_cleared: 0
        }
    );
    assert!(matches!(
        b.verification_state(&id("x")).unwrap(),
        VerificationState::Verified { .. }
    ));
    assert_eq!(
        read(&root, "log.md")
            .matches("**Deprecated** `/x.md`")
            .count(),
        1
    );
    let before = tree(&root);
    let (again, _) = b.deprecate(&id("x"), "obsolete", process(), t).unwrap();
    assert_eq!(again, PutOutcome::Unchanged);
    assert_eq!(tree(&root), before, "no byte written");
}

/// Why: criterion 38 — trusty-okf refuses to write `source_status: deleted`
/// on a concept without `source_id`, and changes no file.
/// What: a put of such a concept, with no and with an empty `source_id`;
/// then with one, which lands.
/// Test: self-contained.
#[test]
fn c38_tombstone_without_source_id_is_refused() {
    let (_t, root) = fresh();
    let mut b = Bundle::open(&root).unwrap().0;
    let t = instant("2026-11-04T10:00:00Z");
    for source_id in [None, Some("")] {
        let mut c = note("gone", "Body.\n");
        c.set("source_status", Value::String("deleted".into()));
        if let Some(s) = source_id {
            c.set("source_id", Value::String(s.into()));
        }
        let got = b.put(c, process(), WriteMode::Lenient, t);
        assert_eq!(conflict_reason(got), "tombstone_without_source_id");
        assert!(tree(&root).is_empty(), "no file changed");
    }
    let mut c = note("gone", "Body.\n");
    c.set("source_status", Value::String("deleted".into()));
    c.set("source_id", Value::String("docs".into()));
    b.put(c, process(), WriteMode::Lenient, t).unwrap();
}

/// Why: criterion 39 — a tombstoned concept whose store item reappears
/// returns to Live, keeps its editorial keys, and gains an `Ingested` ledger
/// record after the `Tombstoned` one.
/// What: ingest, deprecate, sweep (tombstone), re-ingest.
/// Test: self-contained.
#[test]
fn c39_tombstoned_concept_returns_to_live_when_its_item_reappears() {
    let (_t, root) = fresh();
    let mut spec = docstore("docs", "notes");
    spec.tombstone_deleted = true;
    let a = item("a", "Alpha.");
    ingest(
        &root,
        &spec,
        std::slice::from_ref(&a),
        "2026-11-04T10:00:00Z",
    );
    let cid = concept_id_for("notes", "a").unwrap();
    let mut b = Bundle::open(&root).unwrap().0;
    b.deprecate(&cid, "curated", process(), instant("2026-11-04T10:30:00Z"))
        .unwrap();
    let r = sweep_deleted(
        &root,
        &spec,
        &Default::default(),
        instant("2026-11-04T11:00:00Z"),
    )
    .unwrap();
    assert_eq!(r.tombstoned, 1);
    let c = Bundle::open(&root).unwrap().0.get(&cid).unwrap();
    assert_eq!(
        c.get("source_status").and_then(Value::as_str),
        Some("deleted")
    );
    ingest(&root, &spec, &[a], "2026-11-04T12:00:00Z");
    let c = Bundle::open(&root).unwrap().0.get(&cid).unwrap();
    assert!(c.get("source_status").is_none() && c.get("tombstoned").is_none());
    assert_eq!(c.get("status").and_then(Value::as_str), Some("deprecated"));
    let ledger = read(&root, "_sources/docs.jsonl");
    let statuses: Vec<&str> = ledger
        .lines()
        .map(|l| {
            if l.contains("\"tombstoned\"") {
                "T"
            } else {
                "I"
            }
        })
        .collect();
    assert_eq!(statuses, ["I", "T", "I"], "{ledger}");
    assert_eq!(
        Ledger::load(&root, "docs")
            .unwrap()
            .get("a.md")
            .unwrap()
            .status,
        ItemStatus::Ingested
    );
}

/// Why: criterion 27 — a re-ingest of a changed store item keeps
/// `status`, `supersedes`, `superseded_by`, `stale_after` and the
/// verification record, through the real `supersede`.
/// What: ingest, verify, renew `stale_after`, set `supersedes`, supersede,
/// then re-ingest a changed body.
/// Test: self-contained.
#[test]
fn c27_reingest_keeps_editorial_keys_after_a_real_supersede() {
    let (_t, root) = fresh();
    let spec = connector("mail", "mail");
    ingest(&root, &spec, &[item("m", "One.")], "2026-11-04T10:00:00Z");
    let cid = concept_id_for("mail", "m").unwrap();
    let mut b = Bundle::open(&root).unwrap().0;
    verify(&mut b, cid.as_str(), "2026-11-04T10:05:00Z");
    let mut c = b.get(&cid).unwrap();
    c.set("stale_after", Value::String("2027-01-01T00:00:00Z".into()));
    c.set("supersedes", Value::String("/mail/earlier.md".into()));
    b.put(
        c,
        process(),
        WriteMode::Lenient,
        instant("2026-11-04T10:10:00Z"),
    )
    .unwrap();
    b.supersede(
        &cid,
        note("mail/m2", "Successor.\n"),
        "split",
        process(),
        instant("2026-11-04T10:20:00Z"),
    )
    .unwrap();
    ingest(&root, &spec, &[item("m", "Two.")], "2026-11-04T11:00:00Z");
    let c = Bundle::open(&root).unwrap().0.get(&cid).unwrap();
    assert_eq!(c.body, "\nTwo.\n");
    for (k, v) in [
        ("status", "deprecated"),
        ("supersedes", "/mail/earlier.md"),
        ("superseded_by", "/mail/m2.md"),
        ("stale_after", "2027-01-01T00:00:00Z"),
    ] {
        assert_eq!(c.get(k).and_then(Value::as_str), Some(v), "{k}");
    }
    let b = Bundle::open(&root).unwrap().0;
    let events = b.verification_events(&cid).unwrap();
    assert_eq!(events.len(), 1, "the record keeps the event");
    assert!(events[0].invalidated_at.is_some(), "now history");
}

/// Why: criterion 143 — on a legacy concept with hashless events, a
/// content-changing `put` moves every one into the record with reason
/// `legacy_unbound`; on another, `deprecate` and a `stale_after` renewal
/// leave them byte-identical in frontmatter.
/// What: two legacy files with a hashless `verified` list.
/// Test: self-contained.
#[test]
fn c143_hashless_events_move_on_content_change_and_stay_otherwise() {
    let (_t, root) = fresh();
    let block = "verified:\n  - { by: human:old, at: 2025-01-01T00:00:00Z }\n  - { by: process:bot, at: 2025-02-01T00:00:00Z }\n";
    for n in ["l1", "l2"] {
        let text = format!("---\ntype: Note\n{block}---\nLegacy {n}.\n");
        std::fs::write(root.join(format!("{n}.md")), text).unwrap();
    }
    let mut b = Bundle::open(&root).unwrap().0;
    let t = instant("2026-11-04T10:00:00Z");
    let mut c = b.get(&id("l1")).unwrap();
    c.body = "Legacy l1, corrected.\n".into();
    b.put(c, process(), WriteMode::Lenient, t).unwrap();
    assert!(!read(&root, "l1.md").contains("verified"));
    let events = b.verification_events(&id("l1")).unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.hash.is_none()
        && e.reason.as_deref() == Some("legacy_unbound")
        && e.invalidated_at.is_some()));

    b.deprecate(&id("l2"), "retired", process(), t).unwrap();
    let mut c = b.get(&id("l2")).unwrap();
    c.set("stale_after", Value::String("2027-01-01T00:00:00Z".into()));
    b.put(c, process(), WriteMode::Lenient, t).unwrap();
    let text = read(&root, "l2.md");
    assert!(text.contains(block), "{text}");
    assert!(b.verification_events(&id("l2")).unwrap().is_empty());
}

/// Why: criterion 42, the S6 half — deprecated and tombstoned concepts are
/// intended out of the index (3i A): they read WithdrawPending until a
/// confirmed remove and a passed withdrawal probe, then Withdrawn, and a
/// withdrawal probe that finds the path again (a watcher re-index, F9)
/// reads WithdrawPending. The hit check itself is the feed's (S7).
/// What: deprecate one concept, tombstone another, play the journal.
/// Test: self-contained.
#[test]
fn c42_deprecated_and_tombstoned_concepts_read_withdrawn_or_withdraw_pending() {
    let (t, root) = fresh();
    let state = IndexStateDir::new(t.path().join("machine"));
    let mut spec = docstore("docs", "notes");
    spec.tombstone_deleted = true;
    ingest(
        &root,
        &spec,
        &[item("a", "A."), item("d", "D.")],
        "2026-11-04T10:00:00Z",
    );
    ack_all(&root, &state);
    let (a, d) = (
        concept_id_for("notes", "a").unwrap(),
        concept_id_for("notes", "d").unwrap(),
    );
    let mut b = Bundle::open(&root).unwrap().0;
    b.deprecate(&d, "retired", process(), instant("2026-11-04T10:30:00Z"))
        .unwrap();
    let present = std::iter::once("d.md".to_string()).collect();
    sweep_deleted(&root, &spec, &present, instant("2026-11-04T11:00:00Z")).unwrap();
    let index = |cid| {
        Bundle::open(&root)
            .unwrap()
            .0
            .revision_state(cid, &state, INDEX, &live())
            .unwrap()
            .index
    };
    assert_eq!(index(&a), IndexState::WithdrawPending);
    assert_eq!(index(&d), IndexState::WithdrawPending);
    let j = IndexJournal::open(&state, &root, INDEX).unwrap();
    for cid in [&a, &d] {
        j.append(&[
            IndexRecord::RemoveAck {
                path: cid.file_name(),
                identity: Some(identity()),
            },
            IndexRecord::WithdrawProbe {
                path: cid.file_name(),
                identity: Some(identity()),
                passed: true,
            },
        ])
        .unwrap();
        assert_eq!(index(cid), IndexState::Withdrawn);
    }
    j.append(&[IndexRecord::WithdrawProbe {
        path: d.file_name(),
        identity: Some(identity()),
        passed: false,
    }])
    .unwrap();
    assert_eq!(
        index(&d),
        IndexState::WithdrawPending,
        "F9: the path came back"
    );
}
