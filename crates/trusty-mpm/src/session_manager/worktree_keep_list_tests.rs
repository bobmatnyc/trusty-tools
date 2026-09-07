//! Tests for the owner keep-list (#6927).
//!
//! Why: the keep-list is a PROTECTIVE gate, so every test here asserts that
//! something is kept — or, for the invalid-pattern case, that the operator is
//! told their entry protects nothing.
//! Test target: `super::KeepList`.

use super::*;

#[test]
fn an_empty_list_keeps_nothing() {
    let list = KeepList::from_patterns::<String>(&[]);
    assert_eq!(list.keeps(Path::new("/tmp/anything")), None);
    assert!(list.invalid().is_empty());
}

#[test]
fn a_literal_path_entry_keeps_the_directory_and_its_children() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let kept = tmp.path().join("hotstats");
    std::fs::create_dir_all(kept.join(".worktrees/one")).expect("mkdir");
    let list = KeepList::from_patterns(&[kept.to_string_lossy().to_string()]);

    assert_eq!(
        list.keeps(&kept),
        Some(kept.to_string_lossy().to_string().as_str())
    );
    assert!(
        list.keeps(&kept.join(".worktrees/one")).is_some(),
        "a keep-listed workspace must keep the worktrees inside it"
    );
    assert_eq!(
        list.keeps(&tmp.path().join("other")),
        None,
        "a sibling directory is not kept"
    );
}

#[test]
fn a_glob_entry_keeps_every_matching_worktree() {
    let list = KeepList::from_patterns(&["**/hotstats*".to_string()]);
    assert!(list.keeps(Path::new("/a/b/hotstats-2026")).is_some());
    assert!(list.keeps(Path::new("/a/b/unrelated")).is_none());
}

#[test]
fn a_missing_directory_is_still_kept_by_its_literal_spelling() {
    // A stale worktree pointer names a path that no longer exists, so neither
    // side canonicalizes. The raw comparison is what keeps it answerable.
    let list = KeepList::from_patterns(&["/nowhere/hotstats".to_string()]);
    assert!(list.keeps(Path::new("/nowhere/hotstats/wt")).is_some());
}

#[test]
fn an_uncompilable_glob_is_reported_rather_than_silently_dropped() {
    let list = KeepList::from_patterns(&["[".to_string()]);
    assert_eq!(
        list.keeps(Path::new("/a/b")),
        None,
        "the bad entry must not become a matcher"
    );
    assert_eq!(list.invalid().len(), 1, "{:?}", list.invalid());
    assert!(
        list.invalid()[0].starts_with('['),
        "the report must echo the operator's own spelling: {:?}",
        list.invalid()
    );
}

#[test]
fn a_tilde_path_expands_against_home() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let list = KeepList::from_patterns(&["~/hotstats".to_string()]);
    let under_home = PathBuf::from(home).join("hotstats/wt");
    assert!(
        list.keeps(&under_home).is_some(),
        "`~/hotstats` must keep {}",
        under_home.display()
    );
}

#[test]
fn blank_entries_are_ignored() {
    let list = KeepList::from_patterns(&["".to_string(), "   ".to_string()]);
    assert_eq!(list.keeps(Path::new("/a/b")), None);
    assert!(list.invalid().is_empty());
}
