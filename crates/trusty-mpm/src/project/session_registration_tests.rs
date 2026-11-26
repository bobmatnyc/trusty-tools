//! Tests for session-start registration and the checkout lookup (#10368).
//!
//! Every test uses its own temp registry directory; none reads or writes the
//! real `~/.trusty-mpm` state.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::project::store::ProjectStore;

/// `git -C dir args…`, panicking on failure.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A git checkout at `dir` whose origin is `remote`, canonicalized.
fn checkout(dir: &Path, remote: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir checkout");
    git(dir, &["init", "-q"]);
    git(dir, &["remote", "add", "origin", remote]);
    dir.canonicalize().expect("canonical checkout")
}

/// The raw bytes of the registry file, or `None` when absent.
fn registry_bytes(registry: &Path) -> Option<Vec<u8>> {
    std::fs::read(registry.join("projects.json")).ok()
}

/// The regression: a session start in a subdirectory of a checkout registers
/// the checkout root, and the `owner/repo` lookup `tm secrets assign` uses
/// finds it. Before #10368 nothing wrote the row, so the lookup found nothing.
#[tokio::test]
async fn session_start_registers_the_checkout_and_assign_resolves_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let root = checkout(&tmp.path().join("widget"), "git@github.com:Acme/Widget.git");
    let sub = root.join("src");
    std::fs::create_dir(&sub).expect("mkdir sub");

    let outcome = register_session_checkout(&registry, &sub, None)
        .await
        .expect("register");

    assert_eq!(
        outcome,
        SessionRegistration::Registered {
            name: "widget".into()
        }
    );
    assert_eq!(
        registered_checkout(&registry, "acme/widget").expect("resolves"),
        root
    );
    assert!(matches!(
        registered_checkout(&registry, "acme/other"),
        Err(CheckoutLookupError::NotRegistered(_))
    ));
}

/// A second start writes nothing: no duplicate row, and a row someone else
/// configured for the same repository keeps every field it had.
#[tokio::test]
async fn session_start_registration_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let root = checkout(&tmp.path().join("widget"), "https://github.com/acme/widget");

    register_session_checkout(&registry, &root, None)
        .await
        .expect("first");
    let after_first = registry_bytes(&registry).expect("written");
    let again = register_session_checkout(&registry, &root, None)
        .await
        .expect("second");
    assert_eq!(
        again,
        SessionRegistration::AlreadyRegistered {
            name: "widget".into(),
            checkout: Some(root.clone()),
            root: root.clone(),
        }
    );
    assert_eq!(registry_bytes(&registry), Some(after_first));

    // A row an operator registered by URL, under another name, is kept as is.
    let other = tmp.path().join("other-registry");
    let mut store = ProjectStore::load(&other).await.expect("store");
    let mut configured = session_project("acme-widget", "https://github.com/acme/widget");
    configured.gh_account = Some("acme-bot".into());
    configured.description = Some("configured elsewhere".into());
    store.upsert(configured).await.expect("seed");
    let before = registry_bytes(&other).expect("seeded");
    let kept = register_session_checkout(&other, &root, None)
        .await
        .expect("kept");
    assert!(
        matches!(&kept, SessionRegistration::AlreadyRegistered { name, .. } if name == "acme-widget"),
        "{kept:?}"
    );
    assert_eq!(registry_bytes(&other), Some(before));
}

/// Fail closed: a corrupt registry is an error from the session start and from
/// the lookup, never a registration, and the file is left as it was.
#[tokio::test]
async fn session_start_registration_failure_is_an_error_not_a_registration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    std::fs::create_dir(&registry).expect("mkdir registry");
    std::fs::write(registry.join("projects.json"), b"{ not json").expect("corrupt");
    let root = checkout(&tmp.path().join("widget"), "https://github.com/acme/widget");

    let outcome = register_session_checkout(&registry, &root, None).await;

    assert!(
        matches!(
            outcome,
            Err(SessionRegistrationError::Store(
                ProjectStoreError::Serialize(_)
            ))
        ),
        "{outcome:?}"
    );
    assert!(matches!(
        registered_checkout(&registry, "acme/widget"),
        Err(CheckoutLookupError::Store(_))
    ));
    assert_eq!(
        registry_bytes(&registry).as_deref(),
        Some(&b"{ not json"[..])
    );
}

