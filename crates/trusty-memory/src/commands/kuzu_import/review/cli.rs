//! `trusty-memory import kuzu review record|status|finish` (#8624, PR-C).
//!
//! Why: an agent records one verdict per row of a `--export-refusals` run,
//! checks what is left, and closes the run; the import then applies the
//! sealed ledger. v1 has no human confirmation step (Architect ruling).
//! What: each command names a run under the fixed review root; `run_dir`
//! checks the name, the guard and the directory's owner-only mode. `record`
//! validates a verdict against the row's exported shapes and appends it;
//! `status` counts reviewed and pending rows; `finish` seals the ledger.
//! Output carries counts, verdict labels and key prefixes, never row text.
//! Test: `review::ledger_tests`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::engine::filter::SECRET_FILTER_VERSION;
use serde::Deserialize;

use super::ReviewError;
use super::carve_out::row_admissible;
use super::export::{DIR_MODE, EXPORTABLE, MASKED_FILE, RECORD_SCHEMA};
use super::ledger::{
    LEDGER_FILE, LedgerError, Lookup, ReviewLedger, append_entry, is_sealed, parse_ledger,
    read_owner_only, seal,
};
use super::ops::check_ops;
use super::paths::{default_root, guard_destination};
use super::verdict::{LedgerEntry, RedactOp, Verdict};

/// `trusty-memory import kuzu <command>`.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum KuzuCommand {
    /// Review a `--export-refusals` run: record verdicts, check, seal.
    #[command(subcommand)]
    Review(ReviewCommand),
}

/// `trusty-memory import kuzu review <command>`.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum ReviewCommand {
    /// Record an agent verdict for one exported row.
    Record(RecordArgs),
    /// Count a run's reviewed and pending rows.
    Status(RunArgs),
    /// Seal a run's ledger; no verdict can be recorded after this.
    Finish(RunArgs),
}

/// Names one review run.
#[derive(Debug, Clone, clap::Args)]
pub struct RunArgs {
    /// The run directory's name under ~/.trusty-tools/kuzu-review.
    #[arg(long, value_name = "RUN")]
    pub run: String,
}

/// `review record`.
#[derive(Debug, Clone, clap::Args)]
pub struct RecordArgs {
    /// The run directory's name under ~/.trusty-tools/kuzu-review.
    #[arg(long, value_name = "RUN")]
    pub run: String,
    /// The row's `key` from masked.jsonl.
    #[arg(long, value_name = "KEY")]
    pub key: String,
    /// The verdict.
    #[arg(long, value_enum)]
    pub verdict: VerdictKind,
    /// With admit-redacted: `redact_span:<i>`, `delete_span:<i>` or
    /// `drop_line:<i>` (repeatable), `<i>` being the marker's `S<i>`.
    #[arg(long = "op", value_name = "OP:SPAN", value_parser = parse_op)]
    pub ops: Vec<RedactOp>,
}

/// The verdict `record` stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum VerdictKind {
    /// Verbatim, under the carve-out (tagged kuzu-review:override).
    Admit,
    /// After the --op redactions, screened again at import.
    AdmitRedacted,
    /// Leave the row refused.
    KeepRefused,
}

fn parse_op(s: &str) -> Result<RedactOp, String> {
    s.parse()
}

/// One exported row, read back for validation only.
#[derive(Debug, Deserialize)]
struct ExportedRow {
    schema: u32,
    filter_version: u32,
    key: String,
    spans: Vec<ExportedSpan>,
}

#[derive(Debug, Deserialize)]
struct ExportedSpan {
    rule: String,
    len: usize,
    ent_tenths: u16,
}

/// Counts [`status`] reports.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct StatusSummary {
    pub exported: usize,
    pub admit: usize,
    pub admit_redacted: usize,
    pub keep_refused: usize,
    pub pending: usize,
    /// Rows whose ledger line is a duplicate, stale or fails its digest.
    pub invalid: usize,
    /// Ledger keys naming no exported row.
    pub unknown: usize,
    pub sealed: bool,
}

