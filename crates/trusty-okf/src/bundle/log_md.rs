//! `log.md` appends (SPEC-OKF-01 §2.3, §4.5 "`log.md`", OKF §9).
//!
//! Why: every lifecycle event leaves one `log.md` entry, and no trusty-okf
//! write may remove, rewrite or reorder an existing entry: deleting the
//! lines an operation added must yield the old file byte for byte.
//! What: `insert` puts `- <entry>` directly below the newest
//! `## YYYY-MM-DD` heading when that heading is the entry's date; otherwise
//! it adds a new heading and the entry above the newest heading, or at the
//! end when the file has no date heading. A missing `log.md` is created
//! with a `# Log` title. #10746 S5: `prepare_entry` renders the post-image
//! of a journaled `log.md` file step (§4.11); an ingest run writes its
//! entries once through `LogBatch`.
//! Test: `log_entry_inserts_below_todays_heading_or_adds_one`,
//! `c09_move_rewrites_every_link_and_logs_one_entry`,
//! `ingest_run_writes_log_md_once`.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};

use crate::OkfError;
use crate::build::jsonl::read_journal;
use crate::bundle::Bundle;
use crate::bundle::ops::{self, OPS_DIR, OpKind, OpPlan, disk_hash};
use crate::bundle::paths::{append_line, confined_file, io_err, read_rel};

/// Whether `line` is an OKF §9 date heading, returning its date.
fn date_heading(line: &str) -> Option<&str> {
    let d = line.strip_prefix("## ")?.trim_end();
    let b = d.as_bytes();
    let ok = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    ok.then_some(d)
}

/// The new `log.md` text with `entry` inserted for `date`, and the added
/// lines. Pure, so the insertion rule is tested without a file.
pub(crate) fn insert(old: &str, date: &str, entry: &str) -> (String, Vec<String>) {
    let item = format!("- {entry}");
    if old.is_empty() {
        let lines = vec![
            "# Log".to_string(),
            String::new(),
            format!("## {date}"),
            String::new(),
            item,
        ];
        return (format!("{}\n", lines.join("\n")), lines);
    }
    // Byte offsets of each line start.
    let mut starts = vec![0usize];
    starts.extend(
        old.match_indices('\n')
            .map(|(i, _)| i + 1)
            .filter(|&i| i < old.len()),
    );
    let line_at = |i: usize| {
        let end = starts.get(i + 1).copied().unwrap_or(old.len());
        old[starts[i]..end].trim_end_matches(['\n', '\r'])
    };
    let newest = (0..starts.len()).find(|&i| date_heading(line_at(i)).is_some());
    match newest {
        Some(h) if date_heading(line_at(h)) == Some(date) => {
            // Below the heading, after one blank line when there is one.
            let mut at = h + 1;
            if at < starts.len() && line_at(at).is_empty() {
                at += 1;
            }
            let offset = starts.get(at).copied().unwrap_or(old.len());
            let mut out = String::with_capacity(old.len() + item.len() + 2);
            out.push_str(&old[..offset]);
            if offset == old.len() && !old.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&item);
            out.push('\n');
            out.push_str(&old[offset..]);
            (out, vec![item])
        }
        Some(h) => {
            let added = vec![format!("## {date}"), String::new(), item, String::new()];
            let offset = starts[h];
            let out = format!("{}{}\n{}", &old[..offset], added.join("\n"), &old[offset..]);
            (out, added)
        }
        None => {
            let sep = if old.ends_with('\n') { "" } else { "\n" };
            let added = vec![String::new(), format!("## {date}"), String::new(), item];
            let out = format!("{old}{sep}{}\n", added.join("\n"));
            (out, added)
        }
    }
}

