//! Fault injection for the operation journal and the revision journal
//! (SPEC-OKF-01 §4.11, §4.9; criteria 66, 106-109, 126, 127; the log.md
//! cost ruling of #10746 S5).
//!
//! The seam is `ops::failpoint`, compiled only under `cfg(test)`: a hook
//! stops the operation at a named boundary, as a process death would, and
//! the test reopens the bundle.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde_yaml::Mapping;

use super::fail;
use crate::OkfError;
use crate::build::ingest::{SourceItem, ingest_items};
use crate::build::registry::{ConnectorParams, Locator, SourceSpec};
use crate::bundle::recover::read_ops;
use crate::bundle::{Bundle, RecoveryDirection, VerificationEvent, VerificationState};
use crate::format::concept::Concept;
use crate::format::fields::{Actor, Verification};
use crate::format::hash::content_hash;
use crate::format::id::ConceptId;
use crate::format::validate::FindingKind;
use crate::format::write::WriteMode;
use crate::profile::Profile;
use crate::state::revisions;
use crate::state::{IndexStateDir, LiveIdentity, SaveState};

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

fn actor() -> Actor {
    Actor::Process("test".into())
}

/// Every file outside `_sources/ops/`, with its bytes (the §4.11 and
/// criterion 106 comparison scope).
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel.starts_with("_sources/ops") {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// The multiset of folded verification events across every record file.
fn events(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (rel, bytes) in tree(root) {
        if !rel.starts_with("_sources/verification/") {
            continue;
        }
        let lines = String::from_utf8_lossy(&bytes)
            .lines()
            .filter_map(|l| serde_json::from_str::<super::super::record::Line>(l).ok())
            .collect();
        for l in super::super::record::fold(lines) {
            out.push(format!(
                "{}|{}|{:?}|{:?}",
                l.by, l.at, l.hash, l.invalidated_by_hash
            ));
        }
    }
    out.sort();
    out
}

/// No `/<id>.md` link in any concept names a missing file.
fn links_resolve(root: &Path) -> bool {
    tree(root)
        .iter()
        .filter(|(p, _)| p.ends_with(".md"))
        .all(|(_, b)| {
            let text = String::from_utf8_lossy(b);
            text.match_indices("](/").all(|(i, _)| {
                let rest = &text[i + 3..];
                let end = rest.find(')').unwrap_or(rest.len());
                root.join(&rest[..end]).is_file()
            })
        })
}

fn new_concept(name: &str, body: &str) -> Concept {
    let mut m = Mapping::new();
    m.insert("type".into(), "Note".into());
    m.insert("description".into(), name.into());
    Concept::new(id(name), m, body.to_string())
}

/// A bundle with `a` (verified) and `b` (verified, links to `a`).
fn seed(root: &Path) {
    let mut b = Bundle::open(root).unwrap().0;
    let now = at("2026-11-04T10:00:00Z");
    b.put(
        new_concept("a", "Alpha\n"),
        actor(),
        WriteMode::Lenient,
        now,
    )
    .unwrap();
    b.put(
        new_concept("b", "See [a](/a.md).\n"),
        actor(),
        WriteMode::Lenient,
        now,
    )
    .unwrap();
    for n in ["a", "b"] {
        let h = content_hash(&b.get(&id(n)).unwrap());
        let v = Verification {
            by: actor(),
            at: at("2026-11-04T10:05:00Z"),
            stale_after: None,
        };
        b.verify(&id(n), v, &h).unwrap();
    }
}

type Op = fn(&mut Bundle) -> Result<(), OkfError>;

fn op_move(b: &mut Bundle) -> Result<(), OkfError> {
    b.move_concept(&id("a"), &id("z/a"), at("2026-11-04T11:00:00Z"))
        .map(|_| ())
}

fn op_put(b: &mut Bundle) -> Result<(), OkfError> {
    let mut c = b.get(&id("a"))?;
    c.body = "Alpha, corrected\n".into();
    b.put(c, actor(), WriteMode::Lenient, at("2026-11-04T11:00:00Z"))
        .map(|_| ())
}

fn op_verify(b: &mut Bundle) -> Result<(), OkfError> {
    let h = content_hash(&b.get(&id("a"))?);
    let v = Verification {
        by: Actor::Process("second".into()),
        at: at("2026-11-04T11:00:00Z"),
        stale_after: None,
    };
    b.verify(&id("a"), v, &h).map(|_| ())
}

fn fresh() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    seed(&root);
    (tmp, root)
}

