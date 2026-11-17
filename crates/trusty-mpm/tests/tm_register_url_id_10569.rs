//! #10569: `tm register <owner/repo>` registers, with no daemon running.
//!
//! Why: the `Register` positional shared clap's arg id `url` with the global
//! `--url`, so the repo spec landed in `Cli.url` and the #6288 remote-URL
//! refusal ("names a daemon on another host") failed every `tm register`.
//! What: the real `tm` binary under a scratch `$HOME`, a scratch
//! `TRUSTY_MPM_ROOT`, and a socket path nothing serves. `register` writes the
//! standalone registry only, so it needs no network and no daemon.
//! Test: this file IS the test.

use std::process::{Output, Stdio};

use super::common;

/// Run `tm args…` confined to `dir`, with no daemon reachable.
fn tm_in(dir: &std::path::Path, args: &[&str]) -> Output {
    let home = dir.join("home");
    std::fs::create_dir_all(&home).expect("home");
    let mut cmd = common::tm_command_in(&home);
    cmd.current_dir(dir)
        .env("TRUSTY_MPM_ROOT", dir.join("root"))
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env(
            "TRUSTY_MPM_SOCKET",
            dir.join("absent").join("trusty-mpm.sock"),
        )
        .env("TRUSTY_DATA_DIR_OVERRIDE", dir.join("data"))
        .stdin(Stdio::null())
        .args(args);
    cmd.output().expect("run tm")
}

#[test]
fn tm_register_owner_repo_registers_without_a_daemon() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = tm_in(dir.path(), &["register", "bobmatnyc/trusty-tools"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "tm register exited {:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
    assert!(
        stdout.contains(
            "registered bobmatnyc-trusty-tools → https://github.com/bobmatnyc/trusty-tools"
        ),
        "missing the registered line\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("another host") && !stderr.contains("another host"),
        "the #6288 remote-URL refusal fired\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        dir.path().join("root").join("registry.json").is_file(),
        "registry.json was not written under TRUSTY_MPM_ROOT"
    );
}
