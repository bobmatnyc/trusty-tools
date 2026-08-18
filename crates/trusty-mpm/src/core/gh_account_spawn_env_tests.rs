//! Tests for the spawn-time `gh` identity resolver
//! (`resolve_gh_account_env_with`/`resolve_gh_account_env`/
//! `resolve_gh_account_env_for_registry`/`find_pinned_gh_identity` in
//! `gh_account.rs`) — #3025's `GH_TOKEN` minting and #5851's `GH_CONFIG_DIR`
//! selection.
//!
//! #5851 coverage: `config_dir_is_selected_over_a_minted_token` proves the
//! token path is skipped (not merely overridden) when a config dir is pinned;
//! `config_dir_without_credential_still_pins_and_warns` pins the deliberate
//! fail-CLOSED decision for an empty config dir; and
//! `pinned_config_dir_reaches_the_spawn_env` is the end-to-end fail-open check
//! that fails against the pre-fix code. No test here shells out to `gh` or
//! reads `~/.config/gh` — the config dirs are temp dirs and the `hosts.yml`
//! files are written by the fixtures.
//!
//! Why: split into a companion `_tests.rs` file (rather than growing the
//! large inline `#[cfg(test)] mod tests` in `gh_account.rs` itself) purely to
//! keep `gh_account.rs` — a production file capped at 500 SLOC — clear of
//! further growth; this file is classified as a test file by its `_tests.rs`
//! suffix. Every `resolve_gh_account_env_with` case exercises a FAKE token
//! resolver closure so no live `gh` login is ever required in CI; every
//! `find_pinned_gh_identity`/`resolve_gh_account_env_for_registry` case
//! exercises a REAL, temp-dir-backed `ProjectRegistry` fixture — this is the
//! #3025 review-follow-up coverage proving the registry, not the static
//! config file, is consulted.
//! What: the `resolve_gh_account_env_with` outcomes (no account, blank
//! account, success, resolver failure — the fail-open spawn contract; plus
//! #5851's config-dir arms), and the registry-matching coverage: a registered
//! project's pinned `gh_account` and `github.config_dir` ARE picked up for its
//! `repo_url`, an unregistered/mismatched `repo_url` is NOT, and a project
//! pinning neither yields `None`.
//! Test: itself (each `#[test]`/`#[tokio::test]` below is its own coverage unit).

use std::path::{Path, PathBuf};

use super::{
    GH_TOKEN_ENV_VAR, GH_USER_ENV_VAR, find_pinned_gh_identity,
    resolve_gh_account_env_for_registry, resolve_gh_account_env_with,
};
use crate::core::trusty_tools_config::GithubConfig;
use crate::project::Project;
use crate::project::ProjectRegistry;

/// The `GH_CONFIG_DIR` name, spelled out here rather than imported so a rename
/// on the production side cannot silently make these assertions vacuous.
const GH_CONFIG_DIR: &str = "GH_CONFIG_DIR";

/// Assert `vars` carries `name` exactly once, and return its value.
fn value_of(vars: &[(String, String)], name: &str) -> String {
    let matches: Vec<&(String, String)> = vars.iter().filter(|(k, _)| k == name).collect();
    assert_eq!(matches.len(), 1, "expected exactly one {name} in {vars:?}");
    matches[0].1.clone()
}

/// Write a `hosts.yml` naming `account` into `dir`, the shape `gh` writes for
/// a scoped config home that has been logged into.
fn write_hosts_yml(dir: &Path, account: &str) {
    let yaml = format!("github.com:\n    git_protocol: https\n    user: {account}\n");
    std::fs::write(dir.join("hosts.yml"), yaml).expect("write hosts.yml");
}

/// Why: `gh_account: None` (no pinning configured) must resolve to `None` —
/// nothing to inject, no regression for every project that never sets the
/// field.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_unset_is_none() {
    let result = resolve_gh_account_env_with(None, None, |_| Ok("unused".to_string()));
    assert!(result.is_none());
}

/// Why: a blank/whitespace-only `gh_account` must be treated the same as
/// unset — never invokes the resolver, never fabricates env vars from
/// nothing.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_blank_is_none() {
    let result = resolve_gh_account_env_with(Some("   "), None, |_| Ok("unused".to_string()));
    assert!(result.is_none());
}

