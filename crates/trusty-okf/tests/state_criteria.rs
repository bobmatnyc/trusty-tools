//! SPEC-OKF-01 §6.2 criteria 60-65, 68, 70, 118 and 119 for slice S5 of
//! #10746: the three-state read model (§4.9), machine-local index state
//! (§4.9.1) and the index identity check (§4.9.2).
//!
//! Why: saved, indexed and verified are independent axes; an acknowledgement
//! is not retrieval proof; index state belongs to one machine's daemon and
//! one index identity, and fails closed without D1.
//! What: each test drives the bundle through its public API and plays the
//! feed (slice S7) by appending `IndexRecord`s to the caller's
//! `IndexJournal`, as the feed will.
//! Test: `cargo test -p trusty-okf --test state_criteria`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use trusty_okf::OkfError;
use trusty_okf::build::ingest::{SourceItem, concept_id_for, ingest_items};
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceSpec};
use trusty_okf::bundle::{Bundle, HumanConfirmation, PutOutcome, VerificationState};
use trusty_okf::format::{
    Actor, Concept, ConceptId, FindingKind, Severity, TrustTier, Verification, WriteMode,
    content_hash, parse_concept, trust_tier,
};
use trusty_okf::profile::Profile;
use trusty_okf::state::{
    IndexIdentity, IndexJournal, IndexRecord, IndexState, IndexStateDir, LiveIdentity, SaveState,
};

const INDEX: &str = "kb-test";

fn instant(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn now() -> DateTime<FixedOffset> {
    instant("2026-11-04T12:00:00Z")
}

fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

fn ident(instance: &str, generation: u64) -> IndexIdentity {
    IndexIdentity {
        instance_id: instance.into(),
        generation,
    }
}

fn known(instance: &str, generation: u64) -> LiveIdentity {
    LiveIdentity::Known(ident(instance, generation))
}

fn process() -> Actor {
    Actor::Process("test".into())
}

/// A bundle root and a machine-local state directory outside it.
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    state: IndexStateDir,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("bundle");
    std::fs::create_dir_all(&root).unwrap();
    Fixture {
        state: IndexStateDir::new(base.join("machine-a")),
        root,
        _tmp: tmp,
    }
}

/// Create concept `name` through `put`.
fn create(b: &mut Bundle, name: &str, body: &str) {
    let mut m = serde_yaml::Mapping::new();
    m.insert("type".into(), "Note".into());
    m.insert("description".into(), name.into());
    let c = Concept::new(id(name), m, format!("{body}\n"));
    let (out, _) = b.put(c, process(), WriteMode::Lenient, now()).unwrap();
    assert_eq!(out, PutOutcome::Created);
}

fn disk_bytes(root: &Path, name: &str) -> String {
    let bytes = std::fs::read(root.join(format!("{name}.md"))).unwrap();
    trusty_okf::format::hash::bytes_hash(&bytes)
}

/// Play a feed pass that acknowledged and probed `name`'s current bytes.
fn feed_searchable(f: &Fixture, names: &[&str], identity: Option<IndexIdentity>) {
    let j = IndexJournal::open(&f.state, &f.root, INDEX).unwrap();
    let mut records = Vec::new();
    for n in names {
        let bytes = disk_bytes(&f.root, n);
        records.push(IndexRecord::PutAck {
            path: format!("{n}.md"),
            bytes: bytes.clone(),
            identity: identity.clone(),
        });
        records.push(IndexRecord::Probe {
            path: format!("{n}.md"),
            bytes,
            identity: identity.clone(),
            passed: true,
        });
    }
    j.append(&records).unwrap();
}

fn index_of(b: &Bundle, f: &Fixture, name: &str, live: &LiveIdentity) -> IndexState {
    b.revision_state(&id(name), &f.state, INDEX, live)
        .unwrap()
        .index
}

fn pending(s: &IndexState) -> bool {
    matches!(s, IndexState::Pending { .. })
}