/// A `log.md` entry that is confined, read and rendered but not written.
///
/// #10746: `put` and `verify` prepare the entry before the concept rename,
/// so a refused `log.md` path (a link: `PathEscape`) or an unreadable file
/// stops the operation with no file changed.
#[derive(Debug)]
pub(crate) struct PreparedEntry {
    /// #10746 S5: the bytes read, `None` when absent: the pre-image of an
    /// operation-journal file step (§4.11).
    pub(crate) old: Option<Vec<u8>>,
    /// The new text: the post-image.
    pub(crate) text: String,
}

/// Confine and read the bundle-root `log.md` and render `entry` for `now`'s
/// date, writing nothing.
///
/// What: `log.md` must pass `confined_file` (no link); absent is empty; then
/// [`insert`] runs. Test: `put_and_verify_refuse_a_symlinked_log_with_no_file_changed`.
pub(crate) fn prepare_entry(
    root: &Path,
    now: &DateTime<FixedOffset>,
    entry: &str,
) -> Result<PreparedEntry, OkfError> {
    let path = confined_file(root, "log.md")?;
    let old = match std::fs::read(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_err(&path, e)),
    };
    let old_text = String::from_utf8(old.clone().unwrap_or_default()).map_err(|e| {
        io_err(
            &path,
            std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        )
    })?;
    let date = now.format("%Y-%m-%d").to_string();
    let (text, _) = insert(&old_text, &date, entry);
    Ok(PreparedEntry { old, text })
}

/// One pending `log.md` entry of an ingest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingEntry {
    /// The concept file the entry describes, bundle-relative.
    path: String,
    /// The entry text.
    entry: String,
    /// `YYYY-MM-DD` of the write's `now`.
    date: String,
    /// The file's byte hash before the write; `None` when it was absent.
    pre: Option<String>,
}

/// The `log.md` entries of one ingest run, written to `log.md` once.
///
/// Why: #10746 S5 (Architect ruling on log.md cost) — rewriting all of
/// `log.md` for each item made an N-item run rewrite a growing file N times.
/// What: [`LogBatch::record`] appends each entry, with the concept file's
/// pre-image hash, to `_sources/ops/log-<run>.jsonl` before the concept is
/// written (an O(1) append). [`LogBatch::flush`] keeps each entry whose file
/// no longer holds its pre-image (the write landed), inserts them in order
/// with the §4.5 rule, and writes `log.md` once through an `Ingest`
/// operation with the fixed op ID `<run>-log`, then removes the batch file.
/// After a crash, `Bundle::open` finishes the operation or flushes the
/// leftover batch; a batch whose operation already completed is only
/// removed, so no entry is written twice.
/// Test: `ingest_run_writes_log_md_once`,
/// `crash_mid_run_keeps_the_log_entries_of_written_items`.
#[derive(Debug, Clone)]
pub(crate) struct LogBatch {
    run: String,
}

impl LogBatch {
    /// A batch for `run`.
    pub(crate) fn new(run: String) -> Self {
        Self { run }
    }

    /// The batch of a leftover `log-<run>.jsonl`.
    pub(crate) fn resume(run: &str) -> Self {
        Self::new(run.to_string())
    }

    /// The run ID; the revision lines of the run carry it.
    pub(crate) fn run(&self) -> &str {
        &self.run
    }

    fn rel(&self) -> String {
        format!("{OPS_DIR}/log-{}.jsonl", self.run)
    }

    fn op_id(&self) -> String {
        format!("{}-log", self.run)
    }

    /// Record one entry before the concept write.
    pub(crate) fn record(
        &self,
        b: &Bundle,
        path: &str,
        entry: &str,
        now: &DateTime<FixedOffset>,
        pre: Option<String>,
    ) -> Result<(), OkfError> {
        let e = PendingEntry {
            path: path.to_string(),
            entry: entry.to_string(),
            date: now.format("%Y-%m-%d").to_string(),
            pre,
        };
        let json = serde_json::to_string(&e).map_err(|err| OkfError::Parse {
            path: self.rel().into(),
            line: None,
            message: err.to_string(),
        })?;
        append_line(b.root(), &self.rel(), &json)
    }

