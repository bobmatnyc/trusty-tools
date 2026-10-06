//! Re-bind a record to its live pane after a tmux server replacement (#9313).
//!
//! Why: liveness and ownership compare a record's `(pane_id, tmux_server)`
//! with tmux (#9004). A replaced tmux server hands every relaunched session a
//! new server pid and new pane ids, so its record reads `stopped` forever
//! while the Claude in the pane keeps running. The #9238 rename follow needs
//! the record's own pane on the record's own server, so it cannot repair this.
//! What: [`SessionManager::rebind_stale_panes`], the automatic pass the boot
//! reconcile and the managed-routes cores run, and
//! [`SessionManager::rebind_session`], the `tm sessions rebind` verb. Both bind
//! a record only to the ONE pane its tmux session holds, and only when no
//! other record claims that pane or name. A rebind writes the record and
//! nothing else: no tmux session is created, killed, renamed or signalled.
//! Test: `pane_rebind_tests.rs`.

use std::collections::HashSet;

use chrono::Utc;
use tracing::{info, warn};

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::pane_identity::PaneIdentity;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::rename::validate_session_name;

/// What one rebind found for one record (#9313).
///
/// Why: the verb reports each session as rebound, no match or ambiguous, and
/// the automatic pass logs the same finding.
/// What: `Rebound` carries the binding written; `Current` means the record
/// already names the live pane; `NoMatch` and `Ambiguous` carry the reason
/// the record was left as it is.
/// Test: `exactly_one_pane_under_the_name_rebinds_a_stale_server_record`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebindOutcome {
    /// The record now names this session, pane and server, and is `Active`.
    Rebound {
        /// The tmux session the record now names.
        tmux_name: String,
        /// The `%N` pane the record now names.
        pane_id: String,
        /// The `<pid>:<start_time>` server the pane was read on.
        tmux_server: String,
    },
    /// The record already names the live pane on the live server.
    Current,
    /// No live pane matches; the record is unchanged.
    NoMatch(String),
    /// More than one pane or record matches; the record is unchanged.
    Ambiguous(String),
}

impl RebindOutcome {
    /// The wire label: `rebound`, `current`, `no_match` or `ambiguous`.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rebound { .. } => "rebound",
            Self::Current => "current",
            Self::NoMatch(_) => "no_match",
            Self::Ambiguous(_) => "ambiguous",
        }
    }
}

/// Whether a record in `state` may be rebound: only `Active` and `Stopped`
/// records describe a session tm would list as running.
fn rebindable(state: &ManagedSessionState) -> bool {
    matches!(
        state,
        ManagedSessionState::Active | ManagedSessionState::Stopped
    )
}

/// The identity of `pane`, accepted only when tmux echoes that pane in
/// session `target`. Any doubt is `Err`, so it never yields a binding.
fn verified_identity(
    tmux: &dyn ManagedTmuxDriver,
    target: &str,
    pane: &str,
) -> Result<PaneIdentity, ManagedError> {
    let identity = tmux.pane_identity(pane)?;
    // #9004: the same session-name normalisation `pane_identity::capture` uses.
    let expected = trusty_common::tmux::check_session_name(target).ok();
    if identity.pane_id != pane || expected.as_deref() != Some(identity.session_name.as_str()) {
        return Err(ManagedError::TmuxUnavailable(format!(
            "pane {pane} answered as {} in session '{}', not in '{target}'",
            identity.pane_id, identity.session_name
        )));
    }
    Ok(identity)
}

/// The reason another non-terminal record makes binding `id` to `identity`
/// under `target` ambiguous, or `None` when no other record claims either.
fn claimed_elsewhere(
    records: &[SessionRecord],
    id: ManagedSessionId,
    target: &str,
    identity: &PaneIdentity,
) -> Option<String> {
    records
        .iter()
        .filter(|r| r.id != id && !r.state.is_terminal())
        .find_map(|r| {
            let same_pane = r.pane_id.as_deref() == Some(identity.pane_id.as_str())
                && r.tmux_server.as_deref() == Some(identity.server.as_str());
            if same_pane {
                Some(format!(
                    "record {} already holds pane {} on this tmux server",
                    r.id, identity.pane_id
                ))
            } else if r.tmux_name == target {
                Some(format!(
                    "record {} also names tmux session '{target}'",
                    r.id
                ))
            } else {
                None
            }
        })
}

/// The one pane session `target` holds, or the reason there is not one.
enum PaneMatch {
    One(PaneIdentity),
    None(String),
    Many(String),
}

