//! `tm doctor`'s `hook-interpreter` and `notify-supervisor-asset` rows, and
//! the `--fix` steps that add the interpreter (#10263).
//!
//! Why: [`crate::core::hook_interpreter`] decides one command and one file;
//! this module turns a machine-wide file list into the two report rows and the
//! [`RepairStep`]s `--fix` prints, so the report and the repair read the same
//! scan.
//! What: [`hook_interpreter_rows`], [`repair_hook_interpreter`], and
//! [`notify_asset_row`].
//! Test: `hook_interpreter_tests.rs`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};
use crate::core::hook_interpreter::{
    FileError, FileFindings, repair_settings_file, scan_settings_file,
};

/// The doctor row and `--fix` check name for bare-script hook commands.
pub const HOOK_INTERPRETER_CHECK: &str = "hook-interpreter";

/// The doctor row comparing seeded `notify-supervisor.sh` copies to the asset.
pub const NOTIFY_ASSET_CHECK: &str = "notify-supervisor-asset";

/// The `notify-supervisor.sh` this binary ships (`tm fleet init` seeds it).
const SHIPPED_NOTIFY: &str = include_str!("../assets/architect/scripts/notify-supervisor.sh");

/// How many individual problems a row names before it only counts them.
const NAMED_LIMIT: usize = 3;

/// The `hook-interpreter` row and the `notify-supervisor-asset` row.
///
/// Why (#10263): bare-script hook commands sit in settings files across the
/// machine, so the row reads the same machine-wide list `--fix` repairs. A
/// refusal, an unreadable file, or an unparseable file is never a pass: each
/// keeps the row non-OK so the operator sees what `--fix` will not touch.
/// What: scans every file in `files` read-only. `Ok` only when no bare-script
/// command, refusal, or file error was found; `Warn` for rewritable or refused
/// commands and session-less `datg-hook.sh` entries; `Unknown` when a file
/// could not be read or parsed. The second row is [`notify_asset_row`] over
/// every `notify-supervisor.sh` a command names.
/// Test: `the_row_is_ok_only_when_nothing_is_found`,
/// `a_refusal_keeps_the_row_non_ok`, `an_unparseable_file_keeps_the_row_non_ok`,
/// `an_unreadable_file_keeps_the_row_non_ok`.
pub fn hook_interpreter_rows(files: &[PathBuf]) -> Vec<DoctorCheck> {
    let mut tally = Tally::default();
    for path in files {
        match scan_settings_file(path) {
            Ok(None) => {}
            Ok(Some(findings)) => tally.add(path, findings),
            Err(e) => tally.errors.push(format!("{}: {e}", path.display())),
        }
    }
    vec![
        tally.row(),
        notify_asset_row(&tally.notify_scripts, SHIPPED_NOTIFY.as_bytes()),
    ]
}

/// The running totals behind the `hook-interpreter` row.
#[derive(Default)]
struct Tally {
    scanned: usize,
    rewrites: usize,
    rewrite_files: usize,
    refusals: Vec<String>,
    missing_session: Vec<String>,
    errors: Vec<String>,
    notify_scripts: BTreeSet<PathBuf>,
}

impl Tally {
    /// Fold one scanned file in.
    fn add(&mut self, path: &Path, findings: FileFindings) {
        self.scanned += 1;
        if !findings.rewrites.is_empty() {
            self.rewrites += findings.rewrites.len();
            self.rewrite_files += 1;
        }
        for (cmd, why) in findings.refusals {
            self.refusals
                .push(format!("{}: `{cmd}` ({why})", path.display()));
        }
        for cmd in findings.missing_session {
            self.missing_session
                .push(format!("{}: `{cmd}`", path.display()));
        }
        self.notify_scripts.extend(findings.notify_scripts);
    }

    /// The row these totals describe.
    fn row(&self) -> DoctorCheck {
        let mut parts = Vec::new();
        let mut status = CheckStatus::Ok;
        if self.rewrites > 0 {
            status = CheckStatus::Warn;
            parts.push(format!(
                "{} bare script command(s) in {} file(s) — `tm doctor --fix --yes` adds the interpreter",
                self.rewrites, self.rewrite_files
            ));
        }
        if !self.refusals.is_empty() {
            status = status.worst(CheckStatus::Warn);
            parts.push(format!(
                "{} refused, fix by hand: {}",
                self.refusals.len(),
                named(&self.refusals)
            ));
        }
        if !self.missing_session.is_empty() {
            status = status.worst(CheckStatus::Warn);
            parts.push(format!(
                "{} datg-hook.sh entr(y/ies) with no session arg, left unset: {}",
                self.missing_session.len(),
                named(&self.missing_session)
            ));
        }
        if !self.errors.is_empty() {
            status = status.worst(CheckStatus::Unknown);
            parts.push(format!(
                "{} settings file(s) not scanned: {}",
                self.errors.len(),
                named(&self.errors)
            ));
        }
        let message = if parts.is_empty() {
            format!(
                "no bare-script hook commands in {} settings file(s)",
                self.scanned
            )
        } else {
            parts.join("; ")
        };
        DoctorCheck::new(HOOK_INTERPRETER_CHECK, status, message)
    }
}

