//! One-shot repair for a savings ledger polluted by unit-test rows (#7569).
//!
//! Why: #7514 let the instruction-compression producer resolve a *fixture*
//! path to a session id, so every test run that wrote a compiled prompt
//! appended a row to the OPERATOR's real `~/.trusty-mpm/usage/savings.jsonl`.
//! #7514's fix stops new rows arriving; it cannot retract the 228 already
//! there, and those rows claim 1,498,419 of the ledger's 1,501,558
//! instruction-compression tokens — the `💸` statusline average is computed
//! over them. Nothing else in the tree can edit this file: the ledger has
//! exactly one writer ([`crate::core::savings::append_row`]) and three
//! readers, none of which rewrites it.
//!
//! What: [`plan`] classifies every row on the ledger and [`apply`] moves the
//! matched ones to a timestamped sidecar, rewriting the ledger atomically
//! (temp file plus rename in the same directory) from the kept rows' ORIGINAL
//! bytes. Rows are never deleted and never re-serialised, so a reader folds
//! the survivors exactly as it folded them before. [`quarantine_markers`] does
//! the same for the `usage/no-fold-warned/` marker directory #7514 also filled.
//!
//! Two properties the command's safety rests on:
//!
//! - **A malformed line loses nothing.** #7579 left ten physical lines holding
//!   two complete rows with no separator. [`plan`] splits such a line and
//!   judges each half on its own; a half that does not parse is quarantined
//!   under [`Reason::Malformed`] rather than dropped.
//! - **Every write lands after every read.** [`apply`] builds both output
//!   buffers in memory before it opens anything for writing, so a parse or IO
//!   failure aborts with the original ledger untouched.
//! - **A live producer's append is not stranded.** The ledger's writer runs on
//!   a Claude hook and can append at any moment, including between [`plan`]'s
//!   read and [`apply`]'s rename. [`apply`] carries those bytes across rather
//!   than replacing the inode they landed on.
//!
//! Test: `savings_repair_tests.rs` — `the_predicate_matches_only_fixture_rows`,
//! `plan_classifies_a_mixed_ledger`, `apply_quarantines_and_rewrites`,
//! `a_second_apply_finds_nothing`, `apply_keeps_the_original_bytes`,
//! `apply_carries_a_row_appended_after_the_plan`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::savings::{SavingsRow, TECHNIQUE_INSTRUCTION_COMPRESSION};

/// The `basis` substring that identifies a row written by this crate's own
/// unit tests (#7569).
///
/// Why: the fixture compiled prompt behind #7514 is the 13-byte string
/// `COMPILED-BODY`, and `basis` records the compiled size verbatim. A real
/// compiled prompt is tens of kilobytes — the smallest on the operator's
/// ledger is 22,559 B of sources against a 24,667 B compile — so 13 bytes
/// cannot arise from a genuine launch. The other candidate discriminators were
/// measured and rejected: `session_id` is a fixture directory name (`b`) on
/// only 113 of the 228 rows, the remaining 115 carrying the real session UUID
/// exported into the test process, and `sources 22559 B` appears on a
/// surviving genuine row too.
/// What: the literal `compiled 13 B`. The trailing ` B` is what keeps it from
/// matching `compiled 130 B`.
/// Test: `the_predicate_matches_only_fixture_rows`.
pub const FIXTURE_COMPILED_BASIS: &str = "compiled 13 B";

/// Directory under `usage/` holding the per-project "nothing folded" markers.
///
/// Why (#7569): a second copy of this name would let the producer's directory
/// and the repair's sweep drift apart silently — `--markers` would move
/// nothing, count nothing, and still exit 0. So this aliases the producer's own
/// constant rather than re-spelling it.
/// What: [`crate::core::savings_sidecar::NO_FOLD_WARNED_DIR`], re-exported under
/// this module's public name.
/// Test: `marker_dir_nests_under_usage`,
/// `the_repair_sweeps_the_directory_the_producer_writes`.
pub const MARKER_DIR: &str = crate::core::savings_sidecar::NO_FOLD_WARNED_DIR;

/// Why a row is being moved off the ledger.
///
/// Why: the sidecar records the rule that matched each row, because a repair
/// that cannot say why it took a row is one an operator cannot audit.
/// What: `TestFixture` for [`is_test_fixture_row`]; `Malformed` for a fragment
/// that is not parseable JSON at all.
/// Test: `plan_classifies_a_mixed_ledger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The row carries [`FIXTURE_COMPILED_BASIS`] — written by a unit test.
    TestFixture,
    /// The text does not parse as a [`SavingsRow`].
    Malformed,
}