/// D50: a home directory that is itself a git repository, or sits inside one,
/// is never registered; a name another repository holds is an error too.
#[tokio::test]
async fn session_start_never_registers_home_or_its_ancestors() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let home = checkout(&tmp.path().join("home"), "https://github.com/me/dotfiles");
    let scratch = home.join("scratch");
    std::fs::create_dir(&scratch).expect("mkdir scratch");

    let outcome = register_session_checkout(&registry, &scratch, Some(&home)).await;
    assert!(
        matches!(&outcome, Err(SessionRegistrationError::Unregistrable(p)) if *p == home),
        "{outcome:?}"
    );
    let inner = home.join("deeper");
    std::fs::create_dir(&inner).expect("mkdir deeper");
    let outcome =
        register_session_checkout(&registry, &tmp.path().join("home"), Some(&inner)).await;
    assert!(
        matches!(outcome, Err(SessionRegistrationError::Unregistrable(_))),
        "{outcome:?}"
    );
    assert_eq!(registry_bytes(&registry), None);

    // Same basename, another repository: refused, and the lookup finds nothing.
    let first = checkout(&tmp.path().join("a/api"), "https://github.com/acme/api");
    let second = checkout(&tmp.path().join("b/api"), "https://github.com/other/api");
    register_session_checkout(&registry, &first, None)
        .await
        .expect("first api");
    let clash = register_session_checkout(&registry, &second, None).await;
    assert!(
        matches!(&clash, Err(SessionRegistrationError::NameTaken { name, .. }) if name == "api"),
        "{clash:?}"
    );
    assert!(matches!(
        registered_checkout(&registry, "other/api"),
        Err(CheckoutLookupError::NotRegistered(_))
    ));
}

/// Two legacy rows (no stored key) naming two checkouts of one repository
/// resolve to neither.
#[test]
fn registered_checkout_refuses_two_checkouts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let one = checkout(&tmp.path().join("one"), "https://github.com/acme/web");
    let two = checkout(&tmp.path().join("two"), "https://github.com/acme/web");
    let rows: std::collections::HashMap<_, _> = [("one", &one), ("two", &two)]
        .into_iter()
        .map(|(name, dir)| (name, session_project(name, &dir.to_string_lossy())))
        .collect();
    std::fs::create_dir(&registry).expect("mkdir registry");
    let doc = serde_json::json!({ "projects": rows });
    std::fs::write(registry.join("projects.json"), doc.to_string()).expect("write");

    assert!(matches!(
        registered_checkout(&registry, "Acme/Web"),
        Err(CheckoutLookupError::Ambiguous(_))
    ));
}

/// A `.git` entry git does not accept is refused with no write: an invalid
/// `.git` file, and an empty `.git` dir inside a parent repository, which git
/// resolves to the parent (so the child would take the parent's owner/repo).
#[tokio::test]
async fn session_start_refuses_a_root_git_does_not_confirm() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");

    let garbage = tmp.path().join("garbage");
    std::fs::create_dir(&garbage).expect("mkdir garbage");
    std::fs::write(garbage.join(".git"), "not a gitdir pointer\n").expect("bad .git");
    let outcome = register_session_checkout(&registry, &garbage, None).await;
    assert!(
        matches!(outcome, Err(SessionRegistrationError::UnverifiedRoot(_))),
        "{outcome:?}"
    );
    assert_eq!(registry_bytes(&registry), None);

    let parent = checkout(&tmp.path().join("parent"), "https://github.com/acme/parent");
    let child = parent.join("child");
    std::fs::create_dir_all(child.join(".git")).expect("empty .git dir");
    let outcome = register_session_checkout(&registry, &child, None).await;
    assert!(
        matches!(&outcome, Err(SessionRegistrationError::UnverifiedRoot(p)) if *p == child),
        "{outcome:?}"
    );
    assert_eq!(registry_bytes(&registry), None);
}

