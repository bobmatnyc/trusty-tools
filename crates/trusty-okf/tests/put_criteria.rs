//! SPEC-OKF-01 §6.2 criteria for slice S3 of #10746 that exercise
//! `Bundle::put` and `Bundle::verify`: verification invalidation (§2.10,
//! criteria 31-36), malformed owned values (125), `stale_after` extension
//! (146), and the Architect's S3 acceptance items A1 (no lost data), A2
//! (strict check on rendered values) and A3 (every bundle file 0600, #10746).

use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};
use trusty_okf::OkfError;
use trusty_okf::bundle::{Bundle, HumanConfirmation, PutOutcome, VerificationState};
use trusty_okf::format::v02::normalize_v02;
use trusty_okf::format::{
    Actor, Concept, ConceptId, Field, FindingKind, Severity, TrustTier, Verification, WriteMode,
    check_write, content_hash, parse_concept, trust_tier,
};

fn instant(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn now() -> DateTime<FixedOffset> {
    instant("2026-11-04T12:00:00Z")
}

fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

fn process() -> Actor {
    Actor::Process("ingest".into())
}

/// #10746 (§2.4): the explicit confirmation a `human:` write needs.
fn confirm() -> HumanConfirmation {
    HumanConfirmation::explicit()
}

fn setup(text: &str) -> (tempfile::TempDir, Bundle) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.md"), text).unwrap();
    let b = Bundle::open(tmp.path()).unwrap().0;
    (tmp, b)
}

fn read(root: &Path) -> String {
    std::fs::read_to_string(root.join("a.md")).unwrap()
}

fn verification(by: Actor, at: &str) -> Verification {
    Verification {
        by,
        at: instant(at),
        stale_after: None,
    }
}

fn conflict_reason<T: std::fmt::Debug>(got: Result<T, OkfError>) -> String {
    match got {
        Err(OkfError::Conflict { reason, .. }) => reason,
        other => panic!("expected Conflict, got {other:?}"),
    }
}

/// A concept with one legacy (hashless) frontmatter event, then verified by
/// `human:jo` through `verify`, so the record binds one event.
const VERIFIED: &str = "---\ntype: Note\ndescription: d\nverified:\n  - { by: human:old, at: 2026-01-01T00:00:00Z }\n---\nBody\n";

fn verified_bundle() -> (tempfile::TempDir, Bundle) {
    let (tmp, mut b) = setup(VERIFIED);
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(Actor::Human("jo".into()), "2026-11-01T09:00:00Z");
    b.verify_confirmed(&id("a"), v, &seen, confirm()).unwrap();
    assert_eq!(
        b.verification_state(&id("a")).unwrap(),
        VerificationState::Verified {
            human: true,
            events: 1,
            history: 0
        }
    );
    (tmp, b)
}

/// Shared checks of criteria 31 and 32 after a content-changing put.
fn assert_cleared(root: &Path, b: &Bundle, outcome: PutOutcome) {
    assert_eq!(
        outcome,
        PutOutcome::Updated {
            content_changed: true,
            verifications_cleared: 2
        }
    );
    let c = b.get(&id("a")).unwrap();
    assert!(c.get("verified").is_none(), "{}", read(root));
    assert_eq!(trust_tier(c.frontmatter()), TrustTier::Unverified);
    match &c.frontmatter().generated {
        Field::Valid(g) => {
            assert_eq!(g.by, process());
            assert_eq!(g.at, Some(now()));
        }
        other => panic!("generated: {other:?}"),
    }
    let new_hash = content_hash(&c);
    let events = b.verification_events(&id("a")).unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    for e in &events {
        assert_eq!(e.invalidated_at, Some(now()));
        assert_eq!(e.invalidated_by_hash.as_ref(), Some(&new_hash));
    }
    let jo = events
        .iter()
        .find(|e| e.v.by == Actor::Human("jo".into()))
        .unwrap();
    assert!(jo.hash.is_some() && jo.reason.is_none());
    let old = events
        .iter()
        .find(|e| e.v.by == Actor::Human("old".into()))
        .unwrap();
    assert!(old.hash.is_none());
    assert_eq!(old.reason.as_deref(), Some("legacy_unbound"));
    assert_eq!(
        b.verification_state(&id("a")).unwrap(),
        VerificationState::Unverified { history: 2 }
    );
}

