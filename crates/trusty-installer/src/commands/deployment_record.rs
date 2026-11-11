//! What `tctl install` placed, and whether the machine still matches it (#10607).
//!
//! Why: `tctl stack health` could answer `ready` while a binary had been swapped
//! for another build, a daemon still ran the build a reinstall replaced, or a
//! member's second binary was gone. Nothing recorded what install placed, so
//! nothing could compare.
//!
//! What: install, upgrade and self-update write [`DeploymentRecord`] (each placed
//! binary's path, version and sha256) to `deployment.json` in the installer's data
//! dir. [`audit`] compares the record with the disk, the running daemons and the
//! installed content, and returns a [`DeploymentAudit`]. `stack health`,
//! `stack doctor`, `install` and `upgrade` fold that audit into their verdicts.
//! A check that cannot run yields an undetermined finding, never a clean state.
//!
//! Test: `deployment_record_tests.rs`, `tests/deployment_drift_e2e.rs`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use trusty_common::integrity::Sha256Digest;

use super::content_step::{CommandRunner, ProcessRunner};
use super::deployment_probe::{audit_content, binary_version, probe_running};
pub use super::deployment_probe::{classify_running, read_version, running_of};

/// The record's schema; a record naming any other one is not trusted.
const SCHEMA: u32 = 1;
/// The app name whose data dir holds the record.
const APP: &str = "trusty-installer";
/// The record's file name in that data dir.
pub const RECORD_FILE: &str = "deployment.json";
/// What an operator runs to bring the machine and the record back in line.
const REMEDY: &str = "run `tctl install`";

/// What an operator runs to bring `member` back in line with the record.
///
/// Why: #10607 review — a daemon still running a replaced build needs a
/// restart; `tctl install` never restarts a loaded daemon.
/// Test: `deployment_probe_tests::a_stale_daemon_names_tctl_restart_10607`.
fn remedy(kind: DriftKind, member: &str) -> String {
    let verb = if kind.is_running() {
        "restart"
    } else {
        "install"
    };
    format!("run `tctl {verb} {member}`")
}

/// One binary as install placed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedBinary {
    /// File name.
    pub name: String,
    /// Concrete path it was placed at.
    pub path: PathBuf,
    /// The version the install verified; `None` for a sibling of the health-gated
    /// binary. A sibling such as `trusty-memory-mcp-bridge` has no `--version`
    /// contract and may not exit, so only its presence and digest are checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Build identity: sha256 of the file. `None` when it could not be read.
    pub sha256: Option<String>,
}

/// Every member's placed binaries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentRecord {
    /// [`SCHEMA`] at write time.
    pub schema: u32,
    /// Crate name to its placed binaries.
    pub members: BTreeMap<String, Vec<RecordedBinary>>,
}

/// One member an install just placed and health-gated.
#[derive(Clone, Debug)]
pub struct Placement {
    /// Crate name.
    pub crate_name: String,
    /// The health-gated binary's concrete path.
    pub path: PathBuf,
    /// The version that binary reported.
    pub version: String,
}

impl Placement {
    /// Builds a placement.
    pub fn new(crate_name: &str, path: &Path, version: &str) -> Self {
        Self {
            crate_name: crate_name.to_owned(),
            path: path.to_path_buf(),
            version: version.to_owned(),
        }
    }
}

/// What reading the record found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordLoad {
    /// No record exists.
    Absent,
    /// A record exists and cannot be trusted; the text says why.
    Corrupt(String),
    /// The record.
    Loaded(DeploymentRecord),
}

/// What a running daemon said about its version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunningVersion {
    /// The member has no version contract to ask: not a daemon, a presence
    /// probe (the `--version` check already covers it), or the accept-only bus.
    NotApplicable,
    /// Nothing answered. The health rows report that as `down`.
    NotServing,
    /// It answered and named this version.
    Reported(String),
    /// It answered healthy and named no version.
    Unreported,
    /// It answered that it runs below its version floor.
    BelowFloor(String),
    /// Something may be running and its version could not be read; the text is
    /// the probe outcome (#10607).
    Unprobed(String),
}

