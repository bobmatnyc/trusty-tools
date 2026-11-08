//! Follow a `tmux rename-session` by the record's own pane (#9238).
//!
//! Why: every liveness reader — the `tm ls` display, the 60-second reaper, the
//! boot reconcile and its dedup — keys on the record's `tmux_name`. A session
//! renamed in tmux (the Architect's `commands.md` does this) keeps its pane,
//! but its record still names the old session. The record then reads
//! stopped, `attach_cmd` targets a session that no longer exists, the reaper
//! marks it `Stopped`, and the next boot dedup decommissions it.
//! What: [`live_name_by_pane`] asks tmux which session holds the record's
//! `%N` pane, and accepts the answer only on the server the record captured
//! the pane on (#9004, #9101). [`SessionManager::name_drifts`] reports every
//! such record; [`SessionManager::follow_tmux_renames`] rewrites the
//! non-terminal ones to the current name. A record on a workspace (`-L`)
//! server is followed on that server only (#10101). Nothing here renames,
//! kills or signals a tmux session.
//! Test: `rename_follow_tests.rs`.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, warn};

use super::manager::{ManagedError, ManagedTmuxDriver, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::rename::validate_session_name;
use super::tmux_route::{self, RoutedTmux};

/// One record whose own pane is live under a tmux name it does not record.
///
/// Why: the reaper, the list route and the doctor row each act on the same
/// finding; one type keeps them reading the same fields.
/// What: the record's id, state, recorded name and pane, and the name of the
/// tmux session that holds that pane now.
/// Test: `a_renamed_session_is_followed_by_its_pane_and_listed_live`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameDrift {
    /// The record.
    pub id: ManagedSessionId,
    /// The record's persisted state.
    pub state: ManagedSessionState,
    /// The `tmux_name` the record carries.
    pub recorded: String,
    /// The record's `%N` pane id.
    pub pane_id: String,
    /// The name of the live tmux session that holds the pane.
    pub current: String,
    /// The record's stored tmux socket; `None` is the default server
    /// (#10101: the follow writes only a record still on this server).
    pub tmux_socket: Option<PathBuf>,
}

/// The live session name holding `record`'s own pane, when that name is not
/// `record.tmux_name` (#9238).
///
/// Why: a tmux pane id outlives a session rename, so the pane, not the name,
/// says where the record's runtime is now.
/// What: `None` when the record has no pane id or no server identity, when
/// its recorded name is in `live` (the name-keyed readers already see it), or
/// when the pane identity cannot be read. `None` too when the identity names
/// another server (a restarted server reused the `%N`), echoes another pane,
/// or names a session outside `live` or equal to the recorded name. Every
/// doubt is `None`, so a missing or foreign pane never reads as live.
/// Test: `a_dead_pane_is_never_followed_and_stays_stopped`,
/// `a_pane_id_on_another_tmux_server_is_never_followed`.
pub(crate) fn live_name_by_pane(
    record: &SessionRecord,
    live: &HashSet<String>,
    tmux: &dyn ManagedTmuxDriver,
) -> Option<String> {
    let pane_id = record.pane_id.as_deref()?;
    let server = record.tmux_server.as_deref()?;
    if live.contains(&record.tmux_name) {
        return None;
    }
    let identity = tmux.pane_identity(pane_id).ok()?;
    let moved = identity.server == server
        && identity.pane_id == pane_id
        && identity.session_name != record.tmux_name
        && live.contains(&identity.session_name);
    moved.then_some(identity.session_name)
}

/// The [`NameDrift`] for `record`, read against `live` on `tmux`'s server.
fn drift_of(
    record: SessionRecord,
    live: &HashSet<String>,
    tmux: &dyn ManagedTmuxDriver,
) -> Option<NameDrift> {
    let current = live_name_by_pane(&record, live, tmux)?;
    Some(NameDrift {
        id: record.id,
        state: record.state,
        recorded: record.tmux_name,
        pane_id: record.pane_id?,
        current,
        tmux_socket: record.tmux_socket,
    })
}