/// A second start reads and writes nothing back: two rows, one carrying a
/// field this binary does not know, keep their exact bytes and order.
#[tokio::test]
async fn session_start_second_start_leaves_the_bytes_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let root = checkout(&tmp.path().join("widget"), "https://github.com/acme/widget");
    std::fs::create_dir(&registry).expect("mkdir registry");
    let doc = format!(
        "{{\n  \"projects\": {{\n    \"zeta\": {{\"name\": \"zeta\", \"repo_url\": \"https://github.com/acme/zeta\", \"default_branch\": \"main\", \"project_key\": \"acme/zeta\"}},\n    \"widget\": {{\"name\": \"widget\", \"repo_url\": {:?}, \"default_branch\": \"main\", \"project_key\": \"acme/widget\", \"field_from_a_newer_tm\": [1, 2]}}\n  }}\n}}\n",
        root.to_string_lossy()
    );
    std::fs::write(registry.join("projects.json"), &doc).expect("seed");

    let outcome = register_session_checkout(&registry, &root, None)
        .await
        .expect("second start");

    assert!(
        matches!(&outcome, SessionRegistration::AlreadyRegistered { name, .. } if name == "widget"),
        "{outcome:?}"
    );
    assert_eq!(
        registry_bytes(&registry).as_deref(),
        Some(doc.as_bytes()),
        "the registry was rewritten"
    );
}

/// D50 through a symlink: a session directory that is a link to the home
/// directory, or to a repository above it, is refused, not registered.
#[cfg(unix)]
#[tokio::test]
async fn session_start_through_a_symlink_to_home_is_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let parent = checkout(&tmp.path().join("parent"), "https://github.com/me/parent");
    let home = parent.join("home");
    std::fs::create_dir(&home).expect("mkdir home");
    let to_parent = tmp.path().join("to-parent");
    std::os::unix::fs::symlink(&parent, &to_parent).expect("symlink parent");
    let to_home = tmp.path().join("to-home");
    std::os::unix::fs::symlink(&home, &to_home).expect("symlink home");

    for dir in [&to_parent, &to_home] {
        let outcome = register_session_checkout(&registry, dir, Some(&home)).await;
        assert!(
            matches!(&outcome, Err(SessionRegistrationError::Unregistrable(p)) if *p == parent),
            "{}: {outcome:?}",
            dir.display()
        );
    }
    assert_eq!(registry_bytes(&registry), None);
}

/// Seed `projects.json` in `registry` with one record and return its bytes.
fn seed_one(registry: &Path, name: &str, repo_url: &str, key: &str) -> Vec<u8> {
    std::fs::create_dir_all(registry).expect("mkdir registry");
    let raw = serde_json::json!({ "projects": { name: {
        "name": name,
        "repo_url": repo_url,
        "default_branch": "main",
        "project_key": key
    }}})
    .to_string();
    std::fs::write(registry.join("projects.json"), &raw).expect("seed");
    raw.into_bytes()
}

/// #10604: a session started in a checkout an old `~` record holds, under the
/// key a URL parse of `~/…` gave, is the same repository. Before the fix it
/// was `NameTaken`, so every session start printed the notice.
#[tokio::test]
#[serial_test::serial]
async fn session_start_in_an_old_tilde_record_checkout_is_the_same_repo_10604() {
    let home = tempfile::tempdir().expect("home");
    let _home = crate::project::project_key::project_key_tests::set_home(home.path());
    let root = checkout(
        &home.path().join("code").join("Trusty_Tools"),
        "https://github.com/BobMatNYC/Trusty_Tools.git",
    );
    let registry = tempfile::tempdir().expect("registry");
    let doc = seed_one(
        registry.path(),
        "Trusty_Tools",
        "~/code/Trusty_Tools",
        "code/trusty-tools",
    );

    let outcome = register_session_checkout(registry.path(), &root, Some(home.path())).await;

    assert!(
        matches!(&outcome, Ok(SessionRegistration::AlreadyRegistered { name, .. }) if name == "Trusty_Tools"),
        "{outcome:?}"
    );
    assert_eq!(
        registry_bytes(registry.path()).as_deref(),
        Some(doc.as_slice()),
        "the old record was rewritten or a second one added"
    );
}

