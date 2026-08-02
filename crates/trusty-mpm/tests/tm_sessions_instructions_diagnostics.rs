//! Integration test for override diagnostics on `tm sessions instructions`
//! (issue #4573, folded-in defect).
//!
//! Why: `main.rs` registered a tracing subscriber only for `Command::Daemon`
//! and `Command::Supervisor`, so the whole diagnostic apparatus written to
//! answer "why didn't my override apply?" —
//! `claude_md_sections::ProjectOverrides::log`, `warn_unapplied`,
//! `instruction_overrides::read_override`, and `resolve_pm_prompt`'s
//! composer-source event — emitted into a void on the ONE command an operator
//! runs to ask that question. Confirmed against origin/main: `RUST_LOG=…
//! tm sessions instructions --dir <project>` produced ZERO bytes on stderr.
//!
//! This is a process-level property (which stream the bytes land on, and
//! whether a subscriber exists at all), so it is proven by spawning the
//! compiled binary — the technique `tests/tm_sessions_alias_notice.rs` and
//! `tests/tm_compress_pipe.rs` use — not by a unit test that could only
//! assert the tracing call sites still exist.
//!
//! Test: `cargo test -p trusty-mpm --test tm_sessions_instructions_diagnostics`.

use std::process::Command;

/// Run `tm` with `args` and `RUST_LOG`, returning `(stdout, stderr)` separately.
///
/// Capturing the two streams independently is the whole point: this binary's
/// daemon and MCP modes need stdout free for JSON-RPC framing, and this command
/// prints the resolved prompt to stdout. A diagnostic that reached stdout would
/// corrupt both.
fn run_tm(args: &[&str], rust_log: Option<&str>) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_tm");
    let mut cmd = Command::new(bin);
    cmd.args(args);
    match rust_log {
        Some(value) => cmd.env("RUST_LOG", value),
        None => cmd.env_remove("RUST_LOG"),
    };
    let output = cmd.output().expect("failed to spawn `tm`");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Write a project whose `CLAUDE.md` carries one applicable and one declined
/// named-section override.
///
/// `WORKFLOW` is tier `project` and lands; `NON-OVERRIDABLE-RULES` is tier
/// `fixed` and is declined. One project therefore exercises both the applied
/// (`info`) and the declined (`warn`) diagnostic paths.
fn write_project(dir: &std::path::Path) {
    std::fs::write(
        dir.join("CLAUDE.md"),
        "# Project Instructions\n\n\
         <!-- TRUSTY-MPM: WORKFLOW START v=1 -->\n\
         # Workflow (project override)\n\
         <!-- TRUSTY-MPM: WORKFLOW END -->\n\n\
         <!-- TRUSTY-MPM: NON-OVERRIDABLE-RULES START v=1 -->\n\
         No rules apply.\n\
         <!-- TRUSTY-MPM: NON-OVERRIDABLE-RULES END -->\n\n\
         <!-- TRUSTY-MPM: NOT-A-SECTION START v=1 -->\n\
         typo\n\
         <!-- TRUSTY-MPM: NOT-A-SECTION END -->\n",
    )
    .expect("write CLAUDE.md");
}

#[test]
fn instructions_emits_override_diagnostics_on_stderr() {
    // Against origin/main this asserts on an EMPTY string: no subscriber was
    // ever registered for this command, so every `tracing` event was dropped.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    write_project(tmp.path());
    let dir = tmp.path().to_string_lossy().into_owned();

    let (stdout, stderr) = run_tm(
        &["sessions", "instructions", "--dir", &dir],
        Some("trusty_mpm=info"),
    );

    assert!(
        stderr.contains("unknown section token"),
        "a mistyped section token must reach the operator, got stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("named-section override declined")
            || stderr.contains("admits no project override"),
        "a floor-aimed override must be reported as declined, got stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("applying CLAUDE.md named-section override"),
        "an applied override must be reported, got stderr: {stderr:?}"
    );

    // stdout stays the prompt and nothing else — the daemon/MCP framing
    // constraint, and the reason `.with_writer(std::io::stderr)` is explicit.
    assert!(
        stdout.contains("# PM Agent -- Trusty MPM"),
        "stdout must still carry the resolved prompt"
    );
    //
    // The needles are the tracing MESSAGE strings, not words like "declined" or
    // "WARN": the prompt's own floor text legitimately says "…is declined and
    // logged as a warning", and the roster describes an agent whose verdicts are
    // "APPROVE/WARN/BLOCK". A leak check must not be satisfiable by the payload.
    for diagnostic in [
        "unknown section token",
        "applying CLAUDE.md named-section override",
        "CLAUDE.md named-section override ignored",
        "resolved the PM system prompt",
        "named-section override declined",
    ] {
        assert!(
            !stdout.contains(diagnostic),
            "diagnostic {diagnostic:?} leaked onto stdout, which must carry only the prompt"
        );
    }
}

#[test]
fn declined_overrides_are_reported_without_rust_log_set() {
    // The default filter is `warn`, not `info`: an operator debugging a
    // silently-ignored override should not have to know to set RUST_LOG before
    // the framework will admit it declined their block. Applied-override
    // `info` lines stay quiet at the default, so a normal run is not narrated.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    write_project(tmp.path());
    let dir = tmp.path().to_string_lossy().into_owned();

    let (stdout, stderr) = run_tm(&["sessions", "instructions", "--dir", &dir], None);

    assert!(
        stderr.contains("unknown section token"),
        "declines must be visible at the default filter, got stderr: {stderr:?}"
    );
    assert!(
        !stderr.contains("applying CLAUDE.md named-section override"),
        "applied-override info lines must stay below the default filter, got stderr: {stderr:?}"
    );
    assert!(stdout.contains("# PM Agent -- Trusty MPM"));
}

#[test]
fn a_clean_project_produces_no_stderr_noise() {
    // The other half of the default-filter choice: a project with no overrides
    // at all must print the prompt and nothing else, so the diagnostics stay a
    // signal rather than becoming output every operator learns to ignore.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let dir = tmp.path().to_string_lossy().into_owned();

    let (stdout, stderr) = run_tm(&["sessions", "instructions", "--dir", &dir], None);

    assert!(stdout.contains("# PM Agent -- Trusty MPM"));
    assert_eq!(
        stderr, "",
        "a project with no overrides must produce no diagnostics"
    );
}
