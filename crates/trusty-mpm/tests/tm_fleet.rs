//! `tm fleet init|status` through the built binary (#8436).
//!
//! Why: the unit tests in `commands::fleet::tests` never start a session. Only
//! the binary proves that `init` starts `tm-architect` detached with the
//! supervisor stamp, and that `status` exits 1 until it does.
//! What: each test owns a scratch HOME and its own tmux server directory
//! (`TMUX_TMPDIR`), and puts a fake `claude` first on `PATH`; the server is
//! killed when the test ends. Nothing reaches the operator's home or tmux.
//! Test: `cargo test -p trusty-mpm --test integration tm_fleet::`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::common;

/// A scratch home, a private tmux server directory and a fake `claude`.
struct FleetEnv {
    home: tempfile::TempDir,
    tmux_dir: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl FleetEnv {
    fn new() -> Self {
        let bin = tempfile::tempdir().expect("fake bin dir");
        let claude = bin.path().join("claude");
        // #8878 ruling A: `tm fleet init` finds its claude by process name, so
        // the fake execs a `sleep` whose name contains `claude`. A symlink, not
        // a copy: macOS kills an unsigned copy of a system binary.
        let sleeper = bin.path().join("claude-sleep");
        std::os::unix::fs::symlink("/bin/sleep", &sleeper).expect("fake claude process");
        std::fs::write(
            &claude,
            format!("#!/bin/sh\nexec {:?} 600\n", sleeper.display().to_string()),
        )
        .expect("fake claude");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            home: tempfile::tempdir().expect("scratch home"),
            // Short path: tmux refuses a socket path over `sun_path` (104 bytes).
            tmux_dir: tempfile::Builder::new()
                .prefix("tmf")
                .tempdir_in("/tmp")
                .expect("tmux dir"),
            bin,
        }
    }

    /// `tm <args>` confined to this environment.
    fn tm(&self, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        common::tm_command_in(self.home.path())
            .args(args)
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            // #5784: the scratch HOME trips the host-state guard; the private
            // TMUX_TMPDIR above is what keeps tmux off the operator's server.
            .env("TRUSTY_MPM_ALLOW_HOST_STATE", "1")
            .env("PATH", path)
            .output()
            .expect("spawn tm")
    }

    fn dir(&self) -> PathBuf {
        self.home.path().join("arch")
    }
}

impl Drop for FleetEnv {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("kill-server")
            .env("TMUX_TMPDIR", self.tmux_dir.path())
            .env_remove("TMUX")
            .output();
    }
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(out)))
}

fn dir_arg(dir: &Path) -> &str {
    dir.to_str().expect("utf-8 scratch path")
}

#[test]
fn fleet_status_exits_nonzero_before_init() {
    let env = FleetEnv::new();
    let out = env.tm(&["fleet", "status", "--dir", dir_arg(&env.dir())]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("MISSING"),
        "{}",
        text(&out)
    );

    let out = env.tm(&["fleet", "status", "--json", "--dir", dir_arg(&env.dir())]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert_eq!(json(&out)["complete"], false, "{}", text(&out));
}

#[test]
fn fleet_init_launches_the_architect_and_status_is_complete() {
    let env = FleetEnv::new();
    let dir = env.dir();
    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("started tmux session tm-architect"),
        "{}",
        text(&out)
    );
    // #8878 ruling A: the launch recorded the claude it started, and only it.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("bound to claude pid"),
        "{}",
        text(&out)
    );
    let root = env.home.path().join(".trusty-mpm");
    let records: Vec<_> = std::fs::read_dir(root.join("architect-launch"))
        .expect("the launch record directory")
        .map(|entry| entry.expect("entry").path())
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    let pid: u32 = records[0]
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(|stem| stem.parse().ok())
        .expect("a `<pid>.architect` record");
    let record = trusty_mpm::core::architect_launch::ARCHITECT_RECORDS
        .read(&root, pid)
        .expect("the record reads")
        .expect("the record exists");
    assert_eq!(record.pid, pid);
    assert!(trusty_mpm::core::process::process_name_is_claude(pid));

    let out = env.tm(&["fleet", "status", "--json", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    let report = json(&out);
    assert_eq!(report["complete"], true, "{}", text(&out));
    assert_eq!(report["session"], "tm-architect");

    let out = env.tm(&["fleet", "init", "--dir", dir_arg(&dir)]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Nothing changed"),
        "{}",
        text(&out)
    );
}

/// #8436: `--dir` goes through the same preflight as the default, so the
/// binary refuses `$HOME` and writes no grant.
#[test]
fn fleet_init_refuses_the_home_directory_before_writing() {
    let env = FleetEnv::new();
    let home = env.home.path();
    let out = env.tm(&["fleet", "init", "--no-launch", "--dir", dir_arg(home)]);
    assert!(!out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("it is your home directory"),
        "{}",
        text(&out)
    );
    assert!(
        !home.join(".trusty-mpm/config.toml").exists(),
        "the grant was written: {}",
        text(&out)
    );
    assert!(!home.join(".trusty-mpm.toml").exists(), "{}", text(&out));
}
