//! Which tmux server a record's session lives on, and the driver that reaches
//! it (#10101, step 4b of #8928).
//!
//! Why: a PM on its workspace's private (`-L`) server is invisible to the
//! default server, and the default server can hold an unrelated session with
//! the same name. A pane query, a teardown or a liveness check that asked the
//! default server would read, signal or count that other session.
//! What: [`record_server`] names a record's server from its stored
//! `tmux_socket`; [`route`] answers the driver aimed at it, or the caller's
//! driver for a default-server record; [`LiveSessions`] is the live set keyed
//! by (server, session). Every failure refuses: an unusable socket path or a
//! driver that cannot reach a named server is `Err`, never the default server.
//! Test: `server_route_tests.rs`, `tmux_route_tests.rs`.

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::sync::Arc;

use super::manager::{ManagedError, ManagedTmuxDriver};
use super::record::SessionRecord;

/// The `-L` server name `record` lives on; `None` for the default server.
///
/// What: the file name of `record.tmux_socket` (a server's socket is
/// `<socket_dir>/<name>`, `core::tmux_server::server_socket_path`). The
/// directory is not checked: `-L` resolves the name in this process's socket
/// directory.
///
/// # Errors
///
/// [`ManagedError::TmuxUnavailable`] when the path has no UTF-8 file name, so
/// no server can be named for it.
///
/// Test: `a_socket_path_names_its_server`, `an_unnamed_socket_path_refuses`.
pub(crate) fn record_server(record: &SessionRecord) -> Result<Option<String>, ManagedError> {
    let Some(socket) = record.tmux_socket.as_deref() else {
        return Ok(None);
    };
    match socket.file_name().and_then(|n| n.to_str()) {
        Some(name) if !name.is_empty() => Ok(Some(name.to_owned())),
        _ => Err(ManagedError::TmuxUnavailable(format!(
            "session '{}' records tmux socket {} with no server name; its server \
             cannot be reached",
            record.tmux_name,
            socket.display()
        ))),
    }
}

/// A driver for one record's server: the caller's own, or one aimed at a
/// named server.
pub(crate) enum RoutedTmux<'a> {
    /// The record is on the default server.
    Default(&'a dyn ManagedTmuxDriver),
    /// The record is on the named server this driver reaches.
    Named(Arc<dyn ManagedTmuxDriver>),
}

impl RoutedTmux<'_> {
    /// Whether this driver reaches a named server.
    pub(crate) fn is_named(&self) -> bool {
        matches!(self, Self::Named(_))
    }
}

impl<'a> Deref for RoutedTmux<'a> {
    type Target = dyn ManagedTmuxDriver + 'a;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Default(tmux) => *tmux,
            Self::Named(tmux) => tmux.as_ref(),
        }
    }
}

/// The driver that reaches `record`'s server (#10101).
///
/// Why: see the module doc.
/// What: `tmux` itself for a default-server record; otherwise
/// [`ManagedTmuxDriver::on_server`] for [`record_server`]'s name.
///
/// # Errors
///
/// [`ManagedError::TmuxUnavailable`] when the server cannot be named or
/// `tmux` cannot reach a named server. The default server is never used in
/// its place.
///
/// Test: `stopping_a_named_server_record_never_tears_down_the_default_servers_session`,
/// `a_driver_that_cannot_reach_a_named_server_refuses`.
pub(crate) fn route<'a>(
    tmux: &'a dyn ManagedTmuxDriver,
    record: &SessionRecord,
) -> Result<RoutedTmux<'a>, ManagedError> {
    let Some(server) = record_server(record)? else {
        return Ok(RoutedTmux::Default(tmux));
    };
    tmux.on_server(&server)
        .map(RoutedTmux::Named)
        .ok_or_else(|| {
            ManagedError::TmuxUnavailable(format!(
                "session '{}' is on tmux server '{server}', which this driver cannot reach",
                record.tmux_name
            ))
        })
}

/// The live managed tmux sessions, keyed by (server, session) (#10101).
///
/// What: the default server's names, and each live workspace server's. A
/// record is held only by its own server's set; a server that was not listed
/// holds nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LiveSessions {
    /// Names on the default server.
    pub(crate) default: HashSet<String>,
    /// Names on each named server that answered, by server name.
    pub(crate) named: HashMap<String, HashSet<String>>,
}

/// A set that answers whether a record's session is live (#10101).
///
/// Why: the dedup planners take a name set from older callers and tests, and
/// a [`LiveSessions`] from the live probe; both must apply the server rule.
pub(crate) trait LiveSet {
    /// Whether `record`'s session is live on `record`'s own server.
    fn holds(&self, record: &SessionRecord) -> bool;
}

impl LiveSet for LiveSessions {
    /// What: a listed server holds the names it listed. A server that was not
    /// listed holds nothing, unless the record's socket still accepts a
    /// connection: that server is alive and unlisted, so the session's state is
    /// unknown and reads as live, the direction no caller destroys on. An
    /// unnamable socket is unknown too.
    /// Test: `live_sessions_hold_a_record_only_on_its_own_server`,
    /// `an_unlisted_server_whose_socket_answers_reads_as_live`.
    fn holds(&self, record: &SessionRecord) -> bool {
        match record_server(record) {
            Ok(None) => self.default.contains(&record.tmux_name),
            Ok(Some(server)) => match self.named.get(&server) {
                Some(names) => names.contains(&record.tmux_name),
                None => record
                    .tmux_socket
                    .as_deref()
                    .is_some_and(|socket| std::os::unix::net::UnixStream::connect(socket).is_ok()),
            },
            Err(_) => true,
        }
    }
}

impl LiveSet for HashSet<String> {
    /// A bare name set is the default server's: a named-server record is not
    /// in it.
    fn holds(&self, record: &SessionRecord) -> bool {
        record.tmux_socket.is_none() && self.contains(&record.tmux_name)
    }
}

/// The live managed sessions on `tmux`'s default server and on every live
/// workspace server it lists (#10101).
///
/// What: [`super::dedup::live_managed_names_of`] for the default server, then
/// [`ManagedTmuxDriver::workspace_servers`] and the same managed-name filter
/// for each.
///
/// # Errors
///
/// Any server that cannot be listed, or a listed server `tmux` cannot reach:
/// the whole observation is unknown, never a partial set read as complete.
///
/// Test: `live_sessions_list_every_workspace_server`,
/// `an_unreachable_workspace_server_makes_liveness_unknown`.
pub(crate) fn live_sessions_of(tmux: &dyn ManagedTmuxDriver) -> Result<LiveSessions, ManagedError> {
    let default = super::dedup::live_managed_names_of(tmux)?;
    let mut named = HashMap::new();
    for server in tmux.workspace_servers()? {
        let driver = tmux.on_server(&server).ok_or_else(|| {
            ManagedError::TmuxUnavailable(format!(
                "tmux server '{server}' is live and this driver cannot reach it"
            ))
        })?;
        let names: HashSet<String> = driver
            .list_sessions()?
            .into_iter()
            .filter(|n| crate::core::names::is_managed_session_name(n))
            .collect();
        named.insert(server, names);
    }
    Ok(LiveSessions { default, named })
}

#[cfg(test)]
#[path = "tmux_route_tests.rs"]
mod tests;
