//! Unit tests for the registry-keyed worktree-isolation decision (#3455, #4300).
//!
//! Why: these pin the two defaults the whole opt-out rests on — an unmatched
//! origin resolves to `true` (no regression for the 31 of 33 registered
//! projects that carry no `worktree` key) and a matched `worktree: false`
//! resolves to `false` — plus the out-of-process (`_at`) reader the CLI paths
//! added by #4300 depend on.
//! What: pure-core cases against [`super::worktree_enabled_in`], live-registry
//! cases against [`super::worktree_enabled_for_origin`], and on-disk cases
//! against [`super::worktree_enabled_for_origin_at`].
//! Test: this file.

use std::path::Path;

use super::{
    REGISTRY_DIR_NAME, registry_data_dir_under, worktree_enabled_for_origin,
    worktree_enabled_for_origin_at, worktree_enabled_in,
};
use crate::project::{Project, ProjectRegistry};

/// Build a `Project` with only the fields these tests care about.
///
/// Why: `Project` has eleven fields and no `Default`; spelling them out in
/// every test would bury the two that matter (`repo_url`, `worktree`).
/// What: a project named `name` at `repo_url` with the given opt-out value.
/// Test: used by every test below.
fn project(name: &str, repo_url: &str, worktree: Option<bool>) -> Project {
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
        worktree,
    }
}

/// An origin that matches no registered project keeps worktree isolation ON.
/// Test: itself.
#[test]
fn worktree_enabled_in_defaults_true() {
    let projects = vec![project(
        "writing",
        "https://github.com/bobmatnyc/writing",
        Some(false),
    )];
    assert!(
        worktree_enabled_in(&projects, "https://github.com/acme/unregistered"),
        "an unmatched origin must default to worktree isolation ON"
    );
    assert!(
        worktree_enabled_in(&[], "https://github.com/acme/anything"),
        "an EMPTY registry must default to worktree isolation ON"
    );
}

/// A matched project's explicit `worktree` value wins; `None` still means ON.
/// Test: itself.
#[test]
fn worktree_enabled_in_honors_false() {
    let opted_out = vec![project(
        "cto",
        "https://github.com/bob-duetto/cto",
        Some(false),
    )];
    assert!(
        !worktree_enabled_in(&opted_out, "https://github.com/bob-duetto/cto"),
        "worktree: false must disable isolation for the matched project"
    );

    let unset = vec![project("cto", "https://github.com/bob-duetto/cto", None)];
    assert!(
        worktree_enabled_in(&unset, "https://github.com/bob-duetto/cto"),
        "an absent `worktree` key must resolve to ON (unwrap_or(true))"
    );

    let opted_in = vec![project(
        "cto",
        "https://github.com/bob-duetto/cto",
        Some(true),
    )];
    assert!(
        worktree_enabled_in(&opted_in, "https://github.com/bob-duetto/cto"),
        "worktree: true must resolve to ON"
    );
}

/// Matching goes through `repo_url_matches`, so SSH/HTTPS/`.git` forms of the
/// SAME repo all resolve to the same answer — and a different repo does not.
/// Test: itself.
#[test]
fn worktree_enabled_in_matches_ssh_form() {
    let projects = vec![project(
        "writing",
        "https://github.com/bobmatnyc/writing",
        Some(false),
    )];
    for origin in [
        "git@github.com:bobmatnyc/writing.git",
        "https://github.com/bobmatnyc/writing.git",
        "https://github.com/BobMatNYC/Writing",
    ] {
        assert!(
            !worktree_enabled_in(&projects, origin),
            "{origin} must match the registered project's opt-out"
        );
    }
    assert!(
        worktree_enabled_in(&projects, "https://github.com/bobmatnyc/trusty-tools"),
        "a DIFFERENT repo must be unaffected by another project's opt-out"
    );
}

/// A project with no registry entry defaults to worktree isolation ON — the
/// no-regression default #3455 requires (moved here from `lifecycle_tests.rs`
/// when #4300 hoisted the decision out of `daemon::managed_routes`).
/// Test: itself.
#[tokio::test]
async fn worktree_enabled_for_origin_defaults_true_when_unregistered() {
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    assert!(
        worktree_enabled_for_origin(&registry, "https://github.com/acme/unregistered").await,
        "an unregistered repo must default to worktree isolation ON"
    );
}

