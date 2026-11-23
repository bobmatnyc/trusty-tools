//! The attachments tree is never an index root (#7370, requirement 5).
//!
//! Why: `<home>/attachments` holds whatever a user dropped into a chat — a
//! contract, a payroll export, a screenshot of a password manager. The OKG
//! tree at `<home>/okg` is indexed and semantically searchable. If the two ever
//! converged, every attachment would silently become retrievable knowledge, and
//! no error would say so. These tests pin the separation from both ends: the
//! knowledge module's default root, and the absence of any call path that hands
//! `attachments_dir()` to an indexer.

use crate::assistants::{AssistantHome, AssistantInstanceId};
use crate::knowledge::KnowledgeStore;
use chrono::{TimeZone, Utc};

/// The knowledge store's protected root is `okg`, and `attachments` is its
/// SIBLING — neither contains the other, so an index walk rooted at one can
/// never reach the other.
///
/// Pins `knowledge/mod.rs`'s `root: self.home.okg_dir()`.
#[test]
fn the_protected_index_root_is_okg_not_attachments() {
    let temp = tempfile::tempdir().unwrap();
    let home = AssistantHome::under(
        temp.path().canonicalize().unwrap(),
        AssistantInstanceId::new("test-assistant").unwrap(),
    );
    let attachments = home.attachments_dir();
    let okg = home.okg_dir();
    let store = KnowledgeStore::new(AssistantHome::under(
        temp.path().canonicalize().unwrap(),
        AssistantInstanceId::new("test-assistant").unwrap(),
    ));

    let state = store
        .initialize(Utc.with_ymd_and_hms(2026, 9, 11, 12, 0, 0).unwrap(), None)
        .unwrap();

    assert_eq!(state.store.root, okg);
    assert_ne!(state.store.root, attachments);
    assert!(!attachments.starts_with(&okg));
    assert!(!okg.starts_with(&attachments));
}

/// No source line in this crate hands `attachments_dir()` to an indexer.
///
/// Why: the structural test above pins today's default. This one pins that a
/// LATER change cannot quietly route the attachments tree into `create_index`,
/// `reindex`, or a `ProtectedStore` root — the three ways a directory becomes
/// indexed here. A source scan is the only check that covers call paths that do
/// not exist yet.
/// What: reads every `.rs` file under `src/`, and fails on any line mentioning
/// `attachments_dir` alongside an indexing term. Skips silently when `src/` is
/// absent (a packaged crate), which is the only case where there is nothing to
/// scan.
#[test]
fn no_call_path_indexes_the_attachments_tree() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    if !src.is_dir() {
        return;
    }
    const INDEXING_TERMS: [&str; 4] = ["create_index", "reindex", "ProtectedStore", "index_root"];
    let mut offenders: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(&src)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        // This file names both on purpose; scanning it would fail itself.
        if path.ends_with("isolation_tests.rs") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        for (number, line) in body.lines().enumerate() {
            if line.contains("attachments_dir")
                && INDEXING_TERMS.iter().any(|term| line.contains(term))
            {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the attachments tree must never be handed to an indexer:\n{}",
        offenders.join("\n")
    );
}
