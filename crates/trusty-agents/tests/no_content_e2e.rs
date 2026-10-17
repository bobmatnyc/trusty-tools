//! #9014: with no instructional content installed, a real `tagent` run makes
//! the remedy visible on stderr.
//!
//! Why: the bundled-agent deploy runs before the tracing subscriber exists, so
//! its error was logged into nothing: `tagent` deployed nothing, said nothing,
//! and `/switch assistant` fell back to `ctrl` silently (the #3405 symptom).
//! What: spawns the built binary with an empty `$HOME` and a cwd outside any
//! checkout, so no content resolves, and asserts one stderr line names
//! `tm content update`.
//! Test: this file IS the test.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_tagent");

/// `system status` with no content installed names `tm content update` once,
/// on an ERROR line, and deploys nothing under `~/.trusty-agents/agents`.
#[test]
fn no_content_startup_names_tm_content_update() {
    let home = tempfile::tempdir().expect("scratch home");
    let cwd = tempfile::tempdir().expect("scratch cwd");
    let mut cmd = Command::new(BIN);
    // #9617: stamp the child so a trusty-search daemon it starts dies with this test.
    trusty_common::parent_death::exit_with_parent(&mut cmd);
    let out = cmd
        .args(["system", "status", "--json"])
        .current_dir(cwd.path())
        .env("HOME", home.path())
        .env_remove("TAGENT_PROJECT_DIR")
        .env_remove("OPEN_MPM_PROJECT_DIR")
        .env_remove("RUST_LOG")
        .output()
        .expect("spawn tagent");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let remedy: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("tm content update"))
        .collect();
    assert_eq!(
        remedy.len(),
        1,
        "exactly one stderr line must name the remedy\nstderr: {stderr}"
    );
    assert!(remedy[0].contains("ERROR"), "{}", remedy[0]);
    assert!(
        !home.path().join(".trusty-agents").join("agents").exists(),
        "nothing may be deployed with no content installed"
    );
}