/// Criterion 31: a body change moves every `verified[]` event to the record
/// with `invalidated_at` and `invalidated_by_hash`, removes `verified`, sets
/// `generated`, and `trust_tier` reads `Unverified`.
#[test]
fn c31_body_change_moves_verification_to_the_record() {
    let (tmp, mut b) = verified_bundle();
    let mut c = b.get(&id("a")).unwrap();
    c.body = "New body\n".into();
    let (outcome, _) = b.put(c, process(), WriteMode::default(), now()).unwrap();
    assert_cleared(tmp.path(), &b, outcome);
}

/// Criterion 32: a change to a non-lifecycle key (`description`) does the
/// same as criterion 31.
#[test]
fn c32_description_change_clears_verification() {
    let (tmp, mut b) = verified_bundle();
    let mut c = b.get(&id("a")).unwrap();
    c.set("description", Value::String("changed".into()));
    let (outcome, _) = b.put(c, process(), WriteMode::default(), now()).unwrap();
    assert_cleared(tmp.path(), &b, outcome);
}

/// Criterion 33: a put that changes only `status` or `stale_after` keeps
/// `verified[]` and leaves `generated` unchanged.
#[test]
fn c33_status_or_stale_after_change_keeps_verification() {
    let (tmp, mut b) = verified_bundle();
    let before = b.get(&id("a")).unwrap();
    for (key, value) in [("status", "draft"), ("stale_after", "2026-12-01T00:00:00Z")] {
        let mut c = b.get(&id("a")).unwrap();
        c.set(key, Value::String(value.into()));
        let (outcome, _) = b.put(c, process(), WriteMode::default(), now()).unwrap();
        assert_eq!(
            outcome,
            PutOutcome::Updated {
                content_changed: false,
                verifications_cleared: 0
            },
            "{key}"
        );
        let after = b.get(&id("a")).unwrap();
        assert_eq!(after.get("verified"), before.get("verified"), "{key}");
        assert_eq!(after.get("generated"), None, "{key}: {}", read(tmp.path()));
        assert!(matches!(
            b.verification_state(&id("a")).unwrap(),
            VerificationState::Verified { human: true, .. }
        ));
    }
}

/// Criterion 34: the §2.8 migration mapping changes no concept's content
/// hash. The `migrate` operation (S6) applies the same `normalize_v02`.
#[test]
fn c34_migration_keeps_the_content_hash() {
    let text = "---\ntype: Note\nupdated: 2026-05-01T10:00:00+02:00\nsources: [https://a.test/x, https://a.test/y]\nverified: { by: human:k, at: 2026-06-01T00:00:00Z }\ntrust: internal\n---\nBody\r\n";
    let (mut c, _) = parse_concept(id("a"), text).unwrap();
    let before = content_hash(&c);
    normalize_v02(&mut c);
    assert!(c.get("updated").is_none(), "the mapping ran");
    assert_eq!(content_hash(&c), before);
}

/// Criterion 35: `verify` with a stale content hash returns `Conflict` and
/// writes nothing.
#[test]
fn c35_verify_with_stale_hash_is_conflict_and_writes_nothing() {
    let (tmp, mut b) = setup(VERIFIED);
    let mut other = b.get(&id("a")).unwrap();
    other.body = "a revision the verifier never saw\n".into();
    let stale = content_hash(&other);
    let v = verification(Actor::Human("jo".into()), "2026-11-01T09:00:00Z");
    assert_eq!(
        conflict_reason(b.verify_confirmed(&id("a"), v, &stale, confirm())),
        "stale_verify_hash"
    );
    assert_eq!(read(tmp.path()), VERIFIED);
    let names: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names,
        vec![std::ffi::OsString::from("a.md")],
        "no record, no log"
    );
}

