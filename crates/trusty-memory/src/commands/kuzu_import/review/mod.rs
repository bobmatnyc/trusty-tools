//! `import kuzu --dry-run --export-refusals`: a masked export of refused rows
//! for review (#8624, PR-B).
//!
//! Why: the one kuzu import window (#277) waits until a reviewer can judge the
//! rows the secret screen refuses. The reviewer may see a refused row's
//! surrounding text and each secret's shape, never the secret itself.
//! What: [`prepare`] guards this run's directory before any store is read;
//! [`ReviewBatch::add_store`] classifies each memory row ([`export`]);
//! [`ReviewBatch::finish`] writes `<root>/<run>/masked.jsonl`, owner-only from
//! creation, and prints the counts. Rows of an auto-refuse rule are counted,
//! never written. The root is fixed (ruling Q4): [`paths::default_root`].
//!
//! PR-C adds the review itself: `import kuzu review record|status|finish`
//! ([`cli`]) writes and seals a ledger of agent verdicts ([`ledger`],
//! [`verdict`]), and `import kuzu --review-ledger` applies it ([`admit`]),
//! with closed redaction [`ops`] and the verbatim [`carve_out`].
//! Test: `review::tests`, `review::export_tests`, `review::paths_tests`,
//! `review::ledger_tests`, `review::admit_tests`.

pub(crate) mod admit;
pub(crate) mod carve_out;
pub mod cli;
pub(crate) mod export;
pub(crate) mod ledger;
pub(crate) mod ops;
pub(crate) mod paths;
pub mod verdict;

#[cfg(test)]
mod admit_tests;
#[cfg(test)]
mod export_tests;
#[cfg(test)]
mod ledger_tests;
#[cfg(test)]
mod paths_tests;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) use admit::apply_verdicts;
pub use cli::{KuzuCommand, ReviewCommand};
pub(crate) use export::OsFs;
use export::{classify_row, write_run, MaskedRecord, ReviewFs, RowVerdict};
pub(crate) use ledger::{load_sealed, ReviewLedger};
pub(crate) use paths::default_root;
use paths::{guard_destination, run_name};

use super::bridge::KuzuExport;
use super::KuzuImportError;

/// Why a review export did not happen. Carries paths and OS errors only,
/// never row content.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ReviewError {
    #[error("--export-refusals needs --dry-run")]
    NeedsDryRun,
    #[error("no home directory to hold the review export")]
    NoHome,
    #[error("refusing review export destination {}: {reason}", path.display())]
    Guard { path: PathBuf, reason: String },
    #[error("review export: could not {step} {}: {source}", path.display())]
    Io {
        step: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("review export: {} has mode {got:o}, not {want:o}; refusing a looser mode", path.display())]
    Mode { path: PathBuf, want: u32, got: u32 },
}

impl From<ReviewError> for KuzuImportError {
    fn from(e: ReviewError) -> Self {
        KuzuImportError::Io(e.to_string())
    }
}

impl From<ledger::LedgerError> for KuzuImportError {
    fn from(e: ledger::LedgerError) -> Self {
        KuzuImportError::Io(e.to_string())
    }
}

/// The review batch for one `import kuzu` run, or `None` without the flag.
///
/// Why (#8624): the destination guard runs before any store is read, so a
/// refused destination costs no bridge run.
/// What: `None` when `export_refusals` is off. Otherwise refuses a run that is
/// not a dry run, or that has no root, then names the run directory under
/// `root` and guards it against a git work tree and `data_root`.
/// Test: `export_refusals_needs_dry_run_even_without_clap`,
/// `guard_error_refuses_the_export_before_any_store_is_read`.
pub(crate) fn prepare(
    export_refusals: bool,
    dry_run: bool,
    root: Option<&Path>,
    data_root: &Path,
) -> Result<Option<ReviewBatch>, ReviewError> {
    if !export_refusals {
        return Ok(None);
    }
    if !dry_run {
        return Err(ReviewError::NeedsDryRun);
    }
    let root = root.ok_or(ReviewError::NoHome)?;
    let batch = ReviewBatch {
        run_dir: root.join(run_name()),
        protected: vec![data_root.to_path_buf()],
        records: Vec::new(),
        auto_refused: BTreeMap::new(),
    };
    guard_destination(&batch.run_dir, &batch.protected)?;
    Ok(Some(batch))
}

/// Refused rows collected across one run's stores.
#[derive(Debug)]
pub(crate) struct ReviewBatch {
    run_dir: PathBuf,
    /// Roots the export must never land in; a git work tree is always refused.
    protected: Vec<PathBuf>,
    records: Vec<MaskedRecord>,
    /// Rows never exported, by rule label or refusal reason.
    auto_refused: BTreeMap<&'static str, usize>,
}

/// What [`ReviewBatch::finish`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewSummary {
    pub exported: usize,
    pub auto_refused: BTreeMap<&'static str, usize>,
    pub run_dir: PathBuf,
}

impl ReviewBatch {
    /// Classify every memory row of one store's export.
    ///
    /// Test: `exportable_rows_are_masked_and_auto_refuse_rows_are_not`.
    pub(crate) fn add_store(&mut self, export: &KuzuExport, store: &str) {
        for row in &export.memories {
            match classify_row(row, store) {
                RowVerdict::Clean => {}
                RowVerdict::Export(record) => self.records.push(record),
                RowVerdict::AutoRefuse(label) => {
                    *self.auto_refused.entry(label).or_default() += 1;
                }
            }
        }
    }

    /// Write the run directory and print the counts to `out`.
    ///
    /// Why (#8624): the export aborts rather than leave a looser or partial
    /// file, and the guard runs again on the created directory, so a symlink
    /// swapped in after [`prepare`] is still refused.
    /// What: re-guards the run directory, then [`write_run`] through `fs`.
    /// Prints exported and auto-refused counts plus the run directory; never
    /// a row, an id or a span.
    /// Test: `umask_000_end_to_end_export_is_owner_only_and_leaks_nothing`,
    /// `each_failed_step_aborts_with_nothing_left_behind`.
    pub(crate) fn finish(
        self,
        fs: &dyn ReviewFs,
        out: &mut dyn Write,
    ) -> Result<ReviewSummary, ReviewError> {
        let run_dir = guard_destination(&self.run_dir, &self.protected)?;
        let recheck = |p: &Path| guard_destination(p, &self.protected).map(drop);
        write_run(fs, &run_dir, &self.records, &recheck)?;
        let summary = ReviewSummary {
            exported: self.records.len(),
            auto_refused: self.auto_refused,
            run_dir,
        };
        // The export is on disk; a closed stdout must not report it failed.
        let _ = writeln!(out, "{}", summary_line(&summary));
        Ok(summary)
    }
}

/// The summary [`ReviewBatch::finish`] prints: counts, labels and the path.
pub(crate) fn summary_line(s: &ReviewSummary) -> String {
    let total: usize = s.auto_refused.values().sum();
    let by_rule: Vec<String> = s
        .auto_refused
        .iter()
        .map(|(label, n)| format!("{label} {n}"))
        .collect();
    let detail = if by_rule.is_empty() {
        String::new()
    } else {
        format!(" ({})", by_rule.join(", "))
    };
    format!(
        "· review export: {} row(s) exported masked, {total} auto-refused{detail}\n  run directory: {}",
        s.exported,
        s.run_dir.display()
    )
}
