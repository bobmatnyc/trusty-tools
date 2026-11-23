//! The daemon side: bind, accept, answer, exit when idle (#11650).
//!
//! Why/What: see the parent module. Every wait in this file is on an event:
//! an accepted connection, a connection ending, the spawner's release, a
//! shutdown signal, a client's EOF. Nothing sleeps and nothing polls.
//! Test: `super::tests`.

use std::future::Future;
use std::os::fd::AsFd as _;
use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify, watch};
use trusty_common::uds::{UdsSecurityError, bind_singleton_hardened, peer_uid};

use super::{
    AskDaemonError, AskReply, AskRequest, MAX_LINE_BYTES, OwnedSocketPath, PROTOCOL_VERSION,
    ReplyErrorKind, ReplyOutcome,
};
use crate::ask::{AskEngine, AskError};

/// Builds the engine for one ask. A fresh engine per ask gives each ask its
/// own adapter state; the turn log carries the context.
pub type EngineFactory = Arc<dyn Fn() -> Result<AskEngine, AskError> + Send + Sync>;

/// The outcome of [`bind`].
#[derive(Debug)]
pub enum Bound {
    /// This process owns the socket.
    Serving(BoundDaemon),
    /// Another daemon serves it, or is binding it right now.
    Busy,
}

/// A bound, not yet serving, daemon socket.
#[derive(Debug)]
pub struct BoundDaemon {
    listener: UnixListener,
    /// #11650: a second handle on the listening socket. Its `accept` asks the
    /// kernel directly, so it sees a queued connection the runtime has not
    /// reported yet.
    backlog: StdUnixListener,
    socket: OwnedSocketPath,
}

/// What [`BoundDaemon::serve`] needs besides its socket.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// The #11550 `agents.health` socket; `None` serves none.
    pub health_socket: Option<PathBuf>,
    /// The uid a peer must have. Production passes this process's uid.
    pub own_uid: u32,
}

/// Why [`BoundDaemon::serve`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeExit {
    /// Released, with no connection open and no ask in flight.
    Idle,
    /// The shutdown future resolved; in-flight asks were cancelled.
    Shutdown,
}

/// Counts from one serve, for the log and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeSummary {
    pub exit: ServeExit,
    /// Connections that passed the peer check.
    pub accepted: usize,
    /// Connections refused by the peer check.
    pub refused: usize,
    /// Asks that returned an answer.
    pub answered: usize,
    /// Asks that returned an error to a connected client.
    pub failed: usize,
    /// Asks whose client left first; each was cancelled.
    pub abandoned: usize,
}

/// Bind `socket` as this instance's only daemon.
///
/// Why: requirement 2 — one daemon per instance. `bind_singleton_hardened`
/// takes over a socket file nobody serves, refuses a live one, and serialises
/// racing binders on a lock beside the socket; the socket is published at
/// 0600 inside a 0700 directory it owns (requirement 3).
/// What: `Busy` when another process serves or is binding the path; any other
/// failure is [`AskDaemonError::Bind`]. There is no unhardened fallback. A
/// failure to duplicate the listener for the backlog check is an I/O error:
/// a daemon that cannot see its backlog does not serve.
/// Test: `super::tests::a_second_daemon_cannot_bind_a_served_socket`,
/// `super::tests::racing_binds_end_with_exactly_one_daemon`,
/// `super::tests::a_stale_socket_file_is_replaced`,
/// `super::tests::the_socket_is_0600_in_a_0700_directory`.
pub async fn bind(socket: &Path) -> Result<Bound, AskDaemonError> {
    let bind_error = |source| AskDaemonError::Bind {
        socket: socket.to_path_buf(),
        source,
    };
    let listener = match bind_singleton_hardened(socket).await {
        Ok(listener) => listener,
        Err(UdsSecurityError::AlreadyServing { .. } | UdsSecurityError::BindInProgress { .. }) => {
            return Ok(Bound::Busy);
        }
        Err(e) => return Err(bind_error(e)),
    };
    let backlog = listener
        .as_fd()
        .try_clone_to_owned()
        .map(StdUnixListener::from)
        .and_then(|backlog| backlog.set_nonblocking(true).map(|()| backlog))
        .map_err(AskDaemonError::io("duplicate the listener"))?;
    let socket =
        OwnedSocketPath::capture(socket).map_err(AskDaemonError::io("stat the bound socket"))?;
    Ok(Bound::Serving(BoundDaemon {
        listener,
        backlog,
        socket,
    }))
}