/// Criterion 36: after an out-of-band body edit to a verified concept, the
/// next read returns a warning, the verification state reads Unverified,
/// and `trust_tier` on the raw frontmatter is unchanged.
#[test]
fn c36_out_of_band_edit_reads_unverified_with_a_warning() {
    let (tmp, b) = verified_bundle();
    let tier_before = trust_tier(b.get(&id("a")).unwrap().frontmatter());
    let edited = read(tmp.path()).replace("Body\n", "Body, edited by hand\n");
    std::fs::write(tmp.path().join("a.md"), edited).unwrap();
    let (b, findings) = Bundle::open(tmp.path()).unwrap();
    assert!(
        findings.iter().any(|f| f.severity == Severity::Warning
            && f.kind == FindingKind::VerificationMismatch
            && f.concept == Some(id("a"))),
        "{findings:?}"
    );
    assert!(matches!(
        b.verification_state(&id("a")).unwrap(),
        VerificationState::Unverified { .. }
    ));
    let tier = trust_tier(b.get(&id("a")).unwrap().frontmatter());
    assert_eq!(tier, tier_before);
    assert_eq!(tier, TrustTier::HumanReviewed);
}

/// Criterion 125: a `put` that changes only `description` moves
/// `verified: "yes"` and `generated: bot` to `legacy_verified` and
/// `legacy_generated` with their raw bytes, writes a new `generated` and no
/// `verified`, and reports one `legacy_value_moved` warning per key. A
/// `verify` moves `verified: "yes"` the same way with the content hash
/// unchanged. An existing `legacy_verified` makes the write a `Conflict`.
#[test]
fn c125_malformed_owned_values_move_to_legacy_keys() {
    let text =
        "---\ntype: Note\nverified: \"yes\"  # kept\ngenerated: bot\ndescription: d\n---\nBody\n";
    let (tmp, mut b) = setup(text);
    let mut c = b.get(&id("a")).unwrap();
    c.set("description", Value::String("e".into()));
    let (_, findings) = b.put(c, process(), WriteMode::Lenient, now()).unwrap();
    let out = read(tmp.path());
    assert!(out.contains("legacy_verified: \"yes\"  # kept\n"), "{out}");
    assert!(out.contains("legacy_generated: bot\n"), "{out}");
    assert!(!out.lines().any(|l| l.starts_with("verified:")), "{out}");
    let generated = b.get(&id("a")).unwrap().frontmatter().generated.clone();
    assert!(
        matches!(&generated, Field::Valid(g) if g.by == process() && g.at == Some(now())),
        "{generated:?}"
    );
    let moved = findings
        .iter()
        .filter(|f| f.kind == FindingKind::LegacyValueMoved && f.severity == Severity::Warning)
        .count();
    assert_eq!(moved, 2, "{findings:?}");

    let (tmp, mut b) = setup("---\ntype: Note\nverified: \"yes\"\n---\nBody\n");
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(Actor::Human("jo".into()), "2026-11-02T00:00:00Z");
    let (_, findings) = b.verify_confirmed(&id("a"), v, &seen, confirm()).unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f.kind == FindingKind::LegacyValueMoved)
    );
    let after = b.get(&id("a")).unwrap();
    assert_eq!(
        after.get("legacy_verified"),
        Some(&Value::String("yes".into()))
    );
    assert_eq!(content_hash(&after), seen);
    assert!(read(tmp.path()).contains("legacy_verified: \"yes\"\n"));

    let taken = "---\ntype: Note\nverified: \"yes\"\nlegacy_verified: x\n---\nBody\n";
    let (tmp, mut b) = setup(taken);
    let mut c = b.get(&id("a")).unwrap();
    c.set("description", Value::String("e".into()));
    let got = b.put(c, process(), WriteMode::Lenient, now());
    assert_eq!(conflict_reason(got), "legacy_verified_exists");
    assert_eq!(read(tmp.path()), taken);
}

