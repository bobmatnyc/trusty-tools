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
use crate::core::gh_account_dir::gh_account_dir_tests::{TableCheck, TableProbe, migrated_dir};
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

/// 🔴 #5850 REGRESSION: an unpinned duplicate record for the same repository
/// must not hide the pinned one from a session spawn.
///
/// Why this shape: `ProjectRegistry::list` returns records in `HashMap` order,
/// so the old first-match lookup picked an arbitrary record. Sixteen unpinned
/// duplicates make that pick land on an unpinned record almost every run
/// (16 in 17), which is enough to fail against the first-match code.
/// Test: itself.
#[tokio::test]
async fn find_pinned_gh_identity_skips_an_unpinned_duplicate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    for i in 0..16 {
        registry
            .register(project(
                &format!("widget-{i:02}"),
                "https://github.com/acme/widget.git",
                None,
            ))
            .await
            .expect("register");
    }
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
        .expect("the pinned record must be found past its unpinned duplicates");
    assert_eq!(found.account.as_deref(), Some("bobmatnyc"));
}

/// 🔴 #5850 REGRESSION: two records pinning the SAME login give the session the
/// one carrying a `config_dir`, not the global account.
///
/// Why: the `seed_from_config` record (login only) and the `tm --user` record
/// (login plus scoped dir) are one identity. Treating them as a conflict spawned
/// the session unpinned.
/// Test: itself.
#[tokio::test]
async fn find_pinned_gh_identity_prefers_the_config_dir_pin_for_one_login() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    let config_dir = PathBuf::from("/home/bob/.config/gh-bob-duetto");
    registry
        .register(project(
            "jev",
            "https://github.com/acme/widget",
            Some("bob-duetto"),
        ))
        .await
        .expect("register");
    registry
        .register(Project {
            gh_account: Some("bob-duetto".to_string()),
            ..project_with_config_dir(
                "jev-matching",
                "https://github.com/acme/widget",
                &config_dir,
            )
        })
        .await
        .expect("register");

    let found = find_pinned_gh_identity(&registry, "https://github.com/acme/widget")
        .await
        .expect("one login must resolve to a pin");
    assert_eq!(found.account.as_deref(), Some("bob-duetto"));
    assert_eq!(found.config_dir.as_deref(), Some(config_dir.as_path()));
}

/// 🔴 #5850 REGRESSION: a no-login `config_dir` record next to a login record on
/// the same dir gives the session that dir AND the login.
///
/// Why: the login keeps `configured_account_pair` enforcement armed; losing it,
/// or refusing the pair, spawned the session as the global account.
/// Test: itself.
#[tokio::test]
async fn find_pinned_gh_identity_inherits_the_login_for_a_no_login_config_dir_pin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    let config_dir = PathBuf::from("/home/bob/.config/gh-bob-duetto");
    registry
        .register(Project {
            gh_account: None,
            ..project_with_config_dir("jev", "https://github.com/acme/widget", &config_dir)
        })
        .await
        .expect("register");
    registry
        .register(Project {
            gh_account: Some("bob-duetto".to_string()),
            github: Some(GithubConfig {
                config_dir: Some(config_dir.clone()),
                account: Some("bob-duetto".to_string()),
                ..GithubConfig::default()
            }),
            ..project("widget", "https://github.com/acme/widget", None)
        })
        .await
        .expect("register");

    let found = find_pinned_gh_identity(&registry, "https://github.com/acme/widget")
        .await
        .expect("a missing login is not a disagreement");
    assert_eq!(found.account.as_deref(), Some("bob-duetto"));
    assert_eq!(found.config_dir.as_deref(), Some(config_dir.as_path()));
}

