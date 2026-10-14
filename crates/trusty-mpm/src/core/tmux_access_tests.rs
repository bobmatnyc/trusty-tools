//! Tests for `core::tmux_access` (#9557). Pure: every input is passed in, and
//! the only filesystem state is each test's own temp dir.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::*;

/// A sandbox data dir with a private `tmux` dir inside it.
fn sandbox_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let tmux = data.join("tmux");
    std::fs::create_dir_all(&tmux).unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
    (root, data, tmux)
}

fn sandbox_env<'a>(data: &'a Path, tmux_tmpdir: Option<&'a Path>) -> TmuxEnv<'a> {
    TmuxEnv {
        sandbox_flag: Some(OsStr::new("1")),
        tmux: None,
        tmux_tmpdir,
        data_dir_override: Some(data),
    }
}

fn host_allows() -> HostStateAccess {
    HostStateAccess::RealEnvironment
}

#[test]
fn outside_a_sandbox_the_host_verdict_decides() {
    let (_root, data, tmux) = sandbox_dirs();
    for flag in [None, Some(OsStr::new("0")), Some(OsStr::new("true"))] {
        let env = TmuxEnv {
            sandbox_flag: flag,
            ..sandbox_env(&data, Some(&tmux))
        };
        assert_eq!(
            classify_tmux_access(&env, host_allows),
            TmuxAccess::Host(HostStateAccess::RealEnvironment),
            "flag {flag:?}"
        );
    }
}

#[test]
fn a_sandbox_with_its_own_socket_dir_is_allowed() {
    let (_root, data, tmux) = sandbox_dirs();
    let verdict = classify_tmux_access(&sandbox_env(&data, Some(&tmux)), || {
        panic!("the host verdict must not decide a sandbox")
    });
    assert_eq!(
        verdict,
        TmuxAccess::SandboxServer {
            tmux_tmpdir: tmux.canonicalize().unwrap()
        }
    );
    assert!(verdict.is_allowed());
    assert_eq!(verdict.skip_reason(), None);
}

/// Why: `TRUSTY_MPM_ALLOW_HOST_STATE=1` or a real `$HOME` must not put a
/// sandbox on the default socket.
#[test]
fn the_sandbox_flag_overrides_a_host_allow() {
    let (_root, data, _tmux) = sandbox_dirs();
    let verdict = classify_tmux_access(&sandbox_env(&data, None), || HostStateAccess::OptedIn);
    assert!(!verdict.is_allowed(), "{verdict:?}");
}

#[test]
fn refuses_when_tmux_is_set() {
    let (_root, data, tmux) = sandbox_dirs();
    let env = TmuxEnv {
        tmux: Some(OsStr::new("/tmp/tmux-1/default,1,0")),
        ..sandbox_env(&data, Some(&tmux))
    };
    let verdict = classify_tmux_access(&env, host_allows);
    assert!(matches!(&verdict, TmuxAccess::SandboxRefused { detail } if detail.contains("$TMUX")));
}

#[test]
fn refuses_without_tmux_tmpdir() {
    let (_root, data, _tmux) = sandbox_dirs();
    let verdict = classify_tmux_access(&sandbox_env(&data, None), host_allows);
    assert!(
        matches!(&verdict, TmuxAccess::SandboxRefused { detail } if detail.contains("TMUX_TMPDIR is not set"))
    );
    let empty = PathBuf::new();
    assert!(!classify_tmux_access(&sandbox_env(&data, Some(&empty)), host_allows).is_allowed());
}

#[test]
fn refuses_a_tmux_tmpdir_outside_the_data_dir() {
    let (root, data, _tmux) = sandbox_dirs();
    let outside = root.path().join("elsewhere");
    std::fs::create_dir(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
    for dir in [outside.as_path(), data.as_path(), Path::new("/tmp")] {
        let verdict = classify_tmux_access(&sandbox_env(&data, Some(dir)), host_allows);
        assert!(
            matches!(&verdict, TmuxAccess::SandboxRefused { detail } if detail.contains("not inside")),
            "{dir:?}: {verdict:?}"
        );
    }
}

#[test]
fn refuses_a_group_writable_tmux_tmpdir() {
    let (_root, data, tmux) = sandbox_dirs();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o770)).unwrap();
    let verdict = classify_tmux_access(&sandbox_env(&data, Some(&tmux)), host_allows);
    assert!(
        matches!(&verdict, TmuxAccess::SandboxRefused { detail } if detail.contains("writable by other users"))
    );
}

/// Why: a link inside the data dir must not carry the socket outside it.
#[test]
fn refuses_a_symlink_out_of_the_data_dir() {
    let (root, data, _tmux) = sandbox_dirs();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
    let link = data.join("link");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let verdict = classify_tmux_access(&sandbox_env(&data, Some(&link)), host_allows);
    assert!(!verdict.is_allowed(), "{verdict:?}");
}

#[test]
fn refuses_a_missing_data_dir_or_socket_dir() {
    let (root, data, _tmux) = sandbox_dirs();
    let missing = data.join("missing");
    assert!(!classify_tmux_access(&sandbox_env(&data, Some(&missing)), host_allows).is_allowed());
    let env = TmuxEnv {
        data_dir_override: None,
        ..sandbox_env(&data, Some(&missing))
    };
    assert!(!classify_tmux_access(&env, host_allows).is_allowed());
    drop(root);
}

#[test]
fn a_refused_sandbox_names_the_variable() {
    let reason = TmuxAccess::SandboxRefused {
        detail: "x".to_string(),
    }
    .skip_reason()
    .expect("a refusal");
    assert!(
        reason.contains("TMUX_TMPDIR") && reason.contains("#9557"),
        "{reason}"
    );
}