/// Open connections, plus whether the spawner has let go.
#[derive(Debug, Default)]
struct Activity {
    open: AtomicUsize,
    released: AtomicBool,
    wake: Notify,
}

impl Activity {
    fn idle(&self) -> bool {
        self.released.load(Ordering::SeqCst) && self.open.load(Ordering::SeqCst) == 0
    }

    fn open(self: &Arc<Self>) -> ConnGuard {
        self.open.fetch_add(1, Ordering::SeqCst);
        ConnGuard(Arc::clone(self))
    }

    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Wait until no connection is open.
    async fn drained(&self) {
        while self.open.load(Ordering::SeqCst) != 0 {
            self.wake.notified().await;
        }
    }
}

/// Holds one connection open in [`Activity`]; `Drop` closes it, panic or not.
struct ConnGuard(Arc<Activity>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
        // `notify_one` keeps a permit when nobody waits, so no wake is lost.
        self.0.wake.notify_one();
    }
}

/// State every connection handler shares.
struct Shared {
    factory: EngineFactory,
    /// One ask at a time per instance: the turn log has one writer.
    ask_lock: Mutex<()>,
    stop: watch::Receiver<bool>,
    counts: std::sync::Mutex<ServeSummary>,
}

impl Shared {
    fn count(&self, f: impl FnOnce(&mut ServeSummary)) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut counts);
    }
}

impl BoundDaemon {
    /// The socket this daemon serves.
    pub fn socket(&self) -> &Path {
        self.socket.path()
    }

    /// Serve asks until idle or shut down, then remove the socket.
    ///
    /// Why: requirements 1, 3, 4 and 5 in one loop.
    /// What: each accepted connection must come from `options.own_uid`, or it
    /// is dropped unread. Asks run one at a time through `factory`'s engine
    /// and `AskEngine::ask_with_cancel`; a client EOF sets that ask's cancel
    /// flag. `lease` resolving marks the spawner's release. The loop ends when
    /// released with nothing open and nothing queued in the listen backlog
    /// ([`accept_queued`]), or when `shutdown` resolves (which cancels
    /// every in-flight ask). On the way out it unlinks the socket first, so no
    /// new client can reach it, accepts any connection already queued, waits
    /// for every handler, and stops the health socket.
    /// Test: `super::tests::an_ask_round_trips_and_the_daemon_exits_idle`,
    /// `super::tests::the_daemon_stays_while_a_connection_is_open`,
    /// `super::tests::a_connection_queued_before_the_release_keeps_the_daemon`,
    /// `super::tests::a_client_disconnect_cancels_its_ask`,
    /// `super::tests::a_connection_from_another_uid_is_refused`,
    /// `super::tests::shutdown_removes_the_socket`,
    /// `super::tests::the_health_socket_is_served_and_removed`.
    pub async fn serve(
        self,
        options: ServeOptions,
        factory: EngineFactory,
        lease: impl Future<Output = ()>,
        shutdown: impl Future<Output = ()>,
    ) -> ServeSummary {
        let (stop_tx, stop_rx) = watch::channel(false);
        let health = options
            .health_socket
            .clone()
            .map(|path| spawn_health(path, stop_rx.clone()));
        let activity = Arc::new(Activity::default());
        let shared = Arc::new(Shared {
            factory,
            ask_lock: Mutex::new(()),
            stop: stop_rx,
            counts: std::sync::Mutex::new(ServeSummary {
                exit: ServeExit::Idle,
                accepted: 0,
                refused: 0,
                answered: 0,
                failed: 0,
                abandoned: 0,
            }),
        });
        let accept = |stream: UnixStream| admit(stream, options.own_uid, &activity, &shared);
        let Self {
            listener,
            backlog,
            socket,
        } = self;

        tokio::pin!(lease, shutdown);
        let mut leased = true;
        let exit = loop {
            if activity.idle() {
                // #11650: idle only when the kernel backlog is empty too. The
                // runtime may not have reported a connection whose `connect()`
                // already returned, and its client may have released us since.
                // Fail-Open Check: an accept error ends the daemon (the drain
                // below still tries the queue); it never loops or reads as
                // "nothing queued" silently.
                match accept_queued(&backlog) {
                    Ok(None) => break ServeExit::Idle,
                    Ok(Some(stream)) => {
                        admit_queued(stream, &accept);
                        continue;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "ask daemon: backlog check failed; exiting");
                        break ServeExit::Idle;
                    }
                }
            }
            tokio::select! {
                () = &mut shutdown => break ServeExit::Shutdown,
                () = &mut lease, if leased => {
                    leased = false;
                    activity.release();
                }
                () = activity.wake.notified() => {}
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => accept(stream),
                    Err(e) => {
                        tracing::error!(error = %e, "ask daemon: accept failed; shutting down");
                        break ServeExit::Shutdown;
                    }
                },
            }
        };
        if exit == ServeExit::Shutdown {
            let _ = stop_tx.send(true);
        }
        // #11650: unlink first, so a new client starts a new daemon instead of
        // queueing on a listener that is about to close.
        socket.remove();
        drop(listener);
        drain_backlog(&backlog, exit, &accept);
        // Closing the last handle refuses any later connect.
        drop(backlog);
        activity.drained().await;
        let _ = stop_tx.send(true);
        if let Some(health) = health
            && let Err(e) = health.await
        {
            tracing::warn!(error = %e, "agents.health task ended abnormally");
        }
        let mut summary = shared
            .counts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        summary.exit = exit;
        summary
    }
}