/// Why one member does not match the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// A recorded binary is gone.
    MissingBinary,
    /// A recorded binary reports another version.
    WrongVersion,
    /// A recorded binary's bytes changed at the same version.
    BuildChanged,
    /// A running daemon reports another version than the placed binary.
    RunningVersion,
    /// A running daemon says it is below its version floor.
    RunningBelowFloor,
    /// The installed content fails its check.
    ContentNotServed,
    /// An installed member is not in the record.
    Unrecorded,
    /// A recorded binary's version or digest could not be read.
    Unverified,
    /// A running daemon did not name its version.
    RunningUnreported,
    /// A daemon's probe could not read its version (#10607).
    RunningUnprobed,
    /// The content check could not run.
    ContentUnverified,
    /// A placed member's binary predates the content check verb.
    ContentUncheckable,
    /// No usable record exists.
    NoRecord,
    /// The record could not be written.
    RecordWriteFailed,
}

impl DriftKind {
    /// Whether this kind means "could not tell" rather than "does not match".
    fn undetermined(self) -> bool {
        matches!(
            self,
            Self::Unrecorded
                | Self::Unverified
                | Self::RunningUnreported
                | Self::RunningUnprobed
                | Self::ContentUnverified
                | Self::ContentUncheckable
                | Self::NoRecord
                | Self::RecordWriteFailed
        )
    }

    /// Whether the finding is about a running daemon, so the remedy is a restart.
    fn is_running(self) -> bool {
        matches!(
            self,
            Self::RunningVersion
                | Self::RunningBelowFloor
                | Self::RunningUnreported
                | Self::RunningUnprobed
        )
    }
}

/// One audit finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DriftFinding {
    /// Crate name, or [`RECORD_MEMBER`] for the record itself.
    pub member: String,
    /// Why it does not match.
    pub kind: DriftKind,
    /// What was found, and the remedy.
    pub detail: String,
}

/// [`DriftFinding::member`] for a finding about the record, not one member.
pub const RECORD_MEMBER: &str = "deployment-record";

/// The audit's result; `state` is `clean`, `drifted` or `undetermined`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeploymentAudit {
    /// Worst finding: `drifted`, else `undetermined`, else `clean`.
    pub state: &'static str,
    /// Every finding.
    pub findings: Vec<DriftFinding>,
}

impl DeploymentAudit {
    fn from_findings(findings: Vec<DriftFinding>) -> Self {
        let state = if findings.iter().any(|f| !f.kind.undetermined()) {
            "drifted"
        } else if findings.is_empty() {
            "clean"
        } else {
            "undetermined"
        };
        Self { state, findings }
    }

    /// Keeps the findings about `members` and the record itself.
    ///
    /// Why: `stack doctor <member>` triages one member.
    /// Test: `scoping_keeps_the_member_and_record_findings`.
    pub fn scoped_to(self, members: &[String]) -> Self {
        Self::from_findings(
            self.findings
                .into_iter()
                .filter(|f| f.member == RECORD_MEMBER || members.contains(&f.member))
                .collect(),
        )
    }

    /// Whether an install or upgrade must not exit 0 over this audit.
    ///
    /// Why: every finding blocks except a binary older than the content check
    /// verb, which #10588 reports as not checked and lets the install pass.
    /// Health still reads that finding as undetermined.
    /// Test: `an_older_review_blocks_no_install_but_health_is_undetermined`.
    pub fn blocks_install(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.kind != DriftKind::ContentUncheckable)
    }
}

/// Folds an audit into a rollup verdict, worst first.
///
/// Why: `stack health` and `stack doctor` share the `degraded` / `undetermined`
/// words and differ only in their passing word, which `rows` already holds.
/// Test: `verdict_fold_is_worst_wins`.
pub fn fold_verdict(rows: &'static str, audit: &DeploymentAudit) -> &'static str {
    if rows == "degraded" || audit.state == "drifted" {
        "degraded"
    } else if rows == "undetermined" || audit.state == "undetermined" {
        "undetermined"
    } else {
        rows
    }
}

/// The record's default path.
pub fn default_record_path() -> Result<PathBuf, String> {
    trusty_common::data_dir::resolve_data_dir(APP)
        .map(|dir| dir.join(RECORD_FILE))
        .map_err(|e| format!("could not resolve the installer's data dir: {e:#}"))
}

/// Reads the record at `path`. A record that cannot be read is
/// [`RecordLoad::Corrupt`], never an empty record.
///
/// Test: `an_unparseable_record_is_undetermined`,
/// `a_record_from_another_schema_is_undetermined`.
pub fn load(path: &Path) -> RecordLoad {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return RecordLoad::Absent,
        Err(e) => return RecordLoad::Corrupt(format!("{}: {e}", path.display())),
    };
    match serde_json::from_str::<DeploymentRecord>(&text) {
        Ok(record) if record.schema == SCHEMA => RecordLoad::Loaded(record),
        Ok(record) => RecordLoad::Corrupt(format!(
            "{} has schema {}, this tctl reads {SCHEMA}",
            path.display(),
            record.schema
        )),
        Err(e) => RecordLoad::Corrupt(format!("{}: {e}", path.display())),
    }
}