/// The drifts of `records`, all on workspace servers, each read on its own
/// server (#10101).
///
/// Why: a workspace server's pane ids and session names are its own; the
/// default server's listing says nothing about them, and may hold a
/// same-named session or the same `%N` id for another pane.
/// What: groups the records by stored socket, so each server is listed at
/// most once. Per server, one [`crate::core::tmux_server::bounded_probe`]
/// keyed on the socket lists its sessions and reads each record's pane there,
/// within [`crate::core::tmux_server::PROBE_BUDGET`]. A server that cannot be
/// routed to, whose listing fails, that does not answer in time, or that
/// another probe holds yields no drift for any of its records: they are left
/// as they are this pass. The default server is never asked.
/// Test: `a_renamed_workspace_server_record_is_followed_on_its_own_server`,
/// `a_same_named_session_on_another_server_does_not_hide_a_rename`,
/// `a_hung_workspace_server_is_skipped_and_its_records_kept`,
/// `an_unreachable_or_failing_workspace_server_is_skipped_never_defaulted`.
fn workspace_drifts(tmux: &dyn ManagedTmuxDriver, records: Vec<SessionRecord>) -> Vec<NameDrift> {
    use crate::core::tmux_server::{PROBE_BUDGET, bounded_probe};
    let mut servers: BTreeMap<PathBuf, (Arc<dyn ManagedTmuxDriver>, Vec<SessionRecord>)> =
        BTreeMap::new();
    for record in records {
        let Some(socket) = record.tmux_socket.clone() else {
            continue;
        };
        let driver = match tmux_route::route(tmux, &record) {
            Ok(RoutedTmux::Named(driver)) => driver,
            // A stored socket always routes to a named server or refuses.
            Ok(RoutedTmux::Default(_)) => continue,
            Err(e) => {
                warn!(id = %record.id, "rename follow skipped for this record: {e} (#10101)");
                continue;
            }
        };
        servers
            .entry(socket)
            .or_insert_with(|| (driver, Vec::new()))
            .1
            .push(record);
    }
    let mut drifts = Vec::new();
    for (socket, (driver, records)) in servers {
        let count = records.len();
        let probe = bounded_probe(&socket, PROBE_BUDGET, move || {
            let live: HashSet<String> = driver.list_sessions()?.into_iter().collect();
            Ok::<_, ManagedError>(
                records
                    .into_iter()
                    .filter_map(|r| drift_of(r, &live, driver.as_ref()))
                    .collect::<Vec<_>>(),
            )
        });
        match probe {
            Some(Ok(found)) => drifts.extend(found),
            Some(Err(e)) => warn!(
                socket = %socket.display(),
                "rename follow skipped {count} record(s): their tmux server could not be listed ({e}) (#10101)"
            ),
            None => warn!(
                socket = %socket.display(),
                "rename follow skipped {count} record(s): their tmux server did not answer within {}s or is already being probed (#10101)",
                PROBE_BUDGET.as_secs_f32()
            ),
        }
    }
    drifts
}

impl SessionManager {
    /// Every record whose own pane is live under another tmux name (#9238).
    ///
    /// Why: `tm doctor` reports these, and [`Self::follow_tmux_renames`]
    /// repairs the non-terminal ones.
    /// What: one default-server `list-sessions`, then [`live_name_by_pane`]
    /// per default-server record, of any state; then the workspace-server
    /// records through [`workspace_drifts`], off the async executor. No store
    /// lock is held across a tmux call (#3698).
    ///
    /// # Errors
    ///
    /// The default server's `list-sessions` failure, so a caller never reads
    /// "tmux could not be asked" as "nothing drifted". A workspace server that
    /// cannot be read drops only its own records from the answer.
    /// Test: `the_doctor_row_flags_a_live_pane_under_another_name`,
    /// `a_renamed_workspace_server_record_is_followed_on_its_own_server`.
    pub async fn name_drifts(&self) -> Result<Vec<NameDrift>, ManagedError> {
        let live: HashSet<String> = self.tmux.list_sessions()?.into_iter().collect();
        // #10101: a named-server record's pane is never looked up in the
        // default server's `live`; it is read on its own server.
        let (default, named): (Vec<_>, Vec<_>) = self
            .list()
            .await
            .into_iter()
            .partition(|r| r.tmux_socket.is_none());
        let mut drifts: Vec<NameDrift> = default
            .into_iter()
            .filter_map(|r| drift_of(r, &live, self.tmux.as_ref()))
            .collect();
        if !named.is_empty() {
            let tmux = Arc::clone(&self.tmux);
            match tokio::task::spawn_blocking(move || workspace_drifts(tmux.as_ref(), named)).await
            {
                Ok(found) => drifts.extend(found),
                Err(e) => warn!("rename follow skipped workspace-server records ({e}) (#10101)"),
            }
        }
        Ok(drifts)
    }

