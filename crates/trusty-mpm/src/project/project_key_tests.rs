//! Tests for [`super`]: project keys and the registry-to-index-id link (#10530).

use super::*;
use crate::project::store::ProjectStore;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use trusty_common::search_index::{IndexOptions, ensure_project_indexed_reporting_as};

fn project(name: &str, repo_url: &str) -> Project {
    Project {
        name: name.into(),
        repo_url: repo_url.into(),
        default_branch: "main".into(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: None,
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
        project_key: None,
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A git checkout under `base` with the given origin.
fn checkout(base: &Path, dirname: &str, origin: &str) -> PathBuf {
    let dir = base.join(dirname);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q"]);
    git(&dir, &["remote", "add", "origin", origin]);
    dir.canonicalize().unwrap()
}

/// Register `repo_url` as `name` in a fresh registry and return the stored key.
async fn register(reg: &Path, name: &str, repo_url: &str) -> String {
    let mut store = ProjectStore::load(reg).await.unwrap();
    store.upsert(project(name, repo_url)).await.unwrap();
    store.get(name).await.unwrap().project_key.unwrap()
}

/// Why: URL spellings of one repo must give one key.
/// Test: this test.
#[test]
fn key_from_url_forms_is_one_owner_repo() {
    for url in [
        "https://github.com/Acme/Widget",
        "https://github.com/acme/widget.git",
        "git@github.com:acme/widget.git",
    ] {
        assert_eq!(
            project_key_from_repo_url(url).as_deref(),
            Some("acme/widget"),
            "{url}"
        );
    }
}

/// Why: no origin means no `owner/repo`; the caller mints a `local:` key.
/// Test: this test.
#[test]
fn key_from_path_without_origin_is_none() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        project_key_from_repo_url(tmp.path().to_str().unwrap()),
        None
    );
    assert!(assign_project_key(tmp.path().to_str().unwrap()).starts_with("local:"));
}

/// Why: only the registered checkout may claim the project's index; a linked
/// worktree or another clone must not.
/// Test: this test.
#[test]
fn registered_project_for_root_matches_the_checkout_only() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let mut p = project("widget", root.to_str().unwrap());
    p.project_key = Some("acme/widget".into());
    let found = registered_project_for_root(std::slice::from_ref(&p), &root).unwrap();
    assert_eq!(
        found,
        trusty_common::RegisteredProject::new("widget", "acme/widget")
    );
    assert_eq!(
        registered_project_for_root(&[p.clone()], &root.join("sub")),
        None
    );
    p.project_key = None;
    assert_eq!(
        registered_project_for_root(&[p], &root),
        None,
        "no key, no match"
    );
}

/// Why: `mv` of the checkout must not change the index id (#4269).
/// Test: this test.
#[tokio::test]
async fn mv_leaves_the_key_and_id_unchanged() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "widget", "https://github.com/acme/widget.git");
    let key = register(reg.path(), "widget", dir.to_str().unwrap()).await;
    let before = trusty_common::derive_registry_index_id("widget", &key);

    let moved = tmp.path().join("moved");
    std::fs::rename(&dir, &moved).unwrap();
    let after_key = register(
        reg.path(),
        "widget",
        moved.canonicalize().unwrap().to_str().unwrap(),
    )
    .await;
    assert_eq!(after_key, key);
    assert_eq!(
        trusty_common::derive_registry_index_id("widget", &after_key),
        before
    );
}

/// Why: a repo rename (origin URL change) must not change the id.
/// Test: this test.
#[tokio::test]
async fn repo_rename_leaves_the_key_and_id_unchanged() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "widget", "https://github.com/acme/widget.git");
    let key = register(reg.path(), "widget", dir.to_str().unwrap()).await;

    git(
        &dir,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/widget-renamed.git",
        ],
    );
    assert_eq!(
        register(reg.path(), "widget", dir.to_str().unwrap()).await,
        key
    );
}

/// Why: `git remote remove origin` must not change the id.
/// Test: this test.
#[tokio::test]
async fn remote_remove_leaves_the_key_and_id_unchanged() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "widget", "https://github.com/acme/widget.git");
    let key = register(reg.path(), "widget", dir.to_str().unwrap()).await;

    git(&dir, &["remote", "remove", "origin"]);
    assert_eq!(
        register(reg.path(), "widget", dir.to_str().unwrap()).await,
        key
    );
}

/// Why: the id tm publishes in the active set must be the id registration
/// creates. Before #10530 they were `derive_project_index_id` and the basename.
/// Test: this test.
#[tokio::test]
async fn index_id_matches_between_registration_and_publication() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    // The directory name differs from the project name on purpose.
    let dir = checkout(
        tmp.path(),
        "some-clone",
        "https://github.com/acme/widget.git",
    );
    let key = register(reg.path(), "widget", dir.to_str().unwrap()).await;

    let registered = crate::core::session_launch::registered_project_for(&dir, reg.path());
    let created = ensure_project_indexed_reporting_as(
        &dir,
        registered.as_ref(),
        IndexOptions::default().with_allow_sensitive_path(true),
    )
    .index_id
    .expect("an id");
    let published = index_id_for_root_in(reg.path(), &dir);

    assert_eq!(created, published);
    assert_eq!(
        created,
        trusty_common::derive_registry_index_id("widget", &key)
    );
    assert_ne!(created, "some-clone", "not the legacy basename");
}
