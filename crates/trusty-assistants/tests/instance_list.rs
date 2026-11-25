//! `ta list` and the unknown-instance error against the built binary (#11742).
//!
//! Why: `~/trusty-agents` holds entries that are not instances, and a user
//! had no command naming the real ones; `ta ask <typo>` did not name them
//! either. Every run is under a temporary `$HOME`.
//! Test: this file IS the test.

mod common;

use std::path::Path;

use tempfile::TempDir;

/// A home whose `trusty-agents` holds two instances and the non-instance
/// entries a real install carries.
fn populated_home() -> TempDir {
    let home = tempfile::tempdir().expect("home");
    let root = home.path().join("trusty-agents");
    for id in ["izzie", "assistant"] {
        std::fs::create_dir_all(root.join(id)).expect("instance");
        std::fs::write(root.join(id).join("instructions.md"), "You help.\n").expect("md");
    }
    std::fs::create_dir_all(root.join("OKG")).expect("OKG");
    std::fs::write(root.join("OKG").join("Welcome.md"), "x\n").expect("welcome");
    std::fs::create_dir_all(root.join("bob-kb").join("people")).expect("kb");
    std::fs::write(root.join("memory-bindings.lock"), "").expect("lock");
    std::fs::write(root.join("README.md"), "x\n").expect("readme");
    home
}

fn ta(home: &TempDir, args: &[&str], data: &Path) -> std::process::Output {
    common::ta(home)
        .args(args)
        .env("TRUSTY_DATA_DIR_OVERRIDE", data)
        .output()
        .expect("run ta")
}

#[test]
fn list_names_only_instance_homes() {
    let home = populated_home();
    let data = tempfile::tempdir().expect("data");
    let out = ta(&home, &["ask", "nosuch", "hi"], data.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown assistant instance `nosuch`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("available instances: assistant, izzie"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());

    let out = ta(&home, &["list"], data.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "assistant\nizzie\n");
}

#[test]
fn list_on_an_empty_root_says_so() {
    let home = tempfile::tempdir().expect("home");
    let data = tempfile::tempdir().expect("data");
    let out = ta(&home, &["list"], data.path());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(out.stdout.is_empty());
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        stderr.starts_with("no assistant instances under "),
        "{stderr}"
    );
}
