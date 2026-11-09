//! Slice S6 of #10746: `rebuild` with history and ownership proof, and
//! `adopt` (SPEC-OKF-01 §4.5, §3 (q) C, §2.10 "Events with no hash"; §6.2
//! criteria 28, 29, 30, 110, 111, 130, 131, 142). Criterion 112 (crash and
//! mid-swap edits) lives in `src/bundle/s6_fault_tests.rs`.

mod s6_support;

use std::collections::BTreeMap;
use std::path::Path;

use s6_support::*;
use serde_yaml::Value;
use trusty_okf::OkfError;
use trusty_okf::build::ingest::{SourceItem, concept_id_for};
use trusty_okf::build::rebuild::{RebuildReport, rebuild};
use trusty_okf::build::registry::{SourceRegistry, SourceSpec};
use trusty_okf::bundle::{Bundle, HumanConfirmation, VerificationState};
use trusty_okf::format::{Actor, Concept, ConceptId, Verification, WriteMode, content_hash};
use trusty_okf::profile::Profile;
use trusty_okf::state::{ByteHash, IndexStateDir};

fn run(root: &Path, sources: &[(SourceSpec, Vec<SourceItem>)], at: &str) -> RebuildReport {
    rebuild(root, &Profile::default_profile(), sources, instant(at)).unwrap()
}

fn reasons(r: &RebuildReport) -> Vec<(String, String)> {
    r.conflicts
        .iter()
        .map(|c| (c.id.as_str().to_string(), c.reason.clone()))
        .collect()
}

fn verify(b: &mut Bundle, cid: &ConceptId, at: &str) {
    let h = content_hash(&b.get(cid).unwrap());
    let v = Verification {
        by: process(),
        at: instant(at),
        stale_after: None,
    };
    b.verify(cid, v, &h).unwrap();
}

fn renew(b: &mut Bundle, cid: &ConceptId, until: &str) {
    let mut c = b.get(cid).unwrap();
    c.set("stale_after", Value::String(until.into()));
    b.put(
        c,
        process(),
        WriteMode::Lenient,
        instant("2026-11-04T10:20:00Z"),
    )
    .unwrap();
}

fn unverified_with_history(root: &Path, cid: &ConceptId) -> bool {
    matches!(
        Bundle::open(root).unwrap().0.verification_state(cid).unwrap(),
        VerificationState::Unverified { history } if history >= 1
    )
}

/// Why: criterion 28 — `rebuild` leaves every hand-owned concept
/// byte-identical, and refuses a hand-edited ledger-owned concept with
/// `Conflict` while the others rebuild.
/// What: ingest three items, add a hand-owned concept, hand-edit one item's
/// concept, rebuild with two changed items.
/// Test: self-contained.
#[test]
fn c28_rebuild_keeps_hand_owned_and_refuses_hand_edits() {
    let (_t, root) = fresh();
    let spec = docstore("docs", "notes");
    ingest(
        &root,
        &spec,
        &[item("a", "A1"), item("b", "B1"), item("c", "C1")],
        "2026-11-04T10:00:00Z",
    );
    let mut m = serde_yaml::Mapping::new();
    m.insert("type".into(), "Note".into());
    Bundle::open(&root)
        .unwrap()
        .0
        .put(
            Concept::new(id("notes/hand"), m, "Mine.\n".into()),
            process(),
            WriteMode::Lenient,
            instant("2026-11-04T10:01:00Z"),
        )
        .unwrap();
    let b_path = root.join("notes/b.md");
    let edited = format!("{}Hand note.\n", read(&root, "notes/b.md"));
    std::fs::write(&b_path, &edited).unwrap();
    let hand = read(&root, "notes/hand.md");
    let r = run(
        &root,
        &[(
            spec,
            vec![item("a", "A2"), item("b", "B2"), item("c", "C1")],
        )],
        "2026-11-04T11:00:00Z",
    );
    assert_eq!(
        reasons(&r),
        [("notes/b".to_string(), "hand_edit".to_string())]
    );
    assert_eq!(r.rebuilt, [id("notes/a")]);
    assert_eq!(read(&root, "notes/b.md"), edited);
    assert_eq!(read(&root, "notes/hand.md"), hand);
    assert_eq!(
        Bundle::open(&root)
            .unwrap()
            .0
            .get(&id("notes/a"))
            .unwrap()
            .body,
        "\nA2\n"
    );
}

