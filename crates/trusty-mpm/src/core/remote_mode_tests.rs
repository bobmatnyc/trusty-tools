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
    assert_eq!(remote_mode(local.path()), Ok(RemoteMode::LocalOnly));

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
    for (k, v) in env.vars() {
        let want = if k == "GH_CONFIG_DIR" {
            LOCAL_ONLY_GH_CONFIG_DIR
        } else {
            LOCAL_ONLY_GH_TOKEN
        };
        assert_eq!(v, want, "{k}");
    }
    assert_eq!(env.vars().len(), 3, "{env:?}");
}