/// Criterion 146: on a concept with a current human verification, a
/// `process:` actor moving `stale_after` later gets
/// `Conflict { reason: "stale_after_extension" }` with the file unchanged;
/// moving it earlier succeeds; a `human:` actor with explicit confirmation
/// (`put_confirmed`, #10746) may move it later; a
/// `verify` of the current revision with `Verification.stale_after` moves it
/// without projecting it into `verified[]`; the same `verify` with a stale
/// `seen` is a `Conflict` with the file unchanged.
#[test]
fn c146_stale_after_extension_needs_a_human_or_a_verify() {
    let text = "---\ntype: Note\nstale_after: 2026-12-01T00:00:00Z\n---\nBody\n";
    let (tmp, mut b) = setup(text);
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(Actor::Human("jo".into()), "2026-11-01T00:00:00Z");
    b.verify_confirmed(&id("a"), v, &seen, confirm()).unwrap();
    let stale = |b: &Bundle, value: &str| {
        let mut c = b.get(&id("a")).unwrap();
        c.set("stale_after", Value::String(value.into()));
        c
    };
    let before = read(tmp.path());
    let got = b.put(
        stale(&b, "2027-01-01T00:00:00Z"),
        process(),
        WriteMode::default(),
        now(),
    );
    assert_eq!(conflict_reason(got), "stale_after_extension");
    assert_eq!(read(tmp.path()), before);
    let c = stale(&b, "2026-11-20T00:00:00Z");
    b.put(c, process(), WriteMode::default(), now()).unwrap();
    let c = stale(&b, "2027-02-01T00:00:00Z");
    let human = Actor::Human("jo".into());
    b.put_confirmed(c, human, confirm(), WriteMode::default(), now())
        .unwrap();
    assert_eq!(
        b.get(&id("a")).unwrap().get("stale_after"),
        Some(&Value::String("2027-02-01T00:00:00Z".into()))
    );

    let seen = content_hash(&b.get(&id("a")).unwrap());
    let mut v = verification(process(), "2026-11-03T00:00:00Z");
    v.stale_after = Some(instant("2027-06-01T00:00:00Z"));
    b.verify(&id("a"), v.clone(), &seen).unwrap();
    let c = b.get(&id("a")).unwrap();
    assert_eq!(
        c.get("stale_after"),
        Some(&Value::String("2027-06-01T00:00:00Z".into()))
    );
    let Some(Value::Sequence(events)) = c.get("verified") else {
        panic!("verified list");
    };
    assert!(events.iter().all(|e| e.get("stale_after").is_none()));

    let mut other = c.clone();
    other.body = "changed".into();
    let stale = content_hash(&other);
    let before = read(tmp.path());
    assert_eq!(
        conflict_reason(b.verify(&id("a"), v, &stale)),
        "stale_verify_hash"
    );
    assert_eq!(read(tmp.path()), before);
}

fn new_concept(name: &str, ty: &str) -> Concept {
    let mut values = Mapping::new();
    values.insert("type".into(), Value::String(ty.into()));
    Concept::new(id(name), values, "New body\n".into())
}

/// A1: a new concept never replaces an existing file; the bytes stay.
#[test]
fn a1_put_never_replaces_an_existing_file_with_a_new_concept() {
    let text = "---\ntype: Note\nkeep: me\n---\nPrecious body\n";
    let (tmp, mut b) = setup(text);
    let got = b.put(
        new_concept("a", "Note"),
        process(),
        WriteMode::default(),
        now(),
    );
    assert_eq!(conflict_reason(got), "exists");
    assert_eq!(read(tmp.path()), text);
}

/// A1: a write refuses when the file no longer holds what was read, or no
/// longer exists; the other writer's bytes stay.
#[test]
fn a1_put_refuses_a_stale_write() {
    let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
    let mut c = b.get(&id("a")).unwrap();
    c.set("description", Value::String("mine".into()));
    let theirs = "---\ntype: Note\ndescription: theirs\n---\nTheir body\n";
    std::fs::write(tmp.path().join("a.md"), theirs).unwrap();
    let got = b.put(c.clone(), process(), WriteMode::default(), now());
    assert_eq!(conflict_reason(got), "stale_write");
    assert_eq!(read(tmp.path()), theirs);

    let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
    let c = b.get(&id("a")).unwrap();
    std::fs::rename(tmp.path().join("a.md"), tmp.path().join("b.md")).unwrap();
    let got = b.put(c, process(), WriteMode::default(), now());
    assert_eq!(conflict_reason(got), "stale_write");
    assert!(!tmp.path().join("a.md").exists());
}

