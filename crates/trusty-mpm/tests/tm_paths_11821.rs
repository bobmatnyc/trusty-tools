//! End-to-end tests for `tm paths show|set` (#11821 P6b), on the real binary
//! with a temp `HOME` and a cleared environment.
//! Test: `cargo test -p trusty-mpm --test integration tm_paths_11821::`.

use std::path::Path;
use std::process::{Command, Output};

/// Run `tm <args>` with only `HOME`, `PATH`, a private `TMUX_TMPDIR` and
/// `extra` set.
fn tm(home: &Path, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tm"));
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("TMUX_TMPDIR", home)
        .args(args);
    for (k, v) in extra {
        cmd.env(k, v);
    }
    cmd.output().expect("run tm")
}

fn install_file(home: &Path) -> std::path::PathBuf {
    trusty_common::install_paths::install_file_path(home)
}

/// #11821 P6b owner-only: a Claude Code Bash child is refused before
/// install.toml is touched, and the marker value is never printed.
#[test]
fn paths_set_refuses_inside_a_claude_code_session() {
    let home = tempfile::tempdir().expect("home");
    let args = ["paths", "set", "--projects-root", "/abs/projects"];
    let markers = [
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_SESSION_ID", "p6b-secret-id"),
    ];
    let out = tm(home.path(), &args, &markers);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("tm paths set refused"), "{stderr}");
    assert!(stderr.contains("`CLAUDECODE` is set"), "{stderr}");
    assert!(!stderr.contains("p6b-secret-id"), "{stderr}");
    assert!(
        !install_file(home.path()).exists(),
        "install.toml was written"
    );
}

/// #11821 P6b: a broken install.toml makes `show` fail with the parse error,
/// never print the `$HOME` default.
#[test]
fn paths_show_on_a_broken_install_file_exits_non_zero() {
    let home = tempfile::tempdir().expect("home");
    let file = install_file(home.path());
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::write(&file, "project_root = \"/typo\"\n").expect("write");
    let out = tm(home.path(), &["paths", "show"], &[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("is broken or unreadable"), "{stderr}");
    assert!(
        !stdout.contains("trusty-mpm-projects"),
        "a default was shown: {stdout}"
    );
}
