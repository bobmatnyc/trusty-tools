//! `tm session start` and `tm connect` deploy under the configured config dir (#12048).
//!
//! Why: `tm session start` built its framework layout with
//! `FrameworkPaths::under(home)`, which ignores `install.toml` `config_dir`, so
//! it deployed under `$HOME/.trusty-mpm` while `tm launch` read
//! `<config_dir>/.trusty-mpm`. A malformed `install.toml` also let the deploy go
//! ahead under `$HOME` instead of refusing.
//! What: each case runs the built `tm` under a scratch `$HOME` whose
//! `install.toml` names a sibling `cfg` dir, against the #10459 stub daemon,
//! with a fake `claude` and a fake `tmux` (exit 1) first on `PATH`. The fake
//! tmux ends the launch after prep, so the test reads where prep deployed. No
//! real daemon, tmux server, Keychain or local env file is reached.
//! Test: this file IS the test (live binary spawns, so `#[ignore]`d; run with
//! `--include-ignored`).

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_common::install_paths::{MPM_DIR_NAME, TOOLS_DIR_NAME};

use crate::architect_version_refusal_10459::StubDaemon;
use crate::common::tm_command_in;

/// Env vars that would pick a path before the scratch `install.toml`.
const PATH_VARS: [&str; 5] = [
    "TRUSTY_MPM_CONFIG_DIR",
    "TRUSTY_MPM_WORKSPACE_ROOT",
    "TRUSTY_MPM_REPOS_ROOT",
    "TRUSTY_SANDBOX",
    "CLAUDE_CONFIG_DIR",
];

/// A scratch root under `/tmp`: `home/` with `install.toml` = `install`, a
/// sibling `cfg/`, a remote-less git `project/`, and `bin/` holding the fakes.
struct Scratch {
    _guard: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    cfg: PathBuf,
    project: PathBuf,
}

impl Scratch {
    fn new(install: impl FnOnce(&Path) -> String) -> Self {
        let guard = tempfile::Builder::new()
            .prefix("tm-test-12048-")
            .tempdir_in("/tmp")
            .expect("scratch");
        let root = guard.path().canonicalize().expect("canonical");
        let home = root.join("home");
        let cfg = root.join("cfg");
        let project = root.join("project");
        std::fs::create_dir_all(home.join(TOOLS_DIR_NAME)).expect("mkdir .trusty-tools");
        std::fs::create_dir_all(&cfg).expect("mkdir cfg");
        std::fs::write(
            trusty_common::install_paths::install_file_path(&home),
            install(&cfg),
        )
        .expect("install.toml");
        std::fs::create_dir_all(&project).expect("mkdir project");
        let status = Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&project)
            .status()
            .expect("git init");
        assert!(status.success(), "git init");
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        // See #10248
        trusty_common::exec_stub::write_executable(
            &bin.join("claude"),
            "#!/bin/sh\n[ \"$1\" = --version ] && { echo '2.1.400 (Claude Code)'; exit 0; }\nexit 9\n",
        )
        .expect("fake claude");
        trusty_common::exec_stub::write_executable(&bin.join("tmux"), "#!/bin/sh\nexit 1\n")
            .expect("fake tmux");
        Self {
            _guard: guard,
            root,
            home,
            cfg,
            project,
        }
    }

    /// Run `tm --url <stub> <args>`; returns the exit success and stderr.
    fn tm(&self, args: &[&str]) -> (bool, String) {
        let sock = self.root.join("sock");
        std::fs::create_dir_all(&sock).expect("sock dir");
        let daemon = StubDaemon::start(&sock);
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = tm_command_in(&self.home);
        for key in PATH_VARS {
            cmd.env_remove(key);
        }
        let out = cmd
            .arg("--url")
            .arg(&daemon.url)
            .args(args)
            .env("TRUSTY_MPM_SOCKET", &daemon.socket)
            .env("PATH", path)
            .env("TM_DISABLE_SPAWN_DISCLAIM", "1")
            .output()
            .expect("spawn tm");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn project_arg(&self) -> String {
        self.project.to_string_lossy().into_owned()
    }
}

/// `config_dir = "<cfg>"`.
fn config_dir_install(cfg: &Path) -> String {
    format!("config_dir = {:?}\n", cfg.display().to_string())
}

/// Prep deployed the framework under `<cfg>/.trusty-mpm`, nothing under
/// `$HOME/.trusty-mpm/framework`.
fn assert_deployed_under_cfg(s: &Scratch, what: &str, stderr: &str) {
    let framework = s.cfg.join(MPM_DIR_NAME).join("framework");
    let entries = std::fs::read_dir(&framework)
        .unwrap_or_else(|e| panic!("{what}: no {}: {e}; stderr: {stderr}", framework.display()))
        .count();
    assert!(entries > 0, "{what}: {} is empty", framework.display());
    let home_framework = s.home.join(MPM_DIR_NAME).join("framework");
    assert!(
        !home_framework.exists(),
        "{what} deployed under {}; stderr: {stderr}",
        home_framework.display()
    );
}

#[test]
#[ignore = "live: spawns the built tm against a stub daemon; run with --include-ignored"]
fn session_start_deploys_real_files_under_the_config_dir_12048() {
    let s = Scratch::new(config_dir_install);
    let (_ok, stderr) = s.tm(&["session", "start", "--dir", &s.project_arg()]);
    assert_deployed_under_cfg(&s, "session start", &stderr);
}

#[test]
#[ignore = "live: spawns the built tm against a stub daemon; run with --include-ignored"]
fn connect_deploys_real_files_under_the_config_dir_12048() {
    let s = Scratch::new(config_dir_install);
    let (_ok, stderr) = s.tm(&["connect", &s.project_arg()]);
    assert_deployed_under_cfg(&s, "connect", &stderr);
}

/// #12048 Fail-Open Check: a malformed `install.toml` refuses the start, and
/// nothing deploys under `$HOME/.trusty-mpm/framework`.
#[test]
#[ignore = "live: spawns the built tm against a stub daemon; run with --include-ignored"]
fn session_start_refuses_a_malformed_install_toml_live_12048() {
    let s = Scratch::new(|_| "config_dir = \"/elsewhere\"\nbogus = 1\n".to_owned());
    let (ok, stderr) = s.tm(&["session", "start", "--dir", &s.project_arg()]);
    assert!(!ok, "a malformed install.toml must fail the start");
    assert!(stderr.contains("install.toml"), "{stderr}");
    let home_framework = s.home.join(MPM_DIR_NAME).join("framework");
    assert!(
        !home_framework.exists(),
        "deployed under {}; stderr: {stderr}",
        home_framework.display()
    );
}
