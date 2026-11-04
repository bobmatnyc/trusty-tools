//! SPEC-OKF-01 §6.2 criteria 13 (registry half), 116 and 117, and the
//! compatibility of the build layer moved from trusty-kb (#10746, slice S4).
//!
//! Why: the build layer reads files that trusty-kb wrote and that live in
//! users' trees. A registry row that no longer loads, a ledger line that no
//! longer parses, or a re-ingest that drops a review would lose data the
//! moment trusty-okf replaced trusty-kb. The ingest boundary must also stop a
//! source from planting a lifecycle or verification key (§2.13.1).
//! What: golden files written by trusty-kb at `c1eed98441`
//! (`fixtures/trusty-kb-c1eed98441/PROVENANCE.md`) are loaded, saved and
//! re-ingested; three inputs plant every protected key; a planted hash equal
//! to the content hash never verifies.
//! Test: `cargo test -p trusty-okf --test build_criteria`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_yaml::{Mapping, Value};
use trusty_okf::build::ingest::{
    INGEST_ACTOR, IngestReport, SourceItem, concept_id_for, ingest_items,
};
use trusty_okf::build::ledger::{ItemStatus, Ledger};
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceRegistry, SourceSpec};
use trusty_okf::build::trust::TrustLabel;
use trusty_okf::bundle::{Bundle, VerificationState};
use trusty_okf::format::concept::Entity;
use trusty_okf::format::fields::parse_instant;
use trusty_okf::format::{ConceptId, content_hash};
use trusty_okf::profile::Profile;

const GOLDEN: &str = "tests/fixtures/trusty-kb-c1eed98441/kb";

fn golden(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN).join(rel)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A temp copy of the golden tree, canonicalised.
fn golden_tree() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("kb");
    copy_tree(&golden(""), &root);
    (tmp, root)
}

fn now(s: &str) -> chrono::DateTime<chrono::FixedOffset> {
    parse_instant(s).unwrap()
}

fn frontmatter(root: &Path, id: &ConceptId) -> Mapping {
    let text = std::fs::read_to_string(root.join(id.file_name())).unwrap();
    match Entity::from_content(&text).unwrap().frontmatter {
        Value::Mapping(m) => m,
        other => panic!("not a mapping: {other:?}"),
    }
}

fn ingest(root: &Path, spec: &SourceSpec, items: &[SourceItem], at: &str) -> IngestReport {
    let r = ingest_items(
        root,
        &Profile::default_profile(),
        spec,
        items,
        false,
        now(at),
    )
    .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    r
}

/// Why: criterion 13 and §2.9 item 10 — a `registry.toml` written by
/// trusty-kb with doc-store, `gmail` and `drive` rows loads without losing a
/// row, and trusty-okf has no connector variant to load them into (B6).
/// What: every golden row loads with its kind, flags and connector keys,
/// `in:sent` included (DOC-63 S-3.3 stays the adapter's query).
/// Test: self-contained.
#[test]
fn golden_registry_loads_every_row() {
    let reg = SourceRegistry::load(&golden("")).unwrap();
    let rows: Vec<(&str, &str)> = reg
        .sources
        .iter()
        .map(|s| (s.id.as_str(), s.kind()))
        .collect();
    assert_eq!(
        rows,
        [
            ("field-notes", "docstore"),
            ("journal", "docstore"),
            ("sent-mail", "gmail"),
            ("board-mail", "gmail"),
            ("team-drive", "drive"),
            ("my-drive", "drive"),
        ]
    );
    let sent = reg.get("sent-mail").unwrap();
    let Locator::Connector { params, .. } = &sent.locator else {
        panic!("gmail is a connector row: {sent:?}");
    };
    let keys: Vec<(&str, &toml::Value)> = params.iter().collect();
    assert_eq!(
        keys,
        [
            ("query", &toml::Value::String("in:sent".into())),
            ("after", &toml::Value::String("2025/01/01".into())),
        ]
    );
    assert!(reg.get("journal").unwrap().user_authored);
    assert!(reg.get("field-notes").unwrap().tombstone_deleted);
    assert_eq!(
        reg.get("my-drive").unwrap().locator,
        Locator::Connector {
            kind: "drive".into(),
            params: {
                let mut p = ConnectorParams::new();
                p.insert("folder_id", toml::Value::String("root".into()));
                p.insert("recursive", toml::Value::Boolean(false));
                p
            },
        }
    );
}