/// Why: a successful resolution must inject BOTH `GH_TOKEN` (the minted
/// token) and `GH_USER` (the pinned account name) — in that order — so a
/// caller applying `vars` sequentially gets a deterministic env. #5851 leaves
/// this arm untouched: a project with no `github.config_dir` behaves exactly
/// as it did before.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_success_returns_token_and_user() {
    let result = resolve_gh_account_env_with(Some("bobmatnyc"), None, |account| {
        assert_eq!(account, "bobmatnyc");
        Ok("ghp_fake_token".to_string())
    });
    let env = result.expect("some").expect("ok");
    assert_eq!(
        env.vars,
        vec![
            (GH_TOKEN_ENV_VAR.to_string(), "ghp_fake_token".to_string()),
            (GH_USER_ENV_VAR.to_string(), "bobmatnyc".to_string()),
        ]
    );
    assert_eq!(env.warning, None);
}

/// Why: a resolver failure (account not logged in, `gh` missing, etc.) must
/// surface as `Some(Err(..))` — never panic, never silently fall through to
/// an empty/placeholder token — so the caller can log a clear warning and
/// proceed WITHOUT injecting `GH_TOKEN` (issue #3025's documented failure
/// mode).
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_failure_returns_err() {
    let result = resolve_gh_account_env_with(Some("bob-duetto"), None, |_| {
        Err("account 'bob-duetto' is not logged in".to_string())
    });
    let err = result.expect("some").expect_err("err");
    assert!(err.contains("bob-duetto"), "err: {err}");
}

/// Why: the account name passed to the resolver must be TRIMMED (leading/
/// trailing whitespace stripped) so a config value with incidental
/// whitespace still resolves correctly.
/// Test: itself.
#[test]
fn resolve_gh_account_env_with_trims_account_name() {
    let result = resolve_gh_account_env_with(Some("  bobmatnyc  "), None, |account| {
        assert_eq!(account, "bobmatnyc");
        Ok("tok".to_string())
    });
    let env = result.expect("some").expect("ok");
    assert!(
        env.vars
            .contains(&(GH_USER_ENV_VAR.to_string(), "bobmatnyc".to_string()))
    );
}

// ── #5851: `config_dir` selects, `gh auth token -u` does not ───────────────

/// Why (#5851, the core of the fix): `gh auth token -u <account>` returns the
/// SAME value for every logged-in account on a keyring-backed host, so a
/// project pinned to `github.config_dir` must be selected by `GH_CONFIG_DIR`
/// and must NOT also carry a `GH_TOKEN` — an env token outranks the scoped
/// config in `gh`'s own resolution order, which would make the config dir
/// decorative. The token resolver panics if called, proving the token path is
/// skipped entirely rather than merely overridden.
/// Test: itself.
#[test]
fn config_dir_is_selected_over_a_minted_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(dir.path(), "bobmatnyc");

    let result = resolve_gh_account_env_with(Some("bobmatnyc"), Some(dir.path()), |_| {
        panic!("the token path must not run when a config_dir is pinned")
    });
    let env = result.expect("some").expect("ok");

    assert_eq!(
        value_of(&env.vars, GH_CONFIG_DIR),
        dir.path().to_string_lossy()
    );
    assert!(
        !env.vars.iter().any(|(k, _)| k == GH_TOKEN_ENV_VAR),
        "GH_TOKEN must never accompany GH_CONFIG_DIR: {:?}",
        env.vars
    );
    assert_eq!(value_of(&env.vars, GH_USER_ENV_VAR), "bobmatnyc");
}

/// Why: a config dir naming an account carries a credential as far as this
/// layer can tell, so it must produce no warning — the warning has to mean
/// something when it does fire.
/// Test: itself.
#[test]
fn config_dir_with_credential_has_no_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(dir.path(), "bobmatnyc");
    let env = resolve_gh_account_env_with(Some("bobmatnyc"), Some(dir.path()), |_| {
        panic!("token path must not run")
    })
    .expect("some")
    .expect("ok");
    assert_eq!(env.warning, None);
}

/// Why (#5851, the decision on the no-credential case): a pinned config dir
/// that holds no credential makes every `gh` call exit 4. The deliberate
/// choice is to STILL pin it — dropping `GH_CONFIG_DIR` would hand the session
/// back to the machine-global account, which is the wrong-identity bug this
/// change exists to close — and to emit one warning that names the directory
/// and the scoped `gh auth login` that fixes it. This test fails if either
/// half regresses: a silent fall-back, or a mute failure.
/// Test: itself.
#[test]
fn config_dir_without_credential_still_pins_and_warns() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Deliberately no hosts.yml — the empty-config-dir case.
    let env = resolve_gh_account_env_with(Some("bobmatnyc"), Some(dir.path()), |_| {
        panic!("the token path must not be used as a fallback for an empty config dir")
    })
    .expect("some")
    .expect("ok");

    assert_eq!(
        value_of(&env.vars, GH_CONFIG_DIR),
        dir.path().to_string_lossy(),
        "an empty config dir must still be pinned — falling back is the fail-open defect"
    );
    assert!(!env.vars.iter().any(|(k, _)| k == GH_TOKEN_ENV_VAR));

    let warning = env.warning.expect("an empty config dir must warn");
    let shown = dir.path().display().to_string();
    assert!(
        warning.contains(&shown),
        "warning must name the dir: {warning}"
    );
    assert!(
        warning.contains("gh auth login"),
        "warning must say what to run: {warning}"
    );
}

