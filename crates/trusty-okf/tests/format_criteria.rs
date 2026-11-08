//! SPEC-OKF-01 §6.2 criteria for slice S2 of #10746: format, profile and
//! validate.
//!
//! Each test is named `c<N>_…` after the criterion it checks. Criteria 7, 8
//! and 9 and the `Bundle` halves of 1, 113 and 115 are in
//! `bundle_criteria.rs` (S3); `deprecate` and `rebuild` come with S6. Since
//! S3 every write here goes through `Bundle::put`.
//!
//! Fixtures: `tests/fixtures/google-okf-ad30107c/` (the four Google OKF
//! bundles, see its PROVENANCE.md) and `tests/fixtures/raw-roundtrip/` (the
//! §2.2.1 fixtures listed above criterion 113).

use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde_yaml::{Mapping, Value};
use trusty_okf::OkfError;
use trusty_okf::bundle::{Bundle, PutOutcome};
use trusty_okf::format::v02::{LEGACY_PRODUCER, generated_at, normalize_v02, sources};
use trusty_okf::format::validate::conformance_findings;
use trusty_okf::format::{
    Actor, Concept, ConceptId, ConceptRef, Field, FindingKind, Severity, Staleness, Status,
    TrustTier, WriteMode, concept_ref_for_path, parse_concept, parse_concept_at, render_concept,
    staleness, trust_tier,
};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn instant(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

/// `Bundle::put` from a process actor at a fixed instant. #10746 S3:
/// `prepare_write` is crate-private; every public write goes through `put`.
fn put(root: &Path, c: Concept, mode: WriteMode) -> Result<PutOutcome, OkfError> {
    let (mut b, _) = Bundle::open(root).unwrap();
    b.put(
        c,
        Actor::Process("test".into()),
        mode,
        instant("2026-11-04T00:00:00Z"),
    )
    .map(|(o, _)| o)
}

fn parse(text: &str) -> Concept {
    parse_concept(id("fixture"), text).unwrap().0
}

/// Every `.md` file under `dir`, sorted, as (bundle-relative path, path).
fn md_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            md_files(root, &p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            out.push((rel, p));
        }
    }
}

/// The concept files of one bundle: every `.md` except `index.md`/`log.md`.
fn concept_files(bundle: &Path) -> Vec<(String, String)> {
    let mut all = Vec::new();
    md_files(bundle, bundle, &mut all);
    all.into_iter()
        .filter(|(rel, _)| {
            let name = rel.rsplit('/').next().unwrap_or(rel);
            !trusty_okf::format::container::is_reserved_file(name)
        })
        .map(|(rel, p)| (rel, std::fs::read_to_string(p).unwrap()))
        .collect()
}

fn google_bundles() -> Vec<PathBuf> {
    let dir = fixtures().join("google-okf-ad30107c/bundles");
    let mut bundles: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    bundles.sort();
    bundles
}

