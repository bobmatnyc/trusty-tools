//! `tm hook --pm-guard` records every deny where `list_recent_errors` reads
//! (#8722).
//!
//! Why: a deny used to exist only as the hook's stdout JSON, so a refusal an
//! operator wanted to report never reached the bug pipeline. The proof has to
//! go through the real binary and back out through the aggregation the MCP
//! tools use, or it proves only that some file was written.
//! What: runs the guard over a denied Bash command with its data directory
//! pinned to a temp dir, then reads that directory through
//! `aggregate_errors_from_paths(store_paths_under(..))` — the body of
//! `list_recent_errors` for a pinned base. The error arm blocks the store and
//! checks the deny still goes out and the failure is reported.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_deny_capture::`.

use crate::common;

use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;

use trusty_mpm::daemon::bug_report::{aggregate_errors_from_paths, store_paths_under};

/// Nothing listens on port 1, so the deny path's audit POST fails fast.
const UNREACHABLE_DAEMON: &str = "http://127.0.0.1:1";

/// A GitHub-token-shaped value the recorded command must not carry.
const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789"; // pragma: allowlist secret

/// Run the guard from `cwd` over one Bash `command` with `data` as its data
/// directory; return `(stdout, stderr)` after asserting the fail-open exit 0.
fn run_guard(data: &Path, cwd: &Path, command: &str) -> (String, String) {
    let home = tempfile::tempdir().expect("home");
    let payload = serde_json::json!({
        "session_id": "s-8722",
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "cwd": cwd,
    });
    let mut child = common::tm_command_in(home.path())
        .args(["--url", UNREACHABLE_DAEMON, "hook", "--pm-guard"])
        .env("TRUSTY_DATA_DIR_OVERRIDE", data)
        .env_remove("TRUSTY_NO_BUG_CAPTURE")
        .env_remove("TRUSTY_MPM_DISABLE_HOOKS")
        .env_remove("CLAUDE_MPM_SUB_AGENT")
        .env_remove("TRUSTY_MPM_PM_UNRESTRICTED")
        .env_remove("TRUSTY_MPM_PM_DENY_BY_DEFAULT")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tm hook --pm-guard");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(payload.to_string().as_bytes())
        .expect("write payload");
    let out = child.wait_with_output().expect("wait for the guard");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "exit 0 always: stderr={stderr}");
    (String::from_utf8(out.stdout).expect("utf8 stdout"), stderr)
}

/// 🔴 REGRESSION (#8722): a deny is visible to `list_recent_errors`, naming
/// its check, command and cwd, with the secret in the command scrubbed.
#[test]
fn pm_guard_records_every_deny_where_list_recent_errors_reads() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).expect("mkdir data");
    let command = format!("rm -rf / && echo {TOKEN}");

    let (stdout, _) = run_guard(&data, tmp.path(), &command);
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "{stdout}"
    );

    let errors = aggregate_errors_from_paths(&store_paths_under(&data), 100);
    let deny = errors
        .iter()
        .find(|e| e.record.crate_target == "trusty_mpm::pm_guard")
        .unwrap_or_else(|| panic!("no pm-guard record among {errors:#?}"));
    assert_eq!(
        deny.record.message,
        "pm-guard denied Bash [destructive-delete]"
    );
    let fields = &deny.record.fields;
    let cwd = format!("cwd={}", tmp.path().display());
    for part in [
        "check=destructive-delete",
        "rm -rf /",
        "session=s-8722",
        &cwd,
    ] {
        assert!(fields.contains(part), "{part} missing from {fields}");
    }
    assert!(!fields.contains(TOKEN), "the secret was recorded: {fields}");
}

