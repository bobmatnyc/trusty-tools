//! Which tmux server a pane id belongs to (#9004).
//!
//! Why: a `%N` pane id is unique within ONE tmux server only. A restarted
//! server hands out `%0`, `%1`, … again, and `tm fleet init` launches its
//! sessions in a fixed order, so a record written before the restart can name
//! the same session AND the same `%N` as an unrelated live session. #8935's
//! ownership check, which compares pane ids alone, then judged the stale
//! record `Owned`, and stop, delete and decommission killed the live session.
//! What: [`PaneIdentity`], one pane's identity on the live server as a single
//! `display-message` read, and [`capture`], which reads a session's pane id
//! together with the server it was read on, for a record to store.
//! Test: `pane_identity_tests.rs`; ownership in `runtime_identity_tests.rs`.

use tracing::warn;

use super::manager::ManagedTmuxDriver;

/// The `display-message` format a [`PaneIdentity`] is parsed from: the pane
/// id echoed back, its session id, the server's pid and start time, and the
/// session name, which comes last because it is the only free-text field.
pub const PANE_IDENTITY_FORMAT: &str =
    "#{pane_id}|#{session_id}|#{pid}:#{start_time}|#{session_name}";

/// One pane's identity on the live tmux server (#9004).
///
/// Why: ownership must prove the pane is the record's on the SAME server
/// instance, and the teardown must kill by an id that a relaunch under the
/// same name cannot inherit.
/// What: the pane's `%N`, its session's `$N`, the server instance
/// (`<pid>:<start_time>`, see [`PaneIdentity::server`]) and the session name.
/// Test: `a_well_formed_identity_line_parses`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneIdentity {
    /// The pane's `%N` id, as tmux echoed it back.
    pub pane_id: String,
    /// The `$N` id of the session holding the pane. tmux never reuses it
    /// within one server instance.
    pub session_id: String,
    /// The server instance: its pid and its start time in epoch seconds. A
    /// restarted server differs in both, so `(server, pane_id)` names one
    /// pane for all time.
    pub server: String,
    /// The name of the session holding the pane.
    pub session_name: String,
}

impl PaneIdentity {
    /// Parse a [`PANE_IDENTITY_FORMAT`] line read for `requested`.
    ///
    /// Why: tmux 3.6b answers `display-message -t %999` for a missing pane
    /// with exit 0 and empty fields, so a zero exit proves nothing. Every
    /// field is checked, and any doubt is an `Err`.
    /// What: `Ok` only when the echoed pane id equals `requested`, the session
    /// id is a `$N` id, the server is `<digits>:<digits>` and the session name
    /// is non-empty; otherwise `Err` naming the line.
    /// Test: `a_missing_pane_answer_is_rejected`, `a_well_formed_identity_line_parses`.
    pub fn parse(requested: &str, line: &str) -> Result<Self, String> {
        let bad = || format!("unreadable identity for pane {requested}: {line:?}");
        let mut fields = line.trim_end_matches(['\n', '\r']).splitn(4, '|');
        let (Some(pane_id), Some(session_id), Some(server), Some(session_name)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(bad());
        };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let server_ok = server
            .split_once(':')
            .is_some_and(|(pid, start)| digits(pid) && digits(start));
        let session_id_ok = session_id.strip_prefix('$').is_some_and(digits);
        if pane_id != requested || !session_id_ok || !server_ok || session_name.is_empty() {
            return Err(bad());
        }
        Ok(Self {
            pane_id: pane_id.to_owned(),
            session_id: session_id.to_owned(),
            server: server.to_owned(),
            session_name: session_name.to_owned(),
        })
    }
}

/// `name`'s pane id and the identity of the tmux server it was read on, for
/// a record to store as `(pane_id, tmux_server)` (#9004).
///
/// Why: every site that captures a record's pane id must capture the server
/// with it, or the record cannot later prove its pane is the one on the live
/// server.
/// What: [`ManagedTmuxDriver::get_pane_id`], then
/// [`ManagedTmuxDriver::pane_identity`] of that pane. The server is `None`
/// when either read fails, or when the identity names a session other than
/// `name` (the server restarted between the two reads), so the record fails
/// closed: it is never `Owned`. A failed read is logged at `warn`.
/// Test: `capture_pairs_the_pane_with_its_server`,
/// `capture_without_a_readable_server_stores_no_server`.
pub fn capture(tmux: &dyn ManagedTmuxDriver, name: &str) -> (Option<String>, Option<String>) {
    let Some(pane_id) = tmux.get_pane_id(name) else {
        return (None, None);
    };
    let identity = match tmux.pane_identity(&pane_id) {
        Ok(identity) => identity,
        Err(e) => {
            warn!(
                session = %name,
                pane_id = %pane_id,
                "pane identity read failed; the record stores no tmux server (#9004): {e}"
            );
            return (Some(pane_id), None);
        }
    };
    // #9004: the same session-name check `same_server` applies at ownership
    // time, so a server restart between the two reads stores no server.
    let expected = trusty_common::tmux::check_session_name(name).ok();
    if expected.as_deref() != Some(identity.session_name.as_str()) {
        warn!(
            session = %name,
            pane_id = %pane_id,
            identity_session = %identity.session_name,
            "pane identity names another session; the record stores no tmux server (#9004)"
        );
        return (Some(pane_id), None);
    }
    (Some(pane_id), Some(identity.server))
}

#[cfg(test)]
#[path = "pane_identity_tests.rs"]
mod tests;