/// Why: criterion 29 — two rebuilds from equal store contents and an equal
/// ledger, with no hand edits, run at different wall-clock times, yield
/// byte-identical `.md` trees; a rebuild over its own output writes nothing.
/// What: two equal bundles, rebuilt a second apart with equal inputs.
/// Test: self-contained.
#[test]
fn c29_rebuilds_at_different_times_are_byte_identical() {
    let spec = docstore("docs", "notes");
    let make = || {
        let (t, root) = fresh();
        ingest(
            &root,
            &spec,
            &[item("a", "A1"), item("b", "B1")],
            "2026-11-04T10:00:00Z",
        );
        (t, root)
    };
    let ((_t1, r1), (_t2, r2)) = (make(), make());
    let sources = [(spec.clone(), vec![item("a", "A2"), item("b", "B2")])];
    run(&r1, &sources, "2026-11-04T11:00:00Z");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    run(&r2, &sources, "2026-11-04T11:00:00Z");
    assert_eq!(md_tree(&r1), md_tree(&r2));
    let before = tree(&r1);
    let again = run(&r1, &sources, "2026-11-04T12:00:00Z");
    assert!(
        again.rebuilt.is_empty() && again.conflicts.is_empty(),
        "{again:?}"
    );
    assert_eq!(tree(&r1), before, "zero bytes written");
}

/// Why: criterion 30 — a `build` with no store change writes no files and
/// yields an empty feed backlog (no concept Pending once fed).
/// What: ingest, record each concept as fed and probed, ingest again.
/// Test: self-contained.
#[test]
fn c30_build_with_no_store_change_writes_nothing() {
    let (t, root) = fresh();
    let state = IndexStateDir::new(t.path().join("machine"));
    let spec = docstore("docs", "notes");
    let items = [item("a", "A1"), item("b", "B1")];
    ingest(&root, &spec, &items, "2026-11-04T10:00:00Z");
    ack_all(&root, &state);
    let before = tree(&root);
    let r = ingest(&root, &spec, &items, "2026-11-04T11:00:00Z");
    assert_eq!((r.ingested, r.updated, r.skipped), (0, 0, 2));
    assert_eq!(tree(&root), before);
    let s = Bundle::open(&root)
        .unwrap()
        .0
        .state_summary(&state, INDEX, &live())
        .unwrap();
    assert_eq!(s.pending + s.acknowledged + s.not_indexed, 0, "{s:?}");
    assert!(s.synced());
}

/// Remove `added` (one contiguous run of lines) from `text`.
fn without(text: &str, added: &[String]) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let n = added.len();
    let at = (0..=lines.len() - n)
        .find(|&i| lines[i..i + n].iter().zip(added).all(|(a, b)| a == b))
        .expect("added lines are contiguous");
    let mut kept = lines[..at].to_vec();
    kept.extend_from_slice(&lines[at + n..]);
    kept.join("\n")
}