/// The boundary names one uninterrupted run of `op` passes, in order.
fn boundaries(op: Op) -> Vec<String> {
    let (_t, root) = fresh();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let s = seen.clone();
    fail::set(move |n| {
        s.borrow_mut().push(n.to_string());
        false
    });
    op(&mut Bundle::open(&root).unwrap().0).unwrap();
    fail::clear();
    seen.borrow().clone()
}

/// Open, then recover every interrupted operation as the only writer would
/// (#10746 S5 r2: `open` without the lock only reports).
fn recover_all(root: &Path) -> Result<Vec<crate::format::validate::Finding>, OkfError> {
    let (mut b, mut found) = Bundle::open(root)?;
    found.extend(b.recover_interrupted()?);
    Ok(found)
}

/// Crash `op` at the `nth` hit of `name`.
fn crash_at(root: &Path, op: Op, name: &str) {
    let target = name.to_string();
    fail::set(move |n| n == target);
    let got = op(&mut Bundle::open(root).unwrap().0);
    fail::clear();
    assert!(got.is_err(), "crash at {name} returned Ok");
}

/// Why: criteria 106 and 107 — a crash at every step boundary of
/// `move_concept`, a content-changing `put` and `verify` recovers on the
/// next open to the uninterrupted tree (before the intent: to the
/// pre-operation tree, §4.11 "Torn or missing intent"), with the same
/// verification events and no link to a missing concept.
/// What: records the boundaries of one clean run, then crashes at each.
/// Test: self-contained.
#[test]
fn c106_crash_at_every_boundary_rolls_forward_byte_identical() {
    for (label, op) in [
        ("move", op_move as Op),
        ("put", op_put),
        ("verify", op_verify),
    ] {
        let (_t0, before_root) = fresh();
        let before = tree(&before_root);
        let (_t1, clean_root) = fresh();
        op(&mut Bundle::open(&clean_root).unwrap().0).unwrap();
        let clean = tree(&clean_root);
        let clean_events = events(&clean_root);
        assert!(links_resolve(&clean_root));
        let names = boundaries(op);
        assert!(names.len() >= 5, "{label}: {names:?}");
        for name in &names {
            let (_t, root) = fresh();
            crash_at(&root, op, name);
            let findings = recover_all(&root).unwrap();
            assert!(
                findings
                    .iter()
                    .all(|f| f.kind != FindingKind::RecoveryConflict),
                "{label}/{name}: {findings:?}"
            );
            let expect = if name == "staged" { &before } else { &clean };
            assert_eq!(&tree(&root), expect, "{label}: crash at {name}");
            if name != "staged" {
                assert_eq!(events(&root), clean_events, "{label}/{name}: events");
            }
            assert!(links_resolve(&root), "{label}/{name}: dangling link");
            assert!(
                !root
                    .join("_sources/ops")
                    .read_dir()
                    .unwrap()
                    .any(|e| e.unwrap().path().is_dir()),
                "{label}/{name}: staging left behind"
            );
        }
    }
}

/// Why: criterion 108 — a crash during recovery is finished by the next
/// open, and a torn intent line leaves the pre-operation tree.
/// What: crash a move mid-way, crash the recovery at each of its steps, open
/// again; then a torn intent after staging.
/// Test: self-contained.
#[test]
fn c108_crash_during_recovery_and_torn_intent() {
    let (_t1, clean_root) = fresh();
    op_move(&mut Bundle::open(&clean_root).unwrap().0).unwrap();
    let clean = tree(&clean_root);
    for j in 0..6 {
        let (_t, root) = fresh();
        crash_at(&root, op_move, "step-2");
        let target = format!("recover-step-{j}");
        fail::set(move |n| n == target);
        let first = recover_all(&root);
        fail::clear();
        assert!(first.is_err(), "recovery crash {j}");
        recover_all(&root).unwrap();
        assert_eq!(tree(&root), clean, "recovery crash at step {j}");
    }

    let (_t0, before_root) = fresh();
    let before = tree(&before_root);
    let (_t, root) = fresh();
    crash_at(&root, op_move, "staged");
    let journal = root.join("_sources/ops/journal.jsonl");
    let mut text = std::fs::read_to_string(&journal).unwrap_or_default();
    text.push_str("{\"rec\":\"intent\",\"op\":\"move-0");
    std::fs::write(&journal, text).unwrap();
    recover_all(&root).unwrap();
    assert_eq!(tree(&root), before);
    assert!(
        !root
            .join("_sources/ops")
            .read_dir()
            .unwrap()
            .any(|e| e.unwrap().path().is_dir()),
        "staging removed"
    );
}