fn raw_fixtures() -> Vec<(String, String)> {
    let dir = fixtures().join("raw-roundtrip");
    let mut out: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
            (stem, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    out.sort();
    assert_eq!(out.len(), 8, "the eight §2.2.1 fixtures");
    out
}

/// The span text of each top-level entry, by key.
fn spans(c: &Concept) -> Vec<(String, String)> {
    c.raw()
        .entries
        .iter()
        .map(|e| (e.key.clone(), c.raw().text[e.span.clone()].to_string()))
        .collect()
}

/// Asserts every entry of `before` other than `owned` keeps its bytes in
/// `after`, and the body is unchanged.
fn assert_only_owned_changed(name: &str, before: &Concept, after_text: &str, owned: &[&str]) {
    let after = parse(after_text);
    assert!(after.raw().patchable, "{name}: rendered block must split");
    let after_spans = spans(&after);
    for (key, text) in spans(before) {
        if owned.contains(&key.as_str()) {
            continue;
        }
        let now = after_spans.iter().find(|(k, _)| *k == key);
        assert_eq!(
            now.map(|(_, t)| t.as_str()),
            Some(text.as_str()),
            "{name}: entry `{key}` must keep its bytes"
        );
    }
    assert_eq!(after.body, before.body, "{name}: body must keep its bytes");
}

fn mapping(yaml: &str) -> Mapping {
    match serde_yaml::from_str::<Value>(yaml).unwrap() {
        Value::Mapping(m) => m,
        other => panic!("not a mapping: {other:?}"),
    }
}

/// Criterion 1 (S2 part): every concept of the four Google bundles parses
/// leniently, and none breaks OKF §11. `Bundle::open` itself is S3.
#[test]
fn c01_google_bundles_parse_leniently() {
    let bundles = google_bundles();
    assert_eq!(
        bundles.len(),
        4,
        "acme_retail, crypto_bitcoin, ga4, stackoverflow"
    );
    for bundle in bundles {
        let files = concept_files(&bundle);
        assert!(!files.is_empty(), "{bundle:?} has concepts");
        for (rel, text) in files {
            let (r, foreign) = concept_ref_for_path(&rel);
            assert!(foreign.is_none(), "{rel}: path passes §2.11");
            let (c, findings) = parse_concept_at(r, &text)
                .unwrap_or_else(|e| panic!("{rel}: lenient parse failed: {e}"));
            assert!(
                findings.iter().all(|f| f.severity != Severity::Error),
                "{rel}: {findings:?}"
            );
            assert!(
                conformance_findings(c.frontmatter(), &rel).is_empty(),
                "{rel}"
            );
        }
    }
}

/// Criterion 2: parse, render and parse again yields equal frontmatter, and
/// every unknown key survives — through the raw-preserving render (byte
/// identical) and through a key-sorted render of the same values.
#[test]
fn c02_google_bundles_parse_render_parse_round_trip() {
    for bundle in google_bundles() {
        for (rel, text) in concept_files(&bundle) {
            let c = parse_concept_at(concept_ref_for_path(&rel).0, &text)
                .unwrap()
                .0;
            assert!(c.raw().patchable, "{rel}: block splits into entries");
            let rendered = render_concept(&c);
            assert_eq!(rendered, text, "{rel}: unchanged render is byte-identical");
            let again = parse_concept_at(c.id.clone(), &rendered).unwrap().0;
            assert_eq!(again.values(), c.values(), "{rel}");
            assert_eq!(again.frontmatter(), c.frontmatter(), "{rel}");

            let fresh = Concept::new(id("fresh"), c.values().clone(), c.body.clone());
            let sorted = render_concept(&fresh);
            let back = parse_concept(id("fresh"), &sorted).unwrap().0;
            assert_eq!(
                back.values(),
                c.values(),
                "{rel}: key-sorted render loses nothing"
            );
            let extra: Vec<&String> = c.frontmatter().extra.keys().collect();
            let extra_back: Vec<&String> = back.frontmatter().extra.keys().collect();
            assert_eq!(extra_back, extra, "{rel}: every unknown key survives");
            assert_eq!(back.body, c.body, "{rel}");
        }
    }
}

/// Criterion 3: a strict write refuses a missing or empty `type` with
/// `Conformance` and changes no file.
#[test]
fn c03_strict_write_refuses_missing_type_and_changes_no_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert_eq!(WriteMode::default(), WriteMode::StrictErrors);

    // A new concept with no `type`: refused, and nothing is created.
    let c = Concept::new(id("notes/no-type"), mapping("title: x\n"), "Body\n".into());
    match put(root, c, WriteMode::default()) {
        Err(OkfError::Conformance(f)) => {
            assert!(f.iter().any(|x| x.kind == FindingKind::MissingType));
        }
        other => panic!("expected Conformance, got {other:?}"),
    }
    assert!(
        std::fs::read_dir(root).unwrap().next().is_none(),
        "no file or dir created"
    );

    // An existing concept whose write drops or empties `type`: refused, bytes kept.
    let text = "---\ntype: Metric\ntitle: x\n---\n\nBody\n";
    std::fs::write(root.join("kept.md"), text).unwrap();
    let mut c = parse_concept(id("kept"), text).unwrap().0;
    c.remove("type");
    assert!(matches!(
        put(root, c.clone(), WriteMode::StrictErrors),
        Err(OkfError::Conformance(_))
    ));
    c.set("type", Value::String(String::new()));
    assert!(matches!(
        put(root, c.clone(), WriteMode::StrictWarnings),
        Err(OkfError::Conformance(_))
    ));
    assert_eq!(std::fs::read_to_string(root.join("kept.md")).unwrap(), text);

    // The same concept with a `type` writes.
    c.set("type", Value::String("Metric".into()));
    c.set("title", Value::String("y".into()));
    let outcome = put(root, c, WriteMode::StrictErrors).unwrap();
    assert!(matches!(outcome, PutOutcome::Updated { .. }), "{outcome:?}");
}