    /// Rewrite each non-terminal record whose pane moved to another tmux
    /// name, so it carries the current name (#9238).
    ///
    /// Why: see the module doc. The reaper, the boot reconcile and the
    /// list, get and attach-cmd routes run this first, so every name-keyed
    /// reader after it sees the session under the name tmux uses.
    /// What: [`Self::name_drifts`], then, under one store write guard, sets
    /// `tmux_name` to the current name for each non-terminal record that still
    /// carries the recorded name and pane. A record is skipped, with a
    /// `warn`, when another non-terminal record already holds the name, on
    /// any server, or the name is not one tm can target. The state is never
    /// changed. A tmux or
    /// store failure is logged and leaves every record as it was. Returns the
    /// records it rewrote.
    /// Test: `a_renamed_session_is_followed_by_its_pane_and_listed_live`,
    /// `attach_cmd_targets_the_current_tmux_name`,
    /// `the_reaper_follows_a_rename_instead_of_stopping_the_session`,
    /// `boot_reconcile_keeps_a_renamed_session_active`,
    /// `a_name_another_record_holds_is_not_taken`,
    /// `the_reaper_follows_a_workspace_server_rename_instead_of_stopping_it`.
    pub async fn follow_tmux_renames(&self) -> Vec<NameDrift> {
        let drifts = match self.name_drifts().await {
            Ok(d) => d,
            Err(e) => {
                warn!("rename follow skipped: tmux could not be listed ({e}) (#9238)");
                return Vec::new();
            }
        };
        let mut drifts: Vec<NameDrift> = drifts
            .into_iter()
            .filter(|d| !d.state.is_terminal())
            .collect();
        if drifts.is_empty() {
            return drifts;
        }
        let mut store = self.store.write().await;
        let mut records = match store.all().await {
            Ok(r) => r,
            Err(e) => {
                warn!("rename follow skipped: the store could not be read ({e}) (#9238)");
                return Vec::new();
            }
        };
        let mut followed = Vec::new();
        for drift in drifts.drain(..) {
            if validate_session_name(&drift.current).is_err() {
                warn!(id = %drift.id, name = %drift.current, "rename follow: tmux name is not one tm can target; record left as is (#9238)");
                continue;
            }
            // #10101: names are unique across servers (create and `rename`
            // check every record), so a holder on any server refuses.
            let held = records.iter().any(|r| {
                r.id != drift.id && !r.state.is_terminal() && r.tmux_name == drift.current
            });
            let Some(record) = records.iter_mut().find(|r| {
                r.id == drift.id
                    && !r.state.is_terminal()
                    && r.tmux_name == drift.recorded
                    && r.pane_id.as_deref() == Some(drift.pane_id.as_str())
                    // #10101: still on the server the pane was read on.
                    && r.tmux_socket == drift.tmux_socket
            }) else {
                continue;
            };
            if held {
                warn!(id = %drift.id, name = %drift.current, "rename follow: another record holds the tmux name; record left as is (#9238)");
                continue;
            }
            let mut updated = record.clone();
            updated.tmux_name = drift.current.clone();
            if let Err(e) = store.upsert(updated.clone()).await {
                warn!(id = %drift.id, "rename follow: store write failed ({e}) (#9238)");
                continue;
            }
            *record = updated;
            info!(id = %drift.id, from = %drift.recorded, to = %drift.current, pane_id = %drift.pane_id, "rename follow: record now carries the tmux session's current name (#9238)");
            followed.push(drift);
        }
        followed
    }
}
