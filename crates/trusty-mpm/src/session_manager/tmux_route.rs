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
use std::path::{Path, PathBuf};
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
    // #10101: `-L` resolves `server` in the driver's socket directory, so a
    // record whose socket lives elsewhere (`TMUX_TMPDIR` changed) is refused
    // rather than re-aimed at a same-named server there.
    if !in_socket_dir(record, tmux.socket_dir().as_deref()) {
        return Err(ManagedError::TmuxUnavailable(format!(
            "session '{}' records tmux socket {} outside this process's tmux \
             socket directory; its server cannot be reached by name",
            record.tmux_name,
            record
                .tmux_socket
                .as_deref()
                .unwrap_or(Path::new(""))
                .display()
        )));
    }
    tmux.on_server(&server)
        .map(RoutedTmux::Named)
        .ok_or_else(|| {
            ManagedError::TmuxUnavailable(format!(
                "session '{}' is on tmux server '{server}', which this driver cannot reach",
                record.tmux_name
            ))
        })
}

/// [`route`] as an owned driver, for a caller that hands the driver on (a
/// runtime adapter, a background task) (#10101).
///
/// # Errors
///
/// As [`route`].
///
/// Test: `a_fresh_launch_creates_its_session_on_the_workspace_server`.
pub(crate) fn route_arc(
    tmux: &Arc<dyn ManagedTmuxDriver>,
    record: &SessionRecord,
) -> Result<Arc<dyn ManagedTmuxDriver>, ManagedError> {
    Ok(match route(tmux.as_ref(), record)? {
        RoutedTmux::Default(_) => Arc::clone(tmux),
        RoutedTmux::Named(named) => named,
    })
}

/// Whether `record`'s session exists on its own server; `false` when the
/// server cannot be reached (#10101).
/// Test: `a_fresh_launch_creates_its_session_on_the_workspace_server`.
pub(crate) fn session_exists_on_own_server(
    tmux: &dyn ManagedTmuxDriver,
    record: &SessionRecord,
) -> bool {
    route(tmux, record).is_ok_and(|t| t.session_exists(&record.tmux_name))
}

/// Whether `record`'s socket sits in `socket_dir`; `None` checks nothing
/// (a driver with no socket directory, i.e. a test fake).
fn in_socket_dir(record: &SessionRecord, socket_dir: Option<&Path>) -> bool {
    match (socket_dir, record.tmux_socket.as_deref()) {
        (Some(dir), Some(socket)) => socket.parent() == Some(dir),
        _ => true,
    }
}

/// The live managed tmux sessions, keyed by (server, session) (#10101).
///
/// What: the default server's names, and each live workspace server's. A
/// record is held only by its own server's set. A server that was live but
/// could not be listed in time is `unknown`: its records read as live.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LiveSessions {
    /// Names on the default server.
    pub(crate) default: HashSet<String>,
    /// Names on each named server that answered, by server name.
    pub(crate) named: HashMap<String, HashSet<String>>,
    /// Live named servers that could not be listed: their sessions' state is
    /// unknown.
    pub(crate) unknown: HashSet<String>,
    /// The workspace server directory could not be read: every named
    /// record's state is unknown.
    pub(crate) named_unlisted: bool,
    /// Where the named servers were listed; `None` checks no record's socket
    /// directory.
    pub(crate) socket_dir: Option<PathBuf>,
}

/// A set that answers whether a record's session is live (#10101).
///
/// Why: the dedup planners take a name set from older callers and tests, and
/// a [`LiveSessions`] from the live probe; both must apply the server rule.
pub(crate) trait LiveSet {
    /// Whether `record`'s session is live on `record`'s own server.
    fn holds(&self, record: &SessionRecord) -> bool;

    /// Whether `record`'s whole server is gone (#6194: a lost server is not a
    /// decision about any one session).
    fn server_lost(&self, record: &SessionRecord) -> bool;
}