/// Criterion 4: a bare `verified` mapping parses as a one-element list.
#[test]
fn c04_bare_verified_mapping_is_one_element_list() {
    let c =
        parse("---\ntype: A\nverified: { by: human:ahormati, at: 2026-06-25T09:00:00Z }\n---\n");
    match &c.frontmatter().verified {
        Field::Valid(list) => {
            assert_eq!(list.len(), 1);
            let v = list[0].valid().expect("a valid event");
            assert_eq!(v.by, Actor::Human("ahormati".into()));
            assert_eq!(v.at, instant("2026-06-25T09:00:00Z"));
        }
        other => panic!("expected a list, got {other:?}"),
    }
}

/// Criterion 5: `trust_tier` follows OKF §5.3.
#[test]
fn c05_trust_tier_follows_okf_5_3() {
    let tier = |fm: &str| trust_tier(parse(&format!("---\ntype: A\n{fm}---\n")).frontmatter());
    assert_eq!(tier(""), TrustTier::Unverified);
    assert_eq!(
        tier(
            "verified:\n  - { by: process:nightly, at: 2026-06-26T02:00:00Z }\n  - { by: agent/v1, at: 2026-06-27T02:00:00Z }\n"
        ),
        TrustTier::MachineConfirmed
    );
    assert_eq!(
        tier(
            "verified:\n  - { by: process:nightly, at: 2026-06-26T02:00:00Z }\n  - { by: human:jsmith, at: 2026-06-25T09:00:00Z }\n"
        ),
        TrustTier::HumanReviewed
    );
    assert_eq!(tier("verified: \"yes\"\n"), TrustTier::Unverified);
}

/// Criterion 6: `staleness` is `Unknown`, with a warning, for a date-only or
/// offset-less `stale_after`.
#[test]
fn c06_staleness_unknown_for_date_only_or_offsetless() {
    let now = instant("2026-11-04T00:00:00Z");
    for value in ["2026-12-01", "2026-12-01T00:00:00"] {
        let text = format!("---\ntype: A\nstale_after: {value}\n---\n");
        let (c, findings) = parse_concept(id("s"), &text).unwrap();
        assert!(
            matches!(c.frontmatter().stale_after, Field::Malformed(_)),
            "{value}"
        );
        assert_eq!(
            staleness(c.frontmatter(), now),
            Staleness::Unknown,
            "{value}"
        );
        assert!(
            findings.iter().any(|f| f.severity == Severity::Warning
                && f.kind == FindingKind::MalformedField
                && f.message.contains("stale_after")),
            "{value}: {findings:?}"
        );
    }
    let fresh = parse("---\ntype: A\nstale_after: 2026-12-01T00:00:00Z\n---\n");
    assert_eq!(staleness(fresh.frontmatter(), now), Staleness::Fresh);
    let stale = parse("---\ntype: A\nstale_after: 2026-01-01T00:00:00+01:00\n---\n");
    assert_eq!(staleness(stale.frontmatter(), now), Staleness::Stale);
    let none = parse("---\ntype: A\n---\n");
    assert_eq!(staleness(none.frontmatter(), now), Staleness::Fresh);
}

/// Criterion 10: no public function deletes a concept file. The one file
/// deletion in `src/` removes the write's own temporary file and carries the
/// `okf: temp-cleanup` marker.
#[test]
fn c10_no_public_function_deletes_a_concept_file() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(!files.is_empty());
    let mut deleting = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let deletes = ["remove_file", "remove_dir"]
                .iter()
                .any(|t| line.contains(t));
            if deletes
                && !line.contains("okf: temp-cleanup")
                && !line.trim_start().starts_with("//")
            {
                deleting.push(format!("{}:{}: {line}", path.display(), n + 1));
            }
            assert!(
                !line.contains("pub fn delete"),
                "{}:{}",
                path.display(),
                n + 1
            );
        }
    }
    assert!(
        deleting.is_empty(),
        "file deletion outside temp cleanup: {deleting:?}"
    );
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

const TRUSTY_KB_CONCEPT: &str = "---\naliases:\n- Countess\ncreated: 2026-01-01T00:00:00Z\nknows:\n- '[[Babbage]]'\nsame_as:\n- https://example.org/ada\nsource_id: docs\nsource_item_id: notes/ada.md\nsource_kind: docstore\nsource_timestamp: 2026-01-02T00:00:00Z\nsources:\n- notes/ada.md\n- https://example.org/bio\ntitle: Ada\ntrust: trusted_internal\ntype: Person\nuid: ada-1\nupdated: 2026-03-04T05:06:07+02:00\n---\n\nBody stays.\n";