/// Why: a whitespace-only `config_dir` is not a directory; it must be treated
/// as unset and fall through to the pre-#5851 token path, matching how
/// `gh_identity::resolve_gh_env` trims the same field.
/// Test: itself.
#[test]
fn blank_config_dir_falls_through_to_the_token_path() {
    let env = resolve_gh_account_env_with(Some("bobmatnyc"), Some(Path::new("   ")), |_| {
        Ok("tok".to_string())
    })
    .expect("some")
    .expect("ok");
    assert_eq!(value_of(&env.vars, GH_TOKEN_ENV_VAR), "tok");
    assert!(!env.vars.iter().any(|(k, _)| k == GH_CONFIG_DIR));
}

/// Why: a project can pin a config dir without naming an account (isolation
/// with no stated preference). `GH_CONFIG_DIR` must still be injected — it is
/// the selector — and `GH_USER`, which is informational only, must be absent
/// rather than fabricated.
/// Test: itself.
#[test]
fn config_dir_without_an_account_still_pins() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(dir.path(), "bobmatnyc");
    let env = resolve_gh_account_env_with(None, Some(dir.path()), |_| panic!("no token path"))
        .expect("some")
        .expect("ok");
    assert_eq!(
        value_of(&env.vars, GH_CONFIG_DIR),
        dir.path().to_string_lossy()
    );
    assert!(!env.vars.iter().any(|(k, _)| k == GH_USER_ENV_VAR));
}

/// Build a minimal `Project` fixture, mirroring the register-route shape.
fn project(name: &str, repo_url: &str, gh_account: Option<&str>) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: gh_account.map(str::to_string),
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// The same fixture with a `github.config_dir` binding attached (#5851).
fn project_with_config_dir(name: &str, repo_url: &str, config_dir: &Path) -> Project {
    Project {
        github: Some(GithubConfig {
            config_dir: Some(config_dir.to_path_buf()),
            ..GithubConfig::default()
        }),
        ..project(name, repo_url, None)
    }
}

/// Why (#3025 review follow-up, CRITICAL fix): this is the exact coverage
/// the review demanded — register a project (via the registry, the REAL
/// write target `tm projects register --gh-account`/the PATCH route/the MCP
/// tool all use) with a pinned `gh_account`, then assert the resolver picks
/// it up for that project's `repo_url`. Proves the registry — not the
/// static `TrustyToolsConfig` file the original implementation mistakenly
/// consulted — is the source of truth.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_picks_up_registered_gh_account() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "widget",
            "https://github.com/acme/widget",
            Some("bobmatnyc"),
        ))
        .await
        .expect("register");

    let found = find_pinned_gh_identity(&registry, "https://github.com/acme/widget")
        .await
        .expect("matched");
    assert_eq!(found.account.as_deref(), Some("bobmatnyc"));
    assert_eq!(found.config_dir, None);

    // Tolerates the `.git`-suffix/scheme variance `repo_url_matches` handles.
    let found_git_suffix = find_pinned_gh_identity(&registry, "https://github.com/acme/widget.git")
        .await
        .expect("matched");
    assert_eq!(found_git_suffix.account.as_deref(), Some("bobmatnyc"));
}

/// Why (#5851): `github.config_dir` was already persisted on the record and
/// already mirrored from config, but the spawn-env lookup never read it — that
/// omission is what forced every session down the non-discriminating
/// `gh auth token -u` path. This asserts the lookup now returns it.
/// Test: itself.
#[tokio::test]
async fn find_pinned_gh_identity_reads_config_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    let config_dir = PathBuf::from("/home/bob/.config/gh-bobmatnyc");
    registry
        .register(project_with_config_dir(
            "widget",
            "https://github.com/acme/widget",
            &config_dir,
        ))
        .await
        .expect("register");

    let found = find_pinned_gh_identity(&registry, "https://github.com/acme/widget")
        .await
        .expect("matched");
    assert_eq!(found.config_dir.as_deref(), Some(config_dir.as_path()));
}

