//! `tm doctor` row and `--fix` repair for credentials in a LaunchAgent (#8236).
//!
//! Why: `trusty_common::launchd::LaunchdConfig::render_plist` now refuses to
//! write a credential into a plist, which protects every FUTURE install. It
//! does nothing for the plaintext already sitting in
//! `~/Library/LaunchAgents/com.trusty.*.plist` on a host that installed before
//! the guard, or whose unit was hand-edited and is never regenerated — which is
//! the whole of the #8236 report. This is the detection and the in-place
//! remediation for those.
//!
//! What: [`check_launchd_plist_secrets`] is the read-only row, and
//! [`repair_launchd_plist_secrets`] is the rewrite `tm doctor --fix --yes`
//! applies. Both name KEYS only; neither ever reads a credential into a message
//! or a log line.
//!
//! **No backup is taken**, unlike every other `--fix` repair. A backup of a
//! plist holding a credential is a second user-readable copy of that
//! credential, which is the defect, not a safety net. The repair removes only
//! the credential-keyed entries and leaves every other byte in place, so there
//! is nothing else to restore.
//!
//! Test: `doctor_launchd_secrets_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_common::launchd_secrets::{ScrubbedPlist, scrub_plist_credential_env};

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// The `tm doctor` row name, shared by the check and every repair step.
const CHECK_NAME: &str = "launchd_secrets";

/// Filename prefix of the LaunchAgents this row judges.
///
/// Why: `--fix` rewrites files, and the only files tm may rewrite are the ones
/// tm's own installers generate. A foreign agent's plist is the operator's, and
/// is neither reported nor touched.
const TRUSTY_PLIST_PREFIX: &str = "com.trusty.";

/// What one installed plist was found to hold.
///
/// Why: the check and the repair share one scan shape so they cannot disagree
/// about which files are implicated.
/// What: the path plus either the credential keys found, or the reason the file
/// could not be judged. Never the value.
/// Test: `scan_names_the_key_not_the_value`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlistFinding {
    /// The plist scanned.
    pub path: PathBuf,
    /// Credential-bearing `EnvironmentVariables` keys, in document order.
    pub keys: Vec<String>,
    /// Why the file could not be judged, when it could not be.
    pub unreadable: Option<String>,
}

impl PlistFinding {
    /// Does this finding require operator action?
    fn actionable(&self) -> bool {
        !self.keys.is_empty() || self.unreadable.is_some()
    }
}

