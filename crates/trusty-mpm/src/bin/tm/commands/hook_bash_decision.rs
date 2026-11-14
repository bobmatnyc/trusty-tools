//! The `PreToolUse` Bash rewrite decision, and why it refused (#10973).
//!
//! Why: the opportunity ledger must say why a Bash result reached the model
//! uncompressed. The `PreToolUse` hook that decides and the `PostToolUse` hook
//! that measures are separate processes with no shared state, so the observer
//! replays the decision: [`bash_skip_reason`] runs [`pretooluse_bash_decision`]
//! — the function the `PreToolUse` rewrite itself calls — on the logged
//! command, cwd and tool input. The decision is a pure function of those
//! inputs plus the settings files and `PATH`, which do not change between the
//! two hooks of one call.
//! What: [`pretooluse_bash_decision`] is the rewrite with its refusal named;
//! [`bash_skip_reason`] maps it to the row's [`SkipReason`], first recognising
//! a command that already IS a rewrite (Claude Code reports the rewritten
//! input on `PostToolUse`).
//! Test: `a_rewritten_command_reads_as_compressed_or_leased`,
//! `the_skip_reason_replays_the_rewrite_decision`, and end to end
//! `a_bash_opportunity_row_records_why_no_rewrite_applied`.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::savings_opportunity::SkipReason;

use super::hook_rewrite::{build_pretooluse_rewrite_response, shlex_words};
use super::pm_guard_bash::build_lease_rewrite::LeaseRewrite;

/// The marker every compressing rewrite carries (brace, isolated and lease shapes).
const COMPRESS_MARKER: &str = "tm compress --tool";

/// The `PreToolUse` response for `cmd`, or the reason no rewrite applies.
///
/// Why (#10973): one decision serves both the rewrite and the observer's
/// `skip_reason`, so the two cannot disagree.
/// What: a heavy build gets the build-lease response, a lease refusal is
/// `HeavyBuild`; otherwise `hook_rewrite_isolated::cwd_rewrite_decision`
/// decides the compression rewrite and its permission. `Ok` exactly when
/// `misc::pretooluse_bash_rewrite` prints a response.
/// Test: `hook_emits_the_guards_lease_rewrite_for_a_heavy_build`,
/// `hook_compresses_a_filter_covered_command_inside_an_isolation_worktree`,
/// `the_skip_reason_replays_the_rewrite_decision`.
pub(crate) fn pretooluse_bash_decision(
    cmd: &str,
    tool_input: Option<&Value>,
    cwd: Option<&Path>,
) -> Result<String, SkipReason> {
    let cwd = cwd.unwrap_or(Path::new(""));
    match super::pm_guard_build_lease::decide_rewrite(cmd, tool_input, cwd) {
        (LeaseRewrite::Rewrite(new), permission) => Ok(
            super::pm_guard_build_lease::rewrite_response(tool_input, &new, permission),
        ),
        (LeaseRewrite::Refuse(_), _) => Err(SkipReason::HeavyBuild),
        // #10973: inside an isolation worktree the rewrite takes the one-command
        // shape its classifier accepts (#7477 refused the brace group there).
        (LeaseRewrite::None, _) => {
            super::hook_rewrite_isolated::cwd_rewrite_decision(cmd, Some(cwd)).map(|r| {
                match r.permission {
                    None => build_pretooluse_rewrite_response(&r.command).to_string(),
                    permission => super::pm_guard_build_lease::rewrite_response(
                        tool_input, &r.command, permission,
                    ),
                }
            })
        }
    }
}

/// Why the Bash call `tool_input` describes was or was not compressed.
///
/// What: `Unknown` with no `command`. A command that is already a rewrite is
/// `Compressed` when it pipes through `tm compress`, `HeavyBuild` when it is a
/// bare `tm build-lease`. Otherwise [`pretooluse_bash_decision`] is replayed:
/// a response that compresses is `Compressed`, one that only leases is
/// `HeavyBuild`, and a refusal is the refusing guard.
/// Test: `a_rewritten_command_reads_as_compressed_or_leased`,
/// `the_skip_reason_replays_the_rewrite_decision`.
pub(crate) fn bash_skip_reason(tool_input: Option<&Value>, cwd: Option<&Path>) -> SkipReason {
    let Some(command) = tool_input.and_then(|t| t["command"].as_str()) else {
        return SkipReason::Unknown;
    };
    if let Some(reason) = already_rewritten(command) {
        return reason;
    }
    match pretooluse_bash_decision(command, tool_input, cwd) {
        Ok(response) if response.contains(COMPRESS_MARKER) => SkipReason::Compressed,
        Ok(_) => SkipReason::HeavyBuild,
        Err(reason) => reason,
    }
}

/// The reason for a command that is itself one of the hook's rewrites.
fn already_rewritten(command: &str) -> Option<SkipReason> {
    if command.contains(COMPRESS_MARKER) {
        return Some(SkipReason::Compressed);
    }
    let words = shlex_words(command);
    let program = words.first()?.rsplit('/').next()?;
    let is_tm = matches!(program, "tm" | "trusty-mpm");
    (is_tm && words.get(1).map(String::as_str) == Some("build-lease"))
        .then_some(SkipReason::HeavyBuild)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reason(command: &str, cwd: &str) -> SkipReason {
        bash_skip_reason(Some(&json!({"command": command})), Some(Path::new(cwd)))
    }

    #[test]
    fn a_rewritten_command_reads_as_compressed_or_leased() {
        for command in [
            "{ git diff; printf '\\n[exit %s]\\n' \"$?\"; } | tm compress --tool \"git diff\"",
            "tm compress --tool \"git diff\" -- git diff",
        ] {
            assert_eq!(
                reason(command, "/repo"),
                SkipReason::Compressed,
                "{command}"
            );
        }
        assert_eq!(
            reason("/opt/bin/tm build-lease -- cargo build", "/repo"),
            SkipReason::HeavyBuild
        );
    }

    #[test]
    fn the_skip_reason_replays_the_rewrite_decision() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let cwd = cwd.path().to_str().expect("utf8");
        for (command, expected) in [
            ("git diff", SkipReason::Compressed),
            // D60: one leading `cd <dir> && ` compresses; a second `cd` or a pipe does not.
            ("cd /x && grep -n foo big.txt", SkipReason::Compressed),
            ("cd a && cd b && git diff", SkipReason::Chained),
            ("git diff | head", SkipReason::Chained),
            ("grep -c '#include' f.c", SkipReason::Unwrappable),
            ("make build", SkipReason::Orchestrator),
            ("cat -n src/lib.rs", SkipReason::SourceRead),
            ("git status", SkipReason::NoFilter),
            ("", SkipReason::EmptyCommand),
        ] {
            assert_eq!(reason(command, cwd), expected, "{command}");
        }
        assert_eq!(reason("git diff", ""), SkipReason::NoCwd);
        assert_eq!(
            bash_skip_reason(Some(&json!({})), None),
            SkipReason::Unknown
        );
    }
}