/// Find the single live pane of session `target` (#9313).
///
/// `Err` for any tmux read that fails, so a caller never reads "tmux could
/// not be asked" as "no pane".
fn single_pane(tmux: &dyn ManagedTmuxDriver, target: &str) -> Result<PaneMatch, ManagedError> {
    if !tmux.session_exists_checked(target)? {
        return Ok(PaneMatch::None(format!(
            "no live tmux session is named '{target}'"
        )));
    }
    let panes = tmux.session_pane_ids(target)?;
    match panes.as_slice() {
        [] => Ok(PaneMatch::None(format!(
            "tmux session '{target}' lists no pane"
        ))),
        [pane] => Ok(PaneMatch::One(verified_identity(tmux, target, pane)?)),
        many => Ok(PaneMatch::Many(format!(
            "tmux session '{target}' holds {} panes ({}); tm will not pick one",
            many.len(),
            many.join(", ")
        ))),
    }
}

impl SessionManager {
    /// Re-bind one record to the single live pane of its tmux session, or of
    /// `tmux` when given (#9313). Backs `tm sessions rebind`.
    ///
    /// Why: an operator repairs a record the automatic pass will not touch:
    /// a renamed session (`--tmux` names the live one), or a same-server
    /// relaunch, which the automatic pass leaves to an explicit decision.
    /// What: `InvalidState` unless the record is `Active` or `Stopped`, and
    /// for a `tmux` name tm cannot target. `NoMatch` when the session is gone
    /// or lists no pane, `Ambiguous` when it holds two or more panes or
    /// another non-terminal record claims the pane or the name, `Current`
    /// when the active record already names the pane. Otherwise writes
    /// `tmux_name`, `pane_id`, `tmux_server` and `Active`. Never launches,
    /// kills or signals anything.
    ///
    /// # Errors
    ///
    /// `SessionNotFound` for an unknown id, `InvalidState` as above, and any
    /// tmux or store failure, which leaves the record unchanged.
    /// Test: `the_manual_verb_rebinds_a_renamed_session_to_the_named_tmux_session`,
    /// `two_panes_in_the_session_refuse_the_rebind`,
    /// `a_tmux_error_in_the_manual_verb_changes_nothing`.
    pub async fn rebind_session(
        &self,
        id: &ManagedSessionId,
        tmux: Option<&str>,
    ) -> Result<RebindOutcome, ManagedError> {
        let record = self.get(id).await?;
        if !rebindable(&record.state) {
            return Err(ManagedError::InvalidState(
                id.to_string(),
                format!(
                    "a {} record cannot be rebound; only active or stopped records can",
                    record.state
                ),
            ));
        }
        let target = match tmux {
            Some(name) => validate_session_name(name)
                .map_err(|why| ManagedError::InvalidState(id.to_string(), why))?,
            None => record.tmux_name.clone(),
        };
        let identity = match single_pane(self.tmux.as_ref(), &target)? {
            PaneMatch::One(identity) => identity,
            PaneMatch::None(why) => return Ok(RebindOutcome::NoMatch(why)),
            PaneMatch::Many(why) => return Ok(RebindOutcome::Ambiguous(why)),
        };
        let current = record.state == ManagedSessionState::Active
            && record.tmux_name == target
            && record.pane_id.as_deref() == Some(identity.pane_id.as_str())
            && record.tmux_server.as_deref() == Some(identity.server.as_str());
        if current {
            return Ok(RebindOutcome::Current);
        }
        self.write_rebind(&record, &target, &identity).await
    }

