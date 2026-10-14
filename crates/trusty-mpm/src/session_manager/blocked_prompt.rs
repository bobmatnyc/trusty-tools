//! The prompt an `Active` session is blocked on, as the idle classifier saw it (#8393).
//!
//! Why: the supervisor's classifier catches free-text prompts the `[Y/n]`
//! regex parser misses, and the fleet metrics must show them. `pending_decision`
//! is not the place: it is the front gate's field (`decision.rs`), and every
//! consumer reads a set one as a hold on the session — `front_gate_hold`, the
//! `/answer` route (409 `not_at_front_gate` off `Provisioning`), `resume`,
//! reconcile, pane rebind and reactivate. A classifier verdict there wedged the
//! session.
//! What: [`BlockedPrompt`], the record's `blocked_prompt` value, and
//! [`SessionManager::sync_blocked_prompt`], its only writer. Nothing but
//! `FleetMetrics::from_records` reads it.
//! Test: `blocked_prompt_tests.rs`; `a_raised_blocked_verdict_does_not_wedge_stop_and_resume`
//! in `supervisor/tests.rs`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::decision::locked_get;
use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState};

/// A free-text prompt the classifier saw an `Active` session blocked on.
///
/// Why: see the module doc; this is observation, never a gate.
/// What: the classifier's summary of the prompt and when the supervisor
/// recorded it.
/// Test: `blocked_prompt_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedPrompt {
    /// The classifier's summary of what the session is blocked on.
    pub question: String,
    /// When the supervisor recorded `question`.
    pub observed_at: DateTime<Utc>,
}

impl SessionManager {
    /// Set, replace or clear `id`'s blocked prompt, in one guarded write (#8393).
    ///
    /// Why: the supervisor raises the prompt from a fresh verdict and must
    /// remove it when a later verdict says the session moved on. A read, then
    /// a write through a second guard, could overwrite a claim or write to a
    /// session that stopped in between.
    /// What: under one held store write guard, reloads the record and
    /// - `Some(q)`: for an `Active` record whose prompt is not already `q`,
    ///   stores `q` stamped now; a record in any other state gets nothing;
    /// - `None`: drops any prompt the record holds.
    ///
    /// Returns `true` iff the record changed, so a repeat sweep over the same
    /// pane writes nothing. Touches no other field.
    /// Test: `blocked_prompt_tests.rs`.
    pub async fn sync_blocked_prompt(
        &self,
        id: &ManagedSessionId,
        question: Option<&str>,
    ) -> Result<bool, ManagedError> {
        let mut store = self.store.write().await;
        let mut record = locked_get(&mut store, id).await?;
        match question {
            Some(q) => {
                if record.state != ManagedSessionState::Active
                    || record
                        .blocked_prompt
                        .as_ref()
                        .is_some_and(|p| p.question == q)
                {
                    return Ok(false);
                }
                record.blocked_prompt = Some(BlockedPrompt {
                    question: q.to_owned(),
                    observed_at: Utc::now(),
                });
            }
            None => {
                if record.blocked_prompt.is_none() {
                    return Ok(false);
                }
                record.blocked_prompt = None;
            }
        }
        // `upsert_decision` puts the old entry back when the save fails, so a
        // prompt that never reached disk is not mistaken for a stored one on
        // the retry. The record was read under this guard, so it is current.
        store.upsert_decision(record).await?;
        Ok(true)
    }
}
