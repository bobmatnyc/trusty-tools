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
    worktree_enabled_for_origin_at, worktree_enabled_for_project, worktree_enabled_in,
    worktree_override_in_project,
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

/// Freeze the on-disk directory name `projects.json` lives in.
///
/// Why: this is a COMPATIBILITY pin, not a divergence check. Renaming
/// `project-registry` orphans every existing `~/.trusty-mpm/project-registry/
/// projects.json` — 33 registered projects on the maintainer's machine —
/// which would present as "every project silently forgot its settings",
/// including its `worktree` opt-out.
/// What it does NOT pin (stated plainly rather than implied): that the daemon
/// and the CLI agree on the path. An earlier revision asserted
/// `registry_data_dir_under(root) == root.join(REGISTRY_DIR_NAME)`, which is a
/// tautology — it restates the one-line function body. Agreement is instead
/// STRUCTURAL: `DaemonState::project_registry` and `registry_data_dir` both
/// call `registry_data_dir_under`, so there is no second copy of the join to
/// drift. The function is the protection; this test only guards the literal.
/// Test: itself.
#[test]
fn registry_dir_name_is_frozen() {
    assert_eq!(
        REGISTRY_DIR_NAME, "project-registry",
        "renaming this orphans every existing projects.json on disk"
    );
    // The literal must also be what a caller actually lands on — a path
    // component, not a nested or absolute path that would escape the root.
    let root = Path::new("/tmp/tm-test-framework-root/.trusty-mpm");
    let resolved = registry_data_dir_under(root);
    assert_eq!(
        resolved.strip_prefix(root).ok(),
        Some(Path::new("project-registry")),
        "the registry dir must be a single component directly under the \
         framework root, got {}",
        resolved.display()
    );
}

// ── Project-level config layer (#5207) ───────────────────────────────────────
//
// Why these five: they pin the full precedence chain the owner ruling
// introduces — project config > registry > built-in `true` — plus the two
// failure modes that decide whether the feature is safe to commit to a shared
// repo (a rejected file, and a clone-based spawn that has no project dir).

/// A project directory carrying `.trusty-mpm.toml` with the given body.
///
/// Why: every case below needs a real on-disk project root, since the whole
/// point of this layer is that the setting travels with the checkout.
/// What: a `TempDir` whose root holds the committed config file.
fn project_dir_with_config(body: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    std::fs::write(
        dir.path()
            .join(crate::core::project_config::PROJECT_CONFIG_FILE),
        body,
    )
    .expect("write project config");
    dir
}

/// The project's committed config OUTRANKS the machine-global registry (#5207).
///
/// Why: this is the ruling itself — "configuration should be at the project
/// level". The registry says isolation is ON for this repo; the repo says OFF,
/// and the repo wins. Without the project layer this resolves to `true`.
/// Test: itself.
#[tokio::test]
async fn worktree_project_config_overrides_registry() {
    let proj = project_dir_with_config("worktree = false\n");
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "widget",
            "https://github.com/acme/widget",
            Some(true),
        ))
        .await
        .expect("register");

    assert!(
        !worktree_enabled_for_project(proj.path(), &registry, "https://github.com/acme/widget")
            .await,
        "the project's committed config must outrank the machine-global registry"
    );
}

/// The project layer wins in BOTH directions, not just toward the opt-out.
///
/// Why: a precedence rule that only ever moves one way is indistinguishable
/// from an `if disabled_anywhere` short-circuit. A project that requires
/// isolation must be able to force it back ON over an operator's local opt-out.
/// Test: itself.
#[tokio::test]
async fn worktree_project_config_can_force_isolation_on() {
    let proj = project_dir_with_config("worktree = true\n");
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "widget",
            "https://github.com/acme/widget",
            Some(false),
        ))
        .await
        .expect("register");

    assert!(
        worktree_enabled_for_project(proj.path(), &registry, "https://github.com/acme/widget")
            .await,
        "a project that requires isolation must override a local registry opt-out"
    );
}

/// With NO project config, the registry still decides — no regression (#5207).
///
/// Why: almost every project has no `.trusty-mpm.toml`. Adding the layer must
/// leave all of them on exactly the pre-#5207 behaviour.
/// Test: itself.
#[tokio::test]
async fn worktree_project_config_absent_falls_back_to_registry() {
    let proj = tempfile::TempDir::new().expect("tempdir");
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "writing",
            "https://github.com/bobmatnyc/writing",
            Some(false),
        ))
        .await
        .expect("register");

    assert!(
        !worktree_enabled_for_project(
            proj.path(),
            &registry,
            "https://github.com/bobmatnyc/writing"
        )
        .await,
        "with no project config the registry opt-out must still apply"
    );
}

/// With NEITHER layer deciding, the built-in `true` stands (#3455 default).
///
/// Why: the floor of the chain. An unregistered repo with no project config
/// gets worktree isolation, exactly as before #3455.
/// Test: itself.
#[tokio::test]
async fn worktree_defaults_true_with_neither_layer() {
    let proj = tempfile::TempDir::new().expect("tempdir");
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");

    assert!(
        worktree_enabled_for_project(
            proj.path(),
            &registry,
            "https://github.com/acme/unregistered"
        )
        .await,
        "neither layer deciding must leave the built-in `true` default intact"
    );
}

/// A REJECTED project config contributes nothing and falls through (#5207).
///
/// Why: this is the runtime half of the `deny_unknown_fields` contract. The
/// file is committed, so one bad push reaches every operator at once — it must
/// not brick their spawns, and it must not half-apply either. `worktre` is a
/// typo, so the whole file is discarded and the registry answers instead. Note
/// what this proves: the typo did NOT silently read as `worktree = false`.
/// Test: itself.
#[tokio::test]
async fn worktree_project_config_malformed_falls_back_to_registry() {
    let proj = project_dir_with_config("worktre = false\n");
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "widget",
            "https://github.com/acme/widget",
            Some(true),
        ))
        .await
        .expect("register");

    assert!(
        worktree_enabled_for_project(proj.path(), &registry, "https://github.com/acme/widget")
            .await,
        "a rejected project config must contribute nothing, leaving the registry to decide"
    );
    assert_eq!(
        worktree_override_in_project(proj.path()),
        None,
        "a file that fails deny_unknown_fields must yield no override at all"
    );
}

/// The clone-based branch — no project dir on disk — still resolves (#5207).
///
/// Why: `spawn_managed_routed`'s clone branch asks the question BEFORE the
/// project exists anywhere, so it keeps calling the registry-only entry point.
/// This pins that the pre-existing path is untouched, and that pointing the
/// project-aware resolver at a non-existent directory degrades to the registry
/// rather than panicking.
/// Test: itself.
#[tokio::test]
async fn worktree_clone_branch_resolves_without_a_project_dir() {
    let dir = crate::test_support::hermetic_temp_dir();
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "writing",
            "https://github.com/bobmatnyc/writing",
            Some(false),
        ))
        .await
        .expect("register");

    // The clone branch's own entry point is unchanged.
    assert!(
        !worktree_enabled_for_origin(&registry, "https://github.com/bobmatnyc/writing").await,
        "the clone-based branch must keep resolving from the registry alone"
    );

    // And the project-aware resolver must not panic or invent an answer when
    // handed a path that does not exist yet.
    let missing = dir.path().join("not-cloned-yet");
    assert!(
        !worktree_enabled_for_project(&missing, &registry, "https://github.com/bobmatnyc/writing")
            .await,
        "a non-existent project dir must fall through to the registry"
    );
}