/// Entry point for `import kuzu review`.
pub(crate) fn handle_review(cmd: &ReviewCommand) -> anyhow::Result<()> {
    let root = default_root()?;
    let data_dir = trusty_common::resolve_data_dir("trusty-memory")?;
    let protected = [crate::resolve_palace_registry_dir(data_dir)];
    run_review(cmd, &root, &protected, &mut std::io::stdout())?;
    Ok(())
}

/// Run `cmd` against runs under `root`, printing to `out`.
pub(crate) fn run_review(
    cmd: &ReviewCommand,
    root: &Path,
    protected: &[PathBuf],
    out: &mut dyn Write,
) -> Result<(), LedgerError> {
    // A closed stdout must not turn a recorded verdict into a failure.
    match cmd {
        ReviewCommand::Record(a) => {
            let line = record(&run_dir(root, &a.run, protected)?, a)?;
            let _ = writeln!(out, "{line}");
        }
        ReviewCommand::Status(a) => {
            let s = status(&run_dir(root, &a.run, protected)?)?;
            let _ = writeln!(
                out,
                "· review run {}: {} exported, {} reviewed (admit {}, admit-redacted {}, \
                 keep-refused {}), {} pending, {} invalid, {} unknown key(s); sealed: {}",
                a.run,
                s.exported,
                s.admit + s.admit_redacted + s.keep_refused,
                s.admit,
                s.admit_redacted,
                s.keep_refused,
                s.pending,
                s.invalid,
                s.unknown,
                if s.sealed { "yes" } else { "no" }
            );
        }
        ReviewCommand::Finish(a) => {
            let dir = run_dir(root, &a.run, protected)?;
            let pending = status(&dir)?.pending;
            let ledger = seal(&dir)?;
            let _ = writeln!(
                out,
                "· sealed review run {}: {} verdict(s); {pending} pending row(s) stay refused",
                a.run,
                ledger.lines()
            );
        }
    }
    Ok(())
}

/// The resolved run directory `root/run`, checked.
///
/// What: `run` must be 1-64 ASCII alphanumerics or `-` (what `run_name`
/// makes); the directory must pass the export's guard, exist as a real
/// directory, and have mode 0700.
/// Test: `run_names_and_modes_are_checked`.
pub(crate) fn run_dir(
    root: &Path,
    run: &str,
    protected: &[PathBuf],
) -> Result<PathBuf, LedgerError> {
    let plain = !run.is_empty()
        && run.len() <= 64
        && run.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !plain {
        return Err(LedgerError::BadRun(run.to_string()));
    }
    let dir = guard_destination(&root.join(run), protected)?;
    let meta = dir.symlink_metadata().map_err(|source| ReviewError::Io {
        step: "open the run directory",
        path: dir.clone(),
        source,
    })?;
    #[cfg(unix)]
    let got = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777;
    #[cfg(not(unix))]
    let got = 0;
    if !meta.is_dir() || got != DIR_MODE {
        return Err(ReviewError::Mode {
            path: dir,
            want: DIR_MODE,
            got,
        }
        .into());
    }
    Ok(dir)
}

/// The run's exported rows.
fn read_export(dir: &Path) -> Result<Vec<ExportedRow>, LedgerError> {
    let path = dir.join(MASKED_FILE);
    let bytes = read_owner_only(&path)?
        .ok_or_else(|| LedgerError::Rejected(format!("{} has no {MASKED_FILE}", dir.display())))?;
    let text = String::from_utf8(bytes).map_err(|_| LedgerError::Corrupt {
        path: path.clone(),
        line: 0,
        reason: "not UTF-8".to_string(),
    })?;
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let row: ExportedRow = serde_json::from_str(line).map_err(|e| LedgerError::Corrupt {
            path: path.clone(),
            line: i + 1,
            reason: e.to_string(),
        })?;
        if row.schema != RECORD_SCHEMA {
            return Err(LedgerError::Rejected(format!(
                "{} line {} has export schema {}",
                path.display(),
                i + 1,
                row.schema
            )));
        }
        rows.push(row);
    }
    Ok(rows)
}

/// The run's ledger as it stands; an absent ledger is empty.
fn read_ledger(dir: &Path) -> Result<ReviewLedger, LedgerError> {
    let path = dir.join(LEDGER_FILE);
    let bytes = read_owner_only(&path)?.unwrap_or_default();
    parse_ledger(&path, &bytes)
}

