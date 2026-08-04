//! The quarantine's RECEIPT contract — what moved, what did not, and why
//! (issue #4448).
//!
//! Why: a sweep that renames an operator's files and reports only a count is
//! unauditable, and #4526 shipped a receipt that was accurate only when the
//! whole run succeeded. A run that half-fails is precisely when the operator
//! needs the record, so the contract here is stated as an invariant rather
//! than a convention:
//!
//! **EVERY candidate the sweep examined appears in exactly one of
//! [`QuarantineReport`]'s three lists, whether or not the run completed.**
//!
//! [`QuarantineReport`] is populated incrementally as the sweep walks the
//! directory and is RETURNED on every exit path, so an I/O failure on file 3
//! of 5 still yields two `moved`, one `failed`, and the remaining candidates
//! classified — never a truncated list and never an `Err` that discards what
//! already happened on disk. The rendered receipt in
//! [`render_receipt`] is a projection of that same struct, so the file on disk
//! and the value the caller logs cannot disagree.
//!
//! NOTHING HERE OR IN THE SWEEP DELETES. There is no removal path, no
//! `remove_file`, no `remove_dir_all`, and a failed move leaves both the
//! original and its backup in place. A quarantine that deletes is a worse
//! defect than the shadowing it fixes (2026-07-21 incident).
//!
//! Test: `quarantine_tests.rs`.

use std::path::{Path, PathBuf};

/// Why a candidate was left exactly where it was.
///
/// Why: "skipped" alone sends an operator nowhere. Each variant names a
/// DIFFERENT owner of the file, and the operator's next step differs by owner
/// — a claude-mpm file is another live project's business, a git-tracked one
/// is their own repo's, and an unknown VCS state is a machine problem.
/// What: one variant per gate in the movability conjunction, in the order the
/// gates are evaluated.
/// Test: `movable_only_when_all_four_gates_agree` covers every variant, plus
/// one `quarantine_never_moves_*` test per gate in `quarantine_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The file does not resolve to a name tm currently ships, so it shadows
    /// nothing — or the ledger records it as a stranded framework file, which
    /// belongs to retraction, not to this sweep.
    NotShadowingBundled,
    /// The ownership ledger records the OPERATOR as the owner. Positive proof
    /// the file is not tm's; outranks any name collision.
    UserOwned,
    /// The project's VCS tracks this file — the repository is claiming it.
    GitTracked,
    /// The VCS could not be consulted, so a claim cannot be ruled out.
    VcsUnknown,
    /// The frontmatter matches claude-mpm's schema. Another live project's
    /// file that merely shares the name.
    ClaudeMpmSchema,
    /// The frontmatter matches no known deploy shape — hand-authored, a source
    /// file, or unparseable.
    UnrecognizedSchema,
}

impl SkipReason {
    /// One line an operator can act on.
    pub fn explain(self) -> &'static str {
        match self {
            Self::NotShadowingBundled => "does not shadow a bundled agent name",
            Self::UserOwned => "the ownership ledger records this as the operator's",
            Self::GitTracked => "git tracks this file — the project is claiming it",
            Self::VcsUnknown => "git could not be consulted; a claim cannot be ruled out",
            Self::ClaudeMpmSchema => "claude-mpm's frontmatter schema — another project's file",
            Self::UnrecognizedSchema => "frontmatter matches no known deploy shape",
        }
    }
}

/// Which step of the move failed.
///
/// Why: the recovery differs per stage, and so does what is on disk
/// afterwards. A [`Backup`](Self::Backup) failure means nothing moved at all;
/// a [`Rename`](Self::Rename) failure means a verified backup exists and the
/// original is still in place; a [`Verify`](Self::Verify) failure means the
/// filesystem did not report what the syscall claimed, which is the one case
/// an operator must look at by hand.
/// Test: `a_failed_backup_leaves_the_original_in_place`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailStage {
    /// Copying the file into the backup root failed. Nothing was moved.
    Backup,
    /// The backup copy did not read back byte-identical. Nothing was moved.
    Verify,
    /// The rename to `.md.disabled` failed. The backup survives; the original
    /// is untouched.
    Rename,
}

impl FailStage {
    /// What is on disk after a failure at this stage.
    pub fn explain(self) -> &'static str {
        match self {
            Self::Backup => "backup copy failed; the original was NOT moved",
            Self::Verify => {
                "the backup did not read back byte-identical; the original was NOT moved"
            }
            Self::Rename => {
                "the rename failed; the verified backup survives and the original is untouched"
            }
        }
    }
}

/// One file the sweep moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedAgent {
    /// The name this file resolves under (frontmatter `name:`, else the stem).
    pub name: String,
    /// Where it was.
    pub from: PathBuf,
    /// Where it is now — the inert `.md.disabled` sibling.
    pub to: PathBuf,
    /// The verified byte-identical copy taken before the rename.
    pub backup: PathBuf,
}

/// One file the sweep deliberately left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedAgent {
    /// The name this file resolves under.
    pub name: String,
    /// Where it is — unchanged.
    pub path: PathBuf,
    /// Which gate refused it.
    pub reason: SkipReason,
}

