//! Per-source append-only ledger — the "pull exactly once, ever" guarantee.
//!
//! Why: every ingest must converge, not accumulate. A re-run over the same
//! corpus has to produce ZERO new entities, a widened Gmail window has to fetch
//! only the messages it has never seen, and a crash halfway through a 5,000-item
//! pull must not force a full re-pull or duplicate the half that landed. All
//! three fall out of one durable structure: an append-only JSONL journal keyed
//! by the source's native item id, fsynced PER ITEM rather than at end of run.
//! Because it is a journal (not a rewritten snapshot), a torn final line from a
//! `kill -9` costs exactly one item, which the next run re-ingests.
//!
//! What: [`LedgerRecord`] is one line. [`Ledger`] loads a file into a
//! last-record-wins index, answers [`Ledger::is_current`] (the skip decision),
//! and [`Ledger::append`] durably adds a record. Re-ingesting a changed item
//! appends a NEW line with the same `item_id` and a new fingerprint — history is
//! never rewritten. [`Watermark`] is the derived status view (coverage span and
//! counts), computed from the journal rather than stored separately, so it can
//! never drift out of sync with what was actually ingested.
//!
//! Test: `append_then_reload_is_current`, `changed_fingerprint_is_not_current`,
//! `tombstoned_item_is_not_current`, `torn_line_is_skipped_not_fatal`,
//! `torn_mid_utf8_codepoint_is_skipped_not_fatal`,
//! `append_after_torn_line_does_not_corrupt_the_new_record`,
//! `watermark_spans_oldest_and_newest`.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::entity::slugify;
use crate::okg::registry::SOURCES_DIR;
use crate::roots::{assert_within, confine};

/// What a ledger line asserts about its item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    /// The item's entity was written into the tree.
    Ingested,
    /// The item vanished from its source and its entity was marked.
    Tombstoned,
}

/// One journal line: the durable record of a single item's ingestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerRecord {
    /// The source's own stable identity for this item — a relative path, a
    /// Gmail message id, a Drive file id. Never re-derived from content.
    pub item_id: String,
    /// Change token. Equal fingerprint = unchanged = skip. A content hash for
    /// doc stores, a revision/modifiedTime for Drive, the message id for Gmail
    /// (immutable, so a Gmail item is pulled exactly once, ever).
    pub fingerprint: String,
    /// `<collection>/<slug>` of the entity this item produced.
    pub entity: String,
    /// When this record was written (ISO-8601).
    pub ingested_at: String,
    /// The item's own timestamp in its source, when it has one. Drives the
    /// coverage watermark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Ingested or tombstoned.
    pub status: ItemStatus,
}

/// Derived coverage summary for one source, computed from its journal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Watermark {
    /// Distinct items currently in the ingested state.
    pub items: usize,
    /// Distinct items currently tombstoned.
    pub tombstoned: usize,
    /// Oldest item timestamp ever ingested — the "how far back" mark. Widening
    /// a window backwards is expected to move this earlier.
    pub oldest: Option<String>,
    /// Newest item timestamp ever ingested.
    pub newest: Option<String>,
    /// `ingested_at` of the most recent journal line — the last-run marker.
    pub last_ingest_at: Option<String>,
    /// Journal lines that failed to parse (e.g. a torn line from a crash).
    pub malformed_lines: usize,
}

/// An open, append-only ledger for one source.
pub struct Ledger {
    /// Journal file path.
    path: PathBuf,
    /// Last record seen per item id — the effective state.
    latest: BTreeMap<String, LedgerRecord>,
    /// `ingested_at` of the last line read or written.
    last_ingest_at: Option<String>,
    /// Count of unparseable lines encountered on load.
    malformed: usize,
    /// The file's final line lacks a terminating newline — the signature of a
    /// write interrupted mid-line. The first [`Ledger::append`] must emit a
    /// newline before its own record, or the two would concatenate into one
    /// unparseable line and the NEW record would be lost too. Without this the
    /// journal never converges: every subsequent run re-ingests that item and
    /// re-corrupts it the same way.
    needs_newline: bool,
    /// Lazily opened append handle, kept across a run.
    handle: Option<File>,
}