impl Reason {
    /// The stable string written into the sidecar and printed by the command.
    ///
    /// Test: `plan_classifies_a_mixed_ledger`.
    pub fn label(self) -> &'static str {
        match self {
            Self::TestFixture => "test-fixture",
            Self::Malformed => "malformed",
        }
    }
}

/// One row's worth of ledger text, with the verdict [`plan`] reached on it.
///
/// Why: `text` is the row's ORIGINAL bytes rather than a re-serialisation of
/// the parsed struct, so a kept row is written back byte-for-byte. Round-
/// tripping through `serde_json` would renormalise float formatting and key
/// order, which the three ledger readers would parse identically but a diff
/// would not — and a repair nobody can diff is one nobody can check.
/// What: `line` is the 1-based physical line the fragment came from (two
/// fragments share it after a #7579 split); `quarantine` is `None` for a row
/// the repair keeps.
/// Test: `apply_keeps_the_original_bytes`, `plan_splits_a_concatenated_line`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    /// 1-based physical line in the ledger this fragment was read from.
    pub line: usize,
    /// The fragment's original bytes, trimmed of surrounding whitespace.
    pub text: String,
    /// `Some` when the repair moves this fragment to the sidecar.
    pub quarantine: Option<Reason>,
}

/// What [`plan`] found, and what [`apply`] would do with it.
///
/// Why: planning and applying are separate so `--dry-run` — the default — can
/// print the whole verdict having opened the ledger read-only.
/// What: every fragment in ledger order, plus the line numbers that need
/// structural repair. `blank_lines` are the empty lines #7579's interleave
/// leaves behind; they carry no row, and dropping them is part of the repair.
/// Test: `plan_classifies_a_mixed_ledger`, `plan_of_a_missing_ledger_is_clean`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LedgerPlan {
    /// Every row-shaped fragment read, in ledger order.
    pub fragments: Vec<Fragment>,
    /// Physical lines that held more or less than exactly one parseable row.
    pub repaired_lines: Vec<usize>,
    /// Physical lines that were empty.
    pub blank_lines: Vec<usize>,
    /// How many physical lines the ledger held.
    pub lines_read: usize,
    /// How many bytes of ledger [`plan`] consumed.
    ///
    /// Why (#7569): the ledger has a LIVE producer. Everything after this
    /// offset arrived between the read and [`apply`]'s rename, and would
    /// otherwise be stranded on the old inode — see [`apply`].
    /// What: the byte length of the text `plan` read; zero for a missing
    /// ledger.
    /// Test: `apply_carries_a_row_appended_after_the_plan`.
    pub bytes_read: u64,
}

impl LedgerPlan {
    /// The fragments the repair writes back to the ledger.
    ///
    /// Test: `apply_quarantines_and_rewrites`.
    pub fn kept(&self) -> impl Iterator<Item = &Fragment> {
        self.fragments.iter().filter(|f| f.quarantine.is_none())
    }

    /// The fragments the repair moves to the sidecar.
    ///
    /// Test: `apply_quarantines_and_rewrites`.
    pub fn quarantined(&self) -> impl Iterator<Item = &Fragment> {
        self.fragments.iter().filter(|f| f.quarantine.is_some())
    }

    /// How many fragments carry `reason`.
    ///
    /// Test: `plan_classifies_a_mixed_ledger`.
    pub fn count_of(&self, reason: Reason) -> usize {
        self.fragments
            .iter()
            .filter(|f| f.quarantine == Some(reason))
            .count()
    }

    /// Is there nothing at all for [`apply`] to do?
    ///
    /// Why: this is what makes a second `--apply` a no-op rather than a second
    /// rewrite — the idempotency the issue's closure conditions ask for.
    /// What: true when no fragment is quarantined and no line needs structural
    /// repair.
    /// Test: `a_second_apply_finds_nothing`.
    pub fn is_clean(&self) -> bool {
        self.quarantined().next().is_none()
            && self.repaired_lines.is_empty()
            && self.blank_lines.is_empty()
    }
}