/// Why: criterion 60 — a saved concept whose push failed reads Saved and
/// Pending, and nothing reads synced. What: `put`, a failed push record.
/// Test: self-contained.
#[test]
fn c60_saved_and_pending_after_a_failed_push() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    IndexJournal::open(&f.state, &f.root, INDEX)
        .unwrap()
        .append(&[IndexRecord::PutFailed {
            path: "a.md".into(),
            bytes: disk_bytes(&f.root, "a"),
            error: "daemon_down".into(),
        }])
        .unwrap();
    let live = known("i1", 1);
    let r = b.revision_state(&id("a"), &f.state, INDEX, &live).unwrap();
    assert_eq!(r.save, SaveState::Saved);
    assert_eq!(
        r.index,
        IndexState::Pending {
            last_error: Some("daemon_down".into())
        }
    );
    let s = b.state_summary(&f.state, INDEX, &live).unwrap();
    assert_eq!((s.saved, s.pending, s.searchable), (1, 1, 0));
    assert!(!s.synced());
}

/// Why: criteria 61 and 62 — a content change moves the old events to
/// history, and only a `verify` with the current hash verifies again.
/// What: verify, change the body, read history, verify with the old then
/// the new hash.
/// Test: self-contained.
#[test]
fn c61_c62_content_change_unverifies_until_matching_evidence() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    let old = content_hash(&b.get(&id("a")).unwrap());
    let v = |at: &str| Verification {
        by: process(),
        at: instant(at),
        stale_after: None,
    };
    b.verify(&id("a"), v("2026-11-04T12:01:00Z"), &old).unwrap();
    let mut c = b.get(&id("a")).unwrap();
    c.body = "Alpha, corrected\n".into();
    b.put(
        c,
        process(),
        WriteMode::Lenient,
        instant("2026-11-04T12:02:00Z"),
    )
    .unwrap();
    let live = known("i1", 1);
    let r = b.revision_state(&id("a"), &f.state, INDEX, &live).unwrap();
    assert!(
        matches!(r.verification, VerificationState::Unverified { history } if history >= 1),
        "{r:?}"
    );
    let events = b.verification_events(&id("a")).unwrap();
    assert!(
        events.iter().all(
            |e| e.invalidated_at.is_some() && e.invalidated_by_hash == Some(r.revision.clone())
        )
    );

    let got = b.verify(&id("a"), v("2026-11-04T12:03:00Z"), &old);
    assert!(
        matches!(&got, Err(OkfError::Conflict { reason, .. }) if reason == "stale_verify_hash"),
        "{got:?}"
    );
    b.verify(&id("a"), v("2026-11-04T12:04:00Z"), &r.revision)
        .unwrap();
    let r = b.revision_state(&id("a"), &f.state, INDEX, &live).unwrap();
    assert!(
        matches!(r.verification, VerificationState::Verified { .. }),
        "{r:?}"
    );
}

/// Why: criterion 63 — an acknowledgement alone is not retrieval proof (F12).
/// What: a put acknowledgement with no probe, then a failed probe.
/// Test: self-contained.
#[test]
fn c63_acknowledgement_alone_is_not_searchable() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    let j = IndexJournal::open(&f.state, &f.root, INDEX).unwrap();
    let bytes = disk_bytes(&f.root, "a");
    let i1 = Some(ident("i1", 1));
    j.append(&[IndexRecord::PutAck {
        path: "a.md".into(),
        bytes: bytes.clone(),
        identity: i1.clone(),
    }])
    .unwrap();
    let live = known("i1", 1);
    assert_eq!(index_of(&b, &f, "a", &live), IndexState::Acknowledged);
    j.append(&[IndexRecord::Probe {
        path: "a.md".into(),
        bytes,
        identity: i1,
        passed: false,
    }])
    .unwrap();
    assert_eq!(index_of(&b, &f, "a", &live), IndexState::Acknowledged);
    let s = b.state_summary(&f.state, INDEX, &live).unwrap();
    assert_eq!((s.acknowledged, s.searchable), (1, 0));
    assert!(!s.synced());
}