/// Validate and append one verdict; returns the line to print.
///
/// Why (#8624 rules 4, 5): a verdict the import would refuse is refused here
/// first, so the agent learns it now; the import still re-checks everything.
/// What: refuses a sealed run, a key no exported row carries (auto-refused
/// rows are never exported), a row exported under another filter version, an
/// admit of any kind for a row holding a span outside [`EXPORTABLE`], an
/// `admit` that fails [`row_admissible`], ops on anything but
/// `admit-redacted`, a bad op list, an unreadable ledger, and a key already
/// in the ledger.
/// Test: `record_status_finish_flow`, `record_refuses_bad_ops`,
/// `record_rejects_admit_for_an_auto_refuse_rule`,
/// `record_rejects_admit_above_the_carve_out_threshold`.
pub(crate) fn record(dir: &Path, a: &RecordArgs) -> Result<String, LedgerError> {
    let reject = |m: &str| Err(LedgerError::Rejected(m.to_string()));
    if is_sealed(dir) {
        return Err(LedgerError::Sealed {
            path: dir.to_path_buf(),
        });
    }
    let rows = read_export(dir)?;
    let Some(row) = rows.iter().find(|r| r.key == a.key) else {
        return reject("no exported row has this key; auto-refused rows are never exported");
    };
    if row.filter_version != SECRET_FILTER_VERSION {
        return reject("the row was exported under another secret filter version; export again");
    }
    let auto = row
        .spans
        .iter()
        .find(|s| !EXPORTABLE.iter().any(|r| r.label() == s.rule));
    let verdict = match a.verdict {
        _ if !a.ops.is_empty() && a.verdict != VerdictKind::AdmitRedacted => {
            return reject("--op is only for admit-redacted");
        }
        VerdictKind::KeepRefused => Verdict::KeepRefused {},
        _ if auto.is_some() => {
            return reject("the row holds an auto-refuse span; it can never be admitted");
        }
        VerdictKind::Admit => {
            let shapes = row
                .spans
                .iter()
                .map(|s| (s.rule.as_str(), s.len, s.ent_tenths));
            if !row_admissible(shapes) {
                return reject(
                    "admit is the verbatim carve-out (mixed_case_alnum or base64_blob, not \
                     high-entropy); use admit-redacted",
                );
            }
            Verdict::Admit {}
        }
        VerdictKind::AdmitRedacted => {
            check_ops(&a.ops, row.spans.len()).map_err(|e| LedgerError::Rejected(e.to_string()))?;
            Verdict::AdmitRedacted { ops: a.ops.clone() }
        }
    };
    if read_ledger(dir)?.lookup(&a.key) != Lookup::Missing {
        return reject("this row already has a verdict");
    }
    let entry = LedgerEntry::new(a.key.clone(), SECRET_FILTER_VERSION, verdict);
    append_entry(&dir.join(LEDGER_FILE), &entry)?;
    Ok(format!(
        "· recorded {} for row {}",
        entry.verdict.label(),
        a.key.get(..12).unwrap_or(&a.key)
    ))
}

/// Count `dir`'s exported rows by their ledger state.
///
/// Test: `record_status_finish_flow`.
pub(crate) fn status(dir: &Path) -> Result<StatusSummary, LedgerError> {
    let rows = read_export(dir)?;
    let ledger = read_ledger(dir)?;
    let mut s = StatusSummary {
        exported: rows.len(),
        sealed: is_sealed(dir),
        ..StatusSummary::default()
    };
    for row in &rows {
        match ledger.lookup(&row.key) {
            Lookup::Missing => s.pending += 1,
            Lookup::Duplicate => s.invalid += 1,
            Lookup::Entry(e)
                if !e.digest_matches() || e.filter_version != SECRET_FILTER_VERSION =>
            {
                s.invalid += 1
            }
            Lookup::Entry(e) => match e.verdict {
                Verdict::Admit {} => s.admit += 1,
                Verdict::AdmitRedacted { .. } => s.admit_redacted += 1,
                Verdict::KeepRefused {} => s.keep_refused += 1,
            },
        }
    }
    s.unknown = ledger
        .keys()
        .filter(|k| !rows.iter().any(|r| r.key == *k))
        .count();
    Ok(s)
}
