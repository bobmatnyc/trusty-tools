//! Unit tests for [`super`] (the #7262 build-tree hook-command classifier).
//!
//! Why: split out of `build_tree.rs` to keep the production file focused;
//! mirrors the `hooks/{cleanup.rs,cleanup_tests.rs}` split already used here.
//! What: pins the incident shape from #7244, the four argv shapes, the
//! installed-binary negative, and the argv restriction that keeps a project's
//! own build-tree hook out of tm's hands.
//! Test: this module IS the test suite for `super`.

use super::*;

/// The executable SHAPE #7244 wrote into `.claude/settings.json`, seven times
/// on 2026-09-09.
///
/// Why: the repository prefix is synthetic. What the predicate reads is the
/// Cargo layout — a `target-<n>` build root, a `debug` profile under it, a
/// `deps` directory under that, and a hash-suffixed test-harness stem — and
/// none of those components is the home directory the incident happened to
/// occur in. Baking a real `$HOME` into a fixture leaks an operator path into
/// the repository for no test value, so the prefix is a placeholder and the
/// four load-bearing components are preserved verbatim.
/// Test: every test in this file, plus the `incident_settings` consumers.
pub(crate) const INCIDENT_EXE: &str =
    "/srv/projects/acme/target-7247/debug/deps/test_session_lifecycle-cd3ba8f03938239b";

/// A `hooks` group wrapping one command, in Claude Code's settings shape.
fn group(cmd: &str) -> serde_json::Value {
    serde_json::json!({
        "matcher": "*",
        "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }]
    })
}

/// The settings file `.claude/settings.json` actually carried after #7244.
///
/// Why: the ONE fixture behind every #7262 regression test — the doctor probe,
/// `clean_settings_file`, and the writer's replace-by-identity strip. Written
/// once so the three cannot drift onto slightly different shapes and each claim
/// to cover "the incident".
/// What: the six lifecycle events plus the PM-guard group, every one pointing
/// at [`INCIDENT_EXE`]; a `statusLine.command` pointing at the same binary; and
/// a correct `trusty-memory prompt-context` entry under `UserPromptSubmit` that
/// every consumer must leave alone.
/// Test: used by `build_tree_hook_commands_lists_the_incident_commands`,
/// `clean_settings_file_force_removes_the_build_tree_incident`,
/// `write_project_hooks_replaces_a_build_tree_group_instead_of_duplicating_it`,
/// `check_hooks_hygiene_reports_the_build_tree_incident_shape`.
pub(crate) fn incident_settings() -> serde_json::Value {
    let hook = format!("{INCIDENT_EXE} hook");
    serde_json::json!({
        "outputStyle": "trusty-mpm",
        "statusLine": {
            "type": "command",
            "command": format!("{INCIDENT_EXE} statusline"),
            "padding": 0
        },
        "hooks": {
            "PreToolUse": [
                group(&hook),
                group(&format!("{INCIDENT_EXE} hook --pm-guard")),
            ],
            "PostToolUse": [group(&hook)],
            "Stop": [group(&hook)],
            "SubagentStop": [group(&hook)],
            "SessionStart": [group(&hook)],
            "SessionEnd": [group(&hook)],
            "UserPromptSubmit": [group("trusty-memory prompt-context")],
        }
    })
}

#[test]
fn build_tree_hook_command_flags_the_incident_shape() {
    assert!(is_build_tree_hook_command(&format!(
        "{INCIDENT_EXE} hook --pm-guard"
    )));
    assert!(is_build_tree_hook_command(&format!("{INCIDENT_EXE} hook")));
}

#[test]
fn build_tree_hook_command_matches_every_tm_argv_shape() {
    for tail in TM_HOOK_ARGV_TAILS {
        assert!(
            is_build_tree_hook_command(&format!("{INCIDENT_EXE}{tail}")),
            "tail {tail:?} was not recognised"
        );
    }
}

#[test]
fn build_tree_hook_command_flags_other_build_layouts() {
    for exe in [
        "/repo/target/debug/deps/whatever-0123456789abcdef",
        "/repo/target/release/some_bin",
        "/repo/target-7224/debug/tm",
        "/home/me/.claude/worktrees/agent-abc/target/debug/deps/x-0123456789abcdef",
    ] {
        assert!(
            is_build_tree_hook_command(&format!("{exe} hook")),
            "{exe} was not recognised as a build-tree path"
        );
    }
}

#[test]
fn build_tree_hook_command_ignores_an_installed_binary() {
    assert!(!is_build_tree_hook_command("/usr/local/bin/tm hook"));
    assert!(!is_build_tree_hook_command(
        "/opt/tm/bin/tm hook --pm-guard"
    ));
    // Relative/bare names are the exact-name branch's business, not this one's.
    assert!(!is_build_tree_hook_command("tm hook"));
}

#[test]
fn build_tree_hook_command_ignores_a_foreign_argv_shape() {
    // The line drawn in the module doc: a build-tree path alone never suffices.
    for cmd in [
        format!("{INCIDENT_EXE} --check"),
        format!("{INCIDENT_EXE} lint --fix"),
        format!("{INCIDENT_EXE} hookup"),
        format!("{INCIDENT_EXE} hook --pm-guard --extra"),
    ] {
        assert!(
            !is_build_tree_hook_command(&cmd),
            "{cmd} should not be claimed"
        );
    }
}

#[test]
fn build_tree_statusline_command_flags_a_build_tree_binary() {
    assert!(is_build_tree_statusline_command(&format!(
        "{INCIDENT_EXE} statusline"
    )));
}

#[test]
fn build_tree_statusline_command_ignores_an_installed_binary() {
    assert!(!is_build_tree_statusline_command(
        "/usr/local/bin/tm statusline"
    ));
    assert!(!is_build_tree_statusline_command("tm statusline"));
}

#[test]
fn statusline_tail_is_not_a_hook_tail() {
    // The two lists must stay disjoint: a strip driven by the hook predicate
    // must never claim a `statusLine` command it does not put back.
    assert!(!is_build_tree_hook_command(&format!(
        "{INCIDENT_EXE} statusline"
    )));
    assert!(!TM_HOOK_ARGV_TAILS.contains(&STATUSLINE_ARGV_TAIL));
}
