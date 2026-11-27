//! `tm whats-new` and the once-per-release `tm doctor` notice, through the
//! real binary (#11003).
//!
//! Why: the unit tests in `core::whats_new` prove the decision; only the
//! binary proves `tm doctor` calls it, prints the line once, and still exits 0
//! when the store cannot be used.
//! What: runs the built `tm` under a scratch HOME (`common::tm_command_in`),
//! so the store lands under that HOME's framework root.
//! Test: `cargo test -p trusty-mpm --test integration tm_whats_new::`.

use std::path::Path;

use crate::common;

/// Run `tm <args>` in a scratch cwd under `home`; (success, stdout, stderr).
fn run_tm(home: &Path, args: &[&str]) -> (bool, String, String) {
    let cwd = tempfile::tempdir().expect("scratch cwd");
    let out = common::tm_command_in(home)
        .args(args)
        .current_dir(cwd.path())
        .output()
        .expect("spawn tm");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const NOTICE: &str = "— run `tm whats-new`";

#[test]
fn tm_whats_new_prints_the_installed_release_highlights() {
    let home = tempfile::tempdir().expect("scratch home");
    let (ok, stdout, stderr) = run_tm(home.path(), &["whats-new"]);
    assert!(ok, "stderr:\n{stderr}");
    assert!(stdout.contains("# What's new in trusty-mpm"), "{stdout}");
    assert!(stdout.contains("Try: `tm whats-new`"), "{stdout}");
}

/// Doctor prints the notice on its first run after an upgrade and not again.
#[test]
fn tm_doctor_shows_the_whats_new_notice_once() {
    let home = tempfile::tempdir().expect("scratch home");
    let (ok, first, stderr) = run_tm(home.path(), &["doctor"]);
    assert!(ok, "stderr:\n{stderr}");
    assert_eq!(first.matches(NOTICE).count(), 1, "first run:\n{first}");
    assert!(!stderr.contains("whats-new notice"), "{stderr}");
    let (ok, second, stderr) = run_tm(home.path(), &["doctor"]);
    assert!(ok, "stderr:\n{stderr}");
    assert!(!second.contains(NOTICE), "second run:\n{second}");
}

/// A store that cannot be read keeps the notice, warns, and exits 0.
#[test]
fn tm_doctor_reports_an_unreadable_whats_new_store() {
    let home = tempfile::tempdir().expect("scratch home");
    // A directory where the store file belongs: every read fails.
    let root = trusty_mpm::core::paths::FrameworkPaths::under_home(home.path()).root;
    let store = root.join(trusty_mpm::core::whats_new::STORE_FILE);
    std::fs::create_dir_all(&store).expect("seed blocker");
    for _ in 0..2 {
        let (ok, stdout, stderr) = run_tm(home.path(), &["doctor"]);
        assert!(
            ok,
            "doctor must not fail on a store error.\nstderr:\n{stderr}"
        );
        assert!(stdout.contains(NOTICE), "{stdout}");
        assert!(
            stderr.contains("whats-new notice not recorded as seen"),
            "{stderr}"
        );
    }
}