/// Why: criterion 64 — the OKF trust tier and the verification state are
/// separate. What: a human-verified concept gets an out-of-band body edit.
/// Test: self-contained.
#[test]
fn c64_out_of_band_edit_reads_saved_external_and_unverified() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    let h = content_hash(&b.get(&id("a")).unwrap());
    let v = Verification {
        by: Actor::Human("jo".into()),
        at: instant("2026-11-04T12:01:00Z"),
        stale_after: None,
    };
    b.verify_confirmed(&id("a"), v, &h, HumanConfirmation::explicit())
        .unwrap();
    let path = f.root.join("a.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("Alpha", "Alpha edited by hand")).unwrap();

    let c = b.get(&id("a")).unwrap();
    assert_eq!(trust_tier(c.frontmatter()), TrustTier::HumanReviewed);
    let r = b
        .revision_state(&id("a"), &f.state, INDEX, &known("i1", 1))
        .unwrap();
    assert_eq!(r.save, SaveState::SavedExternal);
    assert!(
        matches!(r.verification, VerificationState::Unverified { .. }),
        "{r:?}"
    );
    assert_eq!(r.okf_tier, TrustTier::HumanReviewed);
    assert!(r.findings.iter().any(|x| x.kind == FindingKind::VerificationMismatch
        && x.severity == Severity::Warning));
}

/// Why: criterion 65 — a lifecycle-only `put` keeps the verification state
/// and moves the index state to Pending. What: a Searchable, verified
/// concept gets `status: draft`.
/// Test: self-contained.
#[test]
fn c65_lifecycle_only_put_keeps_verification_and_reads_pending() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    let h = content_hash(&b.get(&id("a")).unwrap());
    let v = Verification {
        by: process(),
        at: instant("2026-11-04T12:01:00Z"),
        stale_after: None,
    };
    b.verify(&id("a"), v, &h).unwrap();
    feed_searchable(&f, &["a"], Some(ident("i1", 1)));
    let live = known("i1", 1);
    assert_eq!(index_of(&b, &f, "a", &live), IndexState::Searchable);
    let before = b.revision_state(&id("a"), &f.state, INDEX, &live).unwrap();

    let mut c = b.get(&id("a")).unwrap();
    c.set("status", serde_yaml::Value::String("draft".into()));
    b.put(c, process(), WriteMode::Lenient, now()).unwrap();
    let after = b.revision_state(&id("a"), &f.state, INDEX, &live).unwrap();
    assert_eq!(after.verification, before.verification);
    assert!(matches!(
        after.verification,
        VerificationState::Verified { .. }
    ));
    assert!(pending(&after.index), "{after:?}");
    assert_eq!(after.save, SaveState::Saved);
}

/// Why: criterion 68 and §4.9.2 — a reindex with the same identity demotes
/// Searchable to Acknowledged until the next probe; a changed identity
/// (new instance or higher generation) reads Pending; an unchanged one stays.
/// What: Searchable, then a `Reindex` record, a probe, then new identities.
/// Test: self-contained.
#[test]
fn c68_reindex_demotes_and_identity_change_reads_pending() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    let i1 = Some(ident("i1", 1));
    feed_searchable(&f, &["a"], i1.clone());
    let live = known("i1", 1);
    assert_eq!(index_of(&b, &f, "a", &live), IndexState::Searchable);
    let j = IndexJournal::open(&f.state, &f.root, INDEX).unwrap();
    j.append(&[IndexRecord::Reindex {
        identity: i1.clone(),
    }])
    .unwrap();
    assert_eq!(index_of(&b, &f, "a", &live), IndexState::Acknowledged);
    j.append(&[IndexRecord::Probe {
        path: "a.md".into(),
        bytes: disk_bytes(&f.root, "a"),
        identity: i1,
        passed: true,
    }])
    .unwrap();
    assert_eq!(index_of(&b, &f, "a", &live), IndexState::Searchable);
    assert!(pending(&index_of(&b, &f, "a", &known("i2", 1))));
    assert!(pending(&index_of(&b, &f, "a", &known("i1", 2))));
    let s = b.state_summary(&f.state, INDEX, &known("i2", 1)).unwrap();
    assert_eq!((s.acknowledged, s.searchable), (0, 0));
    assert!(!s.synced());
}

