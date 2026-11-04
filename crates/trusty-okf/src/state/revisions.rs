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
}

impl RevisionLine {
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
