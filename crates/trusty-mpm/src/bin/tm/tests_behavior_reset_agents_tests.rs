//! CLI parse and report-line tests for `tm install --reset-agents`
//! (issue #2504) — kept in their own `_tests.rs`-suffixed file (1500-SLOC
//! test cap) rather than growing `tests_behavior_a.rs`, which sits close to
//! the 500-SLOC production cap despite living in `src/bin/tm/`, following the
//! existing `tests_behavior_a/b/c/d/e` split convention.
//!
//! Why: issue #2504 found that composed agent files deployed before per-file
//! manifest tracking existed can never receive bundle updates — the deployer
//! conservatively (and correctly) treats an untracked target file as
//! potentially user-owned and skips it forever. These tests cover the CLI
//! surface for the fix's three parts: `--reset-agents` argument parsing
//! (bare flag = all agents, comma list = filtered), and the human-readable
//! report lines for both the deploy adoption/warning path and the reset
//! summary. Core reconciliation logic (adoption, backup-before-overwrite) is
//! covered by `crates/trusty-mpm/src/core/agent_deployer.rs` and
//! `crates/trusty-mpm/src/core/agent_reset.rs` unit tests.
//! What: `cli_parses_install_reset_agents_all`,
//! `cli_parses_install_reset_agents_filtered`,
//! `deploy_report_lines_flags_adopted_and_untracked_modified`,
//! `reset_report_lines_summarizes_counts`.
//! Test: `cargo test -p trusty-mpm` runs this file as part of the `tm` binary
//! test suite.

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::commands::install::{deploy_report_lines, reset_report_lines};

#[test]
fn cli_parses_install_reset_agents_all() {
    // Issue #2504: `--reset-agents` with no value resets every bundled agent.
    let cli = Cli::try_parse_from(["trusty-mpm", "install", "--reset-agents"]).unwrap();
    match cli.command.unwrap() {
        Command::Install { reset_agents, .. } => {
            assert_eq!(reset_agents, Some(vec![]));
        }
        other => panic!("expected Install, got {other:?}"),
    }
}

#[test]
fn cli_parses_install_reset_agents_filtered() {
    // A comma-separated value list restricts the reset scope.
    let cli =
        Cli::try_parse_from(["trusty-mpm", "install", "--reset-agents", "engineer,qa"]).unwrap();
    match cli.command.unwrap() {
        Command::Install { reset_agents, .. } => {
            assert_eq!(
                reset_agents,
                Some(vec!["engineer".to_string(), "qa".to_string()])
            );
        }
        other => panic!("expected Install, got {other:?}"),
    }
}

#[test]
fn deploy_report_lines_flags_adopted_and_untracked_modified() {
    // Issue #2504: adopted files get a distinct glyph, untracked-modified
    // files that are also skipped get a distinct message, and a single
    // trailing summary line points at `--reset-agents` — never one warning
    // line per untracked file.
    let mut result = trusty_mpm::core::agent_deployer::DeployResult::default();
    result.adopted.push("adopted-agent.md".to_string());
    result.skipped.push("stale-agent.md".to_string());
    result.untracked_modified.push("stale-agent.md".to_string());
    result.skipped.push("edited-agent.md".to_string());

    let dir = tempfile::tempdir().unwrap();
    let lines = deploy_report_lines(&result, dir.path());

    assert!(
        lines
            .iter()
            .any(|l| l.contains("adopted-agent.md") && l.contains("adopted")),
        "lines = {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("stale-agent.md") && l.contains("differs from bundle")),
        "lines = {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("edited-agent.md") && l.contains("user-modified")),
        "lines = {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("1 untracked agent file") && l.contains("--reset-agents")),
        "lines = {lines:?}"
    );
}

#[test]
fn reset_report_lines_summarizes_counts() {
    // Issue #2504: `--reset-agents` output must include a scannable total,
    // not just per-file lines, since a full reset can touch dozens of files.
    let result = trusty_mpm::core::agent_reset::ResetResult {
        recomposed: vec!["a.md".to_string(), "b.md".to_string()],
        adopted: vec!["c.md".to_string()],
        backed_up: vec!["b.md".to_string()],
        not_found: vec!["missing-agent".to_string()],
    };

    let lines = reset_report_lines(&result);
    assert!(lines.iter().any(|l| l.contains("a.md (recomposed)")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("b.md (recomposed, backed up)"))
    );
    assert!(lines.iter().any(|l| l.contains("c.md (adopted)")));
    assert!(lines.iter().any(|l| l.contains("missing-agent")));
    assert!(
        lines.iter().any(|l| l.contains("2 recomposed")
            && l.contains("1 adopted")
            && l.contains("1 backed up")
            && l.contains("1 not found")),
        "lines = {lines:?}"
    );
}