/// Why: §4.9.2 rules 4-5 — no state reads Searchable without D1, and an
/// unreadable identity fails closed. What: records with no identity under
/// every live value, and records with an identity under `Unsupported` and
/// `Unreadable`; then the same records read as recorded again.
/// Test: self-contained.
#[test]
fn no_state_reads_searchable_without_d1() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    create(&mut b, "b", "Beta");
    feed_searchable(&f, &["a"], None);
    feed_searchable(&f, &["b"], Some(ident("i1", 1)));
    let unsupported = IndexState::Pending {
        last_error: Some("index_identity_unsupported".into()),
    };
    assert_eq!(
        index_of(&b, &f, "a", &LiveIdentity::Unsupported),
        unsupported
    );
    assert_eq!(
        index_of(&b, &f, "b", &LiveIdentity::Unsupported),
        unsupported
    );
    assert_eq!(
        index_of(&b, &f, "a", &known("i1", 1)),
        IndexState::Pending { last_error: None }
    );
    let down = LiveIdentity::Unreadable("daemon down".into());
    assert!(matches!(
        index_of(&b, &f, "b", &down),
        IndexState::Pending { last_error: Some(e) } if e.contains("daemon down")
    ));
    for live in [LiveIdentity::Unsupported, down] {
        let s = b.state_summary(&f.state, INDEX, &live).unwrap();
        assert_eq!((s.acknowledged, s.searchable, s.pending), (0, 0, 2));
    }
    // Control: the bound record reads as recorded under its identity.
    assert_eq!(
        index_of(&b, &f, "b", &known("i1", 1)),
        IndexState::Searchable
    );
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target);
        } else {
            std::fs::copy(e.path(), &target).unwrap();
        }
    }
}

fn listing(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if p.is_dir() {
                out.push((rel, Vec::new()));
                walk(root, &p, out);
            } else {
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// Why: criterion 118 — index state is machine-local: a bundle copied to
/// machine B, `_sources/` included, reads Pending there.
/// What: machine A feeds every concept to Searchable; the tree is copied;
/// machine B has an empty state directory. Control: A still reads
/// Searchable.
/// Test: self-contained.
#[test]
fn c118_copied_bundle_reads_pending_on_another_machine() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    create(&mut b, "n/b", "Beta");
    feed_searchable(&f, &["a", "n/b"], Some(ident("i1", 1)));
    let live = known("i1", 1);
    let a = b.state_summary(&f.state, INDEX, &live).unwrap();
    assert_eq!(a.searchable, 2);
    assert!(a.synced());

    let tmp_b = tempfile::tempdir().unwrap();
    let base_b = tmp_b.path().canonicalize().unwrap();
    let root_b = base_b.join("bundle");
    copy_tree(&f.root, &root_b);
    let state_b = IndexStateDir::new(base_b.join("machine-b"));
    let bundle_b = Bundle::open(&root_b).unwrap().0;
    for name in ["a", "n/b"] {
        let r = bundle_b
            .revision_state(&id(name), &state_b, INDEX, &live)
            .unwrap();
        assert!(pending(&r.index), "{name}: {r:?}");
        assert_eq!(r.save, SaveState::Saved, "the revision journal travels");
    }
    let s = bundle_b.state_summary(&state_b, INDEX, &live).unwrap();
    assert_eq!((s.acknowledged, s.searchable, s.pending), (0, 0, 2));
    assert!(!s.synced());
}

/// Why: criterion 119 — index state never lives in the bundle. What: a
/// journal append creates no file under the bundle root; a legacy
/// `_sources/<id>.index.jsonl` stays byte-identical, is reported as info and
/// is never read as Acknowledged; a state directory inside the root is
/// refused.
/// Test: self-contained.
#[test]
fn c119_index_state_never_lives_in_the_bundle() {
    let f = fixture();
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "a", "Alpha");
    let legacy = f.root.join("_sources/notes.index.jsonl");
    let legacy_text = format!(
        "{{\"kind\":\"put_ack\",\"path\":\"a.md\",\"bytes\":\"{}\",\"identity\":null}}\n",
        disk_bytes(&f.root, "a")
    );
    std::fs::write(&legacy, &legacy_text).unwrap();
    let before = listing(&f.root);
    feed_searchable(&f, &["a"], Some(ident("i1", 1)));
    assert_eq!(
        listing(&f.root),
        before,
        "the feed's journal wrote nothing in the bundle"
    );

    let (b2, findings) = Bundle::open(&f.root).unwrap();
    assert!(
        findings
            .iter()
            .any(|x| x.kind == FindingKind::LegacyIndexJournal
                && x.severity == Severity::Info
                && x.path == "_sources/notes.index.jsonl")
    );
    let empty = IndexStateDir::new(f.root.parent().unwrap().join("machine-c"));
    let r = b2
        .revision_state(&id("a"), &empty, INDEX, &LiveIdentity::Unsupported)
        .unwrap();
    assert!(pending(&r.index), "{r:?}");
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), legacy_text);

    for inside in [
        f.root.join("_state/index"),
        f.root.clone(),
        f.root.join("x/../_sources"),
    ] {
        let dir = IndexStateDir::new(inside);
        assert!(matches!(
            IndexJournal::open(&dir, &f.root, INDEX),
            Err(OkfError::StateDirInBundle(_))
        ));
        assert!(matches!(
            b2.revision_state(&id("a"), &dir, INDEX, &LiveIdentity::Unsupported),
            Err(OkfError::StateDirInBundle(_))
        ));
    }
    assert!(matches!(
        IndexJournal::open(&f.state, &f.root, "../escape"),
        Err(OkfError::PathEscape(_))
    ));
}