    /// Write the batch into `log.md` once (see the type doc). `done` holds
    /// the completed op IDs. Returns the entries written.
    pub(crate) fn flush(&self, b: &mut Bundle, done: &BTreeSet<String>) -> Result<usize, OkfError> {
        let rel = self.rel();
        let mut written = 0;
        if !done.contains(&self.op_id()) {
            let path = confined_file(b.root(), &rel)?;
            let mut kept = Vec::new();
            for e in read_journal::<PendingEntry>(&path)?.records {
                if disk_hash(b, &e.path)? != e.pre {
                    kept.push(e);
                }
            }
            if !kept.is_empty() {
                let log = confined_file(b.root(), "log.md")?;
                let old = read_rel(b.root(), "log.md")?;
                let mut text = String::from_utf8(old.clone().unwrap_or_default()).map_err(|e| {
                    io_err(
                        &log,
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e),
                    )
                })?;
                for e in &kept {
                    text = insert(&text, &e.date, &e.entry).0;
                }
                let now = kept.last().map(|e| e.date.clone()).unwrap_or_default();
                let mut plan =
                    OpPlan::new(OpKind::Ingest, &now, INGEST_LOG_ACTOR).with_id(self.op_id());
                plan.file("log.md", old, Some(text.into_bytes()));
                ops::run(b, plan)?;
                written = kept.len();
            }
        }
        let path = confined_file(b.root(), &rel)?;
        let removed = std::fs::remove_file(&path); // okf: temp-cleanup (flushed log batch)
        match removed {
            Ok(()) => Ok(written),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(written),
            Err(e) => Err(io_err(&path, e)),
        }
    }

    /// Flush, reading the completed op IDs from the journal.
    pub(crate) fn flush_now(&self, b: &mut Bundle) -> Result<usize, OkfError> {
        let (_, done) = crate::bundle::recover::read_ops(b)?;
        self.flush(b, &done)
    }
}

/// The actor an ingest run's `log.md` operation names.
const INGEST_LOG_ACTOR: &str = "process:trusty-okf-ingest";

#[cfg(test)]
mod tests {
    use super::*;

    /// Removing the added lines must give the old text back.
    fn without(new: &str, added: &[String]) -> String {
        let mut lines: Vec<&str> = new.split('\n').collect();
        for a in added {
            let i = lines
                .iter()
                .position(|l| l == a)
                .expect("added line present");
            lines.remove(i);
        }
        lines.join("\n")
    }

    /// Why: §4.5 — deleting the lines an operation added yields the old
    /// `log.md` byte for byte, and entries go newest first.
    /// What: inserts below today's heading, adds a new heading above an
    /// older one, appends to a file with no heading, and creates a file.
    /// Test: self-contained.
    #[test]
    fn log_entry_inserts_below_todays_heading_or_adds_one() {
        let old = "# Log\n\n## 2026-11-04\n\n- **A** one\n\n## 2026-11-01\n\n- **B** two\n";
        let (new, added) = insert(old, "2026-11-04", "**C** three");
        assert_eq!(
            new,
            "# Log\n\n## 2026-11-04\n\n- **C** three\n- **A** one\n\n## 2026-11-01\n\n- **B** two\n"
        );
        assert_eq!(without(&new, &added), old);

        let (new, added) = insert(old, "2026-11-05", "**D** four");
        assert!(new.starts_with("# Log\n\n## 2026-11-05\n\n- **D** four\n\n## 2026-11-04\n"));
        assert_eq!(without(&new, &added), old);

        let bare = "# Log\n\nIntro.";
        let (new, added) = insert(bare, "2026-11-05", "**E** five");
        assert_eq!(new, "# Log\n\nIntro.\n\n## 2026-11-05\n\n- **E** five\n");
        assert_eq!(added.len(), 4);

        let (new, _) = insert("", "2026-11-05", "**F** six");
        assert_eq!(new, "# Log\n\n## 2026-11-05\n\n- **F** six\n");
    }
}
