//! The `outbox.jsonl` lines a commit stages (LDG-4.4, #9758 F3).
//!
//! Why: the Architect ingests a PM store from its outbox, keyed `(code, id)`
//! and applied only when `rev` is higher (#9760), so the writer appends those
//! lines in the same journalled commit as `state.json` (LDG-2.8.2).
//! What: [`lines`] builds one [`OutboxLine`] per record a commit changed —
//! `op` is the record's last event, `archived` when it left `state.json` —
//! numbered on from the store's last `seq`, under its `epoch` (minted when
//! the outbox has no line). Reads only; the journal appends.
//! Test: `ledger_writer_appends_one_outbox_line_per_changed_record`,
//! `ledger_writer_kill_after_the_outbox_append_keeps_one_outbox_line`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-04~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-04~draft)

use serde::{Deserialize, Serialize};

use super::archived_id;
use super::journal::read_lossy;
use crate::error::LedgerError;
use crate::ledger::model::Archived;
use crate::ledger::ops::Now;
use crate::ledger::rpc::BatchEvent;
use crate::ledger::store::StorePaths;

/// One `outbox.jsonl` line (LDG-4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxLine {
    /// Per store, gapless, from 1.
    pub seq: u64,
    /// The store's outbox epoch; a new one means the store was re-created.
    pub epoch: String,
    /// The store's id code.
    pub code: String,
    /// The record id.
    pub id: String,
    /// The `v` of the commit that wrote the line.
    pub rev: u64,
    /// The record's last event in the commit, or `archived`.
    pub op: String,
    /// The commit minute, `YYYY-MM-DDTHH:MMZ`.
    pub at: String,
}

/// The fixed inputs of one commit's outbox lines.
pub(super) struct Rev<'a> {
    pub code: &'a str,
    pub v: u64,
    pub now: &'a Now,
}

/// The outbox text for one commit: a line per changed record, in order of
/// first change; empty when nothing changed. The writer's own pseudo-event
/// `<code>-prune` names no record and is left out; each record it archived
/// gets its own line.
///
/// # Errors
/// [`LedgerError::Io`] when `outbox.jsonl` exists but cannot be read.
pub(super) fn lines(
    paths: &StorePaths,
    rev: &Rev<'_>,
    events: &[BatchEvent],
    gone: &[Archived],
) -> Result<String, LedgerError> {
    let prune = format!("{}-prune", rev.code);
    let mut records: Vec<(&str, &str)> = Vec::new();
    let changes = events
        .iter()
        .filter(|e| e.id != prune)
        .map(|e| (e.id.as_str(), e.ev.as_str()))
        .chain(gone.iter().map(|a| (archived_id(a), "archived")));
    for (id, op) in changes {
        match records.iter_mut().find(|(have, _)| *have == id) {
            Some(slot) => slot.1 = op,
            None => records.push((id, op)),
        }
    }
    if records.is_empty() {
        return Ok(String::new());
    }
    let (last, epoch) = match tail(paths)? {
        Some((seq, epoch)) => (seq, epoch),
        None => (0, mint_epoch(rev.now)),
    };
    let mut out = String::new();
    for (n, (id, op)) in (1..).zip(records) {
        let line = OutboxLine {
            seq: last + n,
            epoch: epoch.clone(),
            code: rev.code.to_owned(),
            id: id.to_owned(),
            rev: rev.v,
            op: op.to_owned(),
            at: rev.now.minute.clone(),
        };
        out += &(serde_json::to_string(&line)? + "\n");
    }
    Ok(out)
}

/// The last parseable line's `seq` and `epoch`.
fn tail(paths: &StorePaths) -> Result<Option<(u64, String)>, LedgerError> {
    let text = read_lossy(&paths.outbox)?.unwrap_or_default();
    Ok(text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<OutboxLine>(l).ok())
        .map(|l| (l.seq, l.epoch)))
}

/// A new epoch: the commit's microseconds and this process id, in hex.
fn mint_epoch(now: &Now) -> String {
    let micros = (now.unix * 1e6) as u64;
    format!("{micros:x}{:04x}", std::process::id() & 0xffff)
}