/// A bundle handle that runs no recovery, to inspect the journal as left.
fn raw(root: &Path) -> Bundle {
    Bundle {
        root: root.to_path_buf(),
        conflicts: Vec::new(),
        held: None,
    }
}

/// The one incomplete op ID.
fn pending_op(root: &Path) -> String {
    let (p, _) = read_ops(&raw(root)).unwrap();
    assert_eq!(p.len(), 1);
    p[0].op.clone()
}

/// Why: criterion 109 — a file changed between the crash and the next open
/// is not overwritten; a write to the operation's paths returns
/// `RecoveryPending`; `recover` in either direction keeps a copy of it under
/// `conflict/`.
/// What: crash a content-changing put after its revision append, edit the
/// concept, open, try a write, recover both ways.
/// Test: self-contained.
#[test]
fn c109_out_of_band_edit_after_crash_is_a_recovery_conflict() {
    for dir in [RecoveryDirection::RollForward, RecoveryDirection::RollBack] {
        let (_t, root) = fresh();
        crash_at(&root, op_put, "step-1");
        let op = pending_op(&root);
        let edited = b"---\ntype: Note\n---\nedited by hand\n".to_vec();
        std::fs::write(root.join("a.md"), &edited).unwrap();
        let (mut b, findings) = Bundle::open(&root).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f.kind == FindingKind::RecoveryConflict)
        );
        assert_eq!(
            std::fs::read(root.join("a.md")).unwrap(),
            edited,
            "not overwritten"
        );
        let mut c = b.get(&id("a")).unwrap();
        c.body = "again\n".into();
        let got = b.put(c, actor(), WriteMode::Lenient, at("2026-11-04T12:00:00Z"));
        assert!(
            matches!(got, Err(OkfError::RecoveryPending { .. })),
            "{got:?}"
        );
        let report = b.recover(&op, dir).unwrap();
        assert_eq!(report.conflicts, vec![std::path::PathBuf::from("a.md")]);
        let copy = root.join(format!("_sources/ops/{op}/conflict/a.md.okf-staged"));
        assert_eq!(std::fs::read(copy).unwrap(), edited, "{dir:?}");
    }
}