/// Why: §2.9 "registry files equal or only appended" — loading and saving
/// an unchanged registry must write nothing, and a new row must append.
/// What: load and save the golden file (no write, equal bytes), then append
/// a row (the old text is a prefix of the new).
/// Test: self-contained.
#[test]
fn golden_registry_saves_byte_for_byte() {
    let (_t, root) = golden_tree();
    let path = SourceRegistry::path(&root).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let mut reg = SourceRegistry::load(&root).unwrap();
    assert!(
        !reg.save(&root).unwrap(),
        "an unchanged registry writes nothing"
    );
    let mut tmp_root = root.clone();
    tmp_root.set_file_name("rewritten");
    reg.save(&tmp_root).unwrap();
    assert_eq!(
        std::fs::read_to_string(SourceRegistry::path(&tmp_root).unwrap()).unwrap(),
        before,
        "a fresh render of the loaded rows equals trusty-kb's bytes"
    );
    reg.upsert(SourceSpec::new(
        "late",
        None,
        Locator::DocStore {
            path: "/srv/late".into(),
            extensions: vec![],
            recursive: true,
        },
        "2026-11-04T00:00:00Z",
    ));
    assert!(reg.save(&root).unwrap());
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(&before), "append only:\n{after}");
}

/// Position of each key in one JSON line; the keys must appear in order.
fn key_order(line: &str, keys: &[&str]) -> Vec<usize> {
    keys.iter()
        .filter_map(|k| line.find(&format!("\"{k}\":")))
        .collect()
}

/// Why: §2.7.2 "unchanged on-disk format" — an existing ledger must read,
/// and a new line must have trusty-kb's shape so trusty-kb (until S11) and
/// any reader of the old format still parse it.
/// What: loads the golden ledgers (last record wins, tombstone kept),
/// re-serialises every golden line to the same bytes, then ingests one item
/// over the golden tree and checks the file only grew by a line of the same
/// key order.
/// Test: self-contained.
#[test]
fn golden_ledger_reads_and_appends_in_the_same_format() {
    let (_t, root) = golden_tree();
    let ledger = Ledger::load(&root, "field-notes").unwrap();
    assert!(ledger.is_current("alpha.md", "sha256:aaab"));
    assert_eq!(
        ledger.get("beta.md").unwrap().status,
        ItemStatus::Tombstoned
    );
    let wm = ledger.watermark();
    assert_eq!((wm.items, wm.tombstoned, wm.malformed_lines), (1, 1, 0));
    assert_eq!(wm.last_ingest_at.as_deref(), Some("2026-07-26T12:00:00Z"));

    let path = Ledger::path_for(&root, "sent-mail").unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    for line in before.lines() {
        let rec: trusty_okf::build::ledger::LedgerRecord = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_string(&rec).unwrap(), line, "same bytes");
    }

    let spec = SourceRegistry::load(&root)
        .unwrap()
        .get("sent-mail")
        .cloned()
        .unwrap();
    let item = SourceItem {
        item_id: "18c0ffef".into(),
        fingerprint: "18c0ffef".into(),
        name: "2026-02-02 next".into(),
        title: "next".into(),
        timestamp: Some("2026-02-02T09:00:00Z".into()),
        body: "Second sent message.".into(),
        ..SourceItem::default()
    };
    ingest(&root, &spec, &[item], "2026-11-04T10:00:00Z");
    let after = std::fs::read_to_string(&path).unwrap();
    let added = after.strip_prefix(&before).expect("append only");
    assert_eq!(added.lines().count(), 1);
    let keys = [
        "item_id",
        "fingerprint",
        "entity",
        "ingested_at",
        "timestamp",
        "status",
    ];
    let order = key_order(added, &keys);
    assert_eq!(order.len(), keys.len(), "{added}");
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{added}");
    let golden_order = key_order(before.lines().next().unwrap(), &keys);
    assert_eq!(
        golden_order.len(),
        keys.len(),
        "the golden line has every key"
    );
    assert!(
        golden_order.windows(2).all(|w| w[0] < w[1]),
        "same key order"
    );
    assert!(
        added.contains("\"ingested_at\":\"2026-11-04T10:00:00Z\""),
        "{added}"
    );
    assert!(added.contains("\"status\":\"ingested\""), "{added}");
}