/// Why: criterion 110 — a rebuild of a bundle whose `log.md` holds
/// deprecate, supersede, move, verify and correction entries removes,
/// rewrites and reorders none: removing the lines it reports as added
/// yields the old `log.md` byte for byte.
/// What: build such a log, rebuild on the same and on a later date.
/// Test: self-contained.
#[test]
fn c110_rebuild_only_inserts_its_own_log_lines() {
    for when in ["2026-11-04T12:00:00Z", "2026-11-05T09:00:00Z"] {
        let (_t, root) = fresh();
        let spec = docstore("docs", "notes");
        ingest(
            &root,
            &spec,
            &[item("a", "A1"), item("b", "B1")],
            "2026-11-04T10:00:00Z",
        );
        let mut b = Bundle::open(&root).unwrap().0;
        let t = instant("2026-11-04T11:00:00Z");
        let mut m = serde_yaml::Mapping::new();
        m.insert("type".into(), "Note".into());
        b.put(
            Concept::new(id("x"), m.clone(), "X.\n".into()),
            process(),
            WriteMode::Lenient,
            t,
        )
        .unwrap();
        verify(&mut b, &id("x"), "2026-11-04T11:01:00Z");
        let mut c = b.get(&id("x")).unwrap();
        c.body = "X, corrected.\n".into();
        b.put(c, process(), WriteMode::Lenient, t).unwrap();
        b.move_concept(&id("x"), &id("z/x"), t).unwrap();
        b.deprecate(&id("z/x"), "old", process(), t).unwrap();
        b.supersede(
            &id("notes/b"),
            Concept::new(id("y"), m, "Y.\n".into()),
            "split",
            process(),
            t,
        )
        .unwrap();
        let old = read(&root, "log.md");
        for verb in [
            "**Deprecated**",
            "**Superseded**",
            "**Moved**",
            "**Verified**",
            "content changed",
        ] {
            assert!(old.contains(verb), "{verb}: {old}");
        }
        let r = run(
            &root,
            &[(spec, vec![item("a", "A2"), item("b", "B2")])],
            when,
        );
        assert_eq!(r.rebuilt.len(), 2, "{r:?}");
        assert!(!r.log_added.is_empty());
        assert_eq!(without(&read(&root, "log.md"), &r.log_added), old, "{when}");
    }
}

/// The golden tree's sources, from its own registry.
fn golden_spec(root: &Path, source: &str) -> SourceSpec {
    SourceRegistry::load(root)
        .unwrap()
        .get(source)
        .unwrap()
        .clone()
}

/// The golden `sent-mail` item, equal to what trusty-kb ingested.
fn mail_item() -> SourceItem {
    SourceItem {
        item_id: "18c0ffee".into(),
        fingerprint: "18c0ffee".into(),
        name: "2026-02-01 Re: update".into(),
        title: "Re: プロジェクト — update".into(),
        timestamp: Some("2026-02-01T09:00:00Z".into()),
        body: "Thanks — sending the plan.\n\n> quoted inbound line".into(),
        fields: BTreeMap::from([
            ("from".to_string(), "me@example.com".to_string()),
            ("thread_id".to_string(), "thread-7".to_string()),
            ("to".to_string(), "team@example.com".to_string()),
        ]),
        ..SourceItem::default()
    }
}

/// The golden `field-notes` item `alpha.md` with `body`.
fn alpha_item(body: &str) -> SourceItem {
    SourceItem {
        item_id: "alpha.md".into(),
        fingerprint: "sha256:aaab".into(),
        name: "alpha".into(),
        title: "alpha".into(),
        timestamp: Some("2026-07-03T08:00:00Z".into()),
        body: body.into(),
        fields: BTreeMap::from([("source_path".to_string(), "alpha.md".to_string())]),
        ..SourceItem::default()
    }
}

/// Why: criterion 111 and §3 (q) A — a legacy ledger-owned concept with no
/// written hash whose bytes the equality proof cannot reproduce stays
/// byte-identical as `no_written_hash`; one it reproduces is proven; a
/// concept whose bytes differ from its stored hash is `hand_edit` and stays
/// byte-identical; the others rebuild.
/// What: the golden tree plus a trusty-okf source.
/// Test: self-contained.
#[test]
fn c111_unproven_and_hand_edited_ledger_concepts_are_preserved() {
    let (_t, root) = golden();
    let docs = docstore("docs", "docs");
    ingest(
        &root,
        &docs,
        &[item("d1", "D1"), item("d2", "D2")],
        "2026-11-04T10:00:00Z",
    );
    let edited = format!("{}hand\n", read(&root, "docs/d1.md"));
    std::fs::write(root.join("docs/d1.md"), &edited).unwrap();
    let before = tree(&root);
    let r = run(
        &root,
        &[
            (
                golden_spec(&root, "field-notes"),
                vec![alpha_item("alpha body, re-revised")],
            ),
            (golden_spec(&root, "sent-mail"), vec![mail_item()]),
            (docs, vec![item("d1", "D1b"), item("d2", "D2b")]),
        ],
        "2026-11-04T11:00:00Z",
    );
    let mut got = reasons(&r);
    got.sort();
    assert_eq!(
        got,
        [
            ("docs/d1".to_string(), "hand_edit".to_string()),
            ("notes/alpha".to_string(), "no_written_hash".to_string()),
        ]
    );
    assert_eq!(r.proven, [id("mail/2026-02-01-re-update")]);
    assert_eq!(r.rebuilt, [id("docs/d2")]);
    let after = tree(&root);
    for rel in [
        "notes/alpha.md",
        "docs/d1.md",
        "mail/2026-02-01-re-update.md",
    ] {
        assert_eq!(after[rel], before[rel], "{rel}");
    }
}