/// The first [`NAMED_LIMIT`] items, then a count of the rest.
fn named(items: &[String]) -> String {
    let mut out = items
        .iter()
        .take(NAMED_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > NAMED_LIMIT {
        out.push_str(&format!(" (+{} more)", items.len() - NAMED_LIMIT));
    }
    out
}

/// Compare each seeded `notify-supervisor.sh` with the shipped bytes.
///
/// Why (#10263): a seed that differs is either hand-edited or from an older
/// release, and the seeder's own rule is to leave an edited file alone — so
/// this row only reports, with both hashes, and nothing overwrites the copy.
/// What: `Ok` when no copy is referenced or every copy matches; `Warn` naming
/// each differing path with its sha256 and the shipped sha256; `Unknown` when
/// a copy exists and cannot be read. A missing copy is skipped here — the
/// `hook-interpreter` row already refuses it.
/// Test: `a_drifted_seed_is_reported_with_both_hashes`,
/// `a_matching_seed_is_ok`.
pub fn notify_asset_row(scripts: &BTreeSet<PathBuf>, shipped: &[u8]) -> DoctorCheck {
    let expected = sha256_hex(shipped);
    let mut drifted = Vec::new();
    let mut errors = Vec::new();
    let mut matched = 0usize;
    for path in scripts {
        match std::fs::read(path) {
            Ok(bytes) if bytes == shipped => matched += 1,
            Ok(bytes) => drifted.push(format!(
                "{}: sha256 {} (shipped {expected})",
                path.display(),
                sha256_hex(&bytes)
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => errors.push(format!("{}: cannot read: {e}", path.display())),
        }
    }
    let (status, message) = if !errors.is_empty() {
        (CheckStatus::Unknown, errors.join(", "))
    } else if !drifted.is_empty() {
        (
            CheckStatus::Warn,
            format!(
                "{} — differs from the shipped asset; review the local edits and replace it by \
                 hand (`--fix` never overwrites it)",
                drifted.join(", ")
            ),
        )
    } else if matched == 0 {
        (
            CheckStatus::Ok,
            "no seeded notify-supervisor.sh is referenced by a hook".to_string(),
        )
    } else {
        (
            CheckStatus::Ok,
            format!("{matched} seeded copy(ies) match the shipped asset (sha256 {expected})"),
        )
    };
    DoctorCheck::new(NOTIFY_ASSET_CHECK, status, message)
}

/// Lower-case hex sha256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The `--fix` steps that add the interpreter to bare table scripts.
///
/// Why (#10263): the repair the `hook-interpreter` row names. It writes only in
/// [`RepairMode::Apply`], and every shape the row refuses stays a refused step.
/// What: per file — one `Planned`/`Applied` step for its rewrites (with the
/// backup path), one `Refused` step per refused command and per session-less
/// `datg-hook.sh` entry, `Refused` for an unparseable file and `Failed` for an
/// unreadable or unwritable one. A file with nothing to say adds no step, so a
/// second run after an apply plans nothing.
/// Test: `repair_applies_backs_up_and_is_idempotent`,
/// `repair_dry_run_writes_nothing`, `repair_refuses_an_unparseable_file`.
pub fn repair_hook_interpreter(files: &[PathBuf], mode: RepairMode) -> Vec<RepairStep> {
    let mut steps = Vec::new();
    for path in files {
        let step = |what: String, status: StepStatus| RepairStep {
            check: HOOK_INTERPRETER_CHECK,
            path: path.clone(),
            what,
            status,
        };
        match repair_settings_file(path, mode == RepairMode::Apply) {
            Ok(None) => {}
            Ok(Some((findings, backup))) => {
                if let Some((old, new)) = findings.rewrites.first() {
                    let what = format!(
                        "add the interpreter to {} hook command(s) (first: `{old}` → `{new}`)",
                        findings.rewrites.len()
                    );
                    let status = match mode {
                        RepairMode::Apply => StepStatus::Applied { backup },
                        RepairMode::DryRun => StepStatus::Planned,
                    };
                    steps.push(step(what, status));
                }
                for cmd in findings.missing_session {
                    steps.push(step(
                        format!("`{cmd}` has no session argument"),
                        StepStatus::Refused(
                            "tm never invents a session; add one by hand if several PM sessions \
                             share this project"
                                .to_string(),
                        ),
                    ));
                }
                for (cmd, why) in findings.refusals {
                    steps.push(step(
                        format!("add the interpreter to `{cmd}`"),
                        StepStatus::Refused(why),
                    ));
                }
            }
            Err(e @ FileError::Unparseable(_)) => steps.push(step(
                "add the interpreter to bare-script hook commands".to_string(),
                StepStatus::Refused(e.to_string()),
            )),
            Err(e) => steps.push(step(
                "add the interpreter to bare-script hook commands".to_string(),
                StepStatus::Failed(e.to_string()),
            )),
        }
    }
    steps
}