/// One file the sweep tried to move and could not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedAgent {
    /// The name this file resolves under.
    pub name: String,
    /// Where it is.
    pub path: PathBuf,
    /// Which step failed.
    pub stage: FailStage,
    /// The underlying error, verbatim.
    pub detail: String,
}

/// The complete record of one sweep.
///
/// Why: see the module doc — this is the partial-failure contract, and it is
/// the value the receipt file is rendered FROM, so the two cannot drift.
/// What: three disjoint lists covering every candidate examined, plus where
/// the receipt was written (`None` when nothing was moved and nothing failed,
/// so a clean project gains no files) and why it could not be written, if it
/// could not. A receipt-write failure never invalidates the moves that already
/// happened, so it is reported alongside them rather than as an `Err`.
/// Test: `a_partial_failure_still_reports_every_candidate`,
/// `a_clean_tier_writes_no_receipt`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QuarantineReport {
    /// Files renamed out of the way, each with a verified backup.
    pub moved: Vec<QuarantinedAgent>,
    /// Files left untouched, each with the gate that refused them.
    pub skipped: Vec<SkippedAgent>,
    /// Files the sweep tried to move and could not.
    pub failed: Vec<FailedAgent>,
    /// Where the receipt was written.
    pub receipt: Option<PathBuf>,
    /// Why the receipt could not be written, if it could not.
    pub receipt_error: Option<String>,
}

impl QuarantineReport {
    /// Whether this run touched the filesystem at all.
    ///
    /// Why: the receipt is written only when there is something to account for,
    /// so a launch into a clean project creates no files.
    pub fn wrote_anything(&self) -> bool {
        !self.moved.is_empty() || !self.failed.is_empty()
    }

    /// Every candidate the sweep examined, moved or not.
    pub fn examined(&self) -> usize {
        self.moved.len() + self.skipped.len() + self.failed.len()
    }
}

/// POSIX-quote a path for pasting into a shell.
///
/// Why: the receipt prints a `mv` an operator is invited to run, and the
/// filename is NOT under tm's control — identity comes from the frontmatter
/// `name:`, so a cloned repository can put anything in the filename. Rust's
/// `{:?}` emits DOUBLE quotes, inside which `$` still expands and backticks
/// still command-substitute; #4526 shipped that and a filename carrying
/// `` `id` `` executed it on paste.
/// What: wraps the path in single quotes, where nothing expands, and escapes
/// each embedded `'` as `'\''` (close, escaped literal quote, reopen) — the
/// standard POSIX idiom, safe for every byte a filename can hold.
/// Test: `restore_command_survives_a_hostile_filename` executes the emitted
/// command through `sh` and asserts the file returns and no payload ran.
pub fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Render a [`QuarantineReport`] as the receipt written next to the backups.
///
/// Why: a projection of the report, never an independent walk of the
/// filesystem — that is what keeps the file on disk and the value the caller
/// logs from disagreeing, including when the run failed part way.
/// What: a Markdown document naming the tier, the backup run directory, and
/// every examined file under `Moved` / `Failed` / `Skipped`, with a
/// [`shell_quote`]d `mv` for each moved file. Sections with no entries are
/// still emitted with a zero count, so a reader can tell "none" from
/// "truncated".
/// Test: `a_partial_failure_still_reports_every_candidate`,
/// `a_clean_tier_writes_no_receipt`,
/// `restore_command_survives_a_hostile_filename`.
pub fn render_receipt(report: &QuarantineReport, tier_dir: &Path, run_id: &str) -> String {
    let mut out = format!(
        "# trusty-mpm agent quarantine — {run_id}\n\n\
         Tier: {}\n\
         Examined: {}\n\n\
         Nothing here was deleted. Every moved file has a byte-identical backup\n\
         and an inert `.md.disabled` sibling; either one restores it.\n\n",
        tier_dir.display(),
        report.examined()
    );

    out.push_str(&format!("## Moved ({})\n\n", report.moved.len()));
    for m in &report.moved {
        out.push_str(&format!(
            "- `{}` — shadowed a bundled agent name\n  - was: {}\n  - now: {}\n  - backup: {}\n  - restore: mv {} {}\n",
            m.name,
            m.from.display(),
            m.to.display(),
            m.backup.display(),
            shell_quote(&m.to),
            shell_quote(&m.from),
        ));
    }

    out.push_str(&format!("\n## Failed ({})\n\n", report.failed.len()));
    for f in &report.failed {
        out.push_str(&format!(
            "- `{}` — {}\n  - path: {}\n  - {}\n  - detail: {}\n",
            f.name,
            match f.stage {
                FailStage::Backup => "backup",
                FailStage::Verify => "verify",
                FailStage::Rename => "rename",
            },
            f.path.display(),
            f.stage.explain(),
            f.detail,
        ));
    }

    out.push_str(&format!("\n## Skipped ({})\n\n", report.skipped.len()));
    for s in &report.skipped {
        out.push_str(&format!(
            "- `{}` — {}\n  - path: {}\n",
            s.name,
            s.reason.explain(),
            s.path.display(),
        ));
    }

    out
}