/// Why: criterion 130 (row 2) — after `verify` with the current hash and a
/// `stale_after` renewal, a changed source item reaches the concept:
/// `rebuild` updates the body, keeps the renewal, moves the verification to
/// history, and reports no `hand_edit`, `caller_edit` or `no_written_hash`;
/// a re-import through a docstore store gives the same result.
/// What: both paths on two equal bundles.
/// Test: self-contained.
#[test]
fn c130_rebuild_follows_a_source_change_over_verify_and_renewal() {
    let spec = docstore("docs", "notes");
    let cid = concept_id_for("notes", "a").unwrap();
    for reimport in [false, true] {
        let (_t, root) = fresh();
        ingest(&root, &spec, &[item("a", "One.")], "2026-11-04T10:00:00Z");
        let mut b = Bundle::open(&root).unwrap().0;
        verify(&mut b, &cid, "2026-11-04T10:10:00Z");
        renew(&mut b, &cid, "2027-03-01T00:00:00Z");
        if reimport {
            ingest(&root, &spec, &[item("a", "Two.")], "2026-11-04T11:00:00Z");
        } else {
            let r = run(
                &root,
                &[(spec.clone(), vec![item("a", "Two.")])],
                "2026-11-04T11:00:00Z",
            );
            assert!(r.conflicts.is_empty(), "{r:?}");
            assert_eq!(r.rebuilt, std::slice::from_ref(&cid));
        }
        let c = Bundle::open(&root).unwrap().0.get(&cid).unwrap();
        assert_eq!(c.body, "\nTwo.\n", "reimport={reimport}");
        assert_eq!(
            c.get("stale_after").and_then(Value::as_str),
            Some("2027-03-01T00:00:00Z")
        );
        assert!(c.get("verified").is_none());
        assert!(unverified_with_history(&root, &cid), "reimport={reimport}");
    }
}