/// Accept every connection already queued on the unlinked socket.
///
/// Why: a client that connected just before the unlink sits in the backlog;
/// dropping it unanswered would turn a race into a failed ask.
/// What: [`accept_queued`] until the queue is empty, so this never waits.
/// After a shutdown each queued connection is dropped unread.
fn drain_backlog(listener: &StdUnixListener, exit: ServeExit, accept: &impl Fn(UnixStream)) {
    loop {
        match accept_queued(listener) {
            Ok(Some(_)) if exit == ServeExit::Shutdown => {} // the client sees EOF
            Ok(Some(stream)) => admit_queued(stream, accept),
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "ask daemon: queued connections not drained");
                return;
            }
        }
    }
}

/// Take one connection from the kernel's listen backlog, without waiting.
///
/// Why: #11650 — the idle decision must see every connection whose
/// `connect()` has returned. The runtime's accept only reports one after its
/// reactor has run, so a queued connection could be missed.
/// What: `Ok(None)` when the queue is empty (`WouldBlock`); an interrupted
/// call or a connection its client reset first is skipped; any other error
/// is returned.
/// Test: `super::tests::a_connection_queued_before_the_release_keeps_the_daemon`.
fn accept_queued(listener: &StdUnixListener) -> std::io::Result<Option<StdUnixStream>> {
    use std::io::ErrorKind;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(Some(stream)),
            Err(e) if e.kind() == ErrorKind::WouldBlock => return Ok(None),
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::Interrupted | ErrorKind::ConnectionAborted
                ) => {}
            Err(e) => return Err(e),
        }
    }
}

/// Hand a connection taken by [`accept_queued`] to `accept`, on the runtime.
fn admit_queued(stream: StdUnixStream, accept: &impl Fn(UnixStream)) {
    match stream
        .set_nonblocking(true)
        .and_then(|()| UnixStream::from_std(stream))
    {
        Ok(stream) => accept(stream),
        Err(e) => tracing::warn!(error = %e, "ask daemon: queued connection dropped"),
    }
}

/// Run the peer check, then hand the connection to a handler task.
///
/// Why: requirement 3 — a connection from another uid is dropped before a
/// byte is read, and never counts as activity.
fn admit(stream: UnixStream, own_uid: u32, activity: &Arc<Activity>, shared: &Arc<Shared>) {
    match peer_uid(&stream) {
        Ok(uid) if uid == own_uid => {}
        verdict => {
            tracing::warn!(
                ?verdict,
                own_uid,
                "ask daemon: refused a connection from another uid"
            );
            shared.count(|c| c.refused += 1);
            return;
        }
    }
    shared.count(|c| c.accepted += 1);
    // The guard is taken here, before the spawn, so the loop's next idle
    // check already sees this connection.
    let guard = activity.open();
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        let _guard = guard;
        handle(stream, &shared).await;
    });
}