/// Why: criteria 126 and 127 — an out-of-band edit of a linking concept
/// after the intent stops `move_concept` before that step, writes no
/// completion record, keeps the edit and every link valid; a write to the
/// operation's paths is refused until `recover`, which keeps a copy, writes
/// the completion record and clears the marker so the next write succeeds.
/// What: a hook edits `b.md` at the intent boundary.
/// Test: self-contained.
#[test]
fn c126_out_of_band_edit_mid_move_stops_with_recovery_conflict() {
    for dir in [RecoveryDirection::RollForward, RecoveryDirection::RollBack] {
        let (_t, root) = fresh();
        let edited = b"---\ntype: Note\n---\nSee [a](/a.md), edited.\n".to_vec();
        let (r2, e2) = (root.clone(), edited.clone());
        fail::set(move |n| {
            if n == "intent" {
                std::fs::write(r2.join("b.md"), &e2).unwrap();
            }
            false
        });
        let mut b = Bundle::open(&root).unwrap().0;
        let got = op_move(&mut b);
        fail::clear();
        let Err(OkfError::RecoveryPending { op_id, paths }) = got else {
            panic!("{got:?}");
        };
        assert!(paths.contains(&std::path::PathBuf::from("b.md")));
        assert_eq!(std::fs::read(root.join("b.md")).unwrap(), edited);
        assert_eq!(pending_op(&root), op_id, "no completion record");
        assert!(links_resolve(&root));
        let mut c = b.get(&id("b")).unwrap();
        c.body = "See [a](/a.md), refused.\n".into();
        let got = b.put(c, actor(), WriteMode::Lenient, at("2026-11-04T12:00:00Z"));
        assert!(
            matches!(got, Err(OkfError::RecoveryPending { .. })),
            "{got:?}"
        );
        let (mut b, findings) = Bundle::open(&root).unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f.kind == FindingKind::RecoveryConflict)
        );
        b.recover(&op_id, dir).unwrap();
        let copy = root.join(format!("_sources/ops/{op_id}/conflict/b.md.okf-staged"));
        assert_eq!(std::fs::read(copy).unwrap(), edited);
        assert!(
            read_ops(&raw(&root)).unwrap().0.is_empty(),
            "completion written"
        );
        assert!(links_resolve(&root), "{dir:?}");
        let mut c = b.get(&id("b")).unwrap();
        let target = if dir == RecoveryDirection::RollForward {
            "z/a"
        } else {
            "a"
        };
        c.body = format!("See [a](/{target}.md), later.\n");
        b.put(c, actor(), WriteMode::Lenient, at("2026-11-04T12:30:00Z"))
            .unwrap();
    }
}

fn spec() -> SourceSpec {
    SourceSpec::new(
        "mail",
        Some("mail"),
        Locator::Connector {
            kind: "gmail".into(),
            params: ConnectorParams::new(),
        },
        "2026-11-04T00:00:00Z",
    )
}

fn item(n: usize, body: &str) -> SourceItem {
    SourceItem {
        item_id: format!("i{n}"),
        fingerprint: format!("fp-{n}-{body}"),
        name: format!("item {n}"),
        title: format!("item {n}"),
        body: body.into(),
        ..SourceItem::default()
    }
}

fn ingest(root: &Path, items: &[SourceItem], when: &str) -> Result<usize, OkfError> {
    ingest_items(
        root,
        &Profile::default_profile(),
        &spec(),
        items,
        false,
        at(when),
    )
    .map(|r| r.ingested + r.updated)
}

/// Why: criterion 66 — a crash between the revision-journal line and the
/// rename leaves the previous revision Saved, and the unwritten bytes never
/// read as Saved.
/// What: a re-ingest of a changed item crashes at that seam.
/// Test: self-contained.
#[test]
fn c66_crash_between_revision_line_and_rename_keeps_previous_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("kb");
    std::fs::create_dir_all(&root).unwrap();
    let machine = tempfile::tempdir().unwrap();
    let state = IndexStateDir::new(machine.path());
    ingest(&root, &[item(1, "one")], "2026-11-04T10:00:00Z").unwrap();
    let cid = crate::build::ingest::concept_id_for("mail", "item 1").unwrap();
    let before = std::fs::read(root.join(cid.file_name())).unwrap();

    fail::set(|n| n == "after-revision-line");
    let r = ingest_items(
        &root,
        &Profile::default_profile(),
        &spec(),
        &[item(1, "two")],
        false,
        at("2026-11-04T11:00:00Z"),
    );
    fail::clear();
    assert_eq!(r.unwrap().errors.len(), 1, "the item failed at the seam");
    assert_eq!(std::fs::read(root.join(cid.file_name())).unwrap(), before);
    let lines = revisions::read(&root).unwrap();
    assert_eq!(lines.iter().filter(|l| l.id == cid.as_str()).count(), 2);
    let b = Bundle::open(&root).unwrap().0;
    let rs = b
        .revision_state(&cid, &state, "kb", &LiveIdentity::Unsupported)
        .unwrap();
    assert_eq!(rs.save, SaveState::Saved);
}