/// What [`apply`] did.
///
/// Test: `apply_quarantines_and_rewrites`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The sidecar the quarantined rows were written to.
    pub quarantine: PathBuf,
    /// How many rows the rewritten ledger holds.
    pub kept: usize,
    /// How many rows moved to the sidecar.
    pub quarantined: usize,
    /// How many rows a live producer appended during the repair and [`apply`]
    /// carried across unclassified (#7569).
    ///
    /// Test: `apply_carries_a_row_appended_after_the_plan`.
    pub carried: usize,
}

/// One quarantined fragment, as written to the sidecar.
///
/// Why: the sidecar is not a ledger — giving it the ledger's own schema would
/// let a mis-aimed reader fold the rows this repair just removed. Wrapping the
/// original text in an envelope makes it inert to every ledger reader while
/// keeping the bytes exactly recoverable.
/// What: the matched rule, the physical line it came from, and the row's
/// original text.
/// Test: `apply_quarantines_and_rewrites`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantinedRow {
    /// [`Reason::label`] of the rule that matched.
    pub reason: String,
    /// 1-based physical line in the original ledger.
    pub source_line: usize,
    /// The row's original bytes.
    pub raw: String,
}

/// Was this row written by one of this crate's unit tests (#7569)?
///
/// Why: THE predicate. Every caller — the dry run, the apply, and the tests —
/// routes through this one function, so the rule a dry run prints and the rule
/// an apply enforces cannot drift apart.
/// What: an `instruction-compression` row whose `basis` records a 13-byte
/// compiled prompt. See [`FIXTURE_COMPILED_BASIS`] for why that, and why not
/// `session_id` or the source-byte count.
/// Test: `the_predicate_matches_only_fixture_rows`.
pub fn is_test_fixture_row(row: &SavingsRow) -> bool {
    row.technique == TECHNIQUE_INSTRUCTION_COMPRESSION && row.basis.contains(FIXTURE_COMPILED_BASIS)
}

/// Split one physical ledger line into the row fragments it actually holds.
///
/// Why (#7579): the ledger's writer issued a row and its newline as two
/// `O_APPEND` writes, so two racing producers could land `{…}{…}` on one line.
/// A reader that treats a physical line as a row loses BOTH halves of such a
/// line; this repair must lose neither.
/// What: streams JSON values off the line, returning each value's original
/// slice beside the parsed row. The first slice that fails to parse is
/// returned with `None` and ends the walk — `serde_json`'s stream cannot
/// resume past an error, and a trailing fragment that is not JSON has no
/// further structure to find.
/// Test: `plan_splits_a_concatenated_line`,
/// `split_line_reports_an_unparseable_line_once`.
fn split_line(line: &str) -> Vec<(&str, Option<SavingsRow>)> {
    let mut fragments = Vec::new();
    let mut stream = serde_json::Deserializer::from_str(line).into_iter::<SavingsRow>();
    let mut start = 0usize;
    while let Some(next) = stream.next() {
        match next {
            Ok(row) => {
                let end = stream.byte_offset();
                fragments.push((line[start..end].trim(), Some(row)));
                start = end;
            }
            Err(_) => {
                let rest = line[start..].trim();
                if !rest.is_empty() {
                    fragments.push((rest, None));
                }
                break;
            }
        }
    }
    fragments
}

/// Read the ledger and classify every row on it, writing nothing.
///
/// Why: `--dry-run` is the command's default, so the whole verdict has to be
/// reachable from a read-only pass.
/// What: walks the ledger line by line, splits each line into fragments (see
/// [`split_line`]), and records a [`Reason`] for each fragment the repair would
/// move. A missing ledger plans to nothing — that is the ordinary state on a
/// machine that has run no producer, not a fault. Any other read error is
/// returned, because a repair must never treat an unreadable ledger as an
/// empty one.
/// Test: `plan_classifies_a_mixed_ledger`, `plan_of_a_missing_ledger_is_clean`,
/// `plan_propagates_a_read_error`.
pub fn plan(ledger: &Path) -> std::io::Result<LedgerPlan> {
    let text = match std::fs::read_to_string(ledger) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LedgerPlan::default());
        }
        Err(source) => return Err(source),
    };

    let mut planned = LedgerPlan {
        // #7569: the offset `apply` measures growth against.
        bytes_read: text.len() as u64,
        ..LedgerPlan::default()
    };
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        planned.lines_read += 1;
        if line.trim().is_empty() {
            planned.blank_lines.push(number);
            continue;
        }
        let fragments = split_line(line);
        if fragments.len() != 1 || fragments.iter().any(|(_, row)| row.is_none()) {
            planned.repaired_lines.push(number);
        }
        for (text, row) in fragments {
            let quarantine = match &row {
                None => Some(Reason::Malformed),
                Some(row) if is_test_fixture_row(row) => Some(Reason::TestFixture),
                Some(_) => None,
            };
            planned.fragments.push(Fragment {
                line: number,
                text: text.to_string(),
                quarantine,
            });
        }
    }
    Ok(planned)
}