/// Scan `<home>/Library/LaunchAgents` for trusty plists holding credentials.
///
/// Why: one scan, used by the row and by the repair, so `tm doctor` and
/// `tm doctor --fix` can never report different files.
/// What: reads every `com.trusty.*.plist` in the directory and runs
/// [`scrub_plist_credential_env`] over it, discarding the rewrite. A file that
/// cannot be read or parsed yields a finding with `unreadable` set — never a
/// silent skip, because "could not read" is the one answer that must not
/// render as clean. `Err` when the directory cannot be listed, or when any
/// single entry in it cannot be resolved — an entry dropped from the walk is
/// a file the scan did not judge, and the caller has to hear that as UNKNOWN
/// rather than as one fewer clean plist. An ABSENT directory is `Ok(vec![])`
/// (a host with no LaunchAgents has no exposure).
/// Test: `scan_names_the_key_not_the_value`, `scan_reports_an_unparseable_plist`,
/// `scan_reports_an_unreadable_plist`, `scan_ignores_foreign_plists`,
/// `scan_is_empty_without_a_launch_agents_dir`,
/// `scan_errors_when_the_directory_cannot_be_listed`.
pub fn scan_launch_agents(home: &Path) -> std::io::Result<Vec<PlistFinding>> {
    let dir = home.join("Library/LaunchAgents");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut out = Vec::new();
    for entry in entries {
        // #8236: `?`, never `.flatten()` — a dropped entry is a plist nobody
        // judged, and the scan would report the host clean without it.
        let path = entry?.path();
        // Lossy, not `to_str()` — a name this cannot decode would be SKIPPED by
        // a `?`-less `to_str()` arm, which is the same silent drop as above.
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !name.starts_with(TRUSTY_PLIST_PREFIX) || !name.ends_with(".plist") {
            continue;
        }
        out.push(match std::fs::read_to_string(&path) {
            Ok(xml) => finding_for(path, &xml),
            Err(e) => PlistFinding {
                path,
                keys: Vec::new(),
                unreadable: Some(format!("could not read it: {}", e.kind())),
            },
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Judge one plist's text.
///
/// Why: separated from the directory walk so the parse-failure arm is testable
/// without a filesystem that can produce one.
/// Test: `scan_reports_an_unparseable_plist`.
fn finding_for(path: PathBuf, xml: &str) -> PlistFinding {
    match scrub_plist_credential_env(xml) {
        Ok(ScrubbedPlist { keys, .. }) => PlistFinding {
            path,
            keys,
            unreadable: None,
        },
        Err(e) => PlistFinding {
            path,
            keys: Vec::new(),
            unreadable: Some(e.reason),
        },
    }
}

/// The `launchd_secrets` doctor row.
///
/// Why: #8236 was found by a human running `plutil -p` on a plist while looking
/// for a log path. Nothing in the diagnostic looked, so the exposure had no
/// expiry date.
/// What: `Fail` when any trusty LaunchAgent carries a credential-keyed
/// `EnvironmentVariables` entry, naming the file and the KEY and pointing at
/// `tm doctor --fix --yes`. `Unknown` — never `Ok` — when the directory or a
/// plist could not be read or parsed, because a scan that did not run has not
/// shown the host clean. `Ok` otherwise.
/// Test: `row_fails_and_names_the_key_not_the_value`, `row_is_ok_when_clean`,
/// `row_is_unknown_when_a_plist_cannot_be_parsed`,
/// `row_is_unknown_when_the_directory_cannot_be_listed`.
pub(crate) fn check_launchd_plist_secrets(home: &Path) -> DoctorCheck {
    match scan_launch_agents(home) {
        Ok(findings) => build_row(&findings),
        Err(e) => DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "could not list ~/Library/LaunchAgents ({}) — whether a LaunchAgent \
                 holds a plaintext credential is UNKNOWN",
                e.kind()
            ),
        ),
    }
}

/// The pure verdict behind [`check_launchd_plist_secrets`].
///
/// Why: keeps the three arms unit-testable without a real home directory.
/// What: see [`check_launchd_plist_secrets`]. A credential found outranks an
/// unreadable file — a confirmed exposure is worse news than an unknown one —
/// but the Fail message still NAMES the files it could not judge, so ranking
/// the statuses never drops the unjudged file from the report.
/// Test: `row_fails_and_names_the_key_not_the_value`, `row_is_ok_when_clean`,
/// `row_is_unknown_when_a_plist_cannot_be_parsed`,
/// `row_fail_still_names_the_plists_it_could_not_judge`.
fn build_row(findings: &[PlistFinding]) -> DoctorCheck {
    let unreadable: Vec<String> = findings
        .iter()
        .filter_map(|f| {
            f.unreadable
                .as_ref()
                .map(|why| format!("{} ({why})", f.path.display()))
        })
        .collect();

    let exposed: Vec<&PlistFinding> = findings.iter().filter(|f| !f.keys.is_empty()).collect();
    if !exposed.is_empty() {
        let detail = exposed
            .iter()
            .map(|f| format!("{} ({})", f.path.display(), f.keys.join(", ")))
            .collect::<Vec<_>>()
            .join("; ");
        // #8236: a confirmed exposure outranks an unknown one, but it never
        // hides it — the unjudged files ride along in the same message.
        let unjudged = if unreadable.is_empty() {
            String::new()
        } else {
            format!(
                ". {} further plist(s) could not be judged at all: {}",
                unreadable.len(),
                unreadable.join("; ")
            )
        };
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Fail,
            format!(
                "a LaunchAgent plist holds a plaintext credential — the file is \
                 user-readable and lands in every backup: {detail}. Run \
                 `tm doctor --fix --yes` to remove the entries, then ROTATE those \
                 credentials and supply them at runtime (process env, `.env.local`, \
                 or the 0600 credential store){unjudged}"
            ),
        );
    }

    if !unreadable.is_empty() {
        return DoctorCheck::new(
            CHECK_NAME,
            CheckStatus::Unknown,
            format!(
                "could not judge {} LaunchAgent plist(s): {} — whether they hold a \
                 plaintext credential is UNKNOWN",
                unreadable.len(),
                unreadable.join("; ")
            ),
        );
    }

    DoctorCheck::new(
        CHECK_NAME,
        CheckStatus::Ok,
        format!(
            "no plaintext credential in {} trusty LaunchAgent plist(s)",
            findings.len()
        ),
    )
}

/// Rewrite every trusty LaunchAgent that holds a credential, in place.
///
/// Why: the renderer guard only covers a unit that gets regenerated. A host
/// whose plist is hand-maintained — which is how #8236's `com.trusty.mpm.plist`
/// came to hold two credentials, since no code in this workspace writes that
/// file — is never reached by an install. This is.
/// What: one [`RepairStep`] per implicated file.
/// [`StepStatus::Planned`] under [`RepairMode::DryRun`];
/// [`StepStatus::Applied`] with NO backup (see the module header) once written;
/// [`StepStatus::Failed`] when the plist cannot be parsed or the rewrite cannot
/// be written — never a warning followed by a pass. A clean host produces no
/// steps at all.
///
/// Rotation is NOT part of this and cannot be: the value was readable by
/// everything on the host for as long as it sat there, so removing it makes the
/// file safe and the credential still compromised. The step text says so.
/// Test: `repair_plans_without_writing`, `repair_removes_the_entry`,
/// `repair_fails_loudly_on_an_unparseable_plist`,
/// `repair_fails_loudly_when_the_plist_is_unwritable`,
/// `repair_fails_loudly_when_the_directory_cannot_be_listed`,
/// `repair_produces_no_steps_for_a_clean_host`.
pub fn repair_launchd_plist_secrets(home: &Path, mode: RepairMode) -> Vec<RepairStep> {
    let findings = match scan_launch_agents(home) {
        Ok(findings) => findings,
        Err(e) => {
            return vec![RepairStep {
                check: CHECK_NAME,
                path: home.join("Library/LaunchAgents"),
                what: "remove plaintext credentials from the trusty LaunchAgent plists".to_string(),
                status: StepStatus::Failed(format!("could not list the directory: {}", e.kind())),
            }];
        }
    };

    findings
        .iter()
        .filter(|f| f.actionable())
        .map(|f| repair_one(f, mode))
        .collect()
}

/// One file's repair.
///
/// Why: the unreadable arm and the rewrite arm both have to produce a step, so
/// an operator running `--fix` sees the file that could NOT be fixed beside the
/// ones that were.
/// Test: as [`repair_launchd_plist_secrets`].
fn repair_one(finding: &PlistFinding, mode: RepairMode) -> RepairStep {
    let what = format!(
        "remove {} plaintext credential entr{} ({}) from EnvironmentVariables — \
         no backup is taken (a backup would be a second readable copy); ROTATE \
         the credential, removing it here does not un-expose it",
        finding.keys.len().max(1),
        if finding.keys.len() == 1 { "y" } else { "ies" },
        if finding.keys.is_empty() {
            "unknown".to_string()
        } else {
            finding.keys.join(", ")
        },
    );
    let step = |status| RepairStep {
        check: CHECK_NAME,
        path: finding.path.clone(),
        what: what.clone(),
        status,
    };

    if let Some(why) = &finding.unreadable {
        return step(StepStatus::Failed(why.clone()));
    }
    if mode == RepairMode::DryRun {
        return step(StepStatus::Planned);
    }

    // Re-read rather than trusting the scan's copy: `--fix` runs after the
    // report, and a reinstall in between would have changed the file.
    let xml = match std::fs::read_to_string(&finding.path) {
        Ok(xml) => xml,
        Err(e) => {
            return step(StepStatus::Failed(format!(
                "could not read it: {}",
                e.kind()
            )));
        }
    };
    let scrubbed = match scrub_plist_credential_env(&xml) {
        Ok(scrubbed) => scrubbed,
        Err(e) => return step(StepStatus::Failed(e.reason)),
    };
    if scrubbed.keys.is_empty() {
        return step(StepStatus::Refused(
            "the plist no longer holds a credential — nothing to remove".to_string(),
        ));
    }
    match std::fs::write(&finding.path, &scrubbed.xml) {
        // No backup, deliberately — see the module header.
        Ok(()) => step(StepStatus::Applied { backup: None }),
        Err(e) => step(StepStatus::Failed(format!(
            "could not write the scrubbed plist: {}",
            e.kind()
        ))),
    }
}

#[cfg(test)]
#[path = "doctor_launchd_secrets_tests.rs"]
mod tests;