/// 🔴 #5850: two records pinning DIFFERENT accounts yield no pin for a session,
/// where the daemon refuses the same registry — neither side guesses.
/// Test: itself.
#[tokio::test]
async fn find_pinned_gh_identity_refuses_disagreeing_pins() {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = ProjectRegistry::load(dir.path()).await.expect("load");
    for (name, account) in [("widget-a", "bobmatnyc"), ("widget-b", "bob-duetto")] {
        registry
            .register(project(
                name,
                "https://github.com/acme/widget",
                Some(account),
            ))
            .await
            .expect("register");
    }
    let found = find_pinned_gh_identity(&registry, "https://github.com/acme/widget").await;
    assert_eq!(
        found, None,
        "disagreeing pins must not be resolved by position"
    );
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

// ── #8510: an account-only spawn pin never falls back to "no identity" ─────

/// A repository whose registry record pins only an account.
const SPAWN_ORIGIN: &str = "https://github.com/duettoresearch/jev-matching";

/// An Enterprise Server repository whose registry record pins only an account.
const GHES_ORIGIN: &str = "https://ghe.corp/duettoresearch/jev-matching";

/// The account-only `bob-duetto` pin.
fn account_only_pin() -> super::PinnedGhIdentity {
    super::PinnedGhIdentity {
        account: Some("bob-duetto".into()),
        config_dir: None,
    }
}

/// Spawn the account-only pin for `origin`, proving through one candidate
/// dir with `probe` and `check` — the production prover over table fakes.
fn spawn_with(
    origin: &str,
    dir: &Path,
    probe: &TableProbe,
    check: &TableCheck,
) -> super::GhSpawnEnv {
    let sources = crate::core::gh_account_dir::AccountDirSources {
        own_config_dir: Some(dir.to_path_buf()),
        ..Default::default()
    };
    let prover = crate::core::gh_account_proof::AccountProver {
        sources: &sources,
        probe,
        check,
    };
    super::pinned_spawn_env(&account_only_pin(), origin, |login| {
        prover
            .prove(login, origin)
            .map_err(|reasons| reasons.join("; "))
    })
    .expect("a pin must produce an env")
    .expect("the spawn env never errs")
}

/// A github.com candidate dir whose `-u bob-duetto` token authenticates as
/// `who`; `who: Err` is a failed `GET /user`.
fn github_spawn(who: Result<&str, &str>) -> super::GhSpawnEnv {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let probe = TableProbe::default().answer(&dir, "github.com", "bob-duetto", Ok("tok-bob"));
    let check = TableCheck::default().answer("https://api.github.com", "tok-bob", who);
    spawn_with(SPAWN_ORIGIN, &dir, &probe, &check)
}

/// An account-only pin spawns with the proven token itself, never a config
/// dir, and with the nobody-token for the other host class.
/// Test: itself.
#[test]
fn an_account_only_spawn_pin_gets_the_proven_token() {
    let env = github_spawn(Ok("bob-duetto"));
    assert_eq!(value_of(&env.vars, "GH_TOKEN"), "tok-bob");
    assert_eq!(
        value_of(&env.vars, "GH_ENTERPRISE_TOKEN"),
        super::REFUSED_GH_TOKEN
    );
    assert_eq!(value_of(&env.vars, "GH_USER"), "bob-duetto");
    assert!(
        !env.vars.iter().any(|(k, _)| k == GH_CONFIG_DIR),
        "{:?}",
        env.vars
    );
    assert!(env.warning.is_none(), "{:?}", env.warning);
}

/// 🔴 #8510: an account-only pin with no proven token must NOT spawn with no
/// identity (the global account). Both token variables get the nobody-token,
/// and a warning names the reason and the fix.
/// Test: itself.
#[test]
fn an_account_only_spawn_pin_with_no_proven_token_fails_closed() {
    let env = super::pinned_spawn_env(&account_only_pin(), SPAWN_ORIGIN, |_| {
        Err("/x: the token gh returned for 'bob-duetto' authenticates as 'bobmatnyc'".into())
    })
    .expect("a pinned account must never resolve to no identity")
    .expect("the refusal rides the env, not an error");
    for var in ["GH_TOKEN", "GH_ENTERPRISE_TOKEN"] {
        assert_eq!(value_of(&env.vars, var), super::REFUSED_GH_TOKEN, "{var}");
    }
    let warning = env.warning.expect("the refusal must be logged");
    assert!(
        warning.contains("authenticates as 'bobmatnyc'")
            && warning.contains(&format!(
                "tm projects register <name> --repo-url {SPAWN_ORIGIN} --gh-account bob-duetto \
                 --gh-config-dir <dir>"
            )),
        "got: {warning}"
    );
}

/// A pinned `config_dir` is used as-is; no token is ever proven.
/// Test: itself.
#[test]
fn a_config_dir_spawn_pin_never_asks_to_prove() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_hosts_yml(dir.path(), "bob-duetto");
    let pinned = super::PinnedGhIdentity {
        account: Some("bob-duetto".into()),
        config_dir: Some(dir.path().to_path_buf()),
    };
    let env = super::pinned_spawn_env(&pinned, SPAWN_ORIGIN, |_| {
        panic!("a pinned config_dir must not be replaced by a proven token")
    })
    .expect("a pin must produce an env")
    .expect("the spawn env never errs");
    assert_eq!(
        value_of(&env.vars, GH_CONFIG_DIR),
        dir.path().to_string_lossy()
    );
}

/// 🔴 #8510 CRITICAL: `GET /user` says the token is another account's: the
/// nobody-token, and the warning names no token.
/// Test: itself.
#[test]
fn a_spawn_pin_refuses_a_token_for_another_account() {
    let env = github_spawn(Ok("bobmatnyc"));
    assert_eq!(value_of(&env.vars, "GH_TOKEN"), super::REFUSED_GH_TOKEN);
    let warning = env.warning.expect("the refusal must be logged");
    assert!(
        warning.contains("authenticates as 'bobmatnyc'") && !warning.contains("tok-"),
        "got: {warning}"
    );
}

