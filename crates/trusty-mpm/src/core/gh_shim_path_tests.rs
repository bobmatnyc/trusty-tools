//! Tests for which `gh` a tm child runs inside a pinned session (#9750).
//! The shim and the "real" gh are fake scripts in a temp dir; no test runs the
//! real gh, reads a keyring or sends a request.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{
    GhPathEnv, apply_gh_path_with, bind_gh_path_with, bypass_session_shim_with, gh_child_path,
};
use crate::core::gh_account::REFUSED_GH_TOKEN;
use crate::core::gh_identity::GhEnv;
use crate::core::gh_session_shim::GH_SHIM_ENV;

/// Session A's shim under the state root, the session's inherited `PATH`.
fn session_env(shim_root: &Path, shim: &Path, rest: &str) -> GhPathEnv {
    let mut path = OsString::from(shim.as_os_str());
    path.push(":");
    path.push(rest);
    GhPathEnv {
        path: Some(path),
        shim: Some(shim.as_os_str().to_os_string()),
        shim_root: shim_root.to_path_buf(),
    }
}

/// `(name, value)` pairs from string slices.
fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

/// #9750: a child that selects its own credential gets the inherited `PATH`
/// with every shim removed — one under the shim root, and the dir the session
/// named even when it lives elsewhere.
#[test]
fn a_child_selecting_a_credential_drops_every_shim_from_path() {
    let env = session_env(
        Path::new("/state/gh-shim"),
        Path::new("/state/gh-shim/a@github.com-1"),
        "/usr/local/bin:/state/gh-shim/old@github.com-2:/usr/bin",
    );
    let bound = vars(&[("GH_CONFIG_DIR", "/b/gh")]);
    assert_eq!(
        gh_child_path(&bound, &[], &env),
        Some(OsString::from("/usr/local/bin:/usr/bin"))
    );
    let outside = GhPathEnv {
        shim_root: PathBuf::from("/elsewhere"),
        ..env.clone()
    };
    let removes = ["GH_TOKEN".to_string()];
    assert_eq!(
        gh_child_path(&[], &removes, &outside),
        Some(OsString::from(
            "/usr/local/bin:/state/gh-shim/old@github.com-2:/usr/bin"
        ))
    );
}

/// #9750, #8383 T1: a child selecting nothing — no binding, `GH_USER` alone,
/// or removing a variable gh never reads — keeps the session's shim and acts
/// as its caller.
#[test]
fn an_ambient_child_keeps_the_session_shim() {
    let env = session_env(
        Path::new("/state/gh-shim"),
        Path::new("/state/gh-shim/a@github.com-1"),
        "/usr/bin",
    );
    assert_eq!(gh_child_path(&[], &[], &env), None);
    let informational = vars(&[("GH_USER", "octo")]);
    assert_eq!(gh_child_path(&informational, &[], &env), None);
    assert_eq!(gh_child_path(&[], &["CLAUDECODE".to_string()], &env), None);
}

/// #9750: a child naming its own shim leads with it, and the inherited `PATH`
/// behind it carries no other session's shim.
#[test]
fn a_shimmed_child_leads_with_its_own_shim_only() {
    let env = session_env(
        Path::new("/state/gh-shim"),
        Path::new("/state/gh-shim/a@github.com-1"),
        "/usr/bin",
    );
    let shimmed = vars(&[(GH_SHIM_ENV, "/state/gh-shim/b@github.com-3")]);
    assert_eq!(
        gh_child_path(&shimmed, &[], &env),
        Some(OsString::from("/state/gh-shim/b@github.com-3:/usr/bin"))
    );
    let bare = GhPathEnv {
        path: None,
        shim: None,
        shim_root: PathBuf::from("/state/gh-shim"),
    };
    assert_eq!(
        gh_child_path(&shimmed, &[], &bare),
        Some(OsString::from("/state/gh-shim/b@github.com-3"))
    );
}

/// The `PATH` a command sets, if any.
fn path_of(cmd: &std::process::Command) -> Option<String> {
    cmd.get_envs()
        .find(|(k, _)| *k == "PATH")
        .and_then(|(_, v)| v)
        .map(|v| v.to_string_lossy().into_owned())
}

/// A temp layout: session A's shim and the real gh, each a script naming
/// itself, the credential it received, and its config dir.
#[cfg(unix)]
struct Layout {
    _tmp: tempfile::TempDir,
    shim_root: PathBuf,
    shim: PathBuf,
    real_bin: PathBuf,
}

#[cfg(unix)]
impl Layout {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let shim_root = tmp.path().join("gh-shim");
        let shim = shim_root.join("a@github.com-1");
        let real_bin = tmp.path().join("bin");
        for (dir, name) in [(&shim, "shim-as-a"), (&real_bin, "real")] {
            std::fs::create_dir_all(dir).expect("dir");
            let gh = dir.join("gh");
            let body = format!(
                "#!/bin/sh\nprintf '{name} GH_TOKEN=%s GH_CONFIG_DIR=%s\\n' \"$GH_TOKEN\" \"$GH_CONFIG_DIR\"\n"
            );
            std::fs::write(&gh, body).expect("script");
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        Self {
            _tmp: tmp,
            shim_root,
            shim,
            real_bin,
        }
    }