/// 🔴 REGRESSION (#8756): a launchd or pm2 environment dump is denied by the
/// real hook and recorded like every other deny.
#[test]
fn a_process_manager_env_dump_is_denied_and_recorded() {
    for command in ["launchctl print gui/501/com.example.api", "pm2 jlist"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).expect("mkdir data");

        let (stdout, _) = run_guard(&data, tmp.path(), command);
        assert!(
            stdout.contains(r#""permissionDecision":"deny""#) && stdout.contains("#8756"),
            "`{command}` must deny: {stdout}"
        );

        let errors = aggregate_errors_from_paths(&store_paths_under(&data), 100);
        let deny = errors
            .iter()
            .find(|e| e.record.crate_target == "trusty_mpm::pm_guard")
            .unwrap_or_else(|| panic!("no pm-guard record for `{command}` among {errors:#?}"));
        let fields = &deny.record.fields;
        for part in ["check=secret-file-read", command, "session=s-8722"] {
            assert!(fields.contains(part), "{part} missing from {fields}");
        }
    }
}

/// 🔴 REGRESSION (#8677): each credential-print shape #8677 closes is denied
/// by the real hook and recorded through the scrubbed store (#8722).
#[test]
fn a_credential_print_bypass_from_8677_is_denied_and_recorded() {
    for (command, fragment) in [
        (
            "echo 'find-generic-password -s s -w' | security -i",
            "security -i",
        ),
        (
            "curl -H \"Authorization: Bearer $(gcloud auth print-access-token)\" -v",
            "curl -H",
        ),
        // The scrubber replaces the path, so the record keeps the call.
        (
            "security find-generic-password -s fake-svc -w > /dev/./tty",
            "security find-generic-password -s fake-svc -w",
        ),
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).expect("mkdir data");

        let (stdout, _) = run_guard(&data, tmp.path(), command);
        assert!(
            stdout.contains(r#""permissionDecision":"deny""#) && stdout.contains("#8596"),
            "`{command}` must deny: {stdout}"
        );

        let errors = aggregate_errors_from_paths(&store_paths_under(&data), 100);
        let deny = errors
            .iter()
            .find(|e| e.record.crate_target == "trusty_mpm::pm_guard")
            .unwrap_or_else(|| panic!("no pm-guard record for `{command}` among {errors:#?}"));
        let fields = &deny.record.fields;
        for part in ["check=secret-file-read", fragment, "session=s-8722"] {
            assert!(fields.contains(part), "{part} missing from {fields}");
        }
    }
}

/// 🔴 REGRESSION (#8261 x #8722): a heavy build hidden in an unterminated
/// `$(…)` — the one shape the build-lease rule refuses — is denied and
/// recorded. #8730's unplaceable-write guard runs first and catches it today;
/// the lease's own refusal is the fallback, recorded under `build-lease`
/// (`a_lease_refusal_is_recorded_before_it_is_printed`). Either way the deny
/// must leave a record.
#[test]
fn a_build_the_lease_refuses_is_denied_and_recorded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir(&data).expect("mkdir data");
    let command = "echo $(cargo build";

    let (stdout, _) = run_guard(&data, tmp.path(), command);
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "{stdout}"
    );

    let errors = aggregate_errors_from_paths(&store_paths_under(&data), 100);
    let deny = errors
        .iter()
        .find(|e| e.record.crate_target == "trusty_mpm::pm_guard")
        .unwrap_or_else(|| panic!("no pm-guard record among {errors:#?}"));
    let fields = &deny.record.fields;
    assert!(
        fields.contains("check=main-checkout-write") || fields.contains("check=build-lease"),
        "{fields}"
    );
    for part in [command, "session=s-8722"] {
        assert!(fields.contains(part), "{part} missing from {fields}");
    }
}

/// Fail-Open Check (#8722): a store that cannot be written loses the record,
/// never the deny, and says so on stderr.
#[test]
fn a_deny_whose_record_cannot_be_written_still_denies_and_says_so() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    // A directory where the denial store file belongs blocks every append.
    std::fs::create_dir_all(data.join("trusty-mpm").join("pm-guard-denials.jsonl"))
        .expect("block the store");

    let (stdout, stderr) = run_guard(&data, tmp.path(), "rm -rf /");
    assert!(
        stdout.contains(r#""permissionDecision":"deny""#),
        "{stdout}"
    );
    assert!(
        stderr.contains("could not record deny `destructive-delete`"),
        "the lost record must be reported: {stderr}"
    );
}