/// Why: #10746 S5 ruling — an ingest run writes `log.md` once, whatever its
/// size, and keeps one entry per written item.
/// What: 40 items, then 40 changed items; each run writes `log.md` once.
/// Test: self-contained.
#[test]
fn ingest_run_writes_log_md_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for (round, body) in ["one", "two"].iter().enumerate() {
        let items: Vec<SourceItem> = (0..40).map(|n| item(n, body)).collect();
        fail::reset_writes();
        let when = format!("2026-11-04T1{round}:00:00Z");
        assert_eq!(ingest(&root, &items, &when).unwrap(), 40);
        assert_eq!(fail::writes("log.md"), 1, "round {round}");
    }
    let log = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert_eq!(log.matches("**Created**").count(), 40);
    assert_eq!(log.matches("**Updated**").count(), 40);
}

/// Why: the batched log keeps the crash guarantees: a run that dies before
/// or during its one `log.md` write still logs every written item on the
/// next open, exactly once, and fails closed.
/// What: crash before the log operation's intent, and after it.
/// Test: self-contained.
#[test]
fn crash_mid_run_keeps_the_log_entries_of_written_items() {
    for name in ["staged", "intent"] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let items: Vec<SourceItem> = (0..3).map(|n| item(n, "one")).collect();
        let target = name.to_string();
        fail::set(move |n| n == target);
        let got = ingest(&root, &items, "2026-11-04T10:00:00Z");
        fail::clear();
        assert!(got.is_err(), "{name}: the run fails closed");
        assert!(!root.join("log.md").exists());
        recover_all(&root).unwrap();
        recover_all(&root).unwrap();
        let log = std::fs::read_to_string(root.join("log.md")).unwrap();
        assert_eq!(log.matches("**Created**").count(), 3, "{name}");
        let left: Vec<_> = std::fs::read_dir(root.join("_sources/ops"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("log-"))
            .collect();
        assert!(left.is_empty(), "{name}: {left:?}");
    }
}

// ---- #10746 S5 fix round (r2) ----

fn op_verify_c(b: &mut Bundle) -> Result<(), OkfError> {
    let h = content_hash(&b.get(&id("c"))?);
    let v = Verification {
        by: Actor::Process("second".into()),
        at: at("2026-11-04T11:00:00Z"),
        stale_after: None,
    };
    b.verify(&id("c"), v, &h).map(|_| ())
}

/// `fresh()` plus an unverified concept `c`.
fn fresh_with_c() -> (tempfile::TempDir, std::path::PathBuf) {
    let (t, root) = fresh();
    let mut b = Bundle::open(&root).unwrap().0;
    b.put(
        new_concept("c", "Gamma\n"),
        actor(),
        WriteMode::Lenient,
        at("2026-11-04T10:10:00Z"),
    )
    .unwrap();
    (t, root)
}

fn verification_of(root: &Path, name: &str) -> (VerificationState, Vec<VerificationEvent>) {
    let b = raw(root);
    (
        b.verification_state(&id(name)).unwrap(),
        b.verification_events(&id(name)).unwrap(),
    )
}

/// Why: r2 #1 — a chunked source calls `ingest_items` once per chunk with
/// one `now`; every call's entries must reach `log.md`, and a retry too.
/// What: two calls and a retry with the same spec and `now`.
/// Test: self-contained.
#[test]
fn two_ingest_calls_with_one_now_log_every_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let when = "2026-11-04T10:00:00Z";
    ingest(&root, &[item(0, "a"), item(1, "a")], when).unwrap();
    ingest(&root, &[item(2, "a"), item(3, "a")], when).unwrap();
    ingest(&root, &[item(0, "b")], when).unwrap();
    let log = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert_eq!(log.matches("**Created**").count(), 4, "{log}");
    assert_eq!(log.matches("**Updated**").count(), 1, "{log}");
}

