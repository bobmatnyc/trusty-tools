//! Tests for `workstreams::path` (DOC-48 §3.1, AC-1.2).

use super::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn default_data_dir_is_dot_trusty_code() {
    let dir = default_data_dir();
    assert_eq!(dir.file_name().unwrap(), ".trusty-code");
}

#[test]
fn slugify_lowercases_and_hyphenates() {
    assert_eq!(slugify("Acme_API"), "acme-api");
    assert_eq!(slugify("acme-api"), "acme-api");
    assert_eq!(slugify("My Project 2"), "my-project-2");
}

#[test]
fn slugify_trims_leading_and_trailing_separators() {
    assert_eq!(slugify("--acme--"), "acme");
    assert_eq!(slugify("___"), "project");
    assert_eq!(slugify(""), "project");
}

#[test]
fn project_hash_is_deterministic() {
    let root = PathBuf::from("/path/to/acme-api");
    assert_eq!(project_hash(&root), project_hash(&root));
    assert_eq!(project_hash(&root).len(), HASH_LEN);
}

#[test]
fn project_hash_differs_for_different_paths() {
    let a = PathBuf::from("/path/to/acme-api");
    let b = PathBuf::from("/other/path/to/acme-api");
    assert_ne!(project_hash(&a), project_hash(&b));
}

#[test]
fn store_filename_for_bound_project() {
    let dir = TempDir::new().expect("tempdir");
    let binding = ProjectBinding::resolve(Some(dir.path().to_path_buf())).expect("resolve");
    let filename = store_filename(&binding);
    assert!(filename.starts_with("workstreams-"));
    assert!(filename.ends_with(".json"));
    // Deterministic for the same binding.
    assert_eq!(filename, store_filename(&binding));
}

#[test]
fn store_filename_for_projectless() {
    assert_eq!(
        store_filename(&ProjectBinding::None),
        "workstreams-projectless.json"
    );
}

#[test]
fn store_filename_disambiguates_same_basename_different_roots() {
    // Why: §3.1 — "a hash suffix disambiguates multiple checkouts of the
    // same project" (e.g. two clones both named `acme-api`). This must
    // construct an ACTUAL same-basename scenario: two directories that
    // share the literal final path component under different temp roots, so
    // the slug component is identical and only the hash can tell them apart.
    let root_a = TempDir::new().expect("tempdir a");
    let root_b = TempDir::new().expect("tempdir b");
    let dir_a = root_a.path().join("acme-api");
    let dir_b = root_b.path().join("acme-api");
    std::fs::create_dir(&dir_a).expect("mkdir a");
    std::fs::create_dir(&dir_b).expect("mkdir b");
    assert_eq!(
        dir_a.file_name(),
        dir_b.file_name(),
        "precondition: both roots must share the same basename"
    );

    let binding_a = ProjectBinding::resolve(Some(dir_a)).expect("resolve a");
    let binding_b = ProjectBinding::resolve(Some(dir_b)).expect("resolve b");
    let filename_a = store_filename(&binding_a);
    let filename_b = store_filename(&binding_b);
    assert_ne!(
        filename_a, filename_b,
        "same-basename projects at different roots must not collide on filename"
    );
    // Same-basename precondition means the slug component is identical; the
    // difference must come from the hash, proving the hash actually varies
    // with the full root path rather than being a constant.
    assert!(filename_a.starts_with("workstreams-acme-api-"));
    assert!(filename_b.starts_with("workstreams-acme-api-"));
}

#[test]
fn store_path_joins_data_dir_and_filename() {
    let data_dir = PathBuf::from("/tmp/trusty-code-data");
    let binding = ProjectBinding::None;
    let path = store_path(&data_dir, &binding);
    assert_eq!(path, data_dir.join("workstreams-projectless.json"));
}
