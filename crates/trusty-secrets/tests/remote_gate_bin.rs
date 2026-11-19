//! The real `trusty-secrets remote-gate` binary (#11464).
//!
//! Why: AC3 — with no GUI-session server the gate exits 69 and starts no
//! server; AC1 — an unlisted verb exits 13. Only the built binary proves
//! the exit status and that stderr stays empty.
//! What: every path is a flag into a `TempDir`, which holds a checkout the
//! allowlist maps. `PATH` finds first a fake `trusty-secrets` that writes a
//! marker file if anything runs it, so a spawn by name would leave the
//! marker; `/usr/bin` follows it for `git`.
//! Test: itself.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_trusty-secrets");

/// Run the gate for host `evo-linux` with `frame` on stdin.
fn run_gate(tmp: &Path, frame: &str) -> Output {
    let mut child = Command::new(BIN)
        .args(["remote-gate", "--host", "evo-linux", "--socket"])
        .arg(tmp.join("run").join("secrets.sock"))
        .arg("--machine-config")
        .arg(tmp.join("config.yaml"))
        // The fake first; `/usr/bin` for the checkout's `git` (#11464).
        .env("PATH", format!("{}:/usr/bin", tmp.join("bin").display()))
        .env("HOME", tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{frame}\n").as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// A temp dir with a checkout of `acme/web`, the allowlist mapping it, and
/// the marker-writing fake server.
fn setup() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", "git@github.com:acme/web.git"],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    let config = format!(
        "secrets:\n  remote_hosts:\n    evo-linux:\n      projects:\n        acme/web: \"{}\"\n      keys: [API_KEY]\n",
        repo.display()
    );
    std::fs::write(tmp.path().join("config.yaml"), config).unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("trusty-secrets");
    let marker = tmp.path().join("spawned");
    std::fs::write(&fake, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    tmp
}

/// Why: AC3 — no server socket: exit 69, a fixed error, no server spawned
/// (no marker, no socket directory), nothing on stderr.
/// Test: itself.
#[test]
fn remote_gate_with_no_server_exits_69_and_spawns_nothing() {
    let tmp = setup();
    let out = run_gate(
        tmp.path(),
        r#"{"method":"resolve","project":"acme/web","key":"API_KEY"}"#,
    );
    assert_eq!(out.status.code(), Some(69), "{out:?}");
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "{\"error\":\"studio_server_absent\",\"ok\":false}\n"
    );
    assert!(
        out.stderr.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!tmp.path().join("spawned").exists(), "a server was spawned");
    assert!(!tmp.path().join("run").exists());
}

/// Why: AC1 — an unlisted verb exits 13 with nothing on stderr.
/// Test: itself.
#[test]
fn remote_gate_refuses_an_unlisted_verb_with_exit_13() {
    let tmp = setup();
    let out = run_gate(
        tmp.path(),
        r#"{"method":"set_agents_may_use","project":"acme/web","key":"API_KEY"}"#,
    );
    assert_eq!(out.status.code(), Some(13), "{out:?}");
    assert!(out.stderr.is_empty());
    assert!(!tmp.path().join("spawned").exists());
}
