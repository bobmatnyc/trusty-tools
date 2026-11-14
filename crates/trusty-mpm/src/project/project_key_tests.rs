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
        decommissioned_at: None,
        archived_at: None,
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
    assert!(
        assign_project_key(tmp.path().to_str().unwrap())
            .unwrap()
            .starts_with("local:")
    );
}

/// Why (#10604): a flat or mixed-case hand clone registered as a `~` path must
/// key by its origin, the same one key its GitHub URL gives — never the
/// `trusty-mpm-projects/trusty-tools` a URL parse of `~/…` produced.
/// Test: this test, against a clean temp HOME.
#[test]
fn a_flat_checkout_spelled_with_a_tilde_keys_by_its_origin_10604() {
    let home = TempDir::new().unwrap();
    let home = home.path().canonicalize().unwrap();
    let origin = "https://github.com/BobMatNYC/Trusty-Tools.git";
    let projects = home.join("trusty-mpm-projects");
    checkout(&projects, "trusty-tools", origin);
    checkout(&home.join("code"), "Trusty-Tools", origin);
    let canonical = project_key_from_repo_url(origin);
    assert_eq!(canonical.as_deref(), Some("bobmatnyc/trusty-tools"));
    for spelled in ["~/trusty-mpm-projects/trusty-tools", "~/code/Trusty-Tools"] {
        assert_eq!(
            project_key_from_repo_url_in(spelled, Some(&home)),
            canonical,
            "{spelled}"
        );
    }
    // No home: no key (registration then refuses it), never a URL guess.
    assert_eq!(
        project_key_from_repo_url_in("~/trusty-mpm-projects/trusty-tools", None),
        None
    );
}

/// Why (#10604): `dirs::home_dir` returns a relative `$HOME` as given; expanding
/// `~/x` against it would point `git -C` at the daemon's cwd. It is unexpandable.
/// Test: this test.
#[test]
fn a_relative_home_expands_no_tilde_path_10604() {
    let relative = Path::new("relative/home");
    assert_eq!(local_repo_path("~/x", Some(relative)), None);
    assert_eq!(local_repo_path("~", Some(relative)), None);
    assert_eq!(project_key_from_repo_url_in("~/x", Some(relative)), None);
    // An absolute path never needed a home.
    assert_eq!(
        local_repo_path("/abs/x", Some(relative)),
        Some(PathBuf::from("/abs/x"))
    );
}

/// Why (#10604): a `~` checkout that cannot be read must not be minted a
/// permanent `local:` key while registration carries on.
/// Test: this test.
#[cfg(unix)]
#[test]
fn a_tilde_checkout_without_a_usable_home_is_refused_10604() {
    use std::os::unix::fs::PermissionsExt as _;
    let refused = |home: Option<&Path>| {
        matches!(
            assign_project_key_in("~/code/widget", home),
            Err(ProjectStoreError::UnresolvableHomePath { .. })
        )
    };
    assert!(refused(None), "no HOME");
    assert!(refused(Some(Path::new("relative/home"))), "relative HOME");
    let home = TempDir::new().unwrap();
    assert!(refused(Some(home.path())), "missing checkout");
    std::fs::create_dir_all(home.path().join("code/widget")).unwrap();
    // A readable directory with no origin still gets a `local:` key.
    let key = assign_project_key_in("~/code/widget", Some(home.path())).unwrap();
    assert!(key.starts_with("local:"), "{key}");
    // An unreadable HOME hides the checkout: refused, not keyed.
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = refused(Some(home.path()));
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(unreadable, "unreadable HOME");
}