impl Ledger {
    /// The confined journal path for a source id under a KB root.
    ///
    /// Why: the id comes from a tool argument; slugging plus [`confine`] means a
    /// hostile id can never name a file outside `_sources/`.
    /// What: `<root>/_sources/<slug(id)>.jsonl`.
    /// Test: exercised by every test here via [`Ledger::load`].
    pub fn path_for(root: &Path, source_id: &str) -> anyhow::Result<PathBuf> {
        let file = format!("{}.jsonl", slugify(source_id));
        let path = confine(root, &[SOURCES_DIR, &file])?;
        assert_within(root, &path)?;
        Ok(path)
    }

    /// Read a source's journal into a last-record-wins index.
    ///
    /// Why: an absent journal is a first run, not an error; and a damaged line
    /// must be SKIPPED rather than abort the load, or one torn write from a
    /// crash would permanently brick the source.
    ///
    /// The parse is deliberately BYTE-oriented. Reading the file as a `String`
    /// first (`read_to_string`) fails the WHOLE load on invalid UTF-8, and a
    /// crash mid-write lands mid-codepoint whenever the record contains
    /// non-ASCII text — which for this crate is the common case, not the exotic
    /// one: a Gmail subject with a dash or an accent, a Drive filename in any
    /// non-Latin script. That turned a one-item loss into a permanently
    /// unloadable source, the exact failure this doc promises cannot happen.
    /// Splitting on `b'\n'` and decoding per line contains the damage to the
    /// torn line, whether it is invalid UTF-8 or merely invalid JSON.
    ///
    /// What: splits the raw bytes on newlines, decodes and parses each line
    /// independently, keeps the last record per item id, and counts every line
    /// it could not use.
    /// Test: `torn_line_is_skipped_not_fatal`,
    /// `torn_mid_utf8_codepoint_is_skipped_not_fatal`.
    pub fn load(root: &Path, source_id: &str) -> anyhow::Result<Self> {
        let path = Self::path_for(root, source_id)?;
        let mut ledger = Self {
            path,
            latest: BTreeMap::new(),
            last_ingest_at: None,
            malformed: 0,
            needs_newline: false,
            handle: None,
        };
        if !ledger.path.exists() {
            return Ok(ledger);
        }
        let bytes = std::fs::read(&ledger.path)?;
        ledger.needs_newline = !bytes.is_empty() && bytes.last() != Some(&b'\n');
        for line in bytes.split(|b| *b == b'\n') {
            // Tolerate CRLF journals and blank separator lines.
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            // `from_slice` handles the UTF-8 check itself, so an invalid-UTF-8
            // line lands in exactly the same `malformed` bucket as bad JSON —
            // no separate decode step, no way for one to abort the other.
            match serde_json::from_slice::<LedgerRecord>(line) {
                Ok(record) => {
                    ledger.last_ingest_at = Some(record.ingested_at.clone());
                    ledger.latest.insert(record.item_id.clone(), record);
                }
                Err(_) => ledger.malformed += 1,
            }
        }
        Ok(ledger)
    }

    /// The effective record for an item, if it has ever been ingested.
    pub fn get(&self, item_id: &str) -> Option<&LedgerRecord> {
        self.latest.get(item_id)
    }

    /// Whether this exact item version is already in the tree — the skip test.
    ///
    /// Why: this single predicate delivers all three idempotency properties.
    /// Unchanged fingerprint → skip (re-run adds nothing). Changed fingerprint
    /// → re-ingest, replacing the prior entity. Tombstoned → NOT current, so an
    /// item that reappears is re-ingested and un-tombstoned rather than
    /// silently ignored forever.
    /// Test: `append_then_reload_is_current`, `changed_fingerprint_is_not_current`,
    /// `tombstoned_item_is_not_current`.
    pub fn is_current(&self, item_id: &str, fingerprint: &str) -> bool {
        self.latest
            .get(item_id)
            .is_some_and(|r| r.status == ItemStatus::Ingested && r.fingerprint == fingerprint)
    }