/// The binaries one placement put on disk.
fn capture(placement: &Placement) -> Vec<RecordedBinary> {
    let placed = placement
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    let mut names = trusty_common::bin_resolve::installed_binaries(&placement.crate_name);
    if !names.contains(&placed) {
        names.push(placed.clone());
    }
    names
        .into_iter()
        .filter_map(|name| {
            let path = placement.path.with_file_name(&name);
            let gated = name == placed;
            // A sibling this release does not ship was not placed.
            (gated || path.exists()).then(|| RecordedBinary {
                sha256: digest(&path).ok(),
                version: gated.then(|| placement.version.clone()),
                name,
                path,
            })
        })
        .collect()
}

fn digest(path: &Path) -> Result<String, String> {
    Sha256Digest::of_file(path)
        .map(|d| d.as_hex().to_owned())
        .map_err(|e| e.to_string())
}

/// Merges `placements` into the record at `path` and writes it atomically.
///
/// Why: a partial install must not forget the other members, and a member that
/// failed keeps its old entry, so the audit still compares the disk with what
/// was last placed. A repeat install writes the same record; a rollback that
/// places an older build records that build.
/// What: replaces each placed member's entry. A corrupt existing record is
/// replaced; its other members then read as unrecorded.
/// Test: `a_repeat_install_writes_the_same_record`,
/// `a_rollback_through_install_reads_clean`,
/// `a_failed_member_keeps_its_old_entry`,
/// `an_unwritable_record_blocks_the_install`.
pub fn record_placements(path: &Path, placements: &[Placement]) -> Result<(), String> {
    let mut record = match load(path) {
        RecordLoad::Loaded(record) => record,
        RecordLoad::Absent | RecordLoad::Corrupt(_) => DeploymentRecord {
            schema: SCHEMA,
            members: BTreeMap::new(),
        },
    };
    for placement in placements {
        record
            .members
            .insert(placement.crate_name.clone(), capture(placement));
    }
    write(path, &record)
}

fn write(path: &Path, record: &DeploymentRecord) -> Result<(), String> {
    let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let dir = path
        .parent()
        .ok_or_else(|| failed(&"no parent directory"))?;
    std::fs::create_dir_all(dir).map_err(|e| failed(&e))?;
    let body = serde_json::to_vec_pretty(record).map_err(|e| failed(&e))?;
    let tmp = tempfile::NamedTempFile::new_in(dir).map_err(|e| failed(&e))?;
    std::fs::write(tmp.path(), body).map_err(|e| failed(&e))?;
    tmp.persist(path).map_err(|e| failed(&e))?;
    Ok(())
}

/// Re-reads one member's digests after `tctl sign` rewrote its binaries.
///
/// Why: signing changes a file's bytes at the same version, and the operator
/// asked for it, so it is not a swapped build.
/// What: updates the member's digests when it is recorded; otherwise no-op.
/// Test: `signing_refreshes_only_the_named_member`.
pub fn refresh_digests_at(path: &Path, crate_name: &str) -> Result<(), String> {
    let RecordLoad::Loaded(mut record) = load(path) else {
        return Ok(());
    };
    let Some(binaries) = record.members.get_mut(crate_name) else {
        return Ok(());
    };
    for binary in binaries.iter_mut() {
        binary.sha256 = digest(&binary.path).ok();
    }
    write(path, &record)
}