/// Why: criterion 131 (row 2, (c2)(i)) — after `verify`, `deprecate` and a
/// renewal, a source change reaches the deprecated concept through
/// `rebuild` and a re-import, keeping `status: deprecated`; a
/// content-changing `put` on another ledger-owned concept makes the next
/// `rebuild` report `caller_edit` and leave it byte-identical; a `migrate`
/// of a concept with no written hash leaves the next `rebuild` reporting
/// `no_written_hash`.
/// What: the three cases in turn.
/// Test: self-contained.
#[test]
fn c131_caller_edit_and_migrated_legacy_concepts_are_preserved() {
    let spec = docstore("docs", "notes");
    let (a, b_id) = (
        concept_id_for("notes", "a").unwrap(),
        concept_id_for("notes", "b").unwrap(),
    );
    for reimport in [false, true] {
        let (_t, root) = fresh();
        ingest(
            &root,
            &spec,
            &[item("a", "One."), item("b", "Bee.")],
            "2026-11-04T10:00:00Z",
        );
        let mut b = Bundle::open(&root).unwrap().0;
        verify(&mut b, &a, "2026-11-04T10:10:00Z");
        b.deprecate(&a, "curated", process(), instant("2026-11-04T10:15:00Z"))
            .unwrap();
        renew(&mut b, &a, "2027-03-01T00:00:00Z");
        let changed = vec![item("a", "Two."), item("b", "Bee.")];
        if reimport {
            ingest(&root, &spec, &changed, "2026-11-04T11:00:00Z");
        } else {
            let r = run(&root, &[(spec.clone(), changed)], "2026-11-04T11:00:00Z");
            assert!(r.conflicts.is_empty(), "{r:?}");
        }
        let c = Bundle::open(&root).unwrap().0.get(&a).unwrap();
        assert_eq!(c.body, "\nTwo.\n");
        assert_eq!(c.get("status").and_then(Value::as_str), Some("deprecated"));
        assert_eq!(
            c.get("stale_after").and_then(Value::as_str),
            Some("2027-03-01T00:00:00Z")
        );
        assert!(unverified_with_history(&root, &a));
        if reimport {
            continue;
        }
        let mut b = Bundle::open(&root).unwrap().0;
        let mut c = b.get(&b_id).unwrap();
        c.body = "Bee, by a caller.\n".into();
        b.put(
            c,
            process(),
            WriteMode::Lenient,
            instant("2026-11-04T12:00:00Z"),
        )
        .unwrap();
        let before = read(&root, "notes/b.md");
        let r = run(
            &root,
            &[(spec.clone(), vec![item("a", "Two."), item("b", "Bee 2.")])],
            "2026-11-04T13:00:00Z",
        );
        assert_eq!(
            reasons(&r),
            [("notes/b".to_string(), "caller_edit".to_string())]
        );
        assert_eq!(read(&root, "notes/b.md"), before);
    }
    // A migrated legacy concept: no written hash, and no longer the v0.1 render.
    let (_t, root) = golden();
    let mut b = Bundle::open(&root).unwrap().0;
    let plan = b.plan_migration().unwrap();
    b.migrate(&plan, instant("2026-11-04T12:00:00Z")).unwrap();
    let before = read(&root, "notes/alpha.md");
    let r = run(
        &root,
        &[(
            golden_spec(&root, "field-notes"),
            vec![alpha_item("alpha body, revised")],
        )],
        "2026-11-04T13:00:00Z",
    );
    assert_eq!(
        reasons(&r),
        [("notes/alpha".to_string(), "no_written_hash".to_string())]
    );
    assert_eq!(read(&root, "notes/alpha.md"), before);
}

/// The golden tree with a hashless human event on `alpha` and the mail
/// concept, in frontmatter and in a hashless record line.
fn legacy_reviewed() -> (tempfile::TempDir, std::path::PathBuf) {
    let (t, root) = golden();
    for (rel, cid) in [
        ("notes/alpha.md", "notes/alpha"),
        ("mail/2026-02-01-re-update.md", "mail/2026-02-01-re-update"),
    ] {
        let text = read(&root, rel).replacen(
            "---\n",
            "---\nverified:\n- {by: 'human:jo', at: '2026-07-30T00:00:00Z'}\n",
            1,
        );
        std::fs::write(root.join(rel), text).unwrap();
        let rec = root.join(format!("_sources/verification/{cid}.jsonl"));
        std::fs::create_dir_all(rec.parent().unwrap()).unwrap();
        std::fs::write(
            rec,
            "{\"by\":\"human:jo\",\"at\":\"2026-07-30T00:00:00Z\"}\n",
        )
        .unwrap();
    }
    (t, root)
}

fn no_event_has_a_hash(root: &Path, cid: &ConceptId) {
    let b = Bundle::open(root).unwrap().0;
    assert!(
        b.verification_events(cid)
            .unwrap()
            .iter()
            .all(|e| e.hash.is_none())
    );
    let c = b.get(cid).unwrap();
    for e in c.get("verified").and_then(Value::as_sequence).unwrap() {
        assert!(e.get("hash").is_none(), "{e:?}");
    }
}