    /// Whether this item has EVER been recorded, at any version.
    ///
    /// Why: callers that pay per fetch (Gmail message bodies, Drive downloads)
    /// use this to skip the expensive fetch entirely for ids already pulled —
    /// the "pull exactly once, ever" optimisation. Content-addressed sources use
    /// [`Ledger::is_current`] instead, since they must re-read to know.
    pub fn contains(&self, item_id: &str) -> bool {
        self.latest.contains_key(item_id)
    }

    /// Item ids currently in the ingested (non-tombstoned) state, sorted.
    pub fn live_item_ids(&self) -> Vec<String> {
        self.latest
            .values()
            .filter(|r| r.status == ItemStatus::Ingested)
            .map(|r| r.item_id.clone())
            .collect()
    }

    /// Durably append one record, updating the in-memory index.
    ///
    /// Why: per-item durability is what makes a crashed ingest converge. The
    /// line is flushed AND `sync_data`'d before the call returns, so a process
    /// killed immediately after has lost nothing already reported as ingested.
    /// What: opens (once per run) in append mode, writes one JSON line, syncs.
    /// Test: `append_then_reload_is_current`.
    pub fn append(&mut self, record: LedgerRecord) -> anyhow::Result<()> {
        if self.handle.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.handle = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let line = serde_json::to_string(&record)?;
        let file = self
            .handle
            .as_mut()
            .expect("handle just opened above; None is unreachable");
        // Heal a torn final line before appending — see `needs_newline`.
        if self.needs_newline {
            writeln!(file)?;
            self.needs_newline = false;
        }
        writeln!(file, "{line}")?;
        file.flush()?;
        // Best-effort durability: a filesystem that cannot fsync must not fail
        // the ingest — the line is already flushed to the OS at this point.
        let _ = file.sync_data();

        self.last_ingest_at = Some(record.ingested_at.clone());
        self.latest.insert(record.item_id.clone(), record);
        Ok(())
    }

    /// Derive the coverage summary from the journal.
    ///
    /// Why/What/Test: see [`Watermark`] and `watermark_spans_oldest_and_newest`.
    pub fn watermark(&self) -> Watermark {
        let mut wm = Watermark {
            last_ingest_at: self.last_ingest_at.clone(),
            malformed_lines: self.malformed,
            ..Watermark::default()
        };
        for record in self.latest.values() {
            match record.status {
                ItemStatus::Ingested => wm.items += 1,
                ItemStatus::Tombstoned => wm.tombstoned += 1,
            }
            let Some(ts) = record.timestamp.as_deref() else {
                continue;
            };
            if wm.oldest.as_deref().is_none_or(|o| ts < o) {
                wm.oldest = Some(ts.to_string());
            }
            if wm.newest.as_deref().is_none_or(|n| ts > n) {
                wm.newest = Some(ts.to_string());
            }
        }
        wm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(item_id: &str, fingerprint: &str, ts: &str) -> LedgerRecord {
        LedgerRecord {
            item_id: item_id.into(),
            fingerprint: fingerprint.into(),
            entity: format!("sources/{item_id}"),
            ingested_at: "2026-07-24T00:00:00Z".into(),
            timestamp: Some(ts.into()),
            status: ItemStatus::Ingested,
        }
    }

    /// Why: durability across process boundaries is the point of the journal —
    /// a record written by one run must be visible to the next.
    /// What: appends, reloads from disk, and asserts the skip predicate holds.
    /// Test: self-contained.
    #[test]
    fn append_then_reload_is_current() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut ledger = Ledger::load(root, "mail").unwrap();
        assert!(
            !ledger.is_current("m1", "f1"),
            "unknown item is not current"
        );
        ledger.append(record("m1", "f1", "2026-07-01")).unwrap();

        let reloaded = Ledger::load(root, "mail").unwrap();
        assert!(reloaded.is_current("m1", "f1"));
        assert!(reloaded.contains("m1"));
        assert_eq!(reloaded.live_item_ids(), vec!["m1"]);
    }