/// #10604: a different repository whose checkout name collides with a record
/// is still `NameTaken` — including when that record is an old `~` row whose
/// checkout is gone, so its current key cannot be looked up.
#[tokio::test]
#[serial_test::serial]
async fn session_start_name_collision_with_another_repo_is_name_taken_10604() {
    let home = tempfile::tempdir().expect("home");
    let _home = crate::project::project_key::project_key_tests::set_home(home.path());
    let root = checkout(
        &home.path().join("other").join("widget"),
        "https://github.com/other/widget.git",
    );
    for (repo_url, key) in [
        ("https://github.com/acme/widget", "acme/widget"),
        ("~/code/widget", "code/widget"),
    ] {
        let registry = tempfile::tempdir().expect("registry");
        let doc = seed_one(registry.path(), "widget", repo_url, key);

        let outcome = register_session_checkout(registry.path(), &root, Some(home.path())).await;

        assert!(
            matches!(&outcome, Err(SessionRegistrationError::NameTaken { name, .. }) if name == "widget"),
            "{repo_url}: {outcome:?}"
        );
        assert_eq!(
            registry_bytes(registry.path()).as_deref(),
            Some(doc.as_slice()),
            "{repo_url}: the registry changed"
        );
    }
}

/// The one row `registry` holds.
fn only_row(registry: &Path) -> Project {
    let mut rows = crate::project::store::read_projects(registry).expect("read registry");
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows.remove(0)
}

/// #11828: the daemon registers a checkout at `<repos_root>/<owner>/<repo>`
/// whose origin is that GitHub repo by its URL, keeping the origin's spelling.
/// Before the fix every checkout stored its path, so the TUI listed a `tm run`
/// clone under `(local)`.
#[tokio::test]
async fn daemon_registers_a_canonical_clone_by_its_github_url_11828() {
    for (origin, url) in [
        // #11828: ssh scp syntax with no `user@`, so the public-content leak
        // guard's EMAIL pattern does not match; the parser drops the user anyway.
        (
            "github.com:acme/widget.git",
            "https://github.com/acme/widget",
        ),
        (
            "https://github.com/Acme/Widget.git",
            "https://github.com/Acme/Widget",
        ),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = tmp.path().join("registry");
        let repos_root = tmp.path().join("repos");
        let root = checkout(&repos_root.join("acme").join("widget"), origin);

        let outcome = register_session_checkout_in(&registry, &root, None, Some(&repos_root))
            .await
            .expect("register");

        assert_eq!(
            outcome,
            SessionRegistration::Registered {
                name: "widget".into()
            },
            "{origin}"
        );
        let row = only_row(&registry);
        assert_eq!(row.repo_url, url, "{origin}");
        assert_eq!(row.project_key.as_deref(), Some("acme/widget"), "{origin}");
    }
}