/// Criterion 18: an offset-bearing `updated` becomes `generated.at` with the
/// same instant, and `updated` is removed.
#[test]
fn c18_offset_updated_migrates_to_generated_at() {
    let mut c = parse(TRUSTY_KB_CONCEPT);
    let findings = normalize_v02(&mut c);
    assert!(findings.is_empty(), "{findings:?}");
    let out = parse(&render_concept(&c));
    assert!(out.get("updated").is_none());
    match &out.frontmatter().generated {
        Field::Valid(g) => {
            assert_eq!(g.at, Some(instant("2026-03-04T05:06:07+02:00")));
            assert_eq!(g.by.to_string(), LEGACY_PRODUCER);
        }
        other => panic!("expected generated, got {other:?}"),
    }
    assert_eq!(out.body, "\nBody stays.\n");
}

/// Criterion 19: a date-only `updated` stays, no `generated` appears, and a
/// warning is reported.
#[test]
fn c19_date_only_updated_stays_with_warning() {
    let text = "---\ntype: Person\nupdated: 2026-03-04\n---\n";
    let mut c = parse(text);
    let findings = normalize_v02(&mut c);
    assert!(
        findings
            .iter()
            .any(|f| f.severity == Severity::Warning && f.kind == FindingKind::UnmappedLegacyValue)
    );
    assert!(c.get("generated").is_none());
    assert_eq!(render_concept(&c), text, "nothing changes");
}

/// Criterion 20: plain-string `sources` become mappings with the same strings
/// as `resource`, in order, with no `id`; a mapping entry passes through.
#[test]
fn c20_plain_string_sources_become_mappings() {
    let mut c = parse("---\ntype: A\nsources:\n- b\n- {resource: m, id: k}\n- a\n---\n");
    normalize_v02(&mut c);
    let out = parse(&render_concept(&c));
    let Field::Valid(list) = &out.frontmatter().sources else {
        panic!("sources must read as valid")
    };
    let got: Vec<(String, Option<String>)> = list
        .iter()
        .map(|s| {
            let s = s.valid().expect("valid entry");
            (s.resource.clone(), s.id.clone())
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("b".to_string(), None),
            ("m".to_string(), Some("k".to_string())),
            ("a".to_string(), None)
        ]
    );
}

/// Criterion 21: the legacy and extension keys keep their bytes through the
/// v0.2 mapping.
#[test]
fn c21_legacy_keys_byte_identical_after_normalize() {
    let before = parse(TRUSTY_KB_CONCEPT);
    let mut c = before.clone();
    normalize_v02(&mut c);
    let after = render_concept(&c);
    // The mapping owns only `updated`, `generated`, `sources` and `verified`.
    assert_only_owned_changed(
        "trusty-kb concept",
        &before,
        &after,
        &["updated", "generated", "sources", "verified"],
    );
    for key in [
        "source_id",
        "source_kind",
        "source_item_id",
        "source_timestamp",
        "trust",
        "created",
        "aliases",
        "same_as",
        "uid",
        "knows",
    ] {
        assert!(
            spans(&before).iter().any(|(k, _)| k == key),
            "fixture has {key}"
        );
    }
}

/// Criterion 22: a v0.1 concept's `timestamp` and body `# Citations` read as
/// `generated.at` and `sources`, and the body is unchanged.
#[test]
fn c22_v01_timestamp_and_citations_read_as_v02() {
    let text = "---\ntype: Metric\ntitle: Income statement\ntimestamp: '2026-05-28T22:53:05+00:00'\n---\n\n# Definition\nText.\n\n# Citations\n- https://wiki.acme/finance/fpa-handbook\n- https://wiki.acme/finance/revenue-recognition\n- https://wiki.acme/finance/cost-allocation\n";
    let c = parse(text);
    assert_eq!(generated_at(&c), Some(instant("2026-05-28T22:53:05+00:00")));
    let resources: Vec<String> = sources(&c).into_iter().map(|s| s.resource).collect();
    assert_eq!(
        resources,
        vec![
            "https://wiki.acme/finance/fpa-handbook",
            "https://wiki.acme/finance/revenue-recognition",
            "https://wiki.acme/finance/cost-allocation"
        ]
    );
    assert_eq!(render_concept(&c), text, "reading never edits the body");
}