/// [`refresh_digests_at`] on the default record.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn refresh_digests(crate_name: &str) -> Result<(), String> {
    refresh_digests_at(&default_record_path()?, crate_name)
}

/// Compares the record with the disk, the daemons and the content.
///
/// Why: the check behind every health surface of #10607.
/// What: no usable record, or one with no members, is a [`DriftKind::NoRecord`]
/// finding. A member of `expected` missing from the record is
/// [`DriftKind::Unrecorded`]. Each recorded binary is checked for presence,
/// digest and `--version`, each member's daemon for its running version, and
/// the content verbs of `tm` and each [`CONTENT_CHECKS`] member.
/// Test: `deployment_record_tests.rs`.
pub fn audit(
    load: &RecordLoad,
    expected: &[String],
    runner: &dyn CommandRunner,
    running: &dyn Fn(&str) -> RunningVersion,
) -> DeploymentAudit {
    let record_finding = |detail: String| DriftFinding {
        member: RECORD_MEMBER.to_owned(),
        kind: DriftKind::NoRecord,
        detail,
    };
    let record = match load {
        RecordLoad::Loaded(record) if !record.members.is_empty() => record,
        RecordLoad::Loaded(_) | RecordLoad::Absent => {
            return DeploymentAudit::from_findings(vec![record_finding(format!(
                "no install record, so what is installed cannot be compared with what was \
                 placed; {REMEDY}"
            ))]);
        }
        RecordLoad::Corrupt(why) => {
            return DeploymentAudit::from_findings(vec![record_finding(format!(
                "install record unusable ({why}); {REMEDY}"
            ))]);
        }
    };
    let mut findings = Vec::new();
    for name in expected {
        if !record.members.contains_key(name) {
            findings.push(DriftFinding {
                member: name.clone(),
                kind: DriftKind::Unrecorded,
                detail: format!(
                    "installed, but no install placed it; {}",
                    remedy(DriftKind::Unrecorded, name)
                ),
            });
        }
    }
    for (member, binaries) in &record.members {
        audit_member(member, binaries, runner, running, &mut findings);
    }
    DeploymentAudit::from_findings(findings)
}

fn audit_member(
    member: &str,
    binaries: &[RecordedBinary],
    runner: &dyn CommandRunner,
    running: &dyn Fn(&str) -> RunningVersion,
    findings: &mut Vec<DriftFinding>,
) {
    let mut find = |kind: DriftKind, detail: String| {
        findings.push(DriftFinding {
            member: member.to_owned(),
            kind,
            detail: format!("{detail}; {}", remedy(kind, member)),
        });
    };
    let mut present = true;
    for binary in binaries {
        if !binary.path.exists() {
            present = false;
            find(
                DriftKind::MissingBinary,
                format!("{} is missing", binary.path.display()),
            );
            continue;
        }
        match (digest(&binary.path), &binary.sha256) {
            (Ok(now), Some(placed)) if now != *placed => find(
                DriftKind::BuildChanged,
                format!(
                    "{} is not the build install placed (sha256 {now}, placed {placed})",
                    binary.path.display()
                ),
            ),
            (Ok(_), Some(_)) => {}
            (Err(e), _) => find(
                DriftKind::Unverified,
                format!("{}: digest unreadable: {e}", binary.path.display()),
            ),
            (Ok(_), None) => find(
                DriftKind::Unverified,
                format!("{}: no digest was recorded", binary.path.display()),
            ),
        }
        let Some(version) = &binary.version else {
            continue;
        };
        match binary_version(runner, &binary.path) {
            Ok(now) if now != *version => find(
                DriftKind::WrongVersion,
                format!(
                    "{} reports {now}, install placed {version}",
                    binary.path.display()
                ),
            ),
            Ok(_) => {}
            Err(e) => find(
                DriftKind::Unverified,
                format!("{}: version unreadable: {e}", binary.path.display()),
            ),
        }
    }
    let placed = binaries
        .iter()
        .find_map(|b| b.version.as_deref())
        .unwrap_or_default();
    match running(member) {
        RunningVersion::NotApplicable | RunningVersion::NotServing => {}
        RunningVersion::Reported(now) if now.trim_start_matches('v') != placed => find(
            DriftKind::RunningVersion,
            format!("the running daemon reports {now}, install placed {placed}"),
        ),
        RunningVersion::Reported(_) => {}
        RunningVersion::Unreported => find(
            DriftKind::RunningUnreported,
            format!("the running daemon names no version, install placed {placed}"),
        ),
        RunningVersion::BelowFloor(status) => find(
            DriftKind::RunningBelowFloor,
            format!("the running daemon reports status `{status}`"),
        ),
        RunningVersion::Unprobed(outcome) => find(
            DriftKind::RunningUnprobed,
            format!("the daemon's probe could not read its version ({outcome})"),
        ),
    }
    if present {
        audit_content(member, binaries, runner, &mut find);
    }
}