/// Where the quarantined rows for a repair run at `now` are written.
///
/// What: `<ledger>.quarantine-<YYYYMMDDTHHMMSSZ>.jsonl`, a sibling of the
/// ledger so the rename in [`apply`] stays within one filesystem.
/// Test: `quarantine_path_is_a_timestamped_sibling`.
pub fn quarantine_path(ledger: &Path, now: chrono::DateTime<chrono::Utc>) -> PathBuf {
    sibling(ledger, &format!(".quarantine-{}.jsonl", stamp(now)))
}

/// `<path>` with `suffix` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// The compact UTC stamp both sidecar names carry.
fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Move the quarantined rows to a sidecar and rewrite the ledger.
///
/// Why: the operator's ledger is the only copy of every genuine row on it, so
/// this is written to fail rather than to half-succeed.
/// What: serialises both outputs into memory FIRST, then writes the sidecar
/// (refusing to clobber an existing one), then writes a temp file in the
/// ledger's own directory and renames it over the ledger. Every failure path
/// returns before that rename, leaving the ledger exactly as it was; the
/// sidecar, if it was written, is a copy and costs nothing. A clean plan
/// writes nothing at all and reports zero.
///
/// The ledger has a LIVE producer — a Claude hook appends to it at any moment —
/// so the rename cannot simply replace what [`plan`] read: a row that arrived
/// since then sits on the old inode and the rename would strand it. So the last
/// thing written to the temp file is [`tail_since`]'s bytes, carried across
/// verbatim and counted in [`Applied::carried`]. That narrows the loss window
/// to the rename itself, which no lock the producers take could close; a row
/// carried this way is unclassified, and the next run classifies it. A ledger
/// that SHRANK since the plan is not the file the plan describes, so it is
/// refused rather than rewritten.
/// Test: `apply_quarantines_and_rewrites`, `a_second_apply_finds_nothing`,
/// `apply_keeps_the_original_bytes`,
/// `apply_leaves_the_ledger_untouched_when_the_sidecar_exists`,
/// `apply_carries_a_row_appended_after_the_plan`,
/// `apply_refuses_a_ledger_that_shrank`,
/// `apply_leaves_no_temp_file_when_the_directory_is_read_only`.
pub fn apply(
    ledger: &Path,
    planned: &LedgerPlan,
    now: chrono::DateTime<chrono::Utc>,
) -> std::io::Result<Applied> {
    let quarantine = quarantine_path(ledger, now);
    if planned.is_clean() {
        return Ok(Applied {
            quarantine,
            kept: planned.kept().count(),
            quarantined: 0,
            carried: 0,
        });
    }

    let mut kept_bytes = String::new();
    let mut kept = 0usize;
    for fragment in planned.kept() {
        kept_bytes.push_str(&fragment.text);
        kept_bytes.push('\n');
        kept += 1;
    }

    let mut quarantined_bytes = String::new();
    let mut quarantined = 0usize;
    for fragment in planned.quarantined() {
        let reason = fragment.quarantine.unwrap_or(Reason::Malformed);
        let entry = QuarantinedRow {
            reason: reason.label().to_string(),
            source_line: fragment.line,
            raw: fragment.text.clone(),
        };
        let encoded = serde_json::to_string(&entry)
            .map_err(|source| std::io::Error::new(std::io::ErrorKind::InvalidData, source))?;
        quarantined_bytes.push_str(&encoded);
        quarantined_bytes.push('\n');
        quarantined += 1;
    }

    if quarantined > 0 {
        // `create_new` so a second run at the same second can never overwrite
        // the rows the first one moved.
        let mut sidecar = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&quarantine)?;
        sidecar.write_all(quarantined_bytes.as_bytes())?;
        sidecar.sync_all()?;
    }

    let temp = sibling(ledger, &format!(".repair-{}.tmp", stamp(now)));
    let written = (|| -> std::io::Result<usize> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(kept_bytes.as_bytes())?;
        // #7569: read the producer's appends LAST, so the window between the
        // read and the rename below is as small as this can make it.
        let tail = tail_since(ledger, planned.bytes_read)?;
        file.write_all(&tail)?;
        file.sync_all()?;
        Ok(rows_in(&tail))
    })();
    let carried = match written {
        Ok(carried) => carried,
        Err(source) => {
            let _ = std::fs::remove_file(&temp);
            return Err(source);
        }
    };
    if let Err(source) = std::fs::rename(&temp, ledger) {
        let _ = std::fs::remove_file(&temp);
        return Err(source);
    }

    Ok(Applied {
        quarantine,
        kept,
        quarantined,
        carried,
    })
}