    /// The automatic rebind the boot reconcile and the managed-routes cores
    /// run (#9313).
    ///
    /// Why: after a tmux server replacement every hand-relaunched session
    /// would otherwise list `stopped` until an operator rebinds it.
    /// What: one `list-sessions`. Then, for each `Active` or `Stopped` record
    /// whose `tmux_name` is live and that captured a `tmux_server`: when the
    /// live server differs from the recorded one, rebind it to the session's
    /// single pane as [`Self::rebind_session`] does. A record on the live
    /// server is left alone, so a same-server pane change is the manual
    /// verb's decision. The live server is read once per pass. Every tmux or
    /// store failure is logged and leaves that record unchanged. Returns
    /// every record it examined as stale, with its outcome.
    /// Test: `exactly_one_pane_under_the_name_rebinds_a_stale_server_record`,
    /// `no_pane_under_the_name_leaves_the_record_unchanged`,
    /// `two_panes_in_the_session_refuse_the_rebind`,
    /// `a_pane_list_error_leaves_that_record_unchanged_and_rebinds_the_rest`,
    /// `boot_reconcile_rebinds_and_lists_the_session_active_without_a_launch_or_kill`.
    pub async fn rebind_stale_panes(&self) -> Vec<(ManagedSessionId, RebindOutcome)> {
        let live: HashSet<String> = match self.tmux.list_sessions() {
            Ok(names) => names.into_iter().collect(),
            Err(e) => {
                warn!("pane rebind skipped: tmux could not be listed ({e}) (#9313)");
                return Vec::new();
            }
        };
        let mut live_server: Option<String> = None;
        let mut results = Vec::new();
        for record in self.list().await {
            let Some(recorded) = record.tmux_server.as_deref() else {
                continue;
            };
            if !rebindable(&record.state) || !live.contains(&record.tmux_name) {
                continue;
            }
            if live_server.as_deref() == Some(recorded) {
                continue;
            }
            let outcome = match self.stale_outcome(&record, &mut live_server).await {
                Ok(Some(outcome)) => outcome,
                Ok(None) => continue,
                Err(e) => {
                    warn!(id = %record.id, name = %record.tmux_name, "pane rebind: tmux could not be read ({e}); record left as is (#9313)");
                    continue;
                }
            };
            match &outcome {
                RebindOutcome::NoMatch(why) | RebindOutcome::Ambiguous(why) => {
                    warn!(id = %record.id, name = %record.tmux_name, "pane rebind: {why}; record left as is (#9313)");
                }
                RebindOutcome::Rebound { .. } | RebindOutcome::Current => {}
            }
            results.push((record.id, outcome));
        }
        results
    }

    /// One record's automatic-pass outcome: `None` when its recorded server
    /// is the live one, so nothing was replaced.
    async fn stale_outcome(
        &self,
        record: &SessionRecord,
        live_server: &mut Option<String>,
    ) -> Result<Option<RebindOutcome>, ManagedError> {
        let name = record.tmux_name.as_str();
        let panes = self.tmux.session_pane_ids(name)?;
        let Some(first) = panes.first() else {
            return Ok(Some(RebindOutcome::NoMatch(format!(
                "tmux session '{name}' lists no pane"
            ))));
        };
        let identity = verified_identity(self.tmux.as_ref(), name, first)?;
        *live_server = Some(identity.server.clone());
        if record.tmux_server.as_deref() == Some(identity.server.as_str()) {
            return Ok(None);
        }
        if panes.len() > 1 {
            return Ok(Some(RebindOutcome::Ambiguous(format!(
                "tmux session '{name}' holds {} panes ({}); tm will not pick one",
                panes.len(),
                panes.join(", ")
            ))));
        }
        self.write_rebind(record, name, &identity).await.map(Some)
    }

    /// Write the binding under one store write guard, only when no other
    /// record claims the pane or name and `before` is still the stored
    /// record. Changes the record and nothing else.
    async fn write_rebind(
        &self,
        before: &SessionRecord,
        target: &str,
        identity: &PaneIdentity,
    ) -> Result<RebindOutcome, ManagedError> {
        let mut store = self.store.write().await;
        let records = store.all().await?;
        if let Some(why) = claimed_elsewhere(&records, before.id, target, identity) {
            return Ok(RebindOutcome::Ambiguous(why));
        }
        let Some(stored) = records.into_iter().find(|r| r.id == before.id) else {
            return Err(ManagedError::SessionNotFound(before.id.to_string()));
        };
        let unchanged = stored.state == before.state
            && stored.tmux_name == before.tmux_name
            && stored.pane_id == before.pane_id
            && stored.tmux_server == before.tmux_server;
        if !unchanged {
            return Ok(RebindOutcome::NoMatch(
                "the record changed while it was being rebound".to_string(),
            ));
        }
        let mut updated = stored;
        updated.tmux_name = target.to_owned();
        updated.pane_id = Some(identity.pane_id.clone());
        updated.tmux_server = Some(identity.server.clone());
        let reactivated = updated.state != ManagedSessionState::Active;
        if reactivated {
            updated.set_lifecycle_state(ManagedSessionState::Active, Utc::now());
        }
        updated.stop_cause = None;
        store.upsert(updated).await?;
        drop(store);
        if reactivated {
            self.bump_residency_generation();
        }
        info!(
            id = %before.id,
            from_name = %before.tmux_name,
            from_pane = ?before.pane_id,
            from_server = ?before.tmux_server,
            to_name = %target,
            to_pane = %identity.pane_id,
            to_server = %identity.server,
            "pane rebind: record now names the live pane; the session was not touched (#9313)"
        );
        Ok(RebindOutcome::Rebound {
            tmux_name: target.to_owned(),
            pane_id: identity.pane_id.clone(),
            tmux_server: identity.server.clone(),
        })
    }
}

#[cfg(test)]
#[path = "pane_rebind_tests.rs"]
mod tests;
