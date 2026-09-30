//! Tests for the #8934 remote-mode predicate and the local-only gh pin.

use std::path::Path;

use super::*;

/// `git init` `dir`, optionally pointing `origin` at `origin_url`.
fn repo(dir: &Path, origin_url: Option<&str>) {
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    if let Some(url) = origin_url {
        git(&["remote", "add", "origin", url]);
    }
}

/// Why: the three answers drive different code paths; each must be reported
/// as itself.
/// Test: itself.
#[test]
fn remote_mode_reads_origin_local_only_and_non_repo() {
    let with = tempfile::tempdir().expect("tempdir");
    repo(with.path(), Some("https://github.com/acme/widget.git"));
    assert_eq!(
        remote_mode(with.path()),
        Ok(RemoteMode::Origin(
            "https://github.com/acme/widget.git".into()
        ))
    );

    let local = tempfile::tempdir().expect("tempdir");
    repo(local.path(), None);
    let root = std::fs::canonicalize(local.path()).expect("canonical");
    assert_eq!(
        remote_mode(local.path()),
        Ok(RemoteMode::LocalOnly { root })
    );

    let plain = tempfile::tempdir().expect("tempdir");
    assert_eq!(remote_mode(plain.path()), Ok(RemoteMode::NotARepository));
}

/// Why: #4734 — a `.git` git cannot read must never read as "local-only",
/// which would route the spawn and pin the gh identity from a guess.
/// Test: itself.
#[cfg(unix)]
#[test]
fn remote_mode_propagates_an_unreadable_git_dir() {
    use std::os::unix::fs::PermissionsExt;
    let local = tempfile::tempdir().expect("tempdir");
    repo(local.path(), None);
    let git_dir = local.path().join(".git");
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let mode = remote_mode(local.path());
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o755)).expect("restore");
    assert!(mode.is_err(), "{mode:?}");
}

/// Why: the local-only spawn serves a repository root only; a subdirectory
/// and a GitHub checkout must both answer `false`.
/// Test: itself.
#[test]
fn local_only_root_is_the_repo_root_only() {
    let local = tempfile::tempdir().expect("tempdir");
    repo(local.path(), None);
    assert_eq!(is_local_only_root(local.path()), Ok(true));
    let sub = local.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir");
    assert_eq!(is_local_only_root(&sub), Ok(false));

    let with = tempfile::tempdir().expect("tempdir");
    repo(with.path(), Some("git@github.com:acme/widget.git"));
    assert_eq!(is_local_only_root(with.path()), Ok(false));
}

/// Why: the pin must select gh's config itself, so the launch strips every
/// inherited token var it does not set — an inherited `GITHUB_TOKEN` would
/// otherwise still reach a tool that reads it.
/// Test: itself.
#[test]
fn local_only_gh_env_strips_every_inherited_token() {
    let env = crate::core::gh_identity::GhEnv::local_only();
    let unset = env.unset_vars();
    for var in ["GITHUB_TOKEN", "GITHUB_ENTERPRISE_TOKEN", "GH_USER"] {
        assert!(unset.iter().any(|k| k == var), "{var} not cleared: {env:?}");
    }
    let value = |key: &str| {
        env.vars()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(value("GH_CONFIG_DIR"), Some(LOCAL_ONLY_GH_CONFIG_DIR));
    assert_eq!(value("GH_TOKEN"), Some(LOCAL_ONLY_GH_TOKEN));
    assert_eq!(value("GH_ENTERPRISE_TOKEN"), Some(LOCAL_ONLY_GH_TOKEN));
}

/// 🔴 #8934 MEDIUM 4: the pin clamps git's credential helpers too, as #8914's
/// `git_credential_pin` does, so HTTPS git cannot reach a stored credential.
/// Test: itself.
#[test]
fn local_only_pin_clamps_git_credentials() {
    let vars = local_only_gh_vars();
    let value = |key: &str| vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    assert_eq!(value("GIT_CONFIG_COUNT"), Some("1"), "{vars:?}");
    assert_eq!(value("GIT_CONFIG_KEY_0"), Some("credential.helper"));
    assert_eq!(value("GIT_CONFIG_VALUE_0"), Some(""));
    assert_eq!(value("GIT_TERMINAL_PROMPT"), Some("0"));
}

/// A no-origin repo whose `.trusty-mpm.toml` asks for `profile`, and its
/// canonical root.
fn local_repo_asking_for(profile: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    repo(dir.path(), None);
    let toml = format!("profile = \"{profile}\"\n");
    std::fs::write(dir.path().join(".trusty-mpm.toml"), toml).expect("toml");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    (dir, root)
}

/// 🔴 #8934 HIGH 2 (07:47Z ruling): an allow-listed supervisor's local-only
/// directory spawns with no gh pin — the Architect keeps the machine's gh.
/// Test: itself.
#[test]
fn an_allow_listed_supervisor_keeps_gh() {
    let (_dir, root) = local_repo_asking_for("supervisor");
    let mut config = MpmConfig::default();
    config.supervisor.projects = vec![root.clone()];
    assert!(!gh_disabled_for(&root, &config));
    assert_eq!(local_only_spawn_vars(&root, &config), Vec::new());
}

/// #8453: a project's own `.trusty-mpm.toml` cannot exempt itself.
/// Test: itself.
#[test]
fn a_self_declared_supervisor_does_not_keep_gh() {
    let (_dir, root) = local_repo_asking_for("supervisor");
    let config = MpmConfig::default();
    assert!(gh_disabled_for(&root, &config));
    assert_eq!(local_only_spawn_vars(&root, &config), local_only_gh_vars());
}

/// #8934 LOW 8: a rev-parse failure other than "not a git repository" (a
/// `safe.directory` refusal) is an error, never "not a repository".
/// Test: itself.
#[test]
fn a_git_failure_that_is_not_not_a_repository_is_an_error() {
    let path = Path::new("/r");
    let dubious = b"fatal: detected dubious ownership in repository at '/r'\n";
    assert!(classify_toplevel(path, false, b"", dubious).is_err());
    let absent = b"fatal: not a git repository (or any of the parent directories): .git\n";
    assert_eq!(
        classify_toplevel(path, false, b"", absent),
        Ok(RemoteMode::NotARepository)
    );
}

/// `git -C dir <args>`, asserting success.
fn run_git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@example.com", "-c", "user.name=T"])
        .args(args)
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git {args:?} failed");
}

/// 🔴 #8934 MEDIUM 6: with the root on a feature branch, a local-only
/// worktree is still cut from the default branch.
/// Test: itself.
#[test]
fn local_default_branch_ignores_a_checked_out_feature_branch() {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "one"]);
    run_git(dir.path(), &["checkout", "-q", "-b", "feature"]);
    assert_eq!(local_default_branch(dir.path()), Ok("main".to_string()));
}

/// #8934 MEDIUM 6: no default branch resolves → a clear refusal.
/// Test: itself.
#[test]
fn local_default_branch_refuses_when_none_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "zz-8934-feature"]);
    run_git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "one"]);
    let err = local_default_branch(dir.path()).expect_err("no default branch");
    assert!(err.contains("no local default branch"), "{err}");
}
