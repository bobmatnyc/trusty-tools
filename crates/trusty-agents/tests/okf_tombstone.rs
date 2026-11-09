//! SPEC-OKF-01 criterion 37 (#10746 S6): a tombstone trusty-okf renders is
//! detected by trusty-common's `is_tombstone`, the predicate trusty-search
//! uses to withdraw it. It lives in trusty-agents because this crate
//! depends on both; trusty-okf's core may not link trusty-common (B8).
//! #10746 S11 moved it here from the deleted trusty-kb, unchanged.

use std::collections::BTreeSet;

use chrono::DateTime;
use trusty_common::knowledge_document::is_tombstone;
use trusty_okf::build::ingest::{SourceItem, concept_id_for, ingest_items, sweep_deleted};
use trusty_okf::build::registry::{Locator, SourceSpec};
use trusty_okf::profile::Profile;

/// Why: criterion 37 — a rendered tombstone passes
/// `trusty_common::knowledge_document::is_tombstone`, and the live concept
/// before it does not.
/// What: ingest one doc-store item, sweep it as deleted, read the file.
/// Test: self-contained.
#[test]
fn c37_rendered_tombstone_passes_is_tombstone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut spec = SourceSpec::new(
        "docs",
        Some("notes"),
        Locator::DocStore {
            path: "/srv/docs".into(),
            extensions: vec!["md".into()],
            recursive: true,
        },
        "2026-11-04T00:00:00Z",
    );
    spec.tombstone_deleted = true;
    let item = SourceItem {
        item_id: "a.md".into(),
        fingerprint: "fp-a".into(),
        name: "a".into(),
        title: "a".into(),
        body: "Alpha.".into(),
        ..SourceItem::default()
    };
    let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap();
    ingest_items(
        &root,
        &Profile::default_profile(),
        &spec,
        &[item],
        false,
        t("2026-11-04T10:00:00Z"),
    )
    .unwrap();
    let path = root.join(concept_id_for("notes", "a").unwrap().file_name());
    assert!(!is_tombstone(&std::fs::read_to_string(&path).unwrap()));
    let r = sweep_deleted(&root, &spec, &BTreeSet::new(), t("2026-11-04T11:00:00Z")).unwrap();
    assert_eq!(r.tombstoned, 1);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(is_tombstone(&text), "{text}");
}
