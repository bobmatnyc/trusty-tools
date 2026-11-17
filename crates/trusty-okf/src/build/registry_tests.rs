//! Unit tests for the source registry, split out of `registry.rs` to keep it
//! under the 500-SLOC cap (#10746). The golden trusty-kb registry is pinned in
//! `tests/build_criteria.rs`.

use super::*;

fn connector(kind: &str, pairs: &[(&str, &str)]) -> Locator {
    let mut params = ConnectorParams::new();
    for (k, v) in pairs {
        params.insert(k, toml::Value::String(v.to_string()));
    }
    Locator::Connector {
        kind: kind.to_string(),
        params,
    }
}

fn mail(query: &str, after: Option<&str>) -> Locator {
    match after {
        Some(a) => connector("gmail", &[("query", query), ("after", a)]),
        None => connector("gmail", &[("query", query)]),
    }
}

/// Why: `id` names a ledger file, so an unslugged traversal string must not
/// survive registration.
/// What: asserts a hostile id is reduced to a single safe segment and an
/// empty collection falls back to the default.
/// Test: self-contained.
#[test]
fn source_id_is_slugged() {
    let spec = SourceSpec::new(
        "../../Etc Passwd",
        None,
        mail("in:sent", None),
        "2026-01-01",
    );
    assert_eq!(spec.id, "etc-passwd");
    assert_eq!(spec.collection, DEFAULT_COLLECTION);
    assert_eq!(spec.kind(), "gmail");
}

/// Why: registering a NEW source must never rebuild or disturb the rows
/// already present — that is the whole additivity promise.
/// What: registers two sources, snapshots the file, registers a third, and
/// asserts the first two rows are unchanged and still first.
/// Test: self-contained.
#[test]
fn upsert_appends_without_disturbing_siblings() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let mut reg = SourceRegistry::default();
    reg.upsert(SourceSpec::new(
        "alpha",
        Some("mail"),
        mail("in:sent", None),
        "t0",
    ));
    reg.upsert(SourceSpec::new(
        "beta",
        None,
        connector("drive", &[("folder_id", "F1")]),
        "t0",
    ));
    assert!(reg.save(root).unwrap());
    let before = std::fs::read_to_string(SourceRegistry::path(root).unwrap()).unwrap();
    let first_two = reg.sources.clone();

    let (created, changed) = reg.upsert(SourceSpec::new(
        "gamma",
        None,
        Locator::DocStore {
            path: "/docs".into(),
            extensions: vec![],
            recursive: true,
        },
        "t1",
    ));
    assert!(created && changed);
    assert_eq!(reg.sources[..2], first_two[..], "existing rows untouched");
    assert_eq!(reg.sources.len(), 3);
    assert!(reg.save(root).unwrap());

    let after = std::fs::read_to_string(SourceRegistry::path(root).unwrap()).unwrap();
    assert!(
        after.starts_with(before.trim_end()),
        "append must extend the file, not rewrite it:\nbefore={before}\nafter={after}"
    );

    // Saving again with no change must not rewrite the file.
    assert!(!reg.save(root).unwrap(), "no-op save must be a no-op");
}

/// Why: widening a window re-registers the SAME id; it must update that row
/// in place, keep its position and `added_at`, and not duplicate the source.
/// What: registers `mail`, then re-registers it with an earlier `after`.
/// Test: self-contained.
#[test]
fn upsert_updates_locator_in_place() {
    let mut reg = SourceRegistry::default();
    reg.upsert(SourceSpec::new(
        "mail",
        None,
        mail("in:sent", Some("2026/06/01")),
        "t0",
    ));
    reg.upsert(SourceSpec::new("other", None, mail("in:inbox", None), "t0"));

    let (created, changed) = reg.upsert(SourceSpec::new(
        "mail",
        None,
        mail("in:sent", Some("2024/01/01")),
        "t9",
    ));
    assert!(!created, "same id must not append a second row");
    assert!(changed, "a widened window is a real change");
    assert_eq!(reg.sources.len(), 2);
    assert_eq!(reg.sources[0].id, "mail", "position preserved");
    assert_eq!(reg.sources[0].added_at, "t0", "added_at never rewritten");
    assert_eq!(
        reg.sources[0].locator,
        mail("in:sent", Some("2024/01/01")),
        "locator widened"
    );

    // Re-registering the identical spec is a no-op.
    let (created, changed) = reg.upsert(SourceSpec::new(
        "mail",
        None,
        mail("in:sent", Some("2024/01/01")),
        "t9",
    ));
    assert!(!created && !changed);
}

/// Why: the registry is the durable contract between runs; it must survive a
/// full write/read cycle with the doc store and connector locators intact.
/// What: saves a doc store and two connectors and reloads, asserting
/// equality; a connector named like the doc store is refused on save.
/// Test: self-contained.
#[test]
fn roundtrip_through_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    assert_eq!(
        SourceRegistry::load(root).unwrap(),
        SourceRegistry::default(),
        "absent registry loads empty"
    );

    let mut reg = SourceRegistry::default();
    reg.upsert(SourceSpec::new(
        "docs",
        Some("notes"),
        Locator::DocStore {
            path: "/tmp/docs".into(),
            extensions: vec!["md".into()],
            recursive: true,
        },
        "t0",
    ));
    reg.upsert(SourceSpec::new(
        "mail",
        None,
        mail("in:sent", Some("2026/01/01")),
        "t0",
    ));
    let mut drive = ConnectorParams::new();
    drive.insert("folder_id", toml::Value::String("abc".into()));
    drive.insert("recursive", toml::Value::Boolean(true));
    reg.upsert(SourceSpec::new(
        "drive",
        None,
        Locator::Connector {
            kind: "drive".into(),
            params: drive,
        },
        "t0",
    ));
    reg.save(root).unwrap();

    let loaded = SourceRegistry::load(root).unwrap();
    assert_eq!(loaded, reg);
    assert_eq!(loaded.get("Docs").map(|s| s.kind()), Some("docstore"));
    assert_eq!(loaded.get("drive").map(|s| s.kind()), Some("drive"));

    let mut bad = SourceRegistry::default();
    bad.upsert(SourceSpec::new(
        "fake",
        None,
        connector(DOC_STORE_TAG, &[("path", "/x")]),
        "t0",
    ));
    assert!(
        bad.save(root).is_err(),
        "a connector named doc_store would reload as a doc store"
    );
}