    /// Why: a changed file must re-ingest — the fingerprint, not mere presence,
    /// decides.
    /// What: appends f1, asserts f2 is not current, appends f2, asserts the new
    /// fingerprint wins and no duplicate item appears.
    /// Test: self-contained.
    #[test]
    fn changed_fingerprint_is_not_current() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut ledger = Ledger::load(root, "docs").unwrap();
        ledger.append(record("a.md", "f1", "2026-07-01")).unwrap();
        assert!(!ledger.is_current("a.md", "f2"));

        ledger.append(record("a.md", "f2", "2026-07-02")).unwrap();
        let reloaded = Ledger::load(root, "docs").unwrap();
        assert!(reloaded.is_current("a.md", "f2"), "last record wins");
        assert!(!reloaded.is_current("a.md", "f1"), "old version superseded");
        assert_eq!(reloaded.watermark().items, 1, "one item, not two");
    }

    /// Why: a tombstoned item that comes back must be re-ingested, not skipped
    /// forever on a stale fingerprint match.
    /// What: ingests, tombstones, then asserts the same fingerprint is no longer
    /// considered current.
    /// Test: self-contained.
    #[test]
    fn tombstoned_item_is_not_current() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut ledger = Ledger::load(root, "docs").unwrap();
        ledger.append(record("a.md", "f1", "2026-07-01")).unwrap();
        let mut dead = record("a.md", "f1", "2026-07-01");
        dead.status = ItemStatus::Tombstoned;
        ledger.append(dead).unwrap();

        assert!(!ledger.is_current("a.md", "f1"));
        assert!(ledger.contains("a.md"));
        assert!(ledger.live_item_ids().is_empty());
        let wm = ledger.watermark();
        assert_eq!((wm.items, wm.tombstoned), (0, 1));
    }

    /// Why: a `kill -9` mid-write leaves a truncated final line. Losing that one
    /// item is acceptable; refusing to load the journal is not.
    /// What: writes a good line plus a truncated one, then asserts the good
    /// record still loads and the damage is counted, not fatal.
    /// Test: self-contained.
    #[test]
    fn torn_line_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut ledger = Ledger::load(root, "docs").unwrap();
        ledger.append(record("a.md", "f1", "2026-07-01")).unwrap();
        // Simulate a crash mid-append.
        let path = Ledger::path_for(root, "docs").unwrap();
        let mut torn = std::fs::read_to_string(&path).unwrap();
        torn.push_str("{\"item_id\":\"b.md\",\"fing");
        std::fs::write(&path, torn).unwrap();

        let reloaded = Ledger::load(root, "docs").unwrap();
        assert!(reloaded.is_current("a.md", "f1"), "intact record survives");
        assert!(!reloaded.contains("b.md"), "torn record is not trusted");
        assert_eq!(reloaded.watermark().malformed_lines, 1);
    }

    /// Why: regression guard (code-critic CRITICAL 1). A crash lands mid-write
    /// at an arbitrary BYTE, and this crate's records routinely carry non-ASCII
    /// text — a Gmail subject with an em-dash, a Drive filename in kana. Tearing
    /// such a record mid-codepoint left the file invalid UTF-8, and the old
    /// `read_to_string` load then failed outright, bricking the source on every
    /// subsequent run instead of losing one item.
    /// What: writes a record whose subject is multi-byte, tears it in the MIDDLE
    /// of a codepoint (asserted non-UTF-8), and proves the load still succeeds,
    /// keeps the intact record, counts the damage, and can append afterwards.
    /// Test: self-contained.
    #[test]
    fn torn_mid_utf8_codepoint_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut ledger = Ledger::load(root, "mail").unwrap();
        ledger.append(record("m1", "f1", "2026-07-01")).unwrap();

        // A realistic second record: non-ASCII subject in the entity name.
        let mut unicode = record("m2", "f2", "2026-07-02");
        unicode.entity = "mail/2026-07-02-プロジェクト—更新".to_string();
        let line = serde_json::to_string(&unicode).unwrap();

        // Tear INSIDE a multi-byte character rather than on a char boundary —
        // cutting one byte off the end would land on the ASCII `}` and prove
        // nothing. Find the first non-ASCII char and cut one byte into it.
        let path = Ledger::path_for(root, "mail").unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let cut = line
            .char_indices()
            .find(|(_, c)| !c.is_ascii())
            .map(|(i, _)| i + 1)
            .expect("fixture must contain a multi-byte character");
        let fragment = &line.as_bytes()[..cut];
        assert!(
            std::str::from_utf8(fragment).is_err(),
            "the fixture must actually be invalid UTF-8 or it proves nothing"
        );
        bytes.extend_from_slice(fragment);
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            String::from_utf8(std::fs::read(&path).unwrap()).is_err(),
            "the journal file as a whole must be invalid UTF-8"
        );

        // The load must survive — this is the regression.
        let mut reloaded =
            Ledger::load(root, "mail").expect("invalid UTF-8 must not brick loading");
        assert!(reloaded.is_current("m1", "f1"), "intact record survives");
        assert!(!reloaded.contains("m2"), "the torn record is not trusted");
        assert_eq!(reloaded.watermark().malformed_lines, 1);

        // And the source keeps working: the lost item re-ingests cleanly.
        reloaded.append(record("m2", "f2", "2026-07-02")).unwrap();
        let settled = Ledger::load(root, "mail").unwrap();
        assert!(settled.is_current("m2", "f2"), "recovery converges");
        assert_eq!(settled.watermark().items, 2);
    }

    /// Why: regression guard. Appending straight after a torn (newline-less)
    /// final line concatenated the two into ONE unparseable line, destroying the
    /// new record as well — so that item was re-ingested and re-corrupted on
    /// every subsequent run and the journal never converged. Found by
    /// `tests/okg_artifacts.rs::crashed_run_converges_on_rerun`.
    /// What: tears the file, appends, and asserts BOTH the new record and every
    /// intact prior record survive a reload, with exactly one line still bad.
    /// Test: self-contained.
    #[test]
    fn append_after_torn_line_does_not_corrupt_the_new_record() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut ledger = Ledger::load(root, "docs").unwrap();
        ledger.append(record("a.md", "f1", "2026-07-01")).unwrap();

        // Simulate a crash mid-append: a fragment with no trailing newline.
        let path = Ledger::path_for(root, "docs").unwrap();
        let mut torn = std::fs::read_to_string(&path).unwrap();
        torn.push_str("{\"item_id\":\"b.md\",\"fing");
        std::fs::write(&path, torn).unwrap();

        // The next run reloads and appends the item it lost.
        let mut recovered = Ledger::load(root, "docs").unwrap();
        recovered
            .append(record("b.md", "f2", "2026-07-02"))
            .unwrap();

        let settled = Ledger::load(root, "docs").unwrap();
        assert!(settled.is_current("a.md", "f1"), "intact record survives");
        assert!(
            settled.is_current("b.md", "f2"),
            "the re-appended record must NOT be swallowed by the torn fragment"
        );
        let wm = settled.watermark();
        assert_eq!(wm.malformed_lines, 1, "only the fragment is bad");
        assert_eq!(wm.items, 2);
    }

    /// Why: the status view must report how far back coverage actually reaches,
    /// because that is what tells the operator whether to widen the window.
    /// What: ingests three timestamps out of order and asserts the span.
    /// Test: self-contained.
    #[test]
    fn watermark_spans_oldest_and_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut ledger = Ledger::load(root, "mail").unwrap();
        for (id, ts) in [
            ("m2", "2026-05-01"),
            ("m1", "2026-07-01"),
            ("m3", "2026-01-01"),
        ] {
            ledger.append(record(id, id, ts)).unwrap();
        }
        let wm = ledger.watermark();
        assert_eq!(wm.items, 3);
        assert_eq!(wm.oldest.as_deref(), Some("2026-01-01"));
        assert_eq!(wm.newest.as_deref(), Some("2026-07-01"));
        assert_eq!(wm.last_ingest_at.as_deref(), Some("2026-07-24T00:00:00Z"));
    }
}