/// Restores `$HOME` on drop; every user is `#[serial_test::serial]`.
pub(crate) struct HomeGuard(Option<std::ffi::OsString>);

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: callers are `#[serial]`, so no other test races the write.
        match self.0.take() {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// Point `$HOME` at `value` until the guard drops.
pub(crate) fn set_home(value: &Path) -> HomeGuard {
    let prior = std::env::var_os("HOME");
    // SAFETY: callers are `#[serial]`.
    unsafe { std::env::set_var("HOME", value) };
    HomeGuard(prior)
}

/// Why (#10604): under an isolated, empty `$HOME`, a hand clone registered as
/// `~/code/Trusty_Tools` keys, resolves and clones as its GitHub URL does.
/// Before the fix the key was `code/trusty-tools` and the checkout
/// `<repos_root>/code/trusty-tools`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn clean_home_tilde_checkout_keys_like_its_github_url_10604() {
    let home = TempDir::new().unwrap();
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
    let _home = set_home(home.path());
    let origin = "https://github.com/BobMatNYC/Trusty_Tools.git";
    let dir = checkout(&home.path().join("code"), "Trusty_Tools", origin);
    let spelled = "~/code/Trusty_Tools";

    assert_eq!(
        project_key_from_repo_url(origin).as_deref(),
        Some("bobmatnyc/trusty-tools")
    );
    assert_eq!(
        project_key_from_repo_url(spelled),
        project_key_from_repo_url(origin)
    );
    let reg = TempDir::new().unwrap();
    assert_eq!(
        register(reg.path(), "trusty-tools", spelled).await,
        "bobmatnyc/trusty-tools"
    );
    let resolved = crate::project::local_checkout_for_url(spelled).expect("a checkout");
    assert!(
        trusty_common::identifies_same_path(&resolved, &dir),
        "{resolved:?}"
    );
    for (owner, repo) in [
        ("BobMatNYC", "Trusty_Tools"),
        ("bobmatnyc", "trusty-tools.git"),
    ] {
        let path = crate::core::repo_paths::base_clone_path(owner, repo);
        assert!(path.ends_with("bobmatnyc/trusty-tools"), "{path:?}");
    }
}

/// Why (#10604): under a relative `$HOME`, registering a `~` checkout fails
/// with the cause and writes nothing.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_tilde_registration_under_a_relative_home_writes_nothing_10604() {
    let _home = set_home(Path::new("relative/home"));
    let reg = TempDir::new().unwrap();
    let mut store = ProjectStore::load(reg.path()).await.unwrap();
    let err = store
        .upsert(project("widget", "~/code/widget"))
        .await
        .expect_err("refused");
    assert!(err.to_string().contains("~/code/widget"), "{err}");
    assert!(matches!(
        store.get("widget").await,
        Err(ProjectStoreError::NotFound(_))
    ));
}

/// Why (#10604): stored keys are not migrated (owner decision, 2026-11-02). A
/// `~` record keyed the pre-fix way still resolves to its checkout and stops a
/// registration of the same checkout by its GitHub URL; its key is unchanged.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn register_unless_present_keeps_an_old_tilde_keyed_record_10604() {
    let home = TempDir::new().unwrap();
    let _home = set_home(home.path());
    let origin = "https://github.com/BobMatNYC/Trusty_Tools.git";
    let dir = checkout(&home.path().join("code"), "Trusty_Tools", origin);
    let reg = TempDir::new().unwrap();
    let raw = serde_json::json!({ "projects": { "Trusty_Tools": {
        "name": "Trusty_Tools",
        "repo_url": "~/code/Trusty_Tools",
        "default_branch": "main",
        "project_key": "code/trusty-tools"
    }}});
    std::fs::write(reg.path().join("projects.json"), raw.to_string()).unwrap();
    let registry = crate::project::ProjectRegistry::load(reg.path())
        .await
        .unwrap();

    let found = registered_project_for_root(&registry.list().await.unwrap(), &dir);
    assert_eq!(
        found,
        Some(trusty_common::RegisteredProject::new(
            "Trusty_Tools",
            "code/trusty-tools"
        ))
    );
    let kept = registry
        .register_unless_present(project("trusty-tools", origin))
        .await
        .unwrap()
        .expect("the old record answers");
    assert_eq!(kept.name, "Trusty_Tools");
    assert_eq!(kept.project_key.as_deref(), Some("code/trusty-tools"));
    assert_eq!(registry.list().await.unwrap().len(), 1);
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