/// #11828: a checkout off the canonical path, one on another host, and one
/// with no origin keep the path form, and `tm secrets assign`'s lookup still
/// finds the first.
#[tokio::test]
async fn daemon_keeps_the_path_form_off_the_canonical_clone_11828() {
    for (dir, origin) in [
        ("elsewhere/widget", Some("github.com:acme/widget.git")),
        ("acme/widget", Some("gitlab.com:acme/widget.git")),
        ("acme/widget", None),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = tmp.path().join("registry");
        let repos_root = tmp.path().join("repos");
        let root = match origin {
            Some(origin) => checkout(&repos_root.join(dir), origin),
            None => {
                std::fs::create_dir_all(repos_root.join(dir)).expect("mkdir");
                git(&repos_root.join(dir), &["init", "-q"]);
                repos_root.join(dir).canonicalize().expect("canonical")
            }
        };

        register_session_checkout_in(&registry, &root, None, Some(&repos_root))
            .await
            .expect("register");

        assert_eq!(
            only_row(&registry).repo_url,
            root.to_string_lossy(),
            "{dir} {origin:?}"
        );
        if dir.starts_with("elsewhere") {
            assert_eq!(
                registered_checkout(&registry, "acme/widget").expect("resolves"),
                root
            );
        }
    }
}

/// #11828: the client (`tm connect`) keeps the path form even at the canonical
/// path; only the daemon, which owns the repos root, stores the URL.
#[tokio::test]
async fn client_registration_keeps_the_path_form_at_the_canonical_path_11828() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let root = checkout(
        &tmp.path().join("repos").join("acme").join("widget"),
        "github.com:acme/widget.git",
    );

    register_session_checkout(&registry, &root, None)
        .await
        .expect("register");

    assert_eq!(only_row(&registry).repo_url, root.to_string_lossy());
}

/// A checkout at `dir` with origin `remote` and one empty commit, so
/// `git worktree add` has a HEAD to branch from (#11217).
fn committed_checkout(dir: &Path, remote: &str) -> PathBuf {
    let root = checkout(dir, remote);
    git(
        &root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    root
}

/// A linked worktree of `main` at `wt` on a new branch, canonicalized.
fn linked_worktree(main: &Path, wt: &Path) -> PathBuf {
    let branch = wt.file_name().and_then(|n| n.to_str()).expect("utf-8 name");
    git(
        main,
        &["worktree", "add", "-q", "-b", branch, &wt.to_string_lossy()],
    );
    wt.canonicalize().expect("canonical worktree")
}

/// #11217 regression: a session in a subdirectory of a linked worktree
/// registers the MAIN checkout's row, never the worktree path, and a second
/// start (from the worktree or the main checkout) writes nothing. Fails on
/// pre-fix main, which returned `LinkedWorktree` and wrote no row.
#[tokio::test]
async fn a_linked_worktree_session_registers_the_main_checkout_11217() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let main = committed_checkout(
        &tmp.path().join("widget"),
        "https://github.com/Acme/Widget.git",
    );
    let wt = linked_worktree(&main, &tmp.path().join("widget-wt"));
    let sub = wt.join("src");
    std::fs::create_dir(&sub).expect("mkdir sub");

    let outcome = register_session_checkout(&registry, &sub, None)
        .await
        .expect("a worktree session registers");

    assert_eq!(
        outcome,
        SessionRegistration::Registered {
            name: "widget".into()
        }
    );
    assert_eq!(
        registered_checkout(&registry, "acme/widget").expect("assign resolves"),
        main
    );
    let raw = String::from_utf8(registry_bytes(&registry).expect("written")).expect("utf-8");
    assert!(
        !raw.contains("widget-wt"),
        "the worktree path was stored: {raw}"
    );
    let first = registry_bytes(&registry);
    for dir in [&wt, &main] {
        let again = register_session_checkout(&registry, dir, None)
            .await
            .expect("second start");
        assert_eq!(
            again,
            SessionRegistration::AlreadyRegistered {
                name: "widget".into(),
                checkout: Some(main.clone()),
                root: main.clone(),
            },
            "{}",
            dir.display()
        );
    }
    assert_eq!(registry_bytes(&registry), first, "no duplicate or rewrite");
}

