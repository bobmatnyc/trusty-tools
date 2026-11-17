//! The revision journal `_sources/revisions.jsonl` (SPEC-OKF-01 §4.9 Axis 1).
//!
//! Why: the save state must tell the bytes trusty-okf wrote from bytes that
//! changed outside it, and a crash between the journal line and the rename
//! must never mark unwritten bytes as Saved (criterion 66).
//! What: every trusty-okf concept write appends one [`RevisionLine`] before
//! its rename: concept ID, content hash, byte hash, operation, `now`, run ID,
//! and, inside an operation-journal operation, the op ID and step index
//! (§4.11 "Steps"). [`save_state`] counts a line only when the disk bytes
//! match its byte hash. Append-only and torn-line tolerant
//! (`build::jsonl::read_journal`). It stays in the bundle: it describes the
//! files, not a daemon (§4.9.1).
//! Test: `c66_crash_between_revision_line_and_rename_keeps_previous_saved`,
//! `c64_out_of_band_edit_reads_saved_external_and_unverified`.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::OkfError;
use crate::build::jsonl::read_journal;
use crate::bundle::paths::{append_line, confined_file};
use crate::state::SaveState;

/// Bundle-relative path of the revision journal.
pub(crate) const REVISIONS_REL: &str = "_sources/revisions.jsonl";

/// One revision-journal line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RevisionLine {
    /// Concept ID.
    pub id: String,
    /// §2.10 content hash of the written concept.
    pub content: String,
    /// Byte hash of the written file.
    pub bytes: String,
    /// The operation: `put`, `verify`, `move`, `ingest`, ...
    pub op_kind: String,
    /// The caller's `now`.
    pub now: String,
    /// The run (or operation) that wrote it.
    pub run: String,
    /// The operation-journal op ID, when written inside one (§4.11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// The operation step index of this append.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<usize>,
    /// #10746 S6: whether these bytes are proven engine-written (§4.5
    /// "Proven or unproven"). Absent on an S5 line: proven only for a
    /// source-derived kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proven: Option<bool>,
    /// #10746 S6: `source` or `caller` (§4.5 "Source or caller content").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// Op kinds whose line is a source-derived engine write (§4.5).
const SOURCE_KINDS: [&str; 2] = ["ingest", "rebuild"];

/// `origin` of a line whose content is the newest source-derived content.
pub(crate) const ORIGIN_SOURCE: &str = "source";
/// `origin` of a line whose content a trusty-okf caller changed.
pub(crate) const ORIGIN_CALLER: &str = "caller";

impl RevisionLine {
    /// Whether the line proves engine ownership of its bytes (§4.5).
    pub(crate) fn is_proven(&self) -> bool {
        self.proven
            .unwrap_or_else(|| SOURCE_KINDS.contains(&self.op_kind.as_str()))
    }

    /// Whether the line's content is source-derived (§4.5).
    pub(crate) fn is_source(&self) -> bool {
        match &self.origin {
            Some(o) => o == ORIGIN_SOURCE,
            None => SOURCE_KINDS.contains(&self.op_kind.as_str()),
        }
    }

    /// #10746 S6: stamp the §4.5 ownership of this line before it is
    /// written. A `source` write (ingest, rebuild) is proven with origin
    /// `source`. Any other write is proven only when the bytes it replaces
    /// (`pre`) are the bytes of a proven line, and has origin `source` only
    /// when its content equals the newest source-origin content of `id`.
    /// Test: `c131_caller_edit_and_migrated_legacy_concepts_are_preserved`.
    pub(crate) fn stamp(
        &mut self,
        root: &Path,
        pre: Option<&[u8]>,
        source: bool,
    ) -> Result<(), OkfError> {
        if source {
            self.proven = Some(true);
            self.origin = Some(ORIGIN_SOURCE.to_string());
            return Ok(());
        }
        let lines = read(root)?;
        let proven = pre
            .map(crate::format::hash::bytes_hash)
            .and_then(|h| record_of(&lines, &self.id, &h))
            .is_some_and(RevisionLine::is_proven);
        let source_content = lines
            .iter()
            .rev()
            .find(|l| l.id == self.id && l.is_source())
            .map(|l| l.content.as_str());
        let origin = if source_content == Some(self.content.as_str()) {
            ORIGIN_SOURCE
        } else {
            ORIGIN_CALLER
        };
        self.proven = Some(proven);
        self.origin = Some(origin.to_string());
        Ok(())
    }

    /// The JSON text of the line.
    pub(crate) fn json(&self) -> Result<String, OkfError> {
        serde_json::to_string(self).map_err(|e| OkfError::Parse {
            path: REVISIONS_REL.into(),
            line: None,
            message: e.to_string(),
        })
    }
}

/// A deterministic run ID: `<prefix>-<16 hex of sha256(parts)>`.
///
/// #10746: no clock and no randomness (§4.3), so an interrupted run and an
/// uninterrupted one name the same run, and their trees compare byte for byte
/// (criterion 106).
pub(crate) fn run_id(prefix: &str, parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    let digest = h.finalize();
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}-{hex}")
}

/// Append one line to the journal, synced (`append_line`).
pub(crate) fn append(root: &Path, line: &RevisionLine) -> Result<(), OkfError> {
    append_line(root, REVISIONS_REL, &line.json()?)
}

/// Every parseable line, oldest first. A torn line is skipped.
pub(crate) fn read(root: &Path) -> Result<Vec<RevisionLine>, OkfError> {
    let path = confined_file(root, REVISIONS_REL)?;
    Ok(read_journal::<RevisionLine>(&path)?.records)
}

/// The record of `id`'s bytes `disk`: the newest line for `id` with those
/// bytes (#10746 S6, §4.5).
///
/// Why: a line for bytes that never landed (a crash between the line and the
/// rename, criterion 66, or a rolled-back operation) must not make the bytes
/// on disk read as a hand edit. So the record is matched by bytes; `None`
/// with other lines for `id` present means the disk bytes are not
/// trusty-okf's (`hand_edit`).
/// Test: `c111_unproven_and_hand_edited_ledger_concepts_are_preserved`.
pub(crate) fn record_of<'a>(
    lines: &'a [RevisionLine],
    id: &str,
    disk: &str,
) -> Option<&'a RevisionLine> {
    lines.iter().rev().find(|l| l.id == id && l.bytes == disk)
}

/// The save axis of `id` whose file bytes hash to `disk` (§4.9 Axis 1).
///
/// What: Saved when some line for `id` carries `disk`; otherwise
/// SavedExternal (a hand edit, a sync merge, another tool, or no record).
pub(crate) fn save_state(lines: &[RevisionLine], id: &str, disk: &str) -> SaveState {
    if lines.iter().any(|l| l.id == id && l.bytes == disk) {
        SaveState::Saved
    } else {
        SaveState::SavedExternal
    }
}