fn spec() -> SourceSpec {
    let mut s = SourceSpec::new(
        "mail",
        Some("mail"),
        Locator::Connector {
            kind: "gmail".into(),
            params: ConnectorParams::new(),
        },
        "2026-11-04T00:00:00Z",
    );
    s.tombstone_deleted = true;
    s
}

fn item(id: &str, body: &str) -> SourceItem {
    SourceItem {
        item_id: id.into(),
        fingerprint: format!("fp-{body}"),
        name: id.into(),
        title: id.into(),
        body: body.into(),
        ..SourceItem::default()
    }
}

/// The three axes of `name`, the index axis reduced to its variant name.
fn axes(f: &Fixture, name: &str, live: &LiveIdentity) -> (SaveState, String, VerificationState) {
    let b = Bundle::open(&f.root).unwrap().0;
    let r = b.revision_state(&id(name), &f.state, INDEX, live).unwrap();
    let index = format!("{:?}", r.index);
    let index = index.split([' ', '{']).next().unwrap().to_string();
    (r.save, index, r.verification)
}

/// Why: criterion 70 — each §4.9 operation-table row that exists in S5 has
/// a check of all three axes after the operation. Rows of S6 (`migrate`,
/// `supersede`, `deprecate`, `rebuild`, correct-in-place as its own call)
/// and S7 (the feed itself) are not callable yet; their state-model rows
/// are played through `put` and journal records here.
/// What: import new/changed/unchanged/tombstoned, a failed push, put
/// content/lifecycle/no change/refused, verify match/stale, an out-of-band
/// edit, move, feed confirm/probe, reindex, and an index re-created.
/// Test: self-contained.
#[test]
fn c70_every_operation_row_checks_three_axes() {
    let f = fixture();
    let live = known("i1", 1);
    let un0 = VerificationState::Unverified { history: 0 };
    let i1 = Some(ident("i1", 1));
    let ingest = |items: &[SourceItem], detect: bool, at: &str| {
        let r = ingest_items(
            &f.root,
            &Profile::default_profile(),
            &spec(),
            items,
            detect,
            instant(at),
        )
        .unwrap();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    };
    let name = concept_id_for("mail", "m").unwrap();
    let m = name.as_str();
    // import, new item.
    ingest(&[item("m", "one")], false, "2026-11-04T10:00:00Z");
    assert_eq!(
        axes(&f, m, &live),
        (SaveState::Saved, "Pending".into(), un0)
    );
    // import or build with a failed index push.
    let j = IndexJournal::open(&f.state, &f.root, INDEX).unwrap();
    j.append(&[IndexRecord::PutFailed {
        path: name.file_name(),
        bytes: disk_bytes(&f.root, m),
        error: "daemon_down".into(),
    }])
    .unwrap();
    assert_eq!(
        axes(&f, m, &live),
        (SaveState::Saved, "Pending".into(), un0)
    );
    // feed pass, put confirmed; then probe passed.
    j.append(&[IndexRecord::PutAck {
        path: name.file_name(),
        bytes: disk_bytes(&f.root, m),
        identity: i1.clone(),
    }])
    .unwrap();
    assert_eq!(axes(&f, m, &live).1, "Acknowledged");
    feed_searchable(&f, &[m], i1.clone());
    assert_eq!(
        axes(&f, m, &live),
        (SaveState::Saved, "Searchable".into(), un0)
    );
    // import, unchanged item.
    ingest(&[item("m", "one")], false, "2026-11-04T10:30:00Z");
    assert_eq!(
        axes(&f, m, &live),
        (SaveState::Saved, "Searchable".into(), un0)
    );
    // verify, seen hash matches.
    let mut b = Bundle::open(&f.root).unwrap().0;
    let h = content_hash(&b.get(&name).unwrap());
    let v = |at: &str| Verification {
        by: process(),
        at: instant(at),
        stale_after: None,
    };
    b.verify(&name, v("2026-11-04T10:40:00Z"), &h).unwrap();
    let verified = axes(&f, m, &live);
    assert!(matches!(verified.2, VerificationState::Verified { .. }));
    assert_eq!(
        (verified.0, verified.1.as_str()),
        (SaveState::Saved, "Pending")
    );
    // verify, stale hash.
    let stale = content_hash(
        &parse_concept(name.clone(), "---\ntype: X\n---\nother\n")
            .unwrap()
            .0,
    );
    assert!(b.verify(&name, v("2026-11-04T10:41:00Z"), &stale).is_err());
    assert_eq!(axes(&f, m, &live), verified);
    // put, no change.
    let c = b.get(&name).unwrap();
    assert_eq!(
        b.put(c, process(), WriteMode::Lenient, now()).unwrap().0,
        PutOutcome::Unchanged
    );
    assert_eq!(axes(&f, m, &live), verified);
    // put, refused.
    let mut c = b.get(&name).unwrap();
    c.remove("verified");
    assert!(b.put(c, process(), WriteMode::Lenient, now()).is_err());
    assert_eq!(axes(&f, m, &live), verified);
    // put, lifecycle-only change.
    feed_searchable(&f, &[m], i1.clone());
    let mut c = b.get(&name).unwrap();
    c.set(
        "stale_after",
        serde_yaml::Value::String("2027-01-01T00:00:00Z".into()),
    );
    b.put(c, process(), WriteMode::Lenient, now()).unwrap();
    assert_eq!(
        axes(&f, m, &live),
        (SaveState::Saved, "Pending".into(), verified.2)
    );
    // import, changed item: content change.
    ingest(&[item("m", "two")], false, "2026-11-04T11:00:00Z");
    let (s, i, v2) = axes(&f, m, &live);
    assert_eq!((s, i.as_str()), (SaveState::Saved, "Pending"));
    assert!(matches!(v2, VerificationState::Unverified { history } if history >= 1));
    // put, content change.
    let mut c = b.get(&name).unwrap();
    c.body = "three\n".into();
    b.put(
        c,
        process(),
        WriteMode::Lenient,
        instant("2026-11-04T11:10:00Z"),
    )
    .unwrap();
    assert_eq!(axes(&f, m, &live).1, "Pending");
    // explicit reindex, same identity; then the index re-created.
    feed_searchable(&f, &[m], i1.clone());
    j.append(&[IndexRecord::Reindex {
        identity: i1.clone(),
    }])
    .unwrap();
    assert_eq!(axes(&f, m, &live).1, "Acknowledged");
    feed_searchable(&f, &[m], i1.clone());
    assert_eq!(axes(&f, m, &known("i9", 1)).1, "Pending");
    // out-of-band edit.
    let path = f.root.join(name.file_name());
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replace("three", "three, by hand")).unwrap();
    let (s, i, _) = axes(&f, m, &live);
    assert_eq!((s, i.as_str()), (SaveState::SavedExternal, "Pending"));
    // move_concept: new path Saved and Pending; verification unchanged.
    let mut b = Bundle::open(&f.root).unwrap().0;
    create(&mut b, "x", "Ex");
    let hx = content_hash(&b.get(&id("x")).unwrap());
    b.verify(&id("x"), v("2026-11-04T11:20:00Z"), &hx).unwrap();
    let vx = axes(&f, "x", &live).2;
    b.move_concept(&id("x"), &id("y"), now()).unwrap();
    assert_eq!(
        axes(&f, "y", &live),
        (SaveState::Saved, "Pending".into(), vx)
    );
    // tombstone, then withdrawal confirmed.
    ingest(&[], true, "2026-11-04T12:30:00Z");
    assert_eq!(axes(&f, m, &live).1, "WithdrawPending");
    j.append(&[
        IndexRecord::RemoveAck {
            path: name.file_name(),
            identity: i1.clone(),
        },
        IndexRecord::WithdrawProbe {
            path: name.file_name(),
            identity: i1,
            passed: true,
        },
    ])
    .unwrap();
    assert_eq!(axes(&f, m, &live).1, "Withdrawn");
}
