//! Shared fixtures for the slice S6 criteria tests (#10746): instants,
//! source specs and items, the golden trusty-kb tree with hand-owned
//! additions, tree snapshots and index-journal acknowledgements.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use trusty_okf::build::ingest::{IngestReport, SourceItem, ingest_items};
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceSpec};
use trusty_okf::bundle::Bundle;
use trusty_okf::format::{Actor, ConceptId};
use trusty_okf::profile::Profile;
use trusty_okf::state::{IndexIdentity, IndexJournal, IndexRecord, IndexStateDir, LiveIdentity};

pub const INDEX: &str = "kb-s6";

pub fn instant(s: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(s).unwrap()
}

pub fn id(s: &str) -> ConceptId {
    ConceptId::parse(s).unwrap()
}

pub fn process() -> Actor {
    Actor::Process("test".into())
}

pub fn live() -> LiveIdentity {
    LiveIdentity::Known(identity())
}

pub fn identity() -> IndexIdentity {
    IndexIdentity {
        instance_id: "i1".into(),
        generation: 1,
    }
}

/// A doc-store source writing into `collection`.
pub fn docstore(id: &str, collection: &str) -> SourceSpec {
    SourceSpec::new(
        id,
        Some(collection),
        Locator::DocStore {
            path: "/srv/s6".into(),
            extensions: vec!["md".into()],
            recursive: true,
        },
        "2026-11-04T00:00:00Z",
    )
}

/// A connector source (never tombstones).
pub fn connector(id: &str, collection: &str) -> SourceSpec {
    SourceSpec::new(
        id,
        Some(collection),
        Locator::Connector {
            kind: "gmail".into(),
            params: ConnectorParams::new(),
        },
        "2026-11-04T00:00:00Z",
    )
}

/// An item whose fingerprint follows its body.
pub fn item(name: &str, body: &str) -> SourceItem {
    SourceItem {
        item_id: format!("{name}.md"),
        fingerprint: format!("fp-{name}-{body}"),
        name: name.into(),
        title: name.into(),
        body: body.into(),
        ..SourceItem::default()
    }
}

pub fn ingest(root: &Path, spec: &SourceSpec, items: &[SourceItem], at: &str) -> IngestReport {
    let r = ingest_items(
        root,
        &Profile::default_profile(),
        spec,
        items,
        false,
        instant(at),
    )
    .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    r
}

/// A fresh, canonical bundle root inside a temp dir.
pub fn fresh() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("kb");
    std::fs::create_dir_all(&root).unwrap();
    (tmp, root)
}

/// Every file under `root` except `_sources/ops/`, `_sources/migrations/`
/// and the lock, with its bytes (the §4.11 comparison scope).
pub fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
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

/// Only the `.md` files of [`tree`].
pub fn md_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    tree(root)
        .into_iter()
        .filter(|(p, _)| p.ends_with(".md"))
        .collect()
}

pub fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
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

/// The golden trusty-kb tree (`c1eed98441`): ledger-owned `notes/alpha`
/// and `mail/2026-02-01-re-update`, tombstoned `notes/beta`.
pub fn golden() -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = fresh();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trusty-kb-c1eed98441/kb");
    copy_dir(&src, &root);
    (tmp, root)
}

/// A hand-owned v0.1 concept: offset `updated`, plain-string `sources`, a
/// bare `verified` mapping, a wiki-link field and an unknown flow key.
pub const HAND: &str = "---\ntype: Note\ntitle: Hand\nupdated: 2026-07-20T10:00:00Z\ncreated: 2026-07-01T10:00:00Z\nsources:\n- https://example.com/a\n- https://example.com/b\nverified:\n  by: human:jo\n  at: 2026-07-21T10:00:00Z\nrelated: '[[alpha]]'\nx_flow: {a: 1, b: [2, 3]} # kept\n---\n\nA hand-written note.\n";

/// A hand-owned concept whose `updated` has no offset (stays, warning).
pub const DATE_ONLY: &str = "---\ntype: Note\nupdated: 2026-07-20\n---\nDate only.\n";

/// The golden tree plus hand-owned concepts, a hand-written `index.md`, a
/// `log.md` and a `_state/conversion-log.md`.
pub fn migration_tree() -> (tempfile::TempDir, PathBuf) {
    let (tmp, root) = golden();
    std::fs::write(root.join("notes/hand.md"), HAND).unwrap();
    std::fs::write(root.join("notes/date-only.md"), DATE_ONLY).unwrap();
    std::fs::write(root.join("index.md"), "# Hand-written index\n").unwrap();
    std::fs::write(
        root.join("log.md"),
        "# Log\n\n## 2026-07-26\n\n- **Converted** the tree\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("_state")).unwrap();
    std::fs::write(root.join("_state/conversion-log.md"), "converted 3 files\n").unwrap();
    (tmp, root)
}

/// Record a confirmed put and a passed probe for every concept's current
/// bytes under [`identity`], so each reads Searchable.
pub fn ack_all(root: &Path, state: &IndexStateDir) {
    let b = Bundle::open(root).unwrap().0;
    let j = IndexJournal::open(state, b.root(), INDEX).unwrap();
    for cid in b.concept_ids().unwrap() {
        let bytes = trusty_okf::format::hash::bytes_hash(
            &std::fs::read(root.join(cid.file_name())).unwrap(),
        );
        j.append(&[
            IndexRecord::PutAck {
                path: cid.file_name(),
                bytes: bytes.clone(),
                identity: Some(identity()),
            },
            IndexRecord::Probe {
                path: cid.file_name(),
                bytes,
                identity: Some(identity()),
                passed: true,
            },
        ])
        .unwrap();
    }
}