/// Why: §2.9 — a re-ingest over a concept trusty-kb wrote must keep the
/// editorial and verification keys a person added, and still replace the
/// source-derived keys.
/// What: adds `status`, `superseded_by`, `stale_after` and a `verified`
/// event to the golden `notes/alpha.md`, re-ingests the same content with a
/// new fingerprint (all kept), then new content (`verified` moves to the
/// record, editorial keys kept).
/// Test: self-contained.
#[test]
fn reingest_over_golden_tree_keeps_editorial_and_verification_keys() {
    let (_t, root) = golden_tree();
    let id = ConceptId::parse("notes/alpha").unwrap();
    let file = root.join(id.file_name());
    let text = std::fs::read_to_string(&file).unwrap();
    let edited = text.replacen(
        "---\n",
        "---\nstatus: draft\nsuperseded_by: /notes/gamma.md\nstale_after: 2027-01-01T00:00:00Z\n\
         verified:\n- by: human:jo\n  at: 2026-07-27T00:00:00Z\n",
        1,
    );
    std::fs::write(&file, edited).unwrap();
    let spec = SourceRegistry::load(&root)
        .unwrap()
        .get("field-notes")
        .cloned()
        .unwrap();
    let mut item = SourceItem {
        item_id: "alpha.md".into(),
        fingerprint: "sha256:aaac".into(),
        name: "alpha".into(),
        title: "alpha".into(),
        timestamp: Some("2026-07-03T08:00:00Z".into()),
        body: "alpha body, revised".into(),
        ..SourceItem::default()
    };
    item.fields.insert("source_path".into(), "alpha.md".into());
    let r = ingest(
        &root,
        &spec,
        std::slice::from_ref(&item),
        "2026-11-04T10:00:00Z",
    );
    assert_eq!(r.updated, 1);
    let fm = frontmatter(&root, &id);
    for key in [
        "status",
        "superseded_by",
        "stale_after",
        "verified",
        "created",
    ] {
        assert!(fm.contains_key(key), "{key} kept: {fm:?}");
    }
    assert_eq!(
        fm["updated"], "2026-07-25T12:00:00Z",
        "unchanged content keeps it"
    );

    item.fingerprint = "sha256:aaad".into();
    item.body = "alpha body, third".into();
    ingest(&root, &spec, &[item], "2026-11-04T11:00:00Z");
    let fm = frontmatter(&root, &id);
    for key in ["status", "superseded_by", "stale_after", "created"] {
        assert!(fm.contains_key(key), "{key} kept: {fm:?}");
    }
    assert!(!fm.contains_key("verified"), "§2.10 clears it: {fm:?}");
    assert!(!fm.contains_key("updated"), "replaced by generated: {fm:?}");
    assert_eq!(fm["generated"]["by"], INGEST_ACTOR);
    let (bundle, _) = Bundle::open(&root).unwrap();
    let history = bundle.verification_events(&id).unwrap();
    assert_eq!(history.len(), 1, "the review is history, not lost");
}

/// The protected keys criterion 116 names, with the value each input plants.
fn planted() -> Vec<(&'static str, Value)> {
    let review: Value =
        serde_yaml::from_str("[{by: 'human:mallory', at: '2026-11-01T00:00:00Z'}]").unwrap();
    vec![
        ("verified", review),
        ("status", "stable".into()),
        ("supersedes", "/notes/x.md".into()),
        ("superseded_by", "/notes/y.md".into()),
        (
            "generated",
            serde_yaml::from_str("{by: 'human:mallory'}").unwrap(),
        ),
        ("stale_after", "2099-01-01T00:00:00Z".into()),
        ("source_status", "deleted".into()),
        ("tombstoned", "2026-11-01T00:00:00Z".into()),
        ("trust", "user-authored".into()),
        ("source_timestamp", "1999-01-01T00:00:00Z".into()),
        ("verification_hash", "sha256:planted".into()),
    ]
}

