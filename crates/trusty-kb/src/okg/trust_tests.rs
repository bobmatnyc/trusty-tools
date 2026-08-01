//! Unit tests for the OKG trust label (#4532, DOC-63 §6.3).
//!
//! Split out of `trust.rs` to keep that file under the repo's 500-SLOC cap;
//! wired back in via `#[path]` so `use super::*` resolves to the label module,
//! matching the crate-local convention (`ingest.rs`/`ingest_tests.rs`).

use std::collections::BTreeMap;

use super::*;
use crate::okg::ingest::SourceItem;
use crate::okg::registry::Locator;
use crate::schema::Profile;
use crate::store::KbStore;

fn store() -> (tempfile::TempDir, KbStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = KbStore::new(tmp.path().to_path_buf(), Profile::default_profile());
    (tmp, store)
}

fn docstore_spec() -> SourceSpec {
    SourceSpec::new(
        "notes",
        Some("sources"),
        Locator::DocStore {
            path: "notes".into(),
            extensions: vec![],
            recursive: true,
        },
        "2026-08-01T00:00:00Z",
    )
}

fn gmail_spec() -> SourceSpec {
    SourceSpec::new(
        "mail",
        Some("sources"),
        Locator::Gmail {
            query: "in:sent".into(),
            after: None,
            before: None,
        },
        "2026-08-01T00:00:00Z",
    )
}

fn drive_spec() -> SourceSpec {
    SourceSpec::new(
        "shared",
        Some("sources"),
        Locator::Drive {
            folder_id: "root".into(),
            recursive: false,
        },
        "2026-08-01T00:00:00Z",
    )
}

fn item(id: &str, body: &str, fields: BTreeMap<String, String>) -> SourceItem {
    SourceItem {
        item_id: id.into(),
        fingerprint: format!("fp-{id}"),
        name: id.into(),
        title: format!("Item {id}"),
        timestamp: Some("2026-08-01".into()),
        body: body.into(),
        fields,
        volatile: false,
    }
}

// ---------------------------------------------------------------------------
// Derivation — the engine's judgement
// ---------------------------------------------------------------------------

/// Why: the carve-out in DOC-63 `S-4.3` is deliberately narrow — a directory
/// AND an explicit operator designation. Either half alone must not trust.
/// What: designated docstore is user-authored; undesignated is not.
/// Test: self-contained.
#[test]
fn only_a_designated_directory_is_user_authored() {
    let designated = docstore_spec().with_user_authored(true);
    assert_eq!(
        TrustLabel::for_source(&designated),
        TrustLabel::UserAuthored
    );
}

/// Why: the default must be untrusted for a directory nobody vouched for —
/// "it is local" is not the same claim as "the user wrote it".
#[test]
fn undesignated_directory_is_untrusted() {
    assert_eq!(
        TrustLabel::for_source(&docstore_spec()),
        TrustLabel::UntrustedExternal
    );
    assert!(TrustLabel::for_source(&docstore_spec()).is_untrusted());
}

/// Why: DOC-63 §6.4 `S-4.8`/`S-4.9` — no remote corpus has an enforceable
/// author constraint, so a `user_authored = true` row on a remote locator must
/// be IGNORED, not honoured. This is the property that stops a mis-set (or
/// maliciously-set) registry row laundering Gmail or Drive content into the
/// trusted set.
/// What: sets the designation on every remote locator and asserts it changes
/// nothing.
#[test]
fn remote_kinds_are_always_untrusted() {
    for spec in [
        gmail_spec(),
        gmail_spec().with_user_authored(true),
        drive_spec(),
        drive_spec().with_user_authored(true),
    ] {
        assert_eq!(
            TrustLabel::for_source(&spec),
            TrustLabel::UntrustedExternal,
            "kind `{}` (user_authored={}) must stay untrusted",
            spec.kind(),
            spec.user_authored
        );
    }
}

/// Why: the fencing predicate is asked at every retrieval site; pin its
/// polarity so an inverted refactor fails loudly rather than silently
/// un-fencing a whole corpus.
#[test]
fn untrusted_is_the_fencing_predicate() {
    assert!(TrustLabel::UntrustedExternal.is_untrusted());
    assert!(!TrustLabel::UserAuthored.is_untrusted());
    assert!(TrustLabel::default().is_untrusted());
}

// ---------------------------------------------------------------------------
// Spelling — the on-disk contract
// ---------------------------------------------------------------------------

