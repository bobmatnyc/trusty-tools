//! Tests for `extends` resolution and the merge.

use super::*;
use tempfile::TempDir;

fn write(repos: &Path, owner: &str, body: &str) {
    let dir = repos.join(owner);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(OWNER_FILE), body).unwrap();
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn an_absent_owner_file_resolves_to_none() {
    let tmp = TempDir::new().unwrap();
    assert!(resolve_chain(tmp.path(), "acme").unwrap().is_none());
}

#[test]
fn a_child_allow_replaces_and_deny_unions() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "parent",
        "[mcp]\nallow = [\"apex\", \"notion\"]\ndeny = [\"heygen\"]\nclaude_ai_connectors = false\n",
    );
    write(
        tmp.path(),
        "child",
        "extends = \"parent\"\n[mcp]\nallow = [\"apex\"]\ndeny = [\"slack-mcp\"]\nclaude_ai_connectors = true\n",
    );
    let resolved = resolve_chain(tmp.path(), "child").unwrap().unwrap();
    assert_eq!(resolved.chain, vec!["child", "parent"]);
    assert_eq!(resolved.policy.allow, Some(set(&["apex"])));
    assert_eq!(resolved.policy.deny, set(&["heygen", "slack-mcp"]));
    assert_eq!(resolved.policy.claude_ai_connectors, Some(true));
    assert_eq!(
        owner_file_path(tmp.path(), "child"),
        tmp.path().join("child").join(OWNER_FILE)
    );
}

#[test]
fn a_child_without_keys_inherits_its_parent() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "parent",
        "[mcp]\nallow = []\nclaude_ai_connectors = false\n",
    );
    write(tmp.path(), "child", "extends = \"parent\"\n");
    let policy = resolve_chain(tmp.path(), "child").unwrap().unwrap().policy;
    assert_eq!(policy.allow, Some(BTreeSet::new()));
    assert_eq!(policy.claude_ai_connectors, Some(false));
}

#[test]
fn an_extends_cycle_is_a_bad_file() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a", "extends = \"b\"\n");
    write(tmp.path(), "b", "extends = \"A\"\n");
    let err = resolve_chain(tmp.path(), "a").unwrap_err();
    assert!(matches!(err, OwnerPolicyError::Cycle { .. }), "{err}");
    assert!(err.to_string().contains("a → b → A"), "{err}");
}

#[test]
fn a_self_extends_is_a_cycle() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a", "extends = \"a\"\n");
    assert!(matches!(
        resolve_chain(tmp.path(), "a"),
        Err(OwnerPolicyError::Cycle { .. })
    ));
}

#[test]
fn a_missing_parent_is_a_bad_file() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a", "extends = \"ghost\"\n");
    assert!(matches!(
        resolve_chain(tmp.path(), "a"),
        Err(OwnerPolicyError::MissingParent { .. })
    ));
}

#[test]
fn an_invalid_parent_is_a_bad_file() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a", "extends = \"b\"\n");
    write(tmp.path(), "b", "[agents]\n");
    let err = resolve_chain(tmp.path(), "a").unwrap_err();
    assert!(
        matches!(err, OwnerPolicyError::InvalidParent { .. }),
        "{err}"
    );
}

#[test]
fn a_chain_of_four_files_is_allowed() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "o1", "extends = \"o2\"\n");
    write(tmp.path(), "o2", "extends = \"o3\"\n");
    write(tmp.path(), "o3", "extends = \"o4\"\n");
    write(tmp.path(), "o4", "[mcp]\ndeny = [\"heygen\"]\n");
    let resolved = resolve_chain(tmp.path(), "o1").unwrap().unwrap();
    assert_eq!(resolved.chain.len(), MAX_CHAIN);
    assert_eq!(resolved.policy.deny, set(&["heygen"]));
}

#[test]
fn a_chain_longer_than_four_files_is_a_bad_file() {
    let tmp = TempDir::new().unwrap();
    for i in 1..=4 {
        write(
            tmp.path(),
            &format!("o{i}"),
            &format!("extends = \"o{}\"\n", i + 1),
        );
    }
    write(tmp.path(), "o5", "");
    assert!(matches!(
        resolve_chain(tmp.path(), "o1"),
        Err(OwnerPolicyError::TooDeep { .. })
    ));
}

#[test]
fn the_source_sha_tracks_every_file_in_the_chain() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "parent", "[mcp]\ndeny = [\"heygen\"]\n");
    write(tmp.path(), "child", "extends = \"parent\"\n");
    let before = resolve_chain(tmp.path(), "child").unwrap().unwrap();
    write(tmp.path(), "parent", "[mcp]\ndeny = [\"notion\"]\n");
    let after = resolve_chain(tmp.path(), "child").unwrap().unwrap();
    assert_ne!(before.source_sha256, after.source_sha256);
}

#[test]
fn with_floor_drops_floor_servers_from_deny() {
    let policy = OwnerPolicy {
        allow: None,
        deny: set(&["heygen", "trusty-search", "claude-in-chrome"]),
        claude_ai_connectors: None,
    }
    .with_floor();
    assert_eq!(policy.deny, set(&["heygen"]));
}
