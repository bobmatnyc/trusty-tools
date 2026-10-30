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

/// Why: `mv` of the checkout must not change the project KEY, so the
/// registry-derived id stays fixed. Documented limitation: `registered_project_for_root`
/// matches by the stored `repo_url` path, so until the new path is registered the
/// naming id (legacy basename, see `REGISTRY_IDS_ADOPTED`) follows the directory.
/// Test: this test.
#[tokio::test]
async fn mv_leaves_the_key_unchanged_and_the_naming_id_follows_the_directory() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "widget", "https://github.com/acme/widget.git");
    let key = register(reg.path(), "widget", dir.to_str().unwrap()).await;
    let before = trusty_common::derive_registry_index_id("widget", &key);
    let named_before = index_id_for_root_in(reg.path(), &dir);
    assert_eq!(named_before, "widget");

    let moved = tmp.path().join("moved");
    std::fs::rename(&dir, &moved).unwrap();
    let moved = moved.canonicalize().unwrap();
    // Not yet re-registered: the old path no longer matches, so no registry project.
    assert_eq!(registered_project_for_root_in(reg.path(), &moved), None);
    assert_eq!(index_id_for_root_in(reg.path(), &moved), "moved");

    let after_key = register(reg.path(), "widget", moved.to_str().unwrap()).await;
    assert_eq!(after_key, key);
    assert_eq!(
        trusty_common::derive_registry_index_id("widget", &after_key),
        before
    );
    assert!(registered_project_for_root_in(reg.path(), &moved).is_some());
}

/// Why: a repo rename (origin URL change) must not change the key; the
/// checkout path still matches, so the registry project is still found.
/// Test: this test.
#[tokio::test]
async fn repo_rename_leaves_the_key_unchanged() {
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

/// Why: `git remote remove origin` must not change the key; the checkout path
/// still matches, so the registry project is still found.
/// Test: this test.
#[tokio::test]
async fn remote_remove_leaves_the_key_unchanged() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "widget", "https://github.com/acme/widget.git");
    let key = register(reg.path(), "widget", dir.to_str().unwrap()).await;

    git(&dir, &["remote", "remove", "origin"]);
    assert!(registered_project_for_root_in(reg.path(), &dir).is_some());
    assert_eq!(
        register(reg.path(), "widget", dir.to_str().unwrap()).await,
        key
    );
}

/// Why: the id tm publishes in the active set must be the id registration
/// creates, and trusty-code (plain entry point) must create the same one.
/// Before #10530 they were `derive_project_index_id` and the basename.
/// Test: this test.
#[tokio::test]
async fn index_id_matches_between_registration_publication_and_trusty_code() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    // The directory name differs from the project name on purpose.
    let dir = checkout(
        tmp.path(),
        "some-clone",
        "https://github.com/acme/widget.git",
    );
    register(reg.path(), "widget", dir.to_str().unwrap()).await;
    let opts = IndexOptions::default().with_allow_sensitive_path(true);

    let registered = crate::core::session_launch::registered_project_for(&dir, reg.path());
    assert!(registered.is_some(), "the root is a registry project");
    let tm = ensure_project_indexed_reporting_as(&dir, registered.as_ref(), opts)
        .index_id
        .expect("an id");
    let code = trusty_common::search_index::ensure_project_indexed_reporting(&dir, opts)
        .index_id
        .expect("an id");
    let published = index_id_for_root_in(reg.path(), &dir);

    assert_eq!(tm, code, "tm and trusty-code create one index");
    assert_eq!(tm, published, "tm publishes the id registration creates");
}

/// Why (#10530 review): a legacy record (no key) that is re-registered mints a
/// key. Launch, the residency producer and the doctor must still agree, with an
/// index already registered under the basename at the root.
/// Test: this test.
#[tokio::test]
async fn legacy_record_reregistered_keeps_launch_residency_and_doctor_in_agreement() {
    let tmp = TempDir::new().unwrap();
    let reg = TempDir::new().unwrap();
    let dir = checkout(tmp.path(), "cto", "https://github.com/acme/cto.git");
    // A legacy projects.json record: no `project_key`.
    let legacy = serde_json::json!({ "projects": { "cto": {
        "name": "cto", "repo_url": dir.to_str().unwrap(), "default_branch": "main" } } });
    std::fs::write(reg.path().join("projects.json"), legacy.to_string()).unwrap();
    let opts = IndexOptions::default().with_allow_sensitive_path(true);
    let basename = "cto";

    let agree = |label: &str| {
        let registered = crate::core::session_launch::registered_project_for(&dir, reg.path());
        let launch = ensure_project_indexed_reporting_as(&dir, registered.as_ref(), opts)
            .index_id
            .expect("an id");
        let residency = index_id_for_root_in(reg.path(), &dir);
        let doctor = accepted_index_ids_for(&dir, registered.as_ref());
        assert_eq!(launch, basename, "{label}: launch pins the existing index");
        assert_eq!(residency, basename, "{label}: residency publishes it");
        assert_eq!(doctor[0], basename, "{label}: doctor names it first");
    };
    agree("legacy record");
    // Re-register: the store mints a key; the existing basename index must still win.
    let key = register(reg.path(), "cto", dir.to_str().unwrap()).await;
    assert_eq!(key, "acme/cto");
    agree("after re-register");
}