/// Why: a `repo_url` that matches NO registered project must yield `None` —
/// not a panic, not a false-positive match against an unrelated project.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_no_match_is_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project(
            "widget",
            "https://github.com/acme/widget",
            Some("bobmatnyc"),
        ))
        .await
        .expect("register");

    let found = find_pinned_gh_identity(&registry, "https://github.com/acme/other-repo").await;
    assert_eq!(found, None);
}

/// Why: a registered project with NO `gh_account` set must yield `None` —
/// the no-regression case for every project that never pins one.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_registered_without_gh_account_is_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project("widget", "https://github.com/acme/widget", None))
        .await
        .expect("register");

    let found = find_pinned_gh_identity(&registry, "https://github.com/acme/widget").await;
    assert_eq!(found, None);
}

/// Why: a workspace with no git origin (a bare, non-git temp dir) must
/// resolve to an EMPTY vec end-to-end via `resolve_gh_account_env_for_registry`
/// itself — no regression for every workspace that predates #3025, and no
/// panic/hang even with an empty registry.
/// Test: itself.
#[tokio::test]
async fn resolve_gh_account_env_for_registry_no_origin_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let vars = resolve_gh_account_env_for_registry(&registry, workspace.path()).await;
    assert!(vars.is_empty(), "vars: {vars:?}");
}

/// `git init` a workspace and point its `origin` at `origin_url`.
fn workspace_with_origin(dir: &Path, origin_url: &str) {
    let init = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(dir)
        .output()
        .expect("git init");
    assert!(init.status.success(), "git init failed");
    let remote = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["remote", "add", "origin", origin_url])
        .output()
        .expect("git remote add");
    assert!(remote.status.success(), "git remote add failed");
}

/// FAIL-OPEN CHECK (#5851) — the blocking regression test.
///
/// Why: before this fix, `find_pinned_gh_account` read only `gh_account` and
/// never looked at `github.config_dir`, so a project pinned by a scoped `gh`
/// config home got NOTHING injected and its session ran under whatever account
/// was globally active. This drives the whole production path end to end — real
/// git origin, real temp-dir-backed registry, no `gh` subprocess anywhere —
/// and asserts `GH_CONFIG_DIR` reaches the spawn env. Against the pre-fix code
/// it returns an empty vec and this test fails on the first assertion; that is
/// the point of it.
/// What: registers a project whose `github.config_dir` is set and whose
/// `gh_account` is NOT, so neither the pre-fix nor the post-fix run can reach
/// `gh auth token`; asserts `GH_CONFIG_DIR` is present and `GH_TOKEN` is not.
/// Test: itself.
#[tokio::test]
async fn pinned_config_dir_reaches_the_spawn_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    let gh_home = tempfile::tempdir().expect("gh home tempdir");
    write_hosts_yml(gh_home.path(), "bobmatnyc");
    registry
        .register(project_with_config_dir(
            "widget",
            "https://github.com/acme/widget",
            gh_home.path(),
        ))
        .await
        .expect("register");

    let workspace = tempfile::tempdir().expect("workspace tempdir");
    workspace_with_origin(workspace.path(), "https://github.com/acme/widget.git");

    let vars = resolve_gh_account_env_for_registry(&registry, workspace.path()).await;
    assert_eq!(
        value_of(&vars, GH_CONFIG_DIR),
        gh_home.path().to_string_lossy(),
        "a project pinned by github.config_dir must have it injected at spawn"
    );
    assert!(
        !vars.iter().any(|(k, _)| k == GH_TOKEN_ENV_VAR),
        "GH_TOKEN must not accompany GH_CONFIG_DIR: {vars:?}"
    );
}

/// Why: a project that pins NEITHER key must still resolve to an empty vec via
/// the full registry path — the no-regression case for every project that
/// predates #3025/#5851, now proved with a real matching origin rather than
/// only the no-origin fall-through.
/// Test: itself.
#[tokio::test]
async fn registered_project_pinning_nothing_injects_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    registry
        .register(project("widget", "https://github.com/acme/widget", None))
        .await
        .expect("register");

    let workspace = tempfile::tempdir().expect("workspace tempdir");
    workspace_with_origin(workspace.path(), "https://github.com/acme/widget.git");

    let vars = resolve_gh_account_env_for_registry(&registry, workspace.path()).await;
    assert!(vars.is_empty(), "vars: {vars:?}");
}