/// Audits the machine for a health rollup that already probed the daemons.
///
/// What: `observed` maps each installed daemon member's crate name to what its
/// probe said; every one of them must be recorded. A record path that cannot be
/// resolved is a [`DriftKind::NoRecord`] finding.
/// Test: `tests/deployment_drift_e2e.rs`.
pub fn audit_for_rollup(observed: &HashMap<String, RunningVersion>) -> DeploymentAudit {
    let load_result = match default_record_path() {
        Ok(path) => load(&path),
        Err(why) => RecordLoad::Corrupt(why),
    };
    let expected: Vec<String> = observed.keys().cloned().collect();
    let running = |name: &str| {
        observed
            .get(name)
            .cloned()
            .unwrap_or(RunningVersion::NotApplicable)
    };
    audit(&load_result, &expected, &ProcessRunner::probe(), &running)
}

/// The record-and-audit step's shape, so install and upgrade take it as a seam.
pub type Finisher<'a> = dyn Fn(&[Placement], bool) -> Option<DeploymentAudit> + 'a;

/// Records what a run placed, then audits unless `--no-verify` skipped that.
///
/// Why: an install must not exit 0 over a deployment it just left mixed, and the
/// record is what every later health check reads, so failing to write it fails
/// the run.
/// What: see [`finish_at`].
/// Test: `finish_at_records_then_audits`, `an_unwritable_record_blocks_the_install`,
/// `deployment_probe_tests::finish_records_under_the_data_dir_10607`.
pub fn finish(placements: &[Placement], verify: bool) -> Option<DeploymentAudit> {
    let path = default_record_path();
    finish_at(
        path,
        placements,
        verify,
        &ProcessRunner::probe(),
        &probe_running,
    )
}

/// [`finish`] with its dependencies named.
///
/// What: writes the record when anything was placed; a write failure returns a
/// [`DriftKind::RecordWriteFailed`] finding. `None` when `verify` is false, or
/// nothing was placed and no record exists to audit.
pub fn finish_at(
    path: Result<PathBuf, String>,
    placements: &[Placement],
    verify: bool,
    runner: &dyn CommandRunner,
    running: &dyn Fn(&str) -> RunningVersion,
) -> Option<DeploymentAudit> {
    let write_failed = |why: String| {
        Some(DeploymentAudit::from_findings(vec![DriftFinding {
            member: RECORD_MEMBER.to_owned(),
            kind: DriftKind::RecordWriteFailed,
            detail: format!("could not record what was placed: {why}"),
        }]))
    };
    let path = match path {
        Ok(path) => path,
        Err(why) if placements.is_empty() => {
            tracing::warn!(%why, "deployment record not audited");
            return None;
        }
        Err(why) => return write_failed(why),
    };
    if !placements.is_empty() {
        if let Err(why) = record_placements(&path, placements) {
            return write_failed(why);
        }
    }
    let loaded = load(&path);
    if !verify || (placements.is_empty() && loaded == RecordLoad::Absent) {
        return None;
    }
    Some(audit(&loaded, &[], runner, running))
}

/// Records the one member `tctl self-update` placed, returning the error for the
/// caller to report.
pub fn record_one(placement: &Placement) -> Result<(), String> {
    record_placements(&default_record_path()?, std::slice::from_ref(placement))
}

/// Prints the audit for a human.
pub fn print_human(audit: &DeploymentAudit) {
    println!("deployment: {}", audit.state);
    for finding in &audit.findings {
        println!("  {:<18} {}", finding.member, finding.detail);
    }
}

#[cfg(test)]
#[path = "deployment_record_tests.rs"]
mod tests;