/// Serve one connection: read the request, run the ask, write the reply.
async fn handle(stream: UnixStream, shared: &Shared) {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    let mut stop = shared.stop.clone();
    let mut line = String::new();
    {
        let mut limited = (&mut read).take(MAX_LINE_BYTES);
        tokio::select! {
            r = limited.read_line(&mut line) => match r {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            },
            () = stopped(&mut stop) => return,
        }
    }
    let question = match parse_request(&line) {
        Ok(question) => question,
        Err((kind, message)) => {
            write_reply(&mut write, ReplyOutcome::Error { kind, message }).await;
            return;
        }
    };

    let cancel = Arc::new(AtomicBool::new(false));
    let work = run_ask(shared, &question, Arc::clone(&cancel));
    tokio::pin!(work);
    let finished = tokio::select! {
        result = &mut work => Some(result),
        () = client_gone(&mut read, &mut stop) => None,
    };
    let Some(result) = finished else {
        // #11650: the client left (or the daemon is stopping): cancel this ask
        // and let it stop at its next turn boundary.
        cancel.store(true, Ordering::SeqCst);
        let result = work.await;
        tracing::info!(ended = ?result.as_ref().map(|_| "answered"), "ask daemon: client left mid-ask");
        shared.count(|c| c.abandoned += 1);
        return;
    };
    let outcome = match result {
        Ok(answer) => {
            shared.count(|c| c.answered += 1);
            ReplyOutcome::Answer { answer }
        }
        Err(e) => {
            shared.count(|c| c.failed += 1);
            ReplyOutcome::Error {
                kind: ReplyErrorKind::Ask,
                message: e.to_string(),
            }
        }
    };
    write_reply(&mut write, outcome).await;
}

/// The question in `line`, or the reply-error to send instead.
fn parse_request(line: &str) -> Result<String, (ReplyErrorKind, String)> {
    let bad = |m: String| (ReplyErrorKind::BadRequest, m);
    if !line.ends_with('\n') {
        return Err(bad(format!(
            "the request is not one line of at most {MAX_LINE_BYTES} bytes"
        )));
    }
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| bad(format!("not JSON: {e}")))?;
    let theirs = value.get("protocol").and_then(serde_json::Value::as_u64);
    if theirs != Some(u64::from(PROTOCOL_VERSION)) {
        let theirs = theirs.map_or_else(|| "none".to_string(), |v| v.to_string());
        return Err((
            ReplyErrorKind::ProtocolMismatch,
            format!("the client speaks protocol {theirs}, this daemon speaks {PROTOCOL_VERSION}"),
        ));
    }
    let request: AskRequest =
        serde_json::from_value(value).map_err(|e| bad(format!("not an ask request: {e}")))?;
    Ok(request.question)
}

/// One ask, after any ask ahead of it.
async fn run_ask(
    shared: &Shared,
    question: &str,
    cancel: Arc<AtomicBool>,
) -> Result<String, AskError> {
    let _turn = shared.ask_lock.lock().await;
    // A client that left while queued never reaches the model.
    if cancel.load(Ordering::SeqCst) {
        return Err(AskError::Cancelled);
    }
    let engine = (shared.factory)()?;
    engine.ask_with_cancel(question, cancel).await
}

/// Resolve when the client closes its end, or when the daemon stops.
///
/// Why: requirement 1 — a client disconnect cancels its ask. The client
/// keeps its write half open, so a read returning EOF (or an error) is the
/// kernel telling us it is gone; no polling is involved.
async fn client_gone(read: &mut BufReader<OwnedReadHalf>, stop: &mut watch::Receiver<bool>) {
    let mut scratch = [0u8; 256];
    loop {
        tokio::select! {
            r = read.read(&mut scratch) => match r {
                Ok(0) | Err(_) => return,
                Ok(_) => {} // bytes after the request are ignored
            },
            () = stopped(stop) => return,
        }
    }
}

/// Resolve once the daemon is stopping.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopping| *stopping).await;
}

/// Write `outcome` as this daemon's reply; a client already gone is not an error.
async fn write_reply(write: &mut OwnedWriteHalf, outcome: ReplyOutcome) {
    let reply = AskReply {
        protocol: PROTOCOL_VERSION,
        daemon_pid: std::process::id(),
        outcome,
    };
    let mut line = match serde_json::to_string(&reply) {
        Ok(line) => line,
        Err(e) => {
            tracing::error!(error = %e, "ask daemon: reply not serialisable");
            return;
        }
    };
    line.push('\n');
    if let Err(e) = write.write_all(line.as_bytes()).await {
        tracing::info!(error = %e, "ask daemon: client left before the reply");
    }
}

/// Serve `agents.health` on `path` until the daemon stops (#11550).
///
/// What: a bind or serve failure is logged and leaves the ask socket alone;
/// the health socket is a status surface, not part of answering.
fn spawn_health(path: PathBuf, mut stop: watch::Receiver<bool>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let shutdown = async move { stopped(&mut stop).await };
        if let Err(e) = crate::operations::health_socket::serve_at(&path, shutdown).await {
            tracing::warn!(socket = %path.display(), error = %e, "agents.health not served");
        }
    })
}