/// Why: criterion 142 (row 17, case 2) — after `Bundle::open`, `migrate`,
/// `adopt` and `rebuild`, in turn, a legacy concept with hashless human
/// events reads Unverified with history; no event gains a hash; a `verify`
/// with the current hash then reads Verified.
/// What: the golden tree with hashless events, through the four steps.
/// Test: self-contained.
#[test]
fn c142_legacy_hashless_events_never_gain_a_hash() {
    let (_t, root) = legacy_reviewed();
    let ids = [id("notes/alpha"), id("mail/2026-02-01-re-update")];
    let check = |step: &str| {
        for cid in &ids {
            assert!(unverified_with_history(&root, cid), "{step}: {cid}");
            no_event_has_a_hash(&root, cid);
        }
    };
    check("open");
    let mut b = Bundle::open(&root).unwrap().0;
    let plan = b.plan_migration().unwrap();
    b.migrate(&plan, instant("2026-11-04T10:00:00Z")).unwrap();
    check("migrate");
    let reviewed: Vec<(ConceptId, ByteHash)> = ids
        .iter()
        .map(|c| {
            (
                c.clone(),
                ByteHash::of(&std::fs::read(root.join(c.file_name())).unwrap()),
            )
        })
        .collect();
    let report = b
        .adopt(
            &reviewed,
            Actor::Human("jo".into()),
            Some(HumanConfirmation::explicit()),
            instant("2026-11-04T10:30:00Z"),
        )
        .unwrap();
    assert_eq!(report.adopted, ids.to_vec(), "{report:?}");
    check("adopt");
    let r = run(
        &root,
        &[
            (
                golden_spec(&root, "field-notes"),
                vec![alpha_item("alpha body, revised")],
            ),
            (golden_spec(&root, "sent-mail"), vec![mail_item()]),
        ],
        "2026-11-04T11:00:00Z",
    );
    assert!(r.conflicts.is_empty(), "{r:?}");
    check("rebuild");
    let mut b = Bundle::open(&root).unwrap().0;
    let h = content_hash(&b.get(&ids[0]).unwrap());
    let v = Verification {
        by: Actor::Human("jo".into()),
        at: instant("2026-11-04T12:00:00Z"),
        stale_after: None,
    };
    b.verify_confirmed(&ids[0], v, &h, HumanConfirmation::explicit())
        .unwrap();
    assert!(matches!(
        b.verification_state(&ids[0]).unwrap(),
        VerificationState::Verified { human: true, .. }
    ));
}

/// Why: criterion 142 (r7.1) — `adopt` with a `Process` actor is
/// `not_human`, without confirmation `not_confirmed`, and a concept edited
/// after its byte hash was taken is refused `byte_hash_mismatch` and stays
/// `no_written_hash` on the next `rebuild`; no refused case changes a file
/// or record. The other §4.5 reasons are `not_found`, `not_ledger_owned`
/// and `already_proven`.
/// What: a second legacy bundle, each refusal in turn.
/// Test: self-contained.
#[test]
fn c142_adopt_refusals_change_nothing() {
    let (_t, root) = legacy_reviewed();
    std::fs::write(root.join("notes/hand.md"), "---\ntype: Note\n---\nHand.\n").unwrap();
    let alpha = id("notes/alpha");
    let mail = id("mail/2026-02-01-re-update");
    let hash = |rel: &str| ByteHash::of(&std::fs::read(root.join(rel)).unwrap());
    let reviewed = vec![(alpha.clone(), hash("notes/alpha.md"))];
    let before = tree(&root);
    let mut b = Bundle::open(&root).unwrap().0;
    let t = instant("2026-11-04T10:00:00Z");
    for (by, confirm, reason) in [
        (process(), Some(HumanConfirmation::explicit()), "not_human"),
        (Actor::Human("jo".into()), None, "not_confirmed"),
    ] {
        match b.adopt(&reviewed, by, confirm, t) {
            Err(OkfError::AdoptRefused { reason: r }) => assert_eq!(r, reason),
            other => panic!("{other:?}"),
        }
        assert_eq!(tree(&root), before, "{reason}: no file changed");
    }
    let edited = format!("{}edited after review\n", read(&root, "notes/alpha.md"));
    std::fs::write(root.join("notes/alpha.md"), &edited).unwrap();
    let before = tree(&root);
    let pairs = vec![
        (alpha.clone(), reviewed[0].1.clone()),
        (id("notes/missing"), hash("notes/hand.md")),
        (id("notes/hand"), hash("notes/hand.md")),
    ];
    let human = || Actor::Human("jo".into());
    let r = b
        .adopt(&pairs, human(), Some(HumanConfirmation::explicit()), t)
        .unwrap();
    let got: Vec<&str> = r.refused.iter().map(|(_, why)| why.as_str()).collect();
    assert_eq!(got, ["byte_hash_mismatch", "not_found", "not_ledger_owned"]);
    assert!(r.adopted.is_empty());
    assert_eq!(tree(&root), before, "refused pairs change nothing");
    let mail_pair = vec![(mail.clone(), hash("mail/2026-02-01-re-update.md"))];
    assert_eq!(
        b.adopt(&mail_pair, human(), Some(HumanConfirmation::explicit()), t)
            .unwrap()
            .adopted,
        std::slice::from_ref(&mail)
    );
    let again = b
        .adopt(&mail_pair, human(), Some(HumanConfirmation::explicit()), t)
        .unwrap();
    assert_eq!(again.refused, [(mail, "already_proven".to_string())]);
    let r = run(
        &root,
        &[(
            golden_spec(&root, "field-notes"),
            vec![alpha_item("alpha body, revised")],
        )],
        "2026-11-04T11:00:00Z",
    );
    assert_eq!(
        reasons(&r),
        [("notes/alpha".to_string(), "no_written_hash".to_string())]
    );
    assert_eq!(read(&root, "notes/alpha.md"), edited);
}

