//! SPEC-OKF-01 §6.2 criteria for slice S3 of #10746 that exercise
//! `Bundle::put` and `Bundle::verify`: verification invalidation (§2.10,
//! criteria 31-36), malformed owned values (125), `stale_after` extension
//! (146), and the Architect's S3 acceptance items A1 (no lost data), A2
//! (strict check on rendered values) and A3 (file mode kept).

use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};
use trusty_okf::OkfError;
use trusty_okf::bundle::{Bundle, PutOutcome, VerificationState};
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
    b.verify(&id("a"), v, &seen).unwrap();
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
        conflict_reason(b.verify(&id("a"), v, &stale)),
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
    let (_, findings) = b.verify(&id("a"), v, &seen).unwrap();
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
/// moving it earlier succeeds; a `human:` actor may move it later; a
/// `verify` of the current revision with `Verification.stale_after` moves it
/// without projecting it into `verified[]`; the same `verify` with a stale
/// `seen` is a `Conflict` with the file unchanged.
#[test]
fn c146_stale_after_extension_needs_a_human_or_a_verify() {
    let text = "---\ntype: Note\nstale_after: 2026-12-01T00:00:00Z\n---\nBody\n";
    let (tmp, mut b) = setup(text);
    let seen = content_hash(&b.get(&id("a")).unwrap());
    let v = verification(Actor::Human("jo".into()), "2026-11-01T00:00:00Z");
    b.verify(&id("a"), v, &seen).unwrap();
    let set_stale = |b: &mut Bundle, by: Actor, value: &str| {
        let mut c = b.get(&id("a")).unwrap();
        c.set("stale_after", Value::String(value.into()));
        b.put(c, by, WriteMode::default(), now())
    };
    let before = read(tmp.path());
    let got = set_stale(&mut b, process(), "2027-01-01T00:00:00Z");
    assert_eq!(conflict_reason(got), "stale_after_extension");
    assert_eq!(read(tmp.path()), before);
    set_stale(&mut b, process(), "2026-11-20T00:00:00Z").unwrap();
    set_stale(&mut b, Actor::Human("jo".into()), "2027-02-01T00:00:00Z").unwrap();

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

/// A3 (gate item): a write keeps the target file's permissions; a 0600
/// concept stays 0600 and a 0640 one stays 0640.
#[cfg(unix)]
#[test]
fn a3_put_preserves_the_target_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    for mode in [0o600, 0o640] {
        let (tmp, mut b) = setup("---\ntype: Note\n---\nBody\n");
        let file = tmp.path().join("a.md");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        let mut c = b.get(&id("a")).unwrap();
        c.set("description", Value::String("changed".into()));
        let (outcome, _) = b.put(c, process(), WriteMode::default(), now()).unwrap();
        assert!(matches!(outcome, PutOutcome::Updated { .. }));
        let got = std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777;
        assert_eq!(got, mode, "mode {mode:o} became {got:o}");
    }
}

/// A3: a new concept file gets the documented default create mode, the
/// mode `std::fs::write` gives a new file in the same directory.
#[cfg(unix)]
#[test]
fn a3_new_file_gets_the_default_create_mode() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let probe = tmp.path().join("probe.txt");
    std::fs::write(&probe, "x").unwrap();
    let expected = std::fs::metadata(&probe).unwrap().permissions().mode() & 0o7777;
    let mut b = Bundle::open(tmp.path()).unwrap().0;
    let (outcome, _) = b
        .put(
            new_concept("n", "Note"),
            process(),
            WriteMode::default(),
            now(),
        )
        .unwrap();
    assert_eq!(outcome, PutOutcome::Created);
    let got = std::fs::metadata(tmp.path().join("n.md"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(got, expected);
}
