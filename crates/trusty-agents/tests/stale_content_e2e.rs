//! #9014: content that is installed but too old for this `tagent` makes the
//! remedy visible on stderr, not only in the log.
//!
//! Why: a content release older than #9014 has no `agents/trusty-agents/`
//! files. The load fails with `Missing`, not `NotInstalled`, and the REPL's
//! tracing goes to `~/.trusty-agents/logs/repl.log`, so the operator saw
//! nothing and `/switch assistant` fell back to `ctrl` silently.
//! What: stages a content bundle that lacks `ctrl.toml`, spawns the built
//! binary on the non-quiet path (`system status`, the one
//! `no_content_startup_names_tm_content_update` uses; it needs no TTY), once
//! with tracing silenced as in the REPL and once with default logging.
//! Test: this file IS the test.

use std::process::Command;

#[path = "support/content.rs"]
mod content_support;

const BIN: &str = env!("CARGO_BIN_EXE_tagent");
const MISSING: &str = "agents/trusty-agents/agents/ctrl.toml";

fn stderr_of(rust_log: Option<&str>) -> (String, std::path::PathBuf, tempfile::TempDir) {
    let home = tempfile::tempdir().expect("scratch home");
    let cwd = tempfile::tempdir().expect("scratch cwd");
    content_support::stage_repo_content_without(home.path(), Some(MISSING));
    let mut cmd = Command::new(BIN);
    // #9617: stamp the child so a trusty-search daemon it starts dies with this test.
    trusty_common::parent_death::exit_with_parent(&mut cmd);
    cmd.args(["system", "status", "--json"])
        .current_dir(cwd.path())
        .env("HOME", home.path())
        .env_remove("TAGENT_PROJECT_DIR")
        .env_remove("OPEN_MPM_PROJECT_DIR")
        .env_remove("RUST_LOG");
    if let Some(filter) = rust_log {
        cmd.env("RUST_LOG", filter);
    }
    let out = cmd.output().expect("spawn tagent");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("tm content update"),
        "the remedy must not reach stdout"
    );
    (stderr, home.path().to_path_buf(), home)
}

/// Content missing `ctrl.toml` prints one `[trusty-agents] ERROR` line naming
/// `tm content update` and the missing path, deploys no agents, and does not
/// repeat the line through tracing.
#[test]
fn stale_content_startup_names_tm_content_update_and_the_missing_file() {
    // The REPL case: tracing never reaches the terminal, only the print does.
    let (stderr, home_path, _home) = stderr_of(Some("off"));
    println!("rendered stderr (RUST_LOG=off):\n{stderr}");
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("tm content update"))
        .collect();
    assert_eq!(lines.len(), 1, "one remedy line expected\nstderr: {stderr}");
    assert!(
        lines[0].starts_with("[trusty-agents] ERROR "),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains(MISSING), "{}", lines[0]);
    assert!(
        !home_path.join(".trusty-agents").join("agents").exists(),
        "no agents may be deployed from content that lacks ctrl.toml"
    );

    // Piped stderr with default logging: tracing is on, the line still appears once.
    let (stderr, _, _home) = stderr_of(None);
    let count = stderr
        .lines()
        .filter(|l| l.contains("tm content update"))
        .count();
    assert_eq!(count, 1, "the remedy must print once\nstderr: {stderr}");
}
