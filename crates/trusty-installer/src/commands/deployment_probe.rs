//! The probes behind the deployment audit: a binary's `--version`, its content
//! check verb, and a running daemon's reported version (#10607).
//!
//! Why: split out of `deployment_record.rs` at the 500-SLOC cap. The audit
//! decides what a mismatch means; this module asks the machine.
//! What: [`binary_version`] and [`audit_content`] run the placed binary through
//! a [`CommandRunner`] whose production form has a deadline, so a hung member
//! is a failed check. [`classify_running`] reads a daemon probe; only "nothing
//! is listening" reads as not serving, and a probe that could not read a
//! version is [`RunningVersion::Unprobed`], never clean.
//! Test: `deployment_probe_tests.rs`, `deployment_record_tests.rs`.

use std::path::Path;

use super::content_step::{
    last_line, CommandRunner, ProcessRunner, CONTENT_CHECKS, MPM_CRATE, UNKNOWN_VERB,
};
use super::deployment_record::{DriftKind, RecordedBinary, RunningVersion};
use super::probe::probe_member_health;
use super::probe_http::{accept_only_member, presence_only, ProbeOutcome};
use super::stable_set::{daemon_members, StableMember};
use super::update_engine::extract_version_from_line;

/// Runs the member's content check verb and reports a failure through `find`.
///
/// What: `tm content status` for trusty-mpm, the [`CONTENT_CHECKS`] verb for a
/// member that has one, nothing otherwise. A runner error, a timeout included,
/// is [`DriftKind::ContentUnverified`].
/// Test: `deployment_record_tests::content_that_fails_its_check_is_drift`,
/// `deployment_probe_tests::a_hung_member_is_a_failed_check_not_a_stall_10607`.
pub(super) fn audit_content(
    member: &str,
    binaries: &[RecordedBinary],
    runner: &dyn CommandRunner,
    find: &mut dyn FnMut(DriftKind, String),
) {
    let check = if member == MPM_CRATE {
        // Exits non-zero when no verified content serves. The `trusty-mpm` alias
        // execs `tm`, so either answers.
        let tm = binaries.iter().find(|b| b.name == "tm");
        tm.or(binaries.first())
            .map(|b| (b, ["content", "status"].as_slice()))
    } else {
        CONTENT_CHECKS
            .iter()
            .find(|(name, _)| *name == member)
            .and_then(|(name, args)| {
                let review = binaries.iter().find(|b| b.name == *name);
                review.map(|b| (b, args.as_slice()))
            })
    };
    let Some((binary, args)) = check else {
        return;
    };
    let command = format!("{} {}", binary.path.display(), args.join(" "));
    match runner.run(&binary.path, args) {
        Ok(out) if out.success => {}
        // #10588: a release older than the verb has no manifest to check.
        Ok(out) if out.code == Some(2) && out.stderr.contains(UNKNOWN_VERB) => find(
            DriftKind::ContentUncheckable,
            format!("not checked: this binary predates `{}`", args.join(" ")),
        ),
        Ok(out) => {
            let why = last_line(&out.stderr).unwrap_or("no output");
            find(
                DriftKind::ContentNotServed,
                format!("`{command}` failed: {why}"),
            );
        }
        // #10607: a timeout lands here, as a check that could not run.
        Err(e) => find(
            DriftKind::ContentUnverified,
            format!("could not run `{command}`: {e}"),
        ),
    }
}

/// The version `path --version` prints; `Err` names why it could not be read,
/// a timeout included (#10607).
///
/// Test: `deployment_record_tests::a_version_that_cannot_be_read_is_undetermined`,
/// `deployment_probe_tests::a_hung_member_is_a_failed_check_not_a_stall_10607`.
pub(super) fn binary_version(runner: &dyn CommandRunner, path: &Path) -> Result<String, String> {
    let out = runner
        .run(path, &["--version"])
        .map_err(|e| e.to_string())?;
    if !out.success {
        return Err(format!("`--version` exited with {:?}", out.code));
    }
    extract_version_from_line(&out.stdout)
        .ok_or_else(|| format!("`--version` printed no version: {:?}", out.stdout.trim()))
}

/// What a probe outcome says about the running version.
///
/// Why: #10607 review — every non-serving outcome read as "not serving", so
/// `tctl upgrade` passed a daemon it could not probe as clean.
/// What: a health envelope is read for its status and version. Nothing
/// listening (`Refused`), no address to dial, or no binary is
/// [`RunningVersion::NotServing`]. Every other outcome (a timeout, an HTTP or
/// RPC error, an unreadable envelope, a probe that could not run) is
/// [`RunningVersion::Unprobed`].
/// Test: `deployment_record_tests::classify_running_reads_each_probe_outcome`,
/// `deployment_probe_tests::an_unprobeable_daemon_is_never_clean_10607`.
pub fn classify_running(outcome: &ProbeOutcome) -> RunningVersion {
    match outcome {
        ProbeOutcome::Serving { status, version } => {
            let word = status.to_ascii_lowercase();
            if word == "stale" || word == "version_below_floor" {
                RunningVersion::BelowFloor(status.clone())
            } else {
                version
                    .clone()
                    .map_or(RunningVersion::Unreported, RunningVersion::Reported)
            }
        }
        ProbeOutcome::Refused | ProbeOutcome::NoAddress | ProbeOutcome::NotInstalled => {
            RunningVersion::NotServing
        }
        // #10607: something may be running a build nobody could read.
        other => RunningVersion::Unprobed(format!("{other:?}")),
    }
}

/// The running version of a stable-set member, given its probe outcome.
///
/// Test: `deployment_probe_tests::running_of_skips_members_with_no_version_contract_10607`.
pub fn running_of(member: &StableMember, outcome: &ProbeOutcome) -> RunningVersion {
    if presence_only(&member.binary) || accept_only_member(&member.binary) {
        RunningVersion::NotApplicable
    } else {
        classify_running(outcome)
    }
}

/// The running version of a recorded member, probed now.
pub(super) fn probe_running(crate_name: &str) -> RunningVersion {
    daemon_members()
        .iter()
        .find(|m| m.crate_name == crate_name)
        .map_or(RunningVersion::NotApplicable, |m| {
            running_of(m, &probe_member_health(&m.binary, m.manage))
        })
}

/// The version a placed binary reports, for a path that verified it some other way.
///
/// Test: `deployment_probe_tests::a_bounded_run_returns_what_the_child_printed_10607`.
pub fn read_version(path: &Path) -> Option<String> {
    binary_version(&ProcessRunner::probe(), path).ok()
}

#[cfg(test)]
#[path = "deployment_probe_tests.rs"]
mod tests;