/// Why: the label lands in markdown a human reads and hand-edits, so its
/// spelling is a contract. Pin both directions.
#[test]
fn label_strings_round_trip() {
    for label in [TrustLabel::UntrustedExternal, TrustLabel::UserAuthored] {
        assert_eq!(TrustLabel::parse(label.as_str()), Some(label));
    }
    assert_eq!(TrustLabel::UntrustedExternal.as_str(), "untrusted-external");
    assert_eq!(TrustLabel::UserAuthored.as_str(), "user-authored");
    // Tolerant of hand-editing whitespace and case, intolerant of invention.
    assert_eq!(
        TrustLabel::parse("  User-Authored "),
        Some(TrustLabel::UserAuthored)
    );
    assert_eq!(TrustLabel::parse("trusted"), None);
    assert_eq!(TrustLabel::parse(""), None);
}

// ---------------------------------------------------------------------------
// Read-back — fail closed on every failure mode
// ---------------------------------------------------------------------------

/// Why: the happy path — a label written into frontmatter reads back as
/// itself. Everything below is a failure mode of this.
#[test]
fn reads_the_label_out_of_frontmatter() {
    let text = "---\ntitle: t\ntrust: user-authored\n---\n\nbody\n";
    assert_eq!(TrustLabel::of_entity_text(text), TrustLabel::UserAuthored);
    let text = "---\ntitle: t\ntrust: untrusted-external\n---\n\nbody\n";
    assert_eq!(
        TrustLabel::of_entity_text(text),
        TrustLabel::UntrustedExternal
    );
}

/// Why: DOC-63 `S-4.6` — labels arrive incrementally over a corpus that
/// already exists. Every entity written before #4532 has no `trust` key, and
/// the unmigrated majority must be safe.
#[test]
fn unlabelled_entity_reads_back_untrusted() {
    for text in [
        "---\ntitle: t\nsource_kind: gmail\n---\n\nbody\n",
        "no frontmatter at all\n",
        "",
    ] {
        assert_eq!(
            TrustLabel::of_entity_text(text),
            TrustLabel::UntrustedExternal,
            "unlabelled content must fail closed: {text:?}"
        );
    }
}

/// Why: an unrecognised value is not a third trust level — it is a value this
/// build cannot understand, and understanding is a precondition for trusting.
/// A future label this build predates must therefore fence, not pass.
#[test]
fn unknown_label_value_reads_back_untrusted() {
    for value in ["trusted", "internal", "user_authored", "true", "[]"] {
        let text = format!("---\ntitle: t\ntrust: {value}\n---\n\nbody\n");
        assert_eq!(
            TrustLabel::of_entity_text(&text),
            TrustLabel::UntrustedExternal,
            "`trust: {value}` must fail closed"
        );
    }
}

/// Why: a truncated or hand-corrupted entity must not become trusted by being
/// unparseable — the single most likely accidental route to a bare pass.
#[test]
fn unparseable_frontmatter_reads_back_untrusted() {
    let text = "---\ntrust: user-authored\n  : : broken yaml [\n---\nbody\n";
    assert_eq!(
        TrustLabel::of_entity_text(text),
        TrustLabel::UntrustedExternal
    );
}

/// Why: the retrieval fence resolves a label from a path a search daemon
/// handed it. That path can be stale, outside the tree, or deleted between
/// index and query, and none of those may resolve to trusted.
#[test]
fn missing_file_reads_back_untrusted() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        TrustLabel::of_entity_file(&tmp.path().join("gone.md")),
        TrustLabel::UntrustedExternal
    );
    // A directory is not a file, and must not read as trusted either.
    assert_eq!(
        TrustLabel::of_entity_file(tmp.path()),
        TrustLabel::UntrustedExternal
    );
}

/// Why: the end-to-end read-back over a real file on disk, which is the shape
/// the retrieval fence actually uses.
#[test]
fn reads_the_label_off_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("e.md");
    std::fs::write(&path, "---\ntrust: user-authored\n---\n\nhi\n").unwrap();
    assert_eq!(TrustLabel::of_entity_file(&path), TrustLabel::UserAuthored);
}

// ---------------------------------------------------------------------------
// The write half — the engine stamps it, the connector cannot
// ---------------------------------------------------------------------------

