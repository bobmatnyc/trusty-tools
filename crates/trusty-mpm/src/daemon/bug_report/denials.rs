//! The store every `tm hook --pm-guard` deny is recorded in (#8722).
//!
//! Why: `list_recent_errors` and `preview_bug_report` read only captured
//! records, and a pm-guard deny never produced one — it existed only as the
//! hook's `permissionDecisionReason` JSON, so a refusal an operator wanted to
//! report was invisible to the bug pipeline. Denials get their own file, beside
//! the daemon's `errors.jsonl`, so a busy session's refusals never push real
//! daemon errors out of the per-store read window.
//! What: [`denial_record`] turns one deny into a [`CapturedError`] carrying the
//! check slug, tool, command and cwd, with the command scrubbed of secrets;
//! [`append_denial`] appends it as one JSON line; [`pm_guard_denials_path`]
//! names the file [`super::aggregate_errors`] reads it back from.
//! Test: this module's suite, and `tests/tm_hook_pm_guard_deny_capture.rs`
//! end to end.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use trusty_common::error_capture::CapturedError;
use trusty_common::error_capture::fingerprint::compute_fingerprint;

/// File name of the denial store, under the `trusty-mpm` data directory.
pub const PM_GUARD_DENIALS_FILE: &str = "pm-guard-denials.jsonl";

/// `crate_target` of every denial record; the `[…]` prefix of its summary.
pub const PM_GUARD_DENY_TARGET: &str = "trusty_mpm::pm_guard";

/// One refused tool call, as the hook saw it.
#[derive(Debug, Clone, Copy)]
pub struct DeniedCall<'a> {
    /// Slug naming the rule that refused the call, e.g. `destructive-delete`.
    pub check: &'a str,
    /// The payload's `tool_name`.
    pub tool: &'a str,
    /// The Bash command, or the file/agent the tool call named. Unscrubbed.
    pub command: &'a str,
    /// The directory the call was made from.
    pub cwd: &'a str,
    /// The payload's `session_id`.
    pub session_id: &'a str,
    /// The refusal text the agent was shown.
    pub reason: &'a str,
}

/// The ambient denial store: `<data_dir>/trusty-mpm/pm-guard-denials.jsonl`.
///
/// `None` when the data directory cannot be resolved; it creates the
/// directory, as `trusty_common::resolve_data_dir` always does.
#[must_use]
pub fn pm_guard_denials_path() -> Option<PathBuf> {
    trusty_common::resolve_data_dir("trusty-mpm")
        .ok()
        .map(|dir| dir.join(PM_GUARD_DENIALS_FILE))
}

/// Build the captured record for one deny.
///
/// Why: the record must be readable by the unchanged aggregation and preview
/// code, so it reuses [`CapturedError`]; and one rule refusing many different
/// commands is one bug, not many, so the fingerprint covers the check and tool
/// only.
/// What: `message` is `pm-guard denied <tool> [<check>]`; `fields` carries
/// `check`, `tool`, `cwd`, `session`, the scrubbed `command` and `reason`. The
/// fingerprint hashes the target and message, never the command.
/// Test: `denial_record_carries_check_command_and_cwd_without_secrets`,
/// `denials_of_one_check_share_a_fingerprint`.
#[must_use]
pub fn denial_record(call: &DeniedCall<'_>, timestamp_secs: u64) -> CapturedError {
    let message = format!("pm-guard denied {} [{}]", call.tool, call.check);
    let command = super::scrub(call.command).text;
    let fields = format!(
        "check={} tool={} cwd={} session={} command={command:?} reason={:?}",
        call.check, call.tool, call.cwd, call.session_id, call.reason
    );
    CapturedError {
        timestamp_secs,
        crate_target: PM_GUARD_DENY_TARGET.to_string(),
        crate_version: env!("CARGO_PKG_VERSION").to_string(),
        fingerprint: compute_fingerprint(PM_GUARD_DENY_TARGET, &message, None, None),
        message,
        fields,
        file: None,
        line: None,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
    }
}

/// Append `record` to the JSONL store at `path` as one line.
///
/// The line goes out in a single `write_all` on an `O_APPEND` handle, so two
/// hooks denying at once do not interleave inside a line.
///
/// # Errors
/// Any failure to serialise, open or write; the caller decides what a lost
/// record costs.
/// Test: `append_denial_writes_one_line_per_record_and_reports_an_unwritable_store`.
pub fn append_denial(path: &Path, record: &CapturedError) -> std::io::Result<()> {
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call<'a>(check: &'a str, command: &'a str) -> DeniedCall<'a> {
        DeniedCall {
            check,
            tool: "Bash",
            command,
            cwd: "/tmp/repo",
            session_id: "s-1",
            reason: "tm pm-guard: refused",
        }
    }

    #[test]
    fn denial_record_carries_check_command_and_cwd_without_secrets() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789"; // pragma: allowlist secret
        let command = format!("rm -rf build && echo {token}");
        let record = denial_record(&call("destructive-delete", &command), 7);
        assert_eq!(record.crate_target, PM_GUARD_DENY_TARGET);
        assert_eq!(record.message, "pm-guard denied Bash [destructive-delete]");
        assert_eq!(record.timestamp_secs, 7);
        for part in ["check=destructive-delete", "cwd=/tmp/repo", "rm -rf build"] {
            assert!(record.fields.contains(part), "{part}: {}", record.fields);
        }
        assert!(!record.fields.contains(token), "{}", record.fields);
    }

    #[test]
    fn denials_of_one_check_share_a_fingerprint() {
        let a = denial_record(&call("worktree-add", "git worktree add /tmp/a"), 1);
        let b = denial_record(&call("worktree-add", "git worktree add /tmp/b"), 2);
        let other = denial_record(&call("head-switch", "git worktree add /tmp/a"), 1);
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_ne!(a.fingerprint, other.fingerprint);
    }

    #[test]
    fn append_denial_writes_one_line_per_record_and_reports_an_unwritable_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(PM_GUARD_DENIALS_FILE);
        let record = denial_record(&call("head-switch", "git switch main"), 3);
        append_denial(&path, &record).expect("append");
        append_denial(&path, &record).expect("append");
        let read = trusty_common::error_capture::ErrorStore::read_records(&path, 10);
        assert_eq!(read, vec![record.clone(), record.clone()]);

        // Error arm: a directory where the file should be is reported, not swallowed.
        let blocked = tmp.path().join("blocked");
        std::fs::create_dir(&blocked).expect("mkdir");
        assert!(append_denial(&blocked, &record).is_err());
    }
}
