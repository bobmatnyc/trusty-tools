//! The broker's sockets: `agent.sock` and `operator.sock`, 0600 in a 0700 state dir.
//!
//! Why: the sockets are the only way in, so each is private, each peer is
//! uid-checked, and one broker owns a state dir (plan section 4; spike
//! `broker.py:190-226`).
//! What: [`BrokerServer::bind`] makes the state dir private, takes an
//! exclusive `flock` on `service.lock` (a second broker is refused), and
//! binds both sockets and chmods them 0600 inside the 0700 dir. [`serve_connection`] refuses a peer
//! whose uid is not `expected_uid` with `foreign_uid` plus an audit line,
//! then answers newline-delimited JSON, one reply per line, until EOF; EOF
//! releases the session's lease.
//! Test: `broker_socket_is_private`, `second_broker_on_a_state_dir_is_refused`,
//! `foreign_or_unreadable_peer_is_refused`, `foreign_uid_connection_is_refused_and_audited`.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use serde_json::{Value, json};

use crate::broker::{Broker, Ctx, base};
use crate::launcher::Launcher;
use crate::{peer, private};

/// The bound sockets and the state-dir lock (held while this lives).
pub struct BrokerServer {
    pub agent: PathBuf,
    pub operator: PathBuf,
    listeners: Vec<(UnixListener, bool)>,
    _lock: File,
}

fn bind_private(path: &Path) -> io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    // No umask change: it is process-global and races other threads' mkdir.
    // The 0700 state dir keeps other users out until the chmod below.
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

impl BrokerServer {
    /// Bind `<state>/agent.sock` and `<state>/operator.sock`.
    ///
    /// Postcondition on Ok: `state` is 0700, both sockets 0600, and no other
    /// broker holds `service.lock`.
    pub fn bind(state: &Path) -> io::Result<BrokerServer> {
        private::ensure_private_dir(state)?;
        let lock = private::open_nofollow(&state.join("service.lock"), false)?;
        // SAFETY: flock on an fd we own.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "another broker holds this state dir",
            ));
        }
        let agent = state.join("agent.sock");
        let operator = state.join("operator.sock");
        let listeners = vec![
            (bind_private(&agent)?, false),
            (bind_private(&operator)?, true),
        ];
        Ok(BrokerServer {
            agent,
            operator,
            listeners,
            _lock: lock,
        })
    }

    /// Accept on both sockets, one thread per connection. Returns at once.
    pub fn spawn<L: Launcher + Send + 'static>(&mut self, broker: Arc<Broker<L>>) {
        for (listener, operator) in self.listeners.drain(..) {
            let broker = Arc::clone(&broker);
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let broker = Arc::clone(&broker);
                    thread::spawn(move || {
                        serve_connection(&broker, stream, private::uid(), operator)
                    });
                }
            });
        }
    }
}

/// Serve one connection whose peer must be `expected_uid`.
///
/// Why: the uid check runs before any byte is read, on every connection.
/// What: a refused peer gets one `{"ok": false, "code": ...}` line and an
/// audit line, then the stream closes. An admitted peer gets a session.
pub fn serve_connection<L: Launcher>(
    broker: &Broker<L>,
    stream: UnixStream,
    expected_uid: u32,
    operator: bool,
) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let uid = match peer::admit(peer::peer_uid(&stream), expected_uid) {
        Ok(uid) => uid,
        Err(refused) => {
            let ctx = Ctx {
                session: "none".into(),
                uid: peer::peer_uid(&stream).unwrap_or(u32::MAX),
                operator,
            };
            let mut rec = base(&ctx, "accept");
            rec.insert("refused".into(), json!(refused.code));
            let reply = broker
                .lock()
                .audited(rec, json!({"ok": false, "code": refused.code}));
            let _ = writeln!(writer, "{reply}");
            return;
        }
    };
    let ctx = Ctx {
        session: broker.open_session(),
        uid,
        operator,
    };
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let request = serde_json::from_str::<Value>(&line).ok();
        let reply = broker.handle(request.as_ref(), &ctx);
        if writeln!(writer, "{reply}")
            .and_then(|()| writer.flush())
            .is_err()
        {
            break;
        }
    }
    broker.close_session(&ctx);
}