    /// Session A's `PATH`: its shim, then the real gh's dir.
    fn env(&self) -> GhPathEnv {
        let rest = self.real_bin.display().to_string();
        session_env(&self.shim_root, &self.shim, &rest)
    }
}

/// The trimmed stdout of `cmd`.
#[cfg(unix)]
fn stdout_of(mut cmd: std::process::Command) -> String {
    let out = cmd.output().expect("gh runs");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// 🔴 #9750: run inside session A (its shim first on `PATH`, the nobody
/// sentinel in `GH_TOKEN`), a `std::process::Command` gh bound to project B's
/// config dir resolves the REAL gh, with B's dir and without the sentinel —
/// never A's shim.
#[cfg(unix)]
#[test]
fn apply_gh_path_with_drops_the_session_shim_for_a_bound_child() {
    let layout = Layout::new();
    let b = GhEnv::from_identity_vars(vars(&[("GH_CONFIG_DIR", "/b/gh")]));
    let mut cmd = std::process::Command::new("gh");
    // Session A's own env; an unbound lookup would find A's fake shim.
    cmd.env("PATH", layout.env().path.expect("path"))
        .env("GH_TOKEN", REFUSED_GH_TOKEN);
    for key in b.unset_vars() {
        cmd.env_remove(key);
    }
    for (key, value) in b.vars() {
        cmd.env(key, value);
    }
    apply_gh_path_with(&mut cmd, b.vars(), b.unset_vars(), &layout.env());
    let path = path_of(&cmd).expect("PATH is set");
    assert!(!path.contains("gh-shim"), "{path}");
    assert_eq!(stdout_of(cmd), "real GH_TOKEN= GH_CONFIG_DIR=/b/gh");
}

/// 🔴 #9750: the same for a [`trusty_common::gh::GhCommand`] — tm's one gh
/// entry point — while an ambient call (no binding) still runs as session A
/// through its shim.
#[cfg(unix)]
#[test]
fn bind_gh_path_with_runs_the_real_gh_for_another_project() {
    use trusty_common::gh::GhCommand;
    let layout = Layout::new();
    let b = GhEnv::from_identity_vars(vars(&[("GH_CONFIG_DIR", "/b/gh")]));
    // Session A's own PATH, so an unbound lookup finds A's fake shim and
    // never this machine's gh.
    let mut cmd = GhCommand::new(["api", "user"])
        .env("PATH", layout.env().path.expect("path"))
        .env("GH_TOKEN", REFUSED_GH_TOKEN);
    for key in b.unset_vars() {
        cmd = cmd.env_remove(key);
    }
    for (key, value) in b.vars() {
        cmd = cmd.env(key, value);
    }
    let bound = bind_gh_path_with(cmd, b.vars(), b.unset_vars(), &layout.env());
    assert_eq!(
        stdout_of(bound.to_std_command()),
        "real GH_TOKEN= GH_CONFIG_DIR=/b/gh"
    );

    let ambient = GhEnv::default();
    let cmd = GhCommand::new(["api", "user"])
        .env("PATH", layout.env().path.expect("path"))
        .env("GH_TOKEN", REFUSED_GH_TOKEN)
        .env_remove("GH_CONFIG_DIR");
    let cmd = bind_gh_path_with(cmd, ambient.vars(), ambient.unset_vars(), &layout.env());
    assert!(stdout_of(cmd.to_std_command()).starts_with("shim-as-a "));
}

/// 🔴 #9750: a gh child that picks its credential by hand drops the session's
/// shim and the inherited nobody sentinel, and leaves any other inherited
/// variable alone.
#[cfg(unix)]
#[test]
fn bypass_session_shim_with_drops_the_shim_and_the_sentinel() {
    let layout = Layout::new();
    let inherited = vars(&[
        ("GH_TOKEN", REFUSED_GH_TOKEN),
        ("GH_ENTERPRISE_TOKEN", REFUSED_GH_TOKEN),
        ("GITHUB_TOKEN", "kept-as-is"),
    ]);
    // The session's env, set on the child so nothing of this process leaks in.
    let cmd = inherited.iter().fold(
        trusty_common::gh::GhCommand::new(["auth", "token", "-u", "octo"])
            .env_remove("GH_CONFIG_DIR"),
        |cmd, (k, v)| cmd.env(k, v),
    );
    let cmd = bypass_session_shim_with(cmd, &inherited, &layout.env()).to_std_command();
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    assert!(removed.iter().any(|k| k == "GH_TOKEN"), "{removed:?}");
    assert!(
        removed.iter().any(|k| k == "GH_ENTERPRISE_TOKEN"),
        "{removed:?}"
    );
    assert!(!removed.iter().any(|k| k == "GITHUB_TOKEN"), "{removed:?}");
    let path = path_of(&cmd).expect("PATH is set");
    assert_eq!(path, layout.real_bin.display().to_string());
    assert_eq!(stdout_of(cmd), "real GH_TOKEN= GH_CONFIG_DIR=");
}