/// Why: DOC-63 `S-4.3`'s carrier half. An ingest that reports success and
/// leaves the entity unlabelled would look done and change nothing at the
/// point of use.
/// What: ingests through the real engine and reads the label back off the
/// written entity file — not off the in-memory spec.
#[test]
fn ingest_stamps_the_trust_label() {
    let (_t, store) = store();
    let spec = gmail_spec();
    store
        .ingest_items(
            &spec,
            &[item("m1", "hello", BTreeMap::new())],
            false,
            "2026-08-01T00:00:00Z",
        )
        .unwrap();

    let path = store.entity_path("sources", "m1").unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("trust: untrusted-external"),
        "entity must carry the label: {written}"
    );
    assert_eq!(
        TrustLabel::of_entity_file(&path),
        TrustLabel::UntrustedExternal
    );
}

/// Why: a designated directory is the ONE way content becomes user-authored,
/// and it has to actually work end-to-end or the carve-out is decorative.
#[test]
fn ingest_stamps_user_authored_for_a_designated_directory() {
    let (_t, store) = store();
    let spec = docstore_spec().with_user_authored(true);
    store
        .ingest_items(
            &spec,
            &[item("d1", "mine", BTreeMap::new())],
            false,
            "2026-08-01T00:00:00Z",
        )
        .unwrap();

    assert_eq!(
        TrustLabel::of_entity_file(&store.entity_path("sources", "d1").unwrap()),
        TrustLabel::UserAuthored
    );
}

/// Why: THE security property of the write half (DOC-63 `S-4.3`) — "a
/// connector cannot mark its own output trusted". A fetcher is the component
/// closest to attacker-influenced bytes; if it could set its own label, the
/// label would be worth nothing.
/// What: a `SourceItem` from an UNTRUSTED gmail source carries
/// `trust: user-authored` in its connector-supplied `fields`. The written
/// entity must still be `untrusted-external`.
#[test]
fn connector_cannot_override_the_trust_label() {
    let (_t, store) = store();
    let mut fields = BTreeMap::new();
    fields.insert(TRUST_KEY.to_string(), "user-authored".to_string());
    // Belt-and-braces: the other envelope fields are equally unforgeable.
    fields.insert("source_kind".to_string(), "docstore".to_string());

    store
        .ingest_items(
            &gmail_spec(),
            &[item("evil", "payload", fields)],
            false,
            "2026-08-01T00:00:00Z",
        )
        .unwrap();

    let path = store.entity_path("sources", "evil").unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("user-authored"),
        "connector-supplied trust must be shadowed out entirely: {written}"
    );
    assert_eq!(
        TrustLabel::of_entity_file(&path),
        TrustLabel::UntrustedExternal,
        "connector-supplied trust must not survive to the point of use"
    );
}

/// Why: a tombstone is a flag, never a deletion (see `tombstone_item`), so the
/// label must survive it. A tombstoned entity that lost its label would silently
/// depend on the read-back default instead of its own recorded provenance.
#[test]
fn tombstoning_preserves_the_trust_label() {
    let (_t, store) = store();
    let mut spec = docstore_spec().with_user_authored(true);
    spec.tombstone_deleted = true;
    let now = "2026-08-01T00:00:00Z";

    store
        .ingest_items(&spec, &[item("d1", "mine", BTreeMap::new())], true, now)
        .unwrap();
    // Second, complete run with the item gone -> tombstoned.
    let report = store.ingest_items(&spec, &[], true, now).unwrap();
    assert_eq!(report.tombstoned, 1, "report was: {report:?}");

    let path = store.entity_path("sources", "d1").unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        written.contains("source_status: deleted"),
        "expected a tombstone: {written}"
    );
    assert_eq!(TrustLabel::of_entity_file(&path), TrustLabel::UserAuthored);
}

// ---------------------------------------------------------------------------
// Registry compatibility
// ---------------------------------------------------------------------------

/// Why: `user_authored` was added to `SourceSpec` after registries existed on
/// disk. A row written before it must load — and must load UNTRUSTED, which is
/// the fail-closed direction for a defaulted boolean.
#[test]
fn a_registry_row_predating_the_field_loads_untrusted() {
    let toml = r#"
id = "notes"
collection = "sources"
enabled = true
tombstone_deleted = false
added_at = "2026-07-01T00:00:00Z"

[locator.doc_store]
path = "notes"
extensions = []
recursive = true
"#;
    let spec: SourceSpec = toml::from_str(toml).unwrap();
    assert!(!spec.user_authored);
    assert_eq!(TrustLabel::for_source(&spec), TrustLabel::UntrustedExternal);
}