/// Why: criterion 142 under owner ruling D64 — a hashless event that lives
/// only in frontmatter is history and counts in `history`, on both the
/// verification axis and `revision_state`. An event in frontmatter and in
/// the record counts once; a frontmatter event carrying a `hash` key is
/// not hashless and stays uncounted.
/// What: three hand-written concepts with no record lines but one, read
/// before and after a `verify` with the current hash.
/// Test: self-contained.
#[test]
fn c142_frontmatter_only_hashless_events_count_as_history() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("kb");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let fm = "verified:\n  - { by: human:old, at: 2026-01-01T00:00:00Z }\n  - { by: human:jo, at: 2026-02-01T00:00:00Z }\n";
    let both = "verified:\n  - { by: human:jo, at: 2026-02-01T00:00:00Z }\n";
    let hashed = "verified:\n  - { by: human:jo, at: 2026-02-01T00:00:00Z, hash: abc }\n";
    for (name, verified) in [("fm", fm), ("both", both), ("hashed", hashed)] {
        let text = format!("---\ntype: Note\n{verified}---\nBody.\n");
        std::fs::write(root.join(format!("{name}.md")), text).unwrap();
    }
    let rec = root.join("_sources/verification/both.jsonl");
    std::fs::create_dir_all(rec.parent().unwrap()).unwrap();
    std::fs::write(
        rec,
        "{\"by\":\"human:jo\",\"at\":\"2026-02-01T00:00:00Z\"}\n",
    )
    .unwrap();
    let state = IndexStateDir::new(t.path().join("machine"));
    let read = |name: &str| {
        let b = Bundle::open(&root).unwrap().0;
        let axis = b.verification_state(&id(name)).unwrap();
        let rs = b.revision_state(&id(name), &state, INDEX, &live()).unwrap();
        assert_eq!(rs.verification, axis, "{name}: both readers agree");
        axis
    };
    let un = |history| VerificationState::Unverified { history };
    assert_eq!(read("fm"), un(2), "frontmatter-only hashless events count");
    assert_eq!(read("both"), un(1), "an event in both places counts once");
    assert_eq!(
        read("hashed"),
        un(0),
        "a hashed frontmatter event is not hashless"
    );
    let mut b = Bundle::open(&root).unwrap().0;
    let h = content_hash(&b.get(&id("fm")).unwrap());
    let v = Verification {
        by: Actor::Human("jo".into()),
        at: instant("2026-11-04T12:00:00Z"),
        stale_after: None,
    };
    b.verify_confirmed(&id("fm"), v, &h, HumanConfirmation::explicit())
        .unwrap();
    assert!(
        matches!(
            read("fm"),
            VerificationState::Verified {
                human: true,
                events: 1,
                history: 2
            }
        ),
        "{:?}",
        read("fm")
    );
}