/// A2: the strict check runs on the values that render. The typed view
/// follows every `set`/`remove`, and the private fields cannot be assigned
/// (the `compile_fail` examples on `Concept`).
#[test]
fn a2_strict_check_runs_on_rendered_values() {
    let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
    let mut c = b.get(&id("a")).unwrap();
    c.remove("type");
    assert!(matches!(c.frontmatter().r#type, Field::Absent));
    assert!(matches!(
        check_write(&c, WriteMode::StrictErrors),
        Err(OkfError::Conformance(_))
    ));
    let got = b.put(c, process(), WriteMode::StrictErrors, now());
    assert!(matches!(got, Err(OkfError::Conformance(_))), "{got:?}");
    assert_eq!(read(tmp.path()), "---\ntype: Note\n---\nBody\n");
    let got = b.put(
        new_concept("n", ""),
        process(),
        WriteMode::StrictErrors,
        now(),
    );
    assert!(matches!(got, Err(OkfError::Conformance(_))), "{got:?}");
    assert!(!tmp.path().join("n.md").exists());
}

/// A2: a patch never emits a duplicate key, and a file that holds one never
/// reads as a writable concept.
#[test]
fn a2_render_never_emits_duplicate_keys() {
    let dup = "---\ntype: A\ntype: B\n---\n";
    assert!(matches!(
        parse_concept(id("a"), dup),
        Err(OkfError::Parse { .. })
    ));
    let (tmp, mut b) = setup("---\ntype: A\nx: 1\n---\nBody\n");
    let mut c = b.get(&id("a")).unwrap();
    c.set("type", Value::String("B".into()));
    c.set("zz", Value::String("one".into()));
    c.set("zz", Value::String("two".into()));
    b.put(c, process(), WriteMode::default(), now()).unwrap();
    let out = read(tmp.path());
    for key in ["type:", "zz:", "x:", "generated:"] {
        assert_eq!(
            out.lines().filter(|l| l.starts_with(key)).count(),
            1,
            "{key}: {out}"
        );
    }
}

/// The permission bits of `root/rel`.
#[cfg(unix)]
fn mode_of(root: &Path, rel: &str) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(root.join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .permissions()
        .mode()
        & 0o7777
}

/// Set `root/rel` to `mode`.
#[cfg(unix)]
fn chmod(root: &Path, rel: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.join(rel), std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Assert each of `rels` under `root` is mode 0600, naming the write path.
#[cfg(unix)]
fn assert_0600(root: &Path, step: &str, rels: &[&str]) {
    for rel in rels {
        let got = mode_of(root, rel);
        assert_eq!(got, 0o600, "{step}: {rel} is {got:o}, want 600");
    }
}

/// A3, #10746 (SPEC-OKF-01 §3k, §2.15): a bundle can hold secrets, so a
/// `put` leaves the concept 0600 whatever mode the replaced file had.
#[cfg(unix)]
#[test]
fn a3_put_writes_the_concept_0600_whatever_its_prior_mode() {
    for mode in [0o600, 0o640, 0o644, 0o664] {
        let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
        chmod(tmp.path(), "a.md", mode);
        let mut c = b.get(&id("a")).unwrap();
        c.set("description", Value::String("changed".into()));
        let (outcome, _) = b.put(c, process(), WriteMode::default(), now()).unwrap();
        assert!(matches!(outcome, PutOutcome::Updated { .. }));
        assert_0600(
            tmp.path(),
            &format!("rewrite of a {mode:o} concept"),
            &["a.md"],
        );
    }
}

/// A3, #10746: every path that creates or replaces a bundle file leaves it
/// 0600: a new concept and `log.md` (temp file then rename, created), a
/// rewritten concept, `log.md` and `index.md` (temp file then rename over a
/// 0644 file), the verification record (append, created and existing), and
/// a moved concept and record. Last, no file in the bundle is wider.
#[cfg(unix)]
#[test]
fn a3_every_bundle_write_path_leaves_files_0600() {
    let tmp = tempfile::tempdir().unwrap();
    let r = tmp.path();
    std::fs::write(r.join("a.md"), VERIFIED).unwrap();
    chmod(r, "a.md", 0o644);
    let mut b = Bundle::open(r).unwrap().0;

    // Create: a new concept and a new `log.md`.
    let (outcome, _) = b
        .put(
            new_concept("n", "Note"),
            process(),
            WriteMode::default(),
            now(),
        )
        .unwrap();
    assert_eq!(outcome, PutOutcome::Created);
    assert_0600(r, "create", &["n.md", "log.md"]);

    // Rewrite over 0644 files, and a new record through the append path.
    chmod(r, "log.md", 0o644);
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(Actor::Human("jo".into()), "2026-11-01T09:00:00Z");
    b.verify_confirmed(&id("a"), v, &seen, confirm()).unwrap();
    let record = "_sources/verification/a.jsonl";
    assert_0600(r, "verify", &["a.md", "log.md", record]);

    // Append to an existing 0644 record; rewrite a 0644 concept.
    for rel in ["a.md", "log.md", record] {
        chmod(r, rel, 0o644);
    }
    let mut c = b.get(&id("a")).unwrap();
    c.body = "Changed body\n".into();
    b.put(c, process(), WriteMode::Lenient, now()).unwrap();
    assert_0600(r, "put after verify", &["a.md", "log.md", record]);

    // `index.md`: created, then rewritten over a 0644 file.
    b.regenerate_indexes().unwrap();
    assert_0600(r, "index create", &["index.md"]);
    chmod(r, "index.md", 0o644);
    b.put(
        new_concept("m", "Note"),
        process(),
        WriteMode::default(),
        now(),
    )
    .unwrap();
    let report = b.regenerate_indexes().unwrap();
    assert!(report.written.iter().any(|w| w == "index.md"), "{report:?}");
    assert_0600(r, "index rewrite", &["index.md"]);

    // Move: the concept and its record land at new paths.
    for rel in ["a.md", record] {
        chmod(r, rel, 0o644);
    }
    b.move_concept(&id("a"), &id("moved/b"), now()).unwrap();
    assert_0600(
        r,
        "move",
        &["moved/b.md", "_sources/verification/moved/b.jsonl"],
    );

    for (rel, _) in snapshot(r) {
        if std::fs::symlink_metadata(r.join(&rel)).unwrap().is_file() {
            assert_0600(r, "final sweep", &[&rel]);
        }
    }
}

/// Every path under `root` with its bytes (a link's target text), links not
/// followed, sorted.
#[cfg(unix)]
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let ft = std::fs::symlink_metadata(&p).unwrap().file_type();
            if ft.is_symlink() {
                let t = std::fs::read_link(&p).unwrap();
                out.push((rel, t.to_string_lossy().into_owned().into_bytes()));
            } else if ft.is_dir() {
                out.push((rel, Vec::new()));
                walk(root, &p, out);
            } else {
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    // #10746 S5: the operation journal under `_sources/ops/` records each
    // attempt; "no file changed" covers every other file (the §4.11 and
    // criterion 106 comparison scope).
    out.retain(|(rel, _)| !rel.starts_with("_sources/ops"));
    // #10746 S5: the §4.4 lock file holds no data; a write that fails after
    // taking the lock leaves it, empty.
    out.retain(|(rel, _)| rel != "_sources/okf.lock" && rel != "_sources");
    out.sort();
    out
}

/// #10746 (§2.10, §4.4, §4.11): when the record append fails, `put` and
/// `verify` return an error with the concept file unchanged, so the legacy
/// frontmatter event is not lost. Read-only `_sources/verification` makes
/// the append fail after every path check passes.
#[cfg(unix)]
#[test]
fn unwritable_record_keeps_the_legacy_event_and_the_concept() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, mut b) = setup(VERIFIED);
    let dir = tmp.path().join("_sources/verification");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(dir.join("probe"), "x").is_ok() {
        // Running as a user that ignores modes (root): nothing to prove.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let before = snapshot(tmp.path());

    let mut c = b.get(&id("a")).unwrap();
    c.body = "Changed body\n".into();
    let got = b.put(c, process(), WriteMode::Lenient, now());
    assert!(matches!(got, Err(OkfError::Io { .. })), "{got:?}");
    assert_eq!(read(tmp.path()), VERIFIED, "the concept is unchanged");
    let kept = b.get(&id("a")).unwrap();
    assert!(
        kept.get("verified")
            .and_then(Value::as_sequence)
            .is_some_and(|s| s.len() == 1),
        "the legacy event stays in frontmatter: {}",
        read(tmp.path())
    );
    assert_eq!(snapshot(tmp.path()), before, "no file changed");

    let seen = content_hash(&kept);
    let v = verification(Actor::Human("jo".into()), "2026-11-01T09:00:00Z");
    let got = b.verify_confirmed(&id("a"), v, &seen, confirm());
    assert!(matches!(got, Err(OkfError::Io { .. })), "{got:?}");
    assert_eq!(snapshot(tmp.path()), before, "verify changed no file");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// #10746 (§2.11, §4.4): a `log.md` that is a symbolic link makes `put` and
/// `verify` return `PathEscape` before any file changes: the concept, the
/// record and the link's target keep their bytes.
#[cfg(unix)]
#[test]
fn put_and_verify_refuse_a_symlinked_log_with_no_file_changed() {
    let (tmp, mut b) = setup(VERIFIED);
    let r = tmp.path();
    std::fs::write(r.join("elsewhere.txt"), "# Log\n").unwrap();
    std::os::unix::fs::symlink(r.join("elsewhere.txt"), r.join("log.md")).unwrap();
    let before = snapshot(r);

    let mut c = b.get(&id("a")).unwrap();
    c.body = "Changed body\n".into();
    let got = b.put(c, process(), WriteMode::Lenient, now());
    assert!(matches!(got, Err(OkfError::PathEscape(_))), "{got:?}");
    assert_eq!(snapshot(r), before, "put changed no file");

    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(Actor::Human("jo".into()), "2026-11-01T09:00:00Z");
    let got = b.verify_confirmed(&id("a"), v, &seen, confirm());
    assert!(matches!(got, Err(OkfError::PathEscape(_))), "{got:?}");
    assert_eq!(snapshot(r), before, "verify changed no file");
}

/// A concept a person reviewed, as found on disk: a `human:jo` event in
/// frontmatter and the same event in the record, bound to the current hash.
const REVIEWED: &str = "---\ntype: Note\nstale_after: 2026-12-01T00:00:00Z\nverified:\n  - { by: human:jo, at: 2026-11-01T00:00:00Z }\n---\nBody\n";

fn reviewed_on_disk() -> (tempfile::TempDir, Bundle) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.md"), REVIEWED).unwrap();
    let hash = content_hash(&parse_concept(id("a"), REVIEWED).unwrap().0);
    let dir = tmp.path().join("_sources/verification");
    std::fs::create_dir_all(&dir).unwrap();
    let line = format!(
        "{{\"by\":\"human:jo\",\"at\":\"2026-11-01T00:00:00Z\",\"hash\":\"{}\"}}\n",
        hash.as_str()
    );
    std::fs::write(dir.join("a.jsonl"), line).unwrap();
    let b = Bundle::open(tmp.path()).unwrap().0;
    assert_eq!(
        b.verification_state(&id("a")).unwrap(),
        VerificationState::Verified {
            human: true,
            events: 1,
            history: 0
        }
    );
    (tmp, b)
}

/// #10746 (§2.4): a `human:` actor alone is not an explicit confirmation.
/// Plain `put` with `Actor::Human` that moves `stale_after` later on a
/// human-reviewed concept is refused with `stale_after_extension`, and no
/// file in the bundle changes.
#[test]
fn human_actor_without_confirmation_cannot_extend_a_review() {
    let (tmp, mut b) = reviewed_on_disk();
    let before = snapshot(tmp.path());
    let mut c = b.get(&id("a")).unwrap();
    c.set("stale_after", Value::String("2027-06-01T00:00:00Z".into()));
    let got = b.put(c, Actor::Human("jo".into()), WriteMode::default(), now());
    assert_eq!(conflict_reason(got), "stale_after_extension");
    assert_eq!(snapshot(tmp.path()), before, "no file changed");
}

/// #10746 (§2.4): plain `verify` with a `human:` actor and no explicit
/// confirmation is refused with `human_not_confirmed`; no file changes and
/// the concept stays Unverified. A `process:` actor still verifies.
#[test]
fn human_verify_without_confirmation_is_refused() {
    let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
    let before = snapshot(tmp.path());
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(Actor::Human("jo".into()), "2026-11-01T09:00:00Z");
    assert_eq!(
        conflict_reason(b.verify(&id("a"), v, &seen)),
        "human_not_confirmed"
    );
    assert_eq!(snapshot(tmp.path()), before, "no file changed");
    assert_eq!(
        b.verification_state(&id("a")).unwrap(),
        VerificationState::Unverified { history: 0 }
    );
    let v = verification(process(), "2026-11-01T09:00:00Z");
    b.verify(&id("a"), v, &seen).unwrap();
}

/// #10746 (§2.4): `verify` refuses, with no file changed, an actor that the
/// record reader parses back as `human:` (`Actor::parse(&by.to_string())`).
#[cfg(unix)]
fn assert_parsed_human_is_refused(by: Actor) {
    let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
    let before = snapshot(tmp.path());
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(by.clone(), "2026-11-01T09:00:00Z");
    assert_eq!(
        conflict_reason(b.verify(&id("a"), v, &seen)),
        "human_not_confirmed",
        "{by:?}"
    );
    assert_eq!(snapshot(tmp.path()), before, "{by:?}: no file changed");
}

/// #10746: `Other("human:jo")` is stored as `human:jo` and read as a human.
#[cfg(unix)]
#[test]
fn human_verify_refuses_an_other_actor_with_a_human_prefix() {
    assert_parsed_human_is_refused(Actor::Other("human:jo".into()));
}

/// #10746: a producer named `human:jo` is stored as `human:jo/1` and read as
/// a human.
#[cfg(unix)]
#[test]
fn human_verify_refuses_a_producer_named_human() {
    assert_parsed_human_is_refused(Actor::Producer {
        name: "human:jo".into(),
        version: "1".into(),
    });
}

/// #10746 (§2.11 fold rule): `verify` applies the `put` fold check. A
/// verify of `A` beside an existing `a.md` is `fold_collision` on every file
/// system, and no second verification record (`_sources/verification/A…`)
/// is written.
#[cfg(unix)]
#[test]
fn verify_refuses_a_fold_twin_and_writes_no_record() {
    let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
    let before = snapshot(tmp.path());
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(process(), "2026-11-01T09:00:00Z");
    assert_eq!(
        conflict_reason(b.verify(&id("A"), v, &seen)),
        "fold_collision"
    );
    assert_eq!(snapshot(tmp.path()), before, "no file changed");
    assert!(!tmp.path().join("_sources/verification").exists());
}

/// #10746 (§2.3, §4.5): an actor holding a control character cannot forge
/// a `log.md` entry or heading. `put`, `put_confirmed`, `verify` and
/// `verify_confirmed` each refuse it with `actor_control_character` and no
/// file changes. A `deprecate` or `supersede` reason holding one is refused
/// with `reason_control_character`, so neither text reaches `log.md`.
#[cfg(unix)]
#[test]
fn put_and_verify_refuse_an_actor_with_a_control_character() {
    let forged = "x`\n\n## 2026-01-01\n\n- **Verified** `/a.md` by `human:jo";
    for by in [
        Actor::Process(forged.into()),
        Actor::Human("jo\r".into()),
        Actor::Other("tab\there".into()),
    ] {
        let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
        let before = snapshot(tmp.path());
        let mut c = b.get(&id("a")).unwrap();
        c.body = "Changed\n".into();
        let got = b.put(c.clone(), by.clone(), WriteMode::default(), now());
        assert_eq!(conflict_reason(got), "actor_control_character", "{by:?}");
        let got = b.put_confirmed(c, by.clone(), confirm(), WriteMode::default(), now());
        assert_eq!(conflict_reason(got), "actor_control_character", "{by:?}");
        let seen = content_hash(&b.get(&id("a")).unwrap());
        let v = verification(by.clone(), "2026-11-01T09:00:00Z");
        let got = b.verify(&id("a"), v.clone(), &seen);
        assert_eq!(conflict_reason(got), "actor_control_character", "{by:?}");
        let got = b.verify_confirmed(&id("a"), v, &seen, confirm());
        assert_eq!(conflict_reason(got), "actor_control_character", "{by:?}");
        assert_eq!(snapshot(tmp.path()), before, "{by:?}: no file changed");
        assert!(!tmp.path().join("log.md").exists(), "{by:?}");
    }
    for reason in [forged, "cr\r", "tab\t"] {
        let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
        let before = snapshot(tmp.path());
        let got = b.deprecate(&id("a"), reason, process(), now());
        assert_eq!(
            conflict_reason(got),
            "reason_control_character",
            "{reason:?}"
        );
        let got = b.supersede(&id("a"), new_concept("b", "Note"), reason, process(), now());
        assert_eq!(
            conflict_reason(got),
            "reason_control_character",
            "{reason:?}"
        );
        assert_eq!(snapshot(tmp.path()), before, "{reason:?}: no file changed");
        assert!(!tmp.path().join("log.md").exists(), "{reason:?}");
    }
}