/// Criterion 113: under `Lenient`, a write that owns `status` (as
/// `deprecate` does) and a `put` that changes only `description` leave the
/// body and every top-level entry the operation does not own byte-identical.
/// The `put` owns `description` and the §2.10 keys (`generated`, `verified`,
/// `legacy_verified`, `legacy_generated`; S3). The `deprecate` entry point
/// itself comes with S6; here the `status` write goes through `put`.
#[test]
fn c113_owned_key_writes_leave_other_entries_byte_identical() {
    for (name, text) in raw_fixtures() {
        let before = parse(&text);
        assert!(before.raw().patchable, "{name}");
        for (key, value) in [("status", "deprecated"), ("description", "changed")] {
            let tmp = tempfile::tempdir().unwrap();
            std::fs::write(tmp.path().join("fixture.md"), &text).unwrap();
            let mut c = before.clone();
            c.set(key, Value::String(value.into()));
            put(tmp.path(), c, WriteMode::Lenient).unwrap();
            let written = std::fs::read_to_string(tmp.path().join("fixture.md")).unwrap();
            let owned: &[&str] = if key == "status" {
                &["status"]
            } else {
                &[
                    "description",
                    "generated",
                    "verified",
                    "legacy_verified",
                    "legacy_generated",
                ]
            };
            assert_only_owned_changed(&name, &before, &written, owned);
            assert_eq!(parse(&written).get(key), Some(&Value::String(value.into())));
        }
    }
}

/// Criterion 114: a read followed by a write of the unchanged concept writes
/// zero bytes; each malformed value reads as `Field::Malformed` with a
/// warning; `generated: {by: x}` reads as valid with `at` absent.
#[test]
fn c114_unchanged_put_writes_zero_bytes_and_reads_malformed() {
    for (name, text) in raw_fixtures() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("fixture.md");
        std::fs::write(&file, &text).unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        let (c, findings) = parse_concept(id("fixture"), &text).unwrap();
        let outcome = put(tmp.path(), c.clone(), WriteMode::Lenient).unwrap();
        assert_eq!(outcome, PutOutcome::Unchanged, "{name}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "{name}");
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            mtime,
            "{name}"
        );

        let fm = c.frontmatter();
        let warned = |kind: FindingKind| {
            findings
                .iter()
                .any(|f| f.severity == Severity::Warning && f.kind == kind)
        };
        match name.as_str() {
            "stale-after-date-only" | "stale-after-no-offset" => {
                assert!(matches!(fm.stale_after, Field::Malformed(_)), "{name}");
                assert!(warned(FindingKind::MalformedField), "{name}");
            }
            "generated-by-only" => match &fm.generated {
                Field::Valid(g) => {
                    assert_eq!(g.at, None);
                    assert_eq!(g.by, Actor::Other("x".into()));
                }
                other => panic!("{name}: {other:?}"),
            },
            "status-archived" => {
                assert_eq!(fm.status, Field::Valid(Status::Other("archived".into())));
                assert!(warned(FindingKind::UnknownStatus), "{name}");
            }
            "type-list" => {
                assert!(matches!(fm.r#type, Field::Malformed(_)), "{name}");
                assert!(warned(FindingKind::MalformedField), "{name}");
            }
            "verified-yes" => {
                assert!(matches!(fm.verified, Field::Malformed(_)), "{name}");
                assert!(warned(FindingKind::MalformedField), "{name}");
            }
            "flow-unknown-with-comment" | "unknown-nested-mapping" => {
                assert!(findings.is_empty(), "{name}: {findings:?}");
                assert!(
                    fm.extra
                        .keys()
                        .any(|k| k.starts_with("custom_flow") || k == "x_extension")
                );
            }
            other => panic!("unexpected fixture {other}"),
        }
    }
}

/// Criterion 115 (S2 part): `notes/a:b.md`, `Index.md` and `_drafts/x.md`
/// read as foreign with one info finding each, and a write to any of them
/// returns `InvalidId` and changes no byte. `Bundle::open`,
/// `regenerate_indexes` and `move_concept` (S3) and `rebuild` (S6) add the
/// rest of the criterion.
#[test]
fn c115_foreign_paths_read_only_and_write_is_invalid_id() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("notes")).unwrap();
    std::fs::create_dir_all(root.join("_drafts")).unwrap();
    let text = "---\ntype: Note\n---\n\nSee [target](/notes/target.md).\n";
    for (rel, rule) in [("notes/a:b.md", 4u8), ("Index.md", 6), ("_drafts/x.md", 7)] {
        std::fs::write(root.join(rel), text).unwrap();
        let (r, finding) = concept_ref_for_path(rel);
        assert!(matches!(r, ConceptRef::Foreign(_)), "{rel}");
        assert_eq!(finding.map(|f| f.severity), Some(Severity::Info), "{rel}");
        let (mut c, _) = parse_concept_at(r, text).unwrap();
        c.set("title", Value::String("changed".into()));
        match put(root, c, WriteMode::Lenient) {
            Err(OkfError::InvalidId { rule: got, .. }) => assert_eq!(got, rule, "{rel}"),
            other => panic!("{rel}: expected InvalidId, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(root.join(rel)).unwrap(),
            text,
            "{rel}"
        );
    }
}
