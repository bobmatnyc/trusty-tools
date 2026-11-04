//! Fault injection for slice S6 of #10746: `supersede`, `migrate` and the
//! `rebuild` swap through the operation journal (SPEC-OKF-01 §4.11, §2.9,
//! §4.5; criteria 24, 106-108, 112, 120, 127), and the
//! migrate-restore-diff test on the trusty-kb fixture tree.
//!
//! The seams are `ops::failpoint` (a crash), and `ops::mutate_seam` (a
//! changed post-image), both compiled only under `cfg(test)`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde_yaml::Mapping;

use crate::OkfError;
use crate::build::ingest::{SourceItem, concept_id_for, ingest_items};
use crate::build::rebuild::rebuild;
use crate::build::registry::{ConnectorParams, Locator, SourceSpec};
use crate::bundle::Bundle;
use crate::bundle::ops::fail;
use crate::format::concept::Concept;
use crate::format::fields::{Actor, Verification};
use crate::format::hash::{bytes_hash, content_hash};
use crate::format::id::ConceptId;
use crate::format::validate::{Finding, FindingKind};
use crate::format::write::WriteMode;
use crate::profile::Profile;

fn at(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

fn actor() -> Actor {
    Actor::Process("test".into())
}

/// Every file outside `_sources/ops/`, `_sources/migrations/` and the lock
/// (the criterion 106 and 108 comparison scope).
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            if rel.starts_with("_sources/ops")
                || rel.starts_with("_sources/migrations")
                || rel == "_sources/okf.lock"
            {
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
            .filter_map(|l| serde_json::from_str::<crate::bundle::record::Line>(l).ok())
            .collect();
        for l in crate::bundle::record::fold(lines) {
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
    Concept::new(id(name), m, body.to_string())
}

fn verify(b: &mut Bundle, name: &str) {
    let h = content_hash(&b.get(&id(name)).unwrap());
    let v = Verification {
        by: actor(),
        at: at("2026-11-04T10:05:00Z"),
        stale_after: None,
    };
    b.verify(&id(name), v, &h).unwrap();
}

/// `a` (verified) and `b` (links to `a`).
fn seed_corrections(root: &Path) {
    let mut b = Bundle::open(root).unwrap().0;
    let now = at("2026-11-04T10:00:00Z");
    for (n, body) in [("a", "Alpha\n"), ("b", "See [a](/a.md).\n")] {
        b.put(new_concept(n, body), actor(), WriteMode::Lenient, now)
            .unwrap();
        verify(&mut b, n);
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dest = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_dir(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).unwrap();
        }
    }
}

/// The golden trusty-kb tree plus a hand-owned v0.1 concept with a
/// verification record, and a `log.md`.
fn seed_legacy(root: &Path) {
    // Cargo runs unit tests from the package root.
    let src = Path::new("tests/fixtures/trusty-kb-c1eed98441/kb");
    copy_dir(&src, root);
    std::fs::write(
        root.join("notes/hand.md"),
        "---\ntype: Note\nupdated: 2026-07-20T10:00:00Z\nsources:\n- https://example.com/a\n- https://example.com/b\nverified:\n  by: human:jo\n  at: 2026-07-21T10:00:00Z\n---\n\nA hand-written note.\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("_sources/verification/notes")).unwrap();
    std::fs::write(
        root.join("_sources/verification/notes/hand.jsonl"),
        "{\"by\":\"human:jo\",\"at\":\"2026-07-21T10:00:00Z\"}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("log.md"),
        "# Log\n\n## 2026-07-26\n\n- **Converted**\n",
    )
    .unwrap();
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

fn item(name: &str, body: &str) -> SourceItem {
    SourceItem {
        item_id: name.into(),
        fingerprint: format!("fp-{name}-{body}"),
        name: name.into(),
        title: name.into(),
        body: body.into(),
        ..SourceItem::default()
    }
}

/// Two ingested items, `a` verified.
fn seed_rebuild(root: &Path) {
    let p = Profile::default_profile();
    ingest_items(
        root,
        &p,
        &spec(),
        &[item("a", "A1"), item("b", "B1")],
        false,
        at("2026-11-04T10:00:00Z"),
    )
    .unwrap();
    let mut b = Bundle::open(root).unwrap().0;
    let a = concept_id_for("mail", "a").unwrap();
    verify(&mut b, a.as_str());
}

type Seed = fn(&Path);
type Op = fn(&mut Bundle) -> Result<(), OkfError>;

fn op_supersede(b: &mut Bundle) -> Result<(), OkfError> {
    b.supersede(
        &id("a"),
        new_concept("a2", "Alpha, corrected\n"),
        "fix",
        actor(),
        at("2026-11-04T11:00:00Z"),
    )
    .map(|_| ())
}

fn op_migrate(b: &mut Bundle) -> Result<(), OkfError> {
    let plan = b.plan_migration()?;
    assert!(plan.changes.len() >= 3, "{plan:?}");
    b.migrate(&plan, at("2026-11-04T11:00:00Z")).map(|_| ())
}

fn op_rebuild(b: &mut Bundle) -> Result<(), OkfError> {
    let sources = [(spec(), vec![item("a", "A2"), item("b", "B2")])];
    let r = rebuild(
        b.root(),
        &Profile::default_profile(),
        &sources,
        at("2026-11-04T11:00:00Z"),
    )?;
    assert_eq!(r.rebuilt.len(), 2, "{r:?}");
    Ok(())
}

fn fresh(seed: Seed) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("kb");
    std::fs::create_dir_all(&root).unwrap();
    seed(&root);
    (tmp, root)
}

/// The boundary names one uninterrupted run of `op` passes, in order.
fn boundaries(seed: Seed, op: Op) -> Vec<String> {
    let (_t, root) = fresh(seed);
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

fn recover_all(root: &Path) -> Result<Vec<Finding>, OkfError> {
    let (mut b, mut found) = Bundle::open(root)?;
    found.extend(b.recover_interrupted()?);
    Ok(found)
}

fn crash_at(root: &Path, op: Op, name: &str) {
    let target = name.to_string();
    fail::set(move |n| n == target);
    let got = op(&mut Bundle::open(root).unwrap().0);
    fail::clear();
    assert!(got.is_err(), "crash at {name} returned Ok");
}

fn no_staging(root: &Path) -> bool {
    !root
        .join("_sources/ops")
        .read_dir()
        .unwrap()
        .any(|e| e.unwrap().path().is_dir())
}

/// Why: criteria 106 and 107 — a crash at every step boundary of
/// `supersede` recovers to the uninterrupted tree (before the intent: to the
/// pre-operation tree), and of `migrate` to the pre-migration tree, with the
/// same verification events and no link to a missing concept. The
/// comparison covers every file outside `_sources/ops/` and
/// `_sources/migrations/`.
/// What: records the boundaries of one clean run, then crashes at each.
/// Test: self-contained.
#[test]
fn c106_supersede_and_migrate_crash_at_every_boundary() {
    for (label, seed, op, rolls_back) in [
        (
            "supersede",
            seed_corrections as Seed,
            op_supersede as Op,
            false,
        ),
        ("migrate", seed_legacy, op_migrate, true),
    ] {
        let (_t0, before_root) = fresh(seed);
        let before = tree(&before_root);
        let (_t1, clean_root) = fresh(seed);
        op(&mut Bundle::open(&clean_root).unwrap().0).unwrap();
        let clean = tree(&clean_root);
        assert_ne!(clean, before, "{label}: the op changes the tree");
        let names = boundaries(seed, op);
        assert!(names.len() >= 5, "{label}: {names:?}");
        for name in &names {
            let (_t, root) = fresh(seed);
            crash_at(&root, op, name);
            let findings = recover_all(&root).unwrap();
            assert!(
                findings
                    .iter()
                    .all(|f| f.kind != FindingKind::RecoveryConflict),
                "{label}/{name}: {findings:?}"
            );
            let (expect, expect_events) = if rolls_back || name == "staged" {
                (&before, events(&before_root))
            } else {
                (&clean, events(&clean_root))
            };
            assert_eq!(&tree(&root), expect, "{label}: crash at {name}");
            assert_eq!(events(&root), expect_events, "{label}/{name}: events");
            assert!(links_resolve(&root), "{label}/{name}: dangling link");
            assert!(no_staging(&root), "{label}/{name}: staging left behind");
        }
    }
}

/// Why: criterion 108 — a crash during recovery itself is finished by the
/// next recovery with the same end state; a torn intent line leaves the
/// pre-operation tree and the next open removes the staging directory.
/// What: crash each op mid-way, crash its recovery at each step, recover
/// again; then a torn intent after staging.
/// Test: self-contained.
#[test]
fn c108_crash_during_recovery_for_supersede_and_migrate() {
    for (label, seed, op, rolls_back) in [
        (
            "supersede",
            seed_corrections as Seed,
            op_supersede as Op,
            false,
        ),
        ("migrate", seed_legacy, op_migrate, true),
    ] {
        let (_t0, before_root) = fresh(seed);
        let before = tree(&before_root);
        let (_t1, clean_root) = fresh(seed);
        op(&mut Bundle::open(&clean_root).unwrap().0).unwrap();
        let expect = if rolls_back {
            before.clone()
        } else {
            tree(&clean_root)
        };
        for j in 0..5 {
            let (_t, root) = fresh(seed);
            crash_at(&root, op, "step-2");
            let target = format!("recover-step-{j}");
            fail::set(move |n| n == target);
            let first = recover_all(&root);
            fail::clear();
            assert!(first.is_err(), "{label}: recovery crash {j}");
            recover_all(&root).unwrap();
            assert_eq!(tree(&root), expect, "{label}: recovery crash at step {j}");
        }
        let (_t, root) = fresh(seed);
        crash_at(&root, op, "staged");
        let journal = root.join("_sources/ops/journal.jsonl");
        let mut text = std::fs::read_to_string(&journal).unwrap_or_default();
        text.push_str("{\"rec\":\"intent\",\"op\":\"x-0");
        std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
        std::fs::write(&journal, text).unwrap();
        recover_all(&root).unwrap();
        assert_eq!(tree(&root), before, "{label}: torn intent");
        assert!(no_staging(&root), "{label}: staging removed");
    }
}

/// The manifest lines of the one migration under `root`.
fn manifest(root: &Path) -> Vec<serde_json::Value> {
    let dir = root.join("_sources/migrations");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .unwrap();
    std::fs::read_to_string(file)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// Each manifest path holds bytes hashing to the manifest's old hash.
fn restored_to_manifest(root: &Path) {
    for m in manifest(root) {
        let path = m["path"].as_str().unwrap();
        let old = m["old"].as_str().unwrap();
        let disk = std::fs::read(root.join(path)).ok().map(|b| bytes_hash(&b));
        assert_eq!(disk.unwrap_or_default(), old, "{path}");
    }
}

/// Why: criterion 127 — a `migrate` that crashes after two file steps
/// rolls back on recovery, restoring both files from the manifest's
/// backups without a pre-image check, so the bundle matches its
/// pre-migration bytes.
/// What: crash at `step-2` (the revision journal, then two concepts).
/// Test: self-contained.
#[test]
fn c127_migrate_crash_after_two_file_steps_rolls_back() {
    let (_t0, before_root) = fresh(seed_legacy);
    let before = tree(&before_root);
    let (_t, root) = fresh(seed_legacy);
    crash_at(&root, op_migrate, "step-2");
    assert_ne!(tree(&root), before, "two files were written");
    recover_all(&root).unwrap();
    assert_eq!(tree(&root), before);
    restored_to_manifest(&root);
}

/// Why: criterion 24 — when a test hook drops one `sources` entry during
/// apply, the audit fails, every changed file is restored, and the result is
/// `MigrationLoss` naming the source.
/// What: a post-image hook removes one entry of `notes/hand.md`.
/// Test: self-contained.
#[test]
fn c24_audit_restores_every_file_when_a_sources_entry_is_dropped() {
    let (_t, root) = fresh(seed_legacy);
    let before = tree(&root);
    fail::set_mutate(|rel, bytes| {
        if rel == "notes/hand.md" {
            let text = String::from_utf8(bytes.clone()).unwrap();
            let kept: Vec<&str> = text
                .split_inclusive('\n')
                .filter(|l| !l.contains("example.com/b"))
                .collect();
            *bytes = kept.concat().into_bytes();
        }
    });
    let got = op_migrate(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    let Err(OkfError::MigrationLoss(items)) = got else {
        panic!("{got:?}");
    };
    assert!(
        items.iter().any(|i| i.path == "notes/hand.md"
            && i.kind == "source"
            && i.detail == "https://example.com/b"),
        "{items:?}"
    );
    assert_eq!(tree(&root), before);
    restored_to_manifest(&root);
    assert!(
        Bundle::open(&root)
            .unwrap()
            .1
            .iter()
            .all(|f| f.kind != FindingKind::InterruptedOperation)
    );
}

/// Why: criterion 120 — a hook changes one body byte during apply: the
/// audit fails, every changed file is restored with its SHA-256 equal to
/// the manifest's old hash, and `MigrationLoss` names the body item. With
/// one backup blob corrupted, the result also lists `restore_failed` for
/// that file, and the blob stays.
/// What: the body hook alone, then with a corrupted blob.
/// Test: self-contained.
#[test]
fn c120_body_byte_change_restores_and_a_corrupt_blob_is_restore_failed() {
    let body_hook = |rel: &str, bytes: &mut Vec<u8>| {
        if rel == "notes/hand.md" {
            let text = String::from_utf8(bytes.clone()).unwrap();
            *bytes = text.replace("written note.", "written notE.").into_bytes();
        }
    };
    let (_t, root) = fresh(seed_legacy);
    let before = tree(&root);
    fail::set_mutate(body_hook);
    let got = op_migrate(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    let Err(OkfError::MigrationLoss(items)) = got else {
        panic!("{got:?}");
    };
    assert!(
        items
            .iter()
            .any(|i| i.path == "notes/hand.md" && i.kind == "body"),
        "{items:?}"
    );
    assert!(
        items.iter().all(|i| i.kind != "restore_failed"),
        "{items:?}"
    );
    assert_eq!(tree(&root), before);
    restored_to_manifest(&root);

    let (_t, root) = fresh(seed_legacy);
    fail::set_mutate(body_hook);
    let r2 = root.clone();
    let blob = std::rc::Rc::new(std::cell::RefCell::new(PathBuf::new()));
    let b2 = blob.clone();
    fail::set(move |n| {
        if n == "staged" {
            let m = manifest(&r2);
            let alpha = m.iter().find(|l| l["path"] == "notes/alpha.md").unwrap();
            let p = r2.join(alpha["blob"].as_str().unwrap());
            std::fs::write(&p, b"corrupt").unwrap();
            *b2.borrow_mut() = p;
        }
        false
    });
    let got = op_migrate(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    let Err(OkfError::MigrationLoss(items)) = got else {
        panic!("{got:?}");
    };
    assert!(items.iter().any(|i| i.kind == "body"), "{items:?}");
    let failed: Vec<&str> = items
        .iter()
        .filter(|i| i.kind == "restore_failed")
        .map(|i| i.path.as_str())
        .collect();
    assert_eq!(failed, ["notes/alpha.md"]);
    assert_eq!(
        std::fs::read(&*blob.borrow()).unwrap(),
        b"corrupt",
        "the blob stays"
    );
    let findings = Bundle::open(&root).unwrap().1;
    assert!(
        findings
            .iter()
            .any(|f| f.kind == FindingKind::RecoveryConflict
                || f.kind == FindingKind::InterruptedOperation),
        "{findings:?}"
    );
}

/// Why: §2.9 "What must be true" and the S6 slice test — a migrated
/// trusty-kb tree restores byte for byte: until the completion record, the
/// backups bring every changed file back to its exact old bytes.
/// What: migrate the trusty-kb fixture tree, stopping just before the
/// completion record, recover, and diff against the original tree; and the
/// same after a failed audit.
/// Test: self-contained.
#[test]
fn migrated_trusty_kb_tree_restores_byte_for_byte() {
    let (_t0, before_root) = fresh(seed_legacy);
    let before = tree(&before_root);
    let (_t, root) = fresh(seed_legacy);
    crash_at(&root, op_migrate, "before-complete");
    assert_ne!(tree(&root), before, "every file was migrated");
    recover_all(&root).unwrap();
    assert_eq!(tree(&root), before);

    let (_t, root) = fresh(seed_legacy);
    fail::set_mutate(|rel, bytes| {
        if rel == "mail/2026-02-01-re-update.md" {
            bytes.extend_from_slice(b"x");
        }
    });
    let got = op_migrate(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(matches!(got, Err(OkfError::MigrationLoss(_))), "{got:?}");
    assert_eq!(tree(&root), before);
}

/// The bytes each target file of the rebuild holds before and after.
fn rebuild_targets() -> Vec<String> {
    ["a", "b"]
        .iter()
        .map(|n| concept_id_for("mail", n).unwrap().file_name())
        .collect()
}

/// Why: criterion 112 — a crash before the rebuild's intent leaves the tree
/// byte-identical; after it, the next open rolls forward, and at every crash
/// point no concept file holds bytes other than its pre- or post-image. A
/// hand edit to a concept, or a line added to `log.md`, between generation
/// and swap stays byte-identical and is a `recovery_conflict` naming it.
/// What: crash at every boundary; then edit at the intent boundary.
/// Test: self-contained.
#[test]
fn c112_rebuild_crash_and_mid_swap_edits() {
    let (_t0, before_root) = fresh(seed_rebuild);
    let before = tree(&before_root);
    let (_t1, clean_root) = fresh(seed_rebuild);
    op_rebuild(&mut Bundle::open(&clean_root).unwrap().0).unwrap();
    let clean = tree(&clean_root);
    for name in boundaries(seed_rebuild, op_rebuild) {
        let (_t, root) = fresh(seed_rebuild);
        crash_at(&root, op_rebuild, &name);
        let mid = tree(&root);
        for rel in rebuild_targets() {
            assert!(
                mid[&rel] == before[&rel] || mid[&rel] == clean[&rel],
                "{name}: {rel} mixed"
            );
        }
        recover_all(&root).unwrap();
        let expect = if name == "staged" { &before } else { &clean };
        assert_eq!(&tree(&root), expect, "crash at {name}");
        assert!(no_staging(&root), "{name}");
    }
    for target in [rebuild_targets()[1].clone(), "log.md".to_string()] {
        let (_t, root) = fresh(seed_rebuild);
        let (r2, t2) = (root.clone(), target.clone());
        let edited = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let e2 = edited.clone();
        fail::set(move |n| {
            if n == "intent" {
                let mut bytes = std::fs::read(r2.join(&t2)).unwrap();
                bytes.extend_from_slice(b"- added by hand\n");
                std::fs::write(r2.join(&t2), &bytes).unwrap();
                *e2.borrow_mut() = bytes;
            }
            false
        });
        let got = op_rebuild(&mut Bundle::open(&root).unwrap().0);
        fail::clear();
        assert!(
            matches!(got, Err(OkfError::RecoveryPending { .. })),
            "{target}: {got:?}"
        );
        assert_eq!(
            std::fs::read(root.join(&target)).unwrap(),
            *edited.borrow(),
            "{target}"
        );
        let findings = Bundle::open(&root).unwrap().1;
        assert!(
            findings
                .iter()
                .any(|f| f.kind == FindingKind::RecoveryConflict && f.message.contains(&target)),
            "{target}: {findings:?}"
        );
        assert_eq!(
            std::fs::read(root.join(&target)).unwrap(),
            *edited.borrow(),
            "{target}: still"
        );
    }
}

/// Why: Fail-Open Check (#10746 S6) — a migration whose backups cannot be
/// removed after its completion record fails the call; it never reports
/// success with a warning. The tree is migrated and a re-run plans nothing.
/// What: an I/O hook fails the backup removal.
/// Test: self-contained.
#[test]
fn migrate_backup_cleanup_failure_is_an_error() {
    let (_t, root) = fresh(seed_legacy);
    fail::set_io(|n| n.starts_with("remove-backups:"));
    let got = op_migrate(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(matches!(got, Err(OkfError::Io { .. })), "{got:?}");
    let b = Bundle::open(&root).unwrap().0;
    assert!(b.plan_migration().unwrap().changes.is_empty(), "migrated");
}

/// Why: Fail-Open Check (#10746 S6) — a post-image that does not parse
/// refuses the migration before any write, instead of recording a default
/// content hash in the revision journal.
/// What: a post-image hook breaks the frontmatter of one file.
/// Test: self-contained.
#[test]
fn unparseable_post_image_refuses_before_any_write() {
    let (_t, root) = fresh(seed_legacy);
    let before = tree(&root);
    fail::set_mutate(|rel, bytes| {
        if rel == "notes/hand.md" {
            *bytes = b"---\n: [unclosed\n---\nbody\n".to_vec();
        }
    });
    let got = op_migrate(&mut Bundle::open(&root).unwrap().0);
    fail::clear();
    assert!(matches!(got, Err(OkfError::Parse { .. })), "{got:?}");
    assert_eq!(tree(&root), before);
    assert!(
        !root.join("_sources/migrations").exists(),
        "no backup written"
    );
}