fn mail_spec() -> SourceSpec {
    let mut p = ConnectorParams::new();
    p.insert("query", toml::Value::String("in:sent".into()));
    SourceSpec::new(
        "mail",
        Some("mail"),
        Locator::Connector {
            kind: "gmail".into(),
            params: p,
        },
        "2026-11-04T00:00:00Z",
    )
}

fn plain(id: &str) -> SourceItem {
    SourceItem {
        item_id: id.into(),
        fingerprint: format!("fp-{id}"),
        name: id.into(),
        title: id.into(),
        timestamp: Some("2026-11-02T00:00:00Z".into()),
        body: format!("body of {id}"),
        ..SourceItem::default()
    }
}

/// trusty-common's `is_tombstone` rule: leading frontmatter with a
/// non-empty `source_id` and `source_status: deleted`. trusty-okf cannot
/// link trusty-common (B8), so trusty-kb's suite also calls the real one.
fn is_tombstone(fm: &Mapping) -> bool {
    fm.get("source_status").and_then(Value::as_str) == Some("deleted")
        && fm
            .get("source_id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
}

/// Why: criterion 116 (§2.13.1) — no value from a source adapter, a worker's
/// `fields` or a source file's frontmatter sets a protected key; each sits
/// unchanged under `source_fields`, and the report says so per key and item.
/// What: three items plant the same eleven keys, one input each: adapter
/// `fields` (strings), worker fields (structured), and the frontmatter of a
/// source `.md` file. Every written concept has the engine's values at the
/// top level, the planted values under `source_fields`, `is_tombstone` false,
/// the registration's trust label and an Unverified state with no history.
/// Test: self-contained.
#[test]
fn c116_protected_keys_from_every_input_are_quarantined() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let spec = mail_spec();
    let keys = planted();

    let mut adapter = plain("adapter");
    for (k, v) in &keys {
        let s = match v {
            Value::String(s) => s.clone(),
            other => serde_yaml::to_string(other).unwrap(),
        };
        adapter.fields.insert(k.to_string(), s);
    }
    let mut worker = plain("worker");
    for (k, v) in &keys {
        worker
            .frontmatter
            .insert(Value::String(k.to_string()), v.clone());
    }
    let mut file = plain("file");
    let mut source_md = String::from("---\n");
    for (k, v) in &keys {
        let mut one = Mapping::new();
        one.insert(Value::String(k.to_string()), v.clone());
        source_md.push_str(&serde_yaml::to_string(&one).unwrap());
    }
    source_md.push_str("---\nbody of file\n");
    let parsed = Entity::from_content(&source_md).unwrap();
    let Value::Mapping(m) = parsed.frontmatter else {
        panic!("source file frontmatter");
    };
    file.frontmatter = m;

    let items = [adapter.clone(), worker.clone(), file.clone()];
    let report = ingest(&root, &spec, &items, "2026-11-04T10:00:00Z");
    assert_eq!(
        report.findings.len(),
        keys.len() * items.len(),
        "{report:?}"
    );
    let (bundle, _) = Bundle::open(&root).unwrap();
    for item in &items {
        let id = concept_id_for("mail", &item.name).unwrap();
        let fm = frontmatter(&root, &id);
        for (k, _) in &keys {
            assert_eq!(
                report
                    .findings
                    .iter()
                    .filter(|f| f.item_id == item.item_id && f.key == *k)
                    .count(),
                1,
                "one quarantined_key finding for {k} on {}",
                item.item_id
            );
            assert!(report.findings.iter().all(|f| f.kind == "quarantined_key"));
        }
        let held = fm["source_fields"].as_mapping().expect("source_fields");
        let inputs: Vec<(Value, Value)> = if item.item_id == "adapter" {
            item.fields
                .iter()
                .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                .collect()
        } else {
            item.frontmatter.clone().into_iter().collect()
        };
        for (k, v) in inputs {
            assert_eq!(
                held.get(&k),
                Some(&v),
                "{k:?} unchanged under source_fields"
            );
        }
        for k in [
            "verified",
            "status",
            "supersedes",
            "superseded_by",
            "stale_after",
            "source_status",
            "tombstoned",
            "verification_hash",
        ] {
            assert!(!fm.contains_key(k), "{k} must not be top level: {fm:?}");
        }
        assert_eq!(fm["generated"]["by"], INGEST_ACTOR);
        assert_eq!(fm["source_timestamp"], "2026-11-02T00:00:00Z");
        assert_eq!(fm["trust"], TrustLabel::for_source(&spec).as_str());
        assert_eq!(fm["trust"], "untrusted-external");
        assert!(!is_tombstone(&fm), "{fm:?}");
        assert_eq!(
            bundle.verification_state(&id).unwrap(),
            VerificationState::Unverified { history: 0 }
        );
    }
}