impl LiveSet for LiveSessions {
    /// What: a listed server holds the names it listed. An `unknown` server,
    /// an unreadable server directory, a socket outside the listed directory
    /// and an unnamable socket all read as live, the direction no caller
    /// destroys on, unless the socket refuses a connection (no server). A
    /// server that was not listed holds nothing, unless its socket answers.
    /// Test: `live_sessions_hold_a_record_only_on_its_own_server`,
    /// `an_unlisted_server_whose_socket_answers_reads_as_live`,
    /// `one_unlistable_server_degrades_only_its_own_records`,
    /// `a_socket_outside_the_socket_dir_is_refused_and_reads_unknown`.
    fn holds(&self, record: &SessionRecord) -> bool {
        let server = match record_server(record) {
            Ok(None) => return self.default.contains(&record.tmux_name),
            Ok(Some(server)) => server,
            Err(_) => return true,
        };
        if self.named_unlisted || self.unknown.contains(&server) {
            return true;
        }
        if in_socket_dir(record, self.socket_dir.as_deref())
            && let Some(names) = self.named.get(&server)
        {
            return names.contains(&record.tmux_name);
        }
        // Not listed here: alive (or stuck) only if its own socket says so.
        record.tmux_socket.as_deref().is_some_and(|socket| {
            crate::core::tmux_server::socket_answers(socket, crate::core::tmux_server::PROBE_BUDGET)
                != Some(false)
        })
    }

    /// What: the default server is lost when it lists nothing; a named server
    /// when it was listed with nothing, or not listed and not unknown.
    /// Test: `the_reaper_never_stops_a_live_pm_on_a_workspace_server`.
    fn server_lost(&self, record: &SessionRecord) -> bool {
        match record_server(record) {
            Ok(None) => self.default.is_empty(),
            Ok(Some(server)) => {
                !self.named_unlisted
                    && !self.unknown.contains(&server)
                    && self.named.get(&server).is_none_or(HashSet::is_empty)
            }
            Err(_) => false,
        }
    }
}

impl LiveSet for HashSet<String> {
    /// A bare name set is the default server's: a named-server record is not
    /// in it.
    fn holds(&self, record: &SessionRecord) -> bool {
        record.tmux_socket.is_none() && self.contains(&record.tmux_name)
    }

    /// An empty bare set is a lost default server.
    fn server_lost(&self, _record: &SessionRecord) -> bool {
        self.is_empty()
    }
}

/// The live managed sessions on `tmux`'s default server and on every live
/// workspace server it lists (#10101).
///
/// What: [`super::dedup::live_managed_names_of`] for the default server, then
/// [`observe_workspace_servers`].
///
/// # Errors
///
/// The default server cannot be observed. A workspace server that cannot be
/// observed degrades only its own records (see [`LiveSessions::unknown`]).
///
/// Test: `live_sessions_list_every_workspace_server`,
/// `one_unlistable_server_degrades_only_its_own_records`.
pub(crate) fn live_sessions_of(tmux: &dyn ManagedTmuxDriver) -> Result<LiveSessions, ManagedError> {
    let default = super::dedup::live_managed_names_of(tmux)?;
    Ok(observe_workspace_servers(tmux, default))
}

/// [`LiveSessions`] with `default` as the default server's names and every
/// live workspace server `tmux` lists, each listed within
/// [`crate::core::tmux_server::PROBE_BUDGET`] (#10101).
///
/// What: an unreadable server directory sets `named_unlisted`; a server `tmux`
/// cannot reach, whose listing fails, or that does not answer in time is
/// `unknown`. A listing that outlives the budget keeps its thread until tmux
/// returns.
/// Test: `one_unlistable_server_degrades_only_its_own_records`,
/// `a_stuck_workspace_server_is_bounded_and_unknown`.
pub(crate) fn observe_workspace_servers(
    tmux: &dyn ManagedTmuxDriver,
    default: HashSet<String>,
) -> LiveSessions {
    let mut live = LiveSessions {
        default,
        socket_dir: tmux.socket_dir(),
        ..LiveSessions::default()
    };
    let servers = match tmux.workspace_servers() {
        Ok(servers) => servers,
        Err(e) => {
            tracing::warn!("tmux workspace servers unknown: {e}");
            live.named_unlisted = true;
            return live;
        }
    };
    for server in servers {
        match bounded_list(tmux.on_server(&server)) {
            Some(names) => {
                live.named.insert(server, names);
            }
            None => {
                tracing::warn!(
                    "tmux server '{server}' could not be listed; its sessions are unknown"
                );
                live.unknown.insert(server);
            }
        }
    }
    live
}

/// The managed names `driver` lists within the probe budget; `None` on no
/// driver, an error, or expiry.
fn bounded_list(driver: Option<Arc<dyn ManagedTmuxDriver>>) -> Option<HashSet<String>> {
    let driver = driver?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(driver.list_sessions());
    });
    let names = rx
        .recv_timeout(crate::core::tmux_server::PROBE_BUDGET)
        .ok()?
        .ok()?;
    Some(
        names
            .into_iter()
            .filter(|n| crate::core::names::is_managed_session_name(n))
            .collect(),
    )
}

#[cfg(test)]
#[path = "tmux_route_tests.rs"]
mod tests;