/// A registered project with `worktree: Some(false)` disables isolation for
/// its own `repo_url` only — a DIFFERENT repo is unaffected (#3455).
/// Test: itself.
#[tokio::test]
async fn worktree_enabled_for_origin_honors_registered_false() {
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "writing",
            "https://github.com/bobmatnyc/writing",
            Some(false),
        ))
        .await
        .expect("register writing project");

    assert!(
        !worktree_enabled_for_origin(&registry, "https://github.com/bobmatnyc/writing.git").await,
        "the registered project's opt-out must be honored (repo_url matching tolerates .git suffix)"
    );
    assert!(
        worktree_enabled_for_origin(&registry, "https://github.com/bobmatnyc/trusty-tools").await,
        "a DIFFERENT repo must be unaffected by another project's opt-out"
    );
}

/// The out-of-process reader (#4300) sees an opt-out written by ANOTHER
/// process — the CLI's whole reason for existing on this path.
/// Test: itself.
#[tokio::test]
async fn worktree_enabled_at_reads_registry_from_disk() {
    let dir = crate::test_support::hermetic_temp_dir();

    // Writer "process": register the opt-out and drop the handle entirely.
    {
        let writer = ProjectRegistry::load(dir.path())
            .await
            .expect("load writer");
        writer
            .register(project(
                "writing",
                "https://github.com/bobmatnyc/writing",
                Some(false),
            ))
            .await
            .expect("register");
        writer
            .register(project(
                "trusty-tools",
                "https://github.com/bobmatnyc/trusty-tools",
                None,
            ))
            .await
            .expect("register");
    }

    assert!(
        !worktree_enabled_for_origin_at(dir.path(), "git@github.com:bobmatnyc/writing.git").await,
        "a reader with no daemon state must see the on-disk opt-out"
    );
    assert!(
        worktree_enabled_for_origin_at(dir.path(), "https://github.com/bobmatnyc/trusty-tools")
            .await,
        "a project with no `worktree` key must still resolve to ON"
    );
}

/// An absent registry directory is the fresh-install case: isolation stays ON.
/// Test: itself.
#[tokio::test]
async fn worktree_enabled_at_defaults_true_when_registry_absent() {
    let dir = crate::test_support::hermetic_temp_dir();
    let missing = dir.path().join("does-not-exist");
    assert!(
        !missing.exists(),
        "fixture precondition: {} must not exist",
        missing.display()
    );

    assert!(
        worktree_enabled_for_origin_at(&missing, "https://github.com/bobmatnyc/writing").await,
        "an absent registry must never disable worktree isolation"
    );
}

/// A corrupt `projects.json` must fail SAFE (isolation ON), never silently
/// strip a project of its worktrees.
/// Test: itself.
#[tokio::test]
async fn worktree_enabled_at_defaults_true_on_malformed_registry() {
    let dir = crate::test_support::hermetic_temp_dir();
    std::fs::write(dir.path().join("projects.json"), "{ not json").expect("write malformed");

    assert!(
        worktree_enabled_for_origin_at(dir.path(), "https://github.com/bobmatnyc/writing").await,
        "a malformed registry must default to worktree isolation ON"
    );
}

/// The CLI and the daemon must derive the SAME registry directory from a
/// framework root — a divergence here would make the CLI read an empty
/// registry and answer `true` for every project, silently re-opening #4300.
/// Test: itself.
#[test]
fn registry_data_dir_under_matches_daemon_layout() {
    let root = Path::new("/tmp/tm-test-framework-root/.trusty-mpm");
    assert_eq!(
        registry_data_dir_under(root),
        root.join(REGISTRY_DIR_NAME),
        "registry dir must be <framework_root>/{REGISTRY_DIR_NAME}"
    );
    assert_eq!(
        REGISTRY_DIR_NAME, "project-registry",
        "the directory name is the daemon's on-disk layout — changing it \
         orphans every existing projects.json"
    );
}