/// Why: criterion 117 (§2.13.1) — a source that supplies a verification
/// hash equal to the deterministic content hash of the revision must not
/// make it Verified, on the first import or on a re-import.
/// What: imports an item, reads its content hash, re-imports it planting
/// that hash (and a `verified` event) through `fields` and `frontmatter`;
/// then plants the new revision's hash again. After each run the state is
/// Unverified with no history and the record holds no event.
/// Test: self-contained.
#[test]
fn c117_planted_verification_hash_never_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let spec = mail_spec();
    let id = concept_id_for("mail", "m").unwrap();
    let mut item = plain("m");
    ingest(
        &root,
        &spec,
        std::slice::from_ref(&item),
        "2026-11-04T10:00:00Z",
    );

    for (round, at) in ["2026-11-04T11:00:00Z", "2026-11-04T12:00:00Z"]
        .iter()
        .enumerate()
    {
        let (bundle, _) = Bundle::open(&root).unwrap();
        let hash = content_hash(&bundle.get(&id).unwrap());
        item.fingerprint = format!("fp-m-{round}");
        item.fields = BTreeMap::from([
            ("verification_hash".to_string(), hash.as_str().to_string()),
            ("verified_hash".to_string(), hash.as_str().to_string()),
        ]);
        item.frontmatter = serde_yaml::from_str(&format!(
            "verification_record: {{hash: '{hash}', by: 'human:mallory', at: '{at}'}}\n\
             verified: [{{by: 'human:mallory', at: '{at}'}}]\n"
        ))
        .unwrap();
        let r = ingest(&root, &spec, std::slice::from_ref(&item), at);
        assert_eq!(r.updated, 1);
        let (bundle, _) = Bundle::open(&root).unwrap();
        assert_eq!(
            bundle.verification_state(&id).unwrap(),
            VerificationState::Unverified { history: 0 },
            "round {round}"
        );
        assert!(bundle.verification_events(&id).unwrap().is_empty());
        let fm = frontmatter(&root, &id);
        assert!(!fm.contains_key("verified"), "{fm:?}");
        assert_eq!(fm["source_fields"]["verification_hash"], hash.as_str());
    }
}

/// Why: DOC-63 S-4.3/S-4.6 over existing files — every concept trusty-kb
/// wrote reads back with its label, and a label a person edits to a value
/// this build does not know reads as untrusted.
/// What: reads the golden concepts' labels, then corrupts one.
/// Test: self-contained.
#[test]
fn golden_trust_labels_read_back_and_fail_closed() {
    let (_t, root) = golden_tree();
    for rel in [
        "notes/alpha.md",
        "notes/beta.md",
        "mail/2026-02-01-re-update.md",
    ] {
        assert_eq!(
            TrustLabel::of_entity_file(&root.join(rel)),
            TrustLabel::UntrustedExternal,
            "{rel}"
        );
    }
    let file = root.join("notes/alpha.md");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("untrusted-external", "trusted")).unwrap();
    assert_eq!(
        TrustLabel::of_entity_file(&file),
        TrustLabel::UntrustedExternal
    );
    std::fs::write(&file, text.replace("trust: untrusted-external\n", "")).unwrap();
    assert_eq!(
        TrustLabel::of_entity_file(&file),
        TrustLabel::UntrustedExternal
    );
}