/// #11217 fail closed: a worktree whose admin dir names a missing common dir,
/// one with a broken `.git` file, and one whose main checkout was deleted all
/// refuse and write no row.
#[tokio::test]
async fn a_worktree_whose_main_checkout_cannot_be_resolved_writes_nothing_11217() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");

    // Missing common dir: the worktree's `commondir` points nowhere.
    let main = committed_checkout(&tmp.path().join("a"), "https://github.com/acme/a");
    let wt = linked_worktree(&main, &tmp.path().join("a-wt"));
    let admin = main.join(".git/worktrees/a-wt");
    std::fs::write(admin.join("commondir"), "/nonexistent/11217/.git\n").expect("break");
    let outcome = register_session_checkout(&registry, &wt, None).await;
    assert!(
        matches!(outcome, Err(SessionRegistrationError::UnverifiedRoot(_))),
        "{outcome:?}"
    );

    // Broken `.git` file in a worktree.
    let main_b = committed_checkout(&tmp.path().join("b"), "https://github.com/acme/b");
    let wt_b = linked_worktree(&main_b, &tmp.path().join("b-wt"));
    std::fs::write(wt_b.join(".git"), "gitdir: /nonexistent/11217\n").expect("break");
    let outcome = register_session_checkout(&registry, &wt_b, None).await;
    assert!(
        matches!(outcome, Err(SessionRegistrationError::UnverifiedRoot(_))),
        "{outcome:?}"
    );

    // Main checkout no longer exists.
    let main_c = committed_checkout(&tmp.path().join("c"), "https://github.com/acme/c");
    let wt_c = linked_worktree(&main_c, &tmp.path().join("c-wt"));
    std::fs::remove_dir_all(&main_c).expect("delete main");
    let outcome = register_session_checkout(&registry, &wt_c, None).await;
    assert!(
        matches!(outcome, Err(SessionRegistrationError::UnverifiedRoot(_))),
        "{outcome:?}"
    );

    assert_eq!(registry_bytes(&registry), None, "a row was written");
}

/// #11217 fail closed: a worktree of a bare repository has no main working
/// tree, so it is `MainCheckoutUnresolved` and nothing is written.
#[tokio::test]
async fn a_bare_repo_worktree_writes_nothing_11217() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let src = committed_checkout(&tmp.path().join("src"), "https://github.com/acme/bare");
    let bare = tmp.path().join("bare.git");
    git(
        tmp.path(),
        &["clone", "-q", "--bare", &src.to_string_lossy(), "bare.git"],
    );
    let wt = linked_worktree(&bare, &tmp.path().join("bare-wt"));

    let outcome = register_session_checkout(&registry, &wt, None).await;

    assert!(
        matches!(&outcome, Err(SessionRegistrationError::MainCheckoutUnresolved(p)) if *p == wt),
        "{outcome:?}"
    );
    assert_eq!(registry_bytes(&registry), None, "a row was written");
}

/// #11217 D50 boundary: a worktree of repo A, placed inside repo B's tree
/// while B is registered, resolves to A's main checkout only; `acme/b` still
/// resolves to B, and exactly two rows exist.
#[tokio::test]
async fn a_worktree_of_repo_a_never_resolves_to_repo_b_11217() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let repo_a = committed_checkout(&tmp.path().join("a"), "https://github.com/acme/a");
    let repo_b = committed_checkout(&tmp.path().join("b"), "https://github.com/acme/b");
    register_session_checkout(&registry, &repo_b, None)
        .await
        .expect("register b");
    let nested = repo_b.join(".claude/worktrees");
    std::fs::create_dir_all(&nested).expect("mkdir nested");
    let wt = linked_worktree(&repo_a, &nested.join("a-task"));

    let outcome = register_session_checkout(&registry, &wt, None)
        .await
        .expect("register a from its worktree");

    assert_eq!(
        outcome,
        SessionRegistration::Registered { name: "a".into() }
    );
    assert_eq!(registered_checkout(&registry, "acme/a").expect("a"), repo_a);
    assert_eq!(registered_checkout(&registry, "acme/b").expect("b"), repo_b);
    let rows = crate::project::store::read_projects(&registry).expect("read");
    assert_eq!(rows.len(), 2, "{rows:?}");
}
