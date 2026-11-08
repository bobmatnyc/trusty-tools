//! `log.md` appends (SPEC-OKF-01 §2.3, §4.5 "`log.md`", OKF §9).
//!
//! Why: every lifecycle event leaves one `log.md` entry, and no trusty-okf
//! write may remove, rewrite or reorder an existing entry: deleting the
//! lines an operation added must yield the old file byte for byte.
//! What: [`append_entry`] inserts `- <entry>` directly below the newest
//! `## YYYY-MM-DD` heading when that heading is the entry's date; otherwise
//! it adds a new heading and the entry above the newest heading, or at the
//! end when the file has no date heading. A missing `log.md` is created
//! with a `# Log` title. Returns the added lines.
//! Test: `log_entry_inserts_below_todays_heading_or_adds_one`,
//! `c09_move_rewrites_every_link_and_logs_one_entry`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};

use crate::OkfError;
use crate::bundle::paths::{atomic_write, confined_file, io_err};

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
    path: PathBuf,
    text: String,
    /// The lines the entry adds.
    pub(crate) added: Vec<String>,
}

impl PreparedEntry {
    /// Write the new `log.md` atomically, keeping its mode.
    pub(crate) fn commit(&self) -> Result<(), OkfError> {
        atomic_write(&self.path, self.text.as_bytes())
    }
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
    let old = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_err(&path, e)),
    };
    let date = now.format("%Y-%m-%d").to_string();
    let (text, added) = insert(&old, &date, entry);
    Ok(PreparedEntry { path, text, added })
}

/// Append one entry to the bundle-root `log.md` for `now`'s date.
///
/// What: [`prepare_entry`], then an atomic write keeping the file's mode.
/// Returns the added lines.
/// Test: `c09_move_rewrites_every_link_and_logs_one_entry`.
pub(crate) fn append_entry(
    root: &Path,
    now: &DateTime<FixedOffset>,
    entry: &str,
) -> Result<Vec<String>, OkfError> {
    let prepared = prepare_entry(root, now, entry)?;
    prepared.commit()?;
    Ok(prepared.added)
}

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