/// The ledger's bytes past `offset`, which arrived after [`plan`] read it.
///
/// Why (#7569): see [`apply`] — these are a live producer's appends, and the
/// rename would strand them on the old inode.
/// What: the bytes from `offset` to end of file, verbatim. A ledger no longer
/// than `offset` yields nothing; one SHORTER than `offset` is a different file
/// from the one the plan describes, so it is an `InvalidData` error naming both
/// lengths rather than a rewrite from a stale plan.
/// Test: `apply_carries_a_row_appended_after_the_plan`,
/// `apply_refuses_a_ledger_that_shrank`.
fn tail_since(ledger: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _};

    let mut file = std::fs::File::open(ledger)?;
    let length = file.metadata()?.len();
    if length < offset {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "savings ledger {} shrank from {offset} to {length} bytes during the repair",
                ledger.display()
            ),
        ));
    }
    if length == offset {
        return Ok(Vec::new());
    }
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    Ok(tail)
}

/// How many non-blank lines `bytes` holds.
///
/// What: counts what an operator would call rows, so a tail of `"\n"` reports
/// zero rather than one. Invalid UTF-8 is counted lossily — this is a report,
/// never the bytes written.
/// Test: `apply_carries_a_row_appended_after_the_plan`.
fn rows_in(bytes: &[u8]) -> usize {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// Where the "nothing folded" markers live under a framework root.
///
/// Test: `marker_dir_nests_under_usage`.
pub fn marker_dir(root: &Path) -> PathBuf {
    root.join("usage").join(MARKER_DIR)
}

/// How many marker files the directory holds.
///
/// What: a count of its entries; a missing directory counts zero.
/// Test: `quarantine_markers_moves_the_whole_directory`.
pub fn count_markers(root: &Path) -> std::io::Result<usize> {
    match std::fs::read_dir(marker_dir(root)) {
        Ok(entries) => Ok(entries.count()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(source) => Err(source),
    }
}

/// Move the whole marker directory aside (#7569).
///
/// Why: the operator's directory holds ~3,809 markers, one per distinct
/// project path, where a real machine has a handful of projects. The content
/// does NOT discriminate: the markers were measured arriving in bursts of tens
/// per minute under EVERY byte pair present, this session's genuine
/// `26810 29172` pair included, so no per-file rule separates a test marker
/// from a real one. What makes moving all of them safe instead is what the
/// marker IS — a warn-once cache read by
/// [`crate::core::savings_sidecar::warn_no_fold_once`]. Losing one costs a
/// single repeated warning on the next launch of that project and nothing
/// else.
/// What: renames `usage/no-fold-warned` to
/// `usage/no-fold-warned.quarantine-<stamp>`, one atomic operation that keeps
/// every file. A missing directory yields `None`. The directory is never
/// deleted, and the next producer recreates an empty one.
/// Test: `quarantine_markers_moves_the_whole_directory`,
/// `quarantine_markers_of_a_missing_directory_is_none`.
pub fn quarantine_markers(
    root: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> std::io::Result<Option<(PathBuf, usize)>> {
    let dir = marker_dir(root);
    let markers = match std::fs::read_dir(&dir) {
        Ok(entries) => entries.count(),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(source),
    };
    let destination = sibling(&dir, &format!(".quarantine-{}", stamp(now)));
    std::fs::rename(&dir, &destination)?;
    Ok(Some((destination, markers)))
}

#[cfg(test)]
#[path = "savings_repair_tests.rs"]
mod tests;