/// 🔴 #8510 MEDIUM: the token lookup succeeds but `GET /user` times out: not
/// proven, so the nobody-token — never the unchecked token.
/// Test: itself.
#[test]
fn a_spawn_pin_refuses_when_the_user_check_fails() {
    let env = github_spawn(Err(
        "GET https://api.github.com/user did not answer in time",
    ));
    assert_eq!(value_of(&env.vars, "GH_TOKEN"), super::REFUSED_GH_TOKEN);
    let warning = env.warning.expect("the refusal must be logged");
    assert!(warning.contains("did not answer in time"), "got: {warning}");
}

/// 🔴 #8510 HIGH: gh on an Enterprise Server host reads `GH_ENTERPRISE_TOKEN`
/// and ignores `GH_TOKEN`, so the proven token goes there, and `GH_TOKEN` gets
/// the nobody-token so it never reaches api.github.com.
/// Test: itself.
#[test]
fn a_ghes_spawn_pin_puts_the_token_in_gh_enterprise_token() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let probe = TableProbe::default().answer(&dir, "ghe.corp", "bob-duetto", Ok("tok-ghe"));
    let check =
        TableCheck::default().answer("https://ghe.corp/api/v3", "tok-ghe", Ok("bob-duetto"));
    let env = spawn_with(GHES_ORIGIN, &dir, &probe, &check);
    assert_eq!(value_of(&env.vars, "GH_ENTERPRISE_TOKEN"), "tok-ghe");
    assert_eq!(value_of(&env.vars, "GH_TOKEN"), super::REFUSED_GH_TOKEN);
    assert!(env.warning.is_none(), "{:?}", env.warning);
}

/// 🔴 #8510 HIGH: an Enterprise Server pin with no proven token fails closed
/// on that host too — `GH_ENTERPRISE_TOKEN` is the nobody-token.
/// Test: itself.
#[test]
fn a_ghes_spawn_pin_refusal_blanks_both_token_vars() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let probe = TableProbe::default().answer(&dir, "ghe.corp", "bob-duetto", Ok("tok-ghe"));
    let check = TableCheck::default().answer("https://ghe.corp/api/v3", "tok-ghe", Ok("other"));
    let env = spawn_with(GHES_ORIGIN, &dir, &probe, &check);
    for var in ["GH_TOKEN", "GH_ENTERPRISE_TOKEN"] {
        assert_eq!(value_of(&env.vars, var), super::REFUSED_GH_TOKEN, "{var}");
    }
}

/// 🔴 #8510 LOW: a pinned project whose identity task panicked fails closed
/// instead of spawning with no vars (the global account).
/// Test: itself.
#[tokio::test]
async fn a_panicked_spawn_env_task_fails_closed() {
    let joined = tokio::task::spawn_blocking(|| -> Vec<(String, String)> {
        panic!("identity task panicked")
    })
    .await;
    let vars = super::joined_spawn_vars(
        joined,
        &account_only_pin(),
        SPAWN_ORIGIN,
        Path::new("/work/jev-matching"),
    );
    for var in ["GH_TOKEN", "GH_ENTERPRISE_TOKEN"] {
        assert_eq!(value_of(&vars, var), super::REFUSED_GH_TOKEN, "{var}");
    }
    assert_eq!(value_of(&vars, "GH_USER"), "bob-duetto");
}

/// A `tracing` writer that appends every formatted line to a shared buffer.
#[derive(Clone, Default)]
struct LogBuffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run a github.com spawn whose token authenticates as `who` through the
/// production logger under a capturing subscriber; return the injected vars
/// and every log line.
fn logged_spawn(who: Result<&str, &str>) -> (Vec<(String, String)>, String) {
    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let env = github_spawn(who);
    let vars = tracing::subscriber::with_default(subscriber, || {
        super::log_spawn_env(Some(Ok(env)), Path::new("/work/jev-matching"))
    });
    let bytes = buffer
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    (vars, String::from_utf8(bytes).expect("utf-8 log"))
}

/// 🔴 #8510: no arm writes a token to the log — neither the injected proven
/// token nor the unproven one a refusal rejected. The refusals still log their
/// reason, so the capture is not vacuous.
/// Test: itself.
#[test]
fn a_spawn_pin_logs_no_token_in_any_arm() {
    let (vars, log) = logged_spawn(Ok("bob-duetto"));
    assert_eq!(value_of(&vars, "GH_TOKEN"), "tok-bob");
    assert!(!log.contains("tok-"), "a token reached the log: {log}");

    for who in [
        Ok("bobmatnyc"),
        Err("GET https://api.github.com/user answered HTTP 401"),
    ] {
        let (vars, log) = logged_spawn(who);
        assert_eq!(value_of(&vars, "GH_TOKEN"), super::REFUSED_GH_TOKEN);
        assert!(
            log.contains("pinned to gh account 'bob-duetto'"),
            "the refusal must be logged: {log}"
        );
        assert!(!log.contains("tok-"), "a token reached the log: {log}");
    }
}
