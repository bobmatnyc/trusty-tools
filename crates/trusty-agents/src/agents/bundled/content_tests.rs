//! Tests for [`super`]: the bundled trees load from content and fail closed
//! when a required file is absent (#9014).

use trusty_agents_common::agent_content::{AgentContentError, checkout_content};

use super::test_support::{fake_checkout, repo_tree, repo_tree_files};
use super::*;

/// Loads `kind` from a fake checkout holding `files`.
fn load_from(
    files: &[(String, Vec<u8>)],
    kind: BundledTreeKind,
) -> Result<BundledTree, AgentContentError> {
    let tmp = tempfile::tempdir().expect("tempdir");
    fake_checkout(tmp.path(), files);
    let content = checkout_content(tmp.path()).expect("fake checkout resolves");
    BundledTree::from_content(&content, kind)
}

/// Why: the checkout's own trees are what a content release ships; each must
/// carry its required files and nothing the allowlist refuses.
/// Test: itself.
#[test]
fn the_repository_trees_load() {
    for kind in [BundledTreeKind::Agents, BundledTreeKind::Workflows] {
        let tree = repo_tree(kind);
        assert_eq!(tree.kind(), kind);
        for required in kind.required() {
            assert!(
                tree.get(required).is_some(),
                "{kind:?} tree lacks {required}"
            );
        }
        for (rel, _) in tree.files() {
            assert!(
                super::super::is_bundled_asset(rel),
                "{rel} is not a bundled asset"
            );
        }
    }
    assert!(
        repo_tree(BundledTreeKind::Agents).files().len() > 20,
        "the roster lost most of its files"
    );
}

/// Why (#9014, Fail-Open Check): a content release that lacks a file startup
/// relies on must fail the load naming that file — never deploy a roster
/// that silently lacks it, and never fall back to an empty set.
/// What: for every required file of both trees, drops it from an otherwise
/// complete copy of the checkout's tree and asserts `Missing` names its full
/// bundle path.
/// Test: itself.
#[test]
fn a_tree_missing_a_required_file_fails_closed_naming_it() {
    for kind in [BundledTreeKind::Agents, BundledTreeKind::Workflows] {
        for required in kind.required() {
            let path = format!("{}/{required}", kind.bundle_path());
            let files: Vec<_> = repo_tree_files(kind)
                .into_iter()
                .filter(|(rel, _)| *rel != format!("content/{path}"))
                .collect();
            match load_from(&files, kind) {
                Err(AgentContentError::Missing { path: named, .. }) => {
                    assert_eq!(named, path, "the error must name the absent file");
                }
                other => panic!("absent {path} must fail closed, got {other:?}"),
            }
        }
    }
}

/// Why: a tree that is not there at all is the same failure — the first
/// required file is named, and the message tells the operator what to run.
/// Test: itself.
#[test]
fn an_absent_tree_fails_closed_naming_its_first_required_file() {
    let err = load_from(&[], BundledTreeKind::Agents).expect_err("no tree must not load");
    let message = err.to_string();
    assert!(
        matches!(&err, AgentContentError::Missing { path, .. }
            if path == "agents/trusty-agents/agents/ctrl.toml"),
        "got {err:?}"
    );
    assert!(
        message.contains("git pull") || message.contains("tm content update"),
        "the error must name a remedy: {message}"
    );
}

/// Why: an empty required file is as unusable as an absent one.
/// Test: itself.
#[test]
fn an_empty_required_file_is_invalid() {
    let files = vec![(
        "content/agents/trusty-agents/workflows/prescriptive.json".to_owned(),
        b"  \n".to_vec(),
    )];
    match load_from(&files, BundledTreeKind::Workflows) {
        Err(AgentContentError::Invalid { path, .. }) => {
            assert_eq!(path, "agents/trusty-agents/workflows/prescriptive.json");
        }
        other => panic!("an empty required file must be invalid, got {other:?}"),
    }
}

/// Why: a stray file in the source tree (an editor backup, a lock sidecar)
/// must never reach the tree a deploy writes (#5226).
/// Test: itself.
#[test]
fn a_stray_file_never_joins_the_tree() {
    let mut files = repo_tree_files(BundledTreeKind::Workflows);
    files.push((
        "content/agents/trusty-agents/workflows/prescriptive.json.bak".to_owned(),
        b"backup".to_vec(),
    ));
    let tree = load_from(&files, BundledTreeKind::Workflows).expect("tree loads");
    assert!(tree.get("prescriptive.json.bak").is_none());
    assert!(tree.get("prescriptive.json").is_some());
}
