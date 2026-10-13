//! The daemon's hold on the channel question table (#8433 P2).
//!
//! Why: `ask_owner` (P4) and inbound answers (P5) must share one table per
//! daemon. If the table cannot open, the channel is off: both refuse, and
//! nothing falls back to an in-memory table that would lose reserved ids.
//! What: [`DaemonState::question_table`] opens `channels/questions.jsonl`
//! under [`DaemonState::user_home`] on first use. A failed open is not
//! cached, so the next call retries; a poisoned table is dropped and the
//! next call reopens it, and its replay decides.
//! Test: `table_open_failure_refuses_ask_owner_and_inbound`.

use std::sync::{Arc, Mutex};

use crate::channels::question_table::{QuestionTable, QuestionTableError, questions_path};

use super::DaemonState;

/// The slot holding the open table, if any.
#[derive(Debug, Default)]
pub(crate) struct QuestionTableHold {
    slot: Mutex<Option<Arc<QuestionTable>>>,
}

impl DaemonState {
    /// The daemon's question table, opening it when none is held.
    ///
    /// Why: "no table, no channel" must be decided in one place (#8433 r6).
    /// What: returns the held table unless it is poisoned; else opens it
    /// again. An open error is returned and not cached. Blocking I/O: async
    /// callers use `spawn_blocking`.
    /// Test: `table_open_failure_refuses_ask_owner_and_inbound`,
    /// `poisoned_table_is_reopened_by_the_holder`.
    pub fn question_table(&self) -> Result<Arc<QuestionTable>, QuestionTableError> {
        let home = self.user_home().ok_or(QuestionTableError::NoHome)?;
        // #8433 r6: a poisoned slot refuses; it is never recovered.
        let mut slot = self
            .channels
            .questions
            .slot
            .lock()
            .map_err(|_| QuestionTableError::Poisoned)?;
        if let Some(table) = slot.as_ref().filter(|t| !t.is_poisoned()) {
            return Ok(Arc::clone(table));
        }
        // #8433 r6 G1: drop a poisoned table and reopen; the replay decides.
        // #8433: it released its flock when it poisoned, so the reopen takes
        // the lock while stale holders finish; they refuse every operation.
        *slot = None;
        let table = Arc::new(QuestionTable::open_at(&questions_path(&home))?);
        *slot = Some(Arc::clone(&table));
        Ok(table)
    }
}