/// Why: r2 #2 — a verify that failed mid-write and was rolled back in
/// process leaves the concept in its pre-operation verification state.
/// What: an I/O error on the concept write of a verify of unverified `c`.
/// Test: self-contained.
#[test]
fn aborted_verify_reads_its_pre_op_state() {
    let (_t, root) = fresh_with_c();
    let before = verification_of(&root, "c");
    fail::set_io(|n| n == "c.md");
    let got = op_verify_c(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(matches!(got, Err(OkfError::Io { .. })), "{got:?}");
    assert_eq!(verification_of(&root, "c"), before);
    assert!(read_ops(&raw(&root)).unwrap().0.is_empty(), "rolled back");
}

/// Why: r2 #2 — `recover(RollBack)` of an interrupted verify leaves the
/// concept in its pre-operation verification state.
/// What: crash a verify of `c` after its record line, then roll back.
/// Test: self-contained.
#[test]
fn recover_roll_back_of_verify_reads_pre_op_state() {
    let (_t, root) = fresh_with_c();
    let before = verification_of(&root, "c");
    crash_at(&root, op_verify_c, "step-0");
    let op = pending_op(&root);
    raw(&root)
        .recover(&op, RecoveryDirection::RollBack)
        .unwrap();
    assert_eq!(verification_of(&root, "c"), before);
}

/// Why: r2 #2 — a roll back interrupted after its compensation line, then
/// rolled forward, reads exactly as the uninterrupted operation: the
/// step's own record line wins again, with no `verification_mismatch`.
/// What: an I/O error on the concept write of a content put on verified
/// `a` makes `run` roll back; a crash stops that roll back after its
/// compensation; recovery then rolls forward.
/// Test: self-contained.
#[test]
fn roll_forward_after_interrupted_roll_back_is_consistent() {
    let (_t1, clean_root) = fresh();
    op_put(&mut Bundle::open(&clean_root).unwrap().0).unwrap();
    let (_t, root) = fresh();
    fail::set_io(|n| n == "a.md");
    fail::set(|n| n == "recover-step-0");
    let got = op_put(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(
        matches!(got, Err(OkfError::RecoveryPending { .. })),
        "{got:?}"
    );
    recover_all(&root).unwrap();
    let (_, findings) = Bundle::open(&root).unwrap();
    assert!(
        findings
            .iter()
            .all(|f| f.kind != FindingKind::VerificationMismatch),
        "{findings:?}"
    );
    assert_eq!(events(&root), events(&clean_root));
    assert_eq!(
        verification_of(&root, "a"),
        verification_of(&clean_root, "a")
    );
    for f in ["a.md", "log.md"] {
        assert_eq!(tree(&root)[f], tree(&clean_root)[f], "{f}");
    }
}

/// Why: r2 #2 — rolling back the `legacy_unbound` invalidation of an event
/// that lived only in frontmatter adds no history.
/// What: an I/O error on the concept write of a content put on a concept
/// with a hashless frontmatter event.
/// Test: self-contained.
#[test]
fn aborted_put_keeps_a_legacy_event_out_of_history() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::write(
        root.join("l.md"),
        "---\ntype: Note\nverified:\n  - { by: human:old, at: 2026-01-01T00:00:00Z }\n---\nBody\n",
    )
    .unwrap();
    let before = verification_of(&root, "l");
    let mut b = Bundle::open(&root).unwrap().0;
    let mut c = b.get(&id("l")).unwrap();
    c.body = "Changed\n".into();
    fail::set_io(|n| n == "l.md");
    let got = b.put(c, actor(), WriteMode::Lenient, at("2026-11-04T11:00:00Z"));
    fail::clear();
    assert!(matches!(got, Err(OkfError::Io { .. })), "{got:?}");
    assert_eq!(verification_of(&root, "l"), before);
}

/// Why: r2 #3 — an ingest flush must not write `log.md` while a conflicted
/// operation owns it; the batch stays and is written after `recover`.
/// What: a put crashed after its revision line, `a.md` edited by hand, an
/// ingest run, then `recover` forward.
/// Test: self-contained.
#[test]
fn ingest_flush_waits_for_a_conflicted_log_op() {
    let (_t, root) = fresh();
    crash_at(&root, op_put, "step-1");
    let op = pending_op(&root);
    std::fs::write(root.join("a.md"), "---\ntype: Note\n---\nby hand\n").unwrap();
    let items: Vec<SourceItem> = (0..3).map(|n| item(n, "one")).collect();
    let got = ingest(&root, &items, "2026-11-04T12:00:00Z");
    assert!(
        matches!(got, Err(OkfError::RecoveryPending { .. })),
        "{got:?}"
    );
    let batches = || {
        std::fs::read_dir(root.join("_sources/ops"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("log-")
            })
            .count()
    };
    assert_eq!(batches(), 1, "the batch is kept");
    let (mut b, _) = Bundle::open(&root).unwrap();
    b.recover(&op, RecoveryDirection::RollForward).unwrap();
    let log = std::fs::read_to_string(root.join("log.md")).unwrap();
    assert_eq!(log.matches("**Created** `/mail/").count(), 3, "{log}");
    assert_eq!(batches(), 0);
}

/// Why: r2 #4 — without the lock, a reader's `open` must neither remove a
/// live writer's staging (a later crash would then be unrecoverable) nor
/// complete its operation under it.
/// What: a reader opens the bundle at the writer's `staged` boundary (the
/// writer then crashes) and at its `intent` boundary (the writer goes on).
/// Test: self-contained.
#[test]
fn concurrent_open_never_recovers_a_live_op() {
    let (_t1, clean_root) = fresh();
    op_put(&mut Bundle::open(&clean_root).unwrap().0).unwrap();
    let clean = tree(&clean_root);

    let (_t, root) = fresh();
    let r = root.clone();
    fail::set(move |n| {
        if n == "staged" {
            Bundle::open(&r).unwrap();
        }
        n == "step-1"
    });
    let got = op_put(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(got.is_err());
    recover_all(&root).unwrap();
    assert_eq!(tree(&root), clean, "staged reader");

    let (_t, root) = fresh();
    let r = root.clone();
    fail::set(move |n| {
        if n == "intent" {
            Bundle::open(&r).unwrap();
        }
        false
    });
    let got = op_put(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(got.is_ok(), "{got:?}");
    assert_eq!(tree(&root), clean, "intent reader");
}

/// Why: r2 #5 — a read error in the pre-image check after earlier steps
/// rolls the operation back instead of leaving it half done.
/// What: the first read of `a.md` during a content put fails.
/// Test: self-contained.
#[test]
fn precheck_read_error_aborts_the_operation() {
    let (_t, root) = fresh();
    let before = verification_of(&root, "a");
    let before_a = tree(&root)["a.md"].clone();
    let mut fired = false;
    fail::set_io(move |n| {
        let hit = n == "read:a.md" && !fired;
        fired |= hit;
        hit
    });
    let got = op_put(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(matches!(got, Err(OkfError::Io { .. })), "{got:?}");
    assert_eq!(verification_of(&root, "a"), before);
    assert_eq!(tree(&root)["a.md"], before_a);
    assert!(read_ops(&raw(&root)).unwrap().0.is_empty());
}

/// Why: r2 #5 — when every step ran but the completion record failed, the
/// handle must stop writing to the operation's paths; when only the
/// staging cleanup failed, the write succeeded and says so in a finding.
/// What: the second journal append fails, then a later put on the same
/// handle; then the staging removal fails.
/// Test: self-contained.
#[test]
fn completion_failure_marks_conflict_and_staging_failure_is_a_finding() {
    let (_t, root) = fresh();
    let mut b = Bundle::open(&root).unwrap().0;
    let mut appends = 0;
    fail::set_io(move |n| {
        if n == "_sources/ops/journal.jsonl" {
            appends += 1;
            return appends == 2;
        }
        false
    });
    let got = op_put(&mut b);
    fail::clear();
    assert!(
        matches!(got, Err(OkfError::RecoveryPending { .. })),
        "{got:?}"
    );
    let mut c = b.get(&id("a")).unwrap();
    c.body = "later\n".into();
    let got = b.put(c, actor(), WriteMode::Lenient, at("2026-11-04T12:00:00Z"));
    assert!(
        matches!(got, Err(OkfError::RecoveryPending { .. })),
        "{got:?}"
    );

    let (_t, root) = fresh();
    let mut b = Bundle::open(&root).unwrap().0;
    let mut c = b.get(&id("b")).unwrap();
    c.body = "changed\n".into();
    fail::set_io(|n| n.starts_with("remove-staging:"));
    let got = b.put(c, actor(), WriteMode::Lenient, at("2026-11-04T12:00:00Z"));
    fail::clear();
    let (_, findings) = got.unwrap();
    assert!(
        findings
            .iter()
            .any(|f| format!("{:?}", f.kind) == "StagingLeft"),
        "{findings:?}"
    );
}
