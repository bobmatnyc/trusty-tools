//! The service's tmux source: one read-only `tmux -C` client per session.
//!
//! Why: a pane that prints a prompt or a window that opens should start a
//! cycle within the debounce, not at the next tick (#9779, plan section 2).
//! What: [`ControlClients::reconcile`] lists the sessions and keeps one client
//! thread per session (the poll session excluded). Each thread runs `tmux -C
//! attach-session -f read-only,ignore-size -t =<name>`, keeps its stdin open
//! (control mode exits on EOF) and sends [`Event::Changed`] for each `%`
//! notification, at most once per [`RATE`]. Its first line sends
//! [`Event::Clear`]. When the client ends, the thread checks `has-session`:
//! a session that is gone ends the thread quietly; otherwise it reconnects
//! with doubling backoff and, once the client has been down for
//! [`CONTROL_ALERT_AFTER`], sends [`Event::Fault`] (`tmux-<name>`).
//! `reconcile` reports a thread that ended without its session going (a
//! panic) as died, and starts a new one.
//! Test: `control_output_is_an_event`, `control_down_alerts_then_clears`,
//! `control_client_for_a_gone_session_ends_quietly` (tests/serve_control.rs).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Event;
use crate::tmux::{SystemTmux, Tmux};

/// A client down this long raises its fault.
pub const CONTROL_ALERT_AFTER: Duration = Duration::from_secs(30);
/// The fault kind of a failed session listing.
pub(super) const RECONCILE: &str = "watch-tmux";
/// At most one [`Event::Changed`] per client per this long.
const RATE: Duration = Duration::from_millis(250);
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// The fault kind of one session's client.
pub(super) fn kind(session: &str) -> String {
    format!("tmux-{session}")
}

#[derive(Debug, Clone, Copy)]
struct Timing {
    alert_after: Duration,
    backoff_start: Duration,
    backoff_max: Duration,
}

struct Client {
    stop: Arc<AtomicBool>,
    gone: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    handle: Option<JoinHandle<()>>,
}

impl Client {
    fn halt(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        kill(&self.child);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn kill(slot: &Mutex<Option<Child>>) {
    if let Some(mut child) = slot.lock().unwrap_or_else(PoisonError::into_inner).take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The per-session control clients; see the module docs.
pub struct ControlClients {
    tmux: SystemTmux,
    exclude: Vec<String>,
    tx: Sender<Event>,
    timing: Timing,
    clients: BTreeMap<String, Client>,
}

impl ControlClients {
    /// Clients on `tmux`'s server for every session not in `exclude`,
    /// reporting to `tx`.
    pub fn new(tmux: SystemTmux, exclude: Vec<String>, tx: Sender<Event>) -> Self {
        Self {
            tmux,
            exclude,
            tx,
            timing: Timing {
                alert_after: CONTROL_ALERT_AFTER,
                backoff_start: BACKOFF_START,
                backoff_max: BACKOFF_MAX,
            },
            clients: BTreeMap::new(),
        }
    }

    /// Shorter timings, for tests.
    pub fn with_timing(mut self, alert_after: Duration, backoff: Duration) -> Self {
        self.timing = Timing {
            alert_after,
            backoff_start: backoff,
            backoff_max: backoff,
        };
        self
    }

    /// Sessions with a running client.
    pub fn sessions(&self) -> Vec<String> {
        self.clients.keys().cloned().collect()
    }

    /// Starts a client per new session, stops those whose session is gone;
    /// returns the sessions whose thread had died.
    ///
    /// # Errors
    /// `list-sessions` failed (the text is one line).
    pub fn reconcile(&mut self) -> Result<Vec<String>, String> {
        let out = self
            .tmux
            .run(&["list-sessions", "-F", "#{session_name}"])
            .map_err(|e| e.to_string())?;
        let names: BTreeSet<String> = out
            .lines()
            .map(str::trim)
            .filter(|n| !n.is_empty() && !self.exclude.iter().any(|x| x == n))
            .map(str::to_owned)
            .collect();
        let mut died = Vec::new();
        let ended: Vec<String> = self
            .clients
            .iter()
            .filter(|(n, c)| {
                !names.contains(*n) || c.handle.as_ref().is_none_or(|h| h.is_finished())
            })
            .map(|(n, _)| n.clone())
            .collect();
        for name in ended {
            if let Some(mut c) = self.clients.remove(&name) {
                let finished = c.handle.as_ref().is_none_or(|h| h.is_finished());
                if finished && !c.gone.load(Ordering::SeqCst) && names.contains(&name) {
                    died.push(name.clone());
                }
                c.halt();
            }
        }
        for name in names {
            if !self.clients.contains_key(&name) {
                let client = self.spawn(&name);
                self.clients.insert(name, client);
            }
        }
        Ok(died)
    }

    /// Stops every client and waits for its thread.
    pub fn shutdown(&mut self) {
        for (_, mut c) in std::mem::take(&mut self.clients) {
            c.halt();
        }
    }

    fn spawn(&self, name: &str) -> Client {
        let stop = Arc::new(AtomicBool::new(false));
        let gone = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let worker = Worker {
            name: name.to_owned(),
            tmux: self.tmux.clone(),
            tx: self.tx.clone(),
            stop: Arc::clone(&stop),
            gone: Arc::clone(&gone),
            child: Arc::clone(&child),
            timing: self.timing,
        };
        let handle = std::thread::Builder::new()
            .name(format!("tmux-control-{name}"))
            .spawn(move || worker.run())
            .ok();
        Client {
            stop,
            gone,
            child,
            handle,
        }
    }
}

impl Drop for ControlClients {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Worker {
    name: String,
    tmux: SystemTmux,
    tx: Sender<Event>,
    stop: Arc<AtomicBool>,
    gone: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    timing: Timing,
}

impl Worker {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn run(self) {
        let kind = kind(&self.name);
        let mut down_since: Option<Instant> = None;
        let mut backoff = self.timing.backoff_start;
        let mut alerted = false;
        while !self.stopped() {
            let result = self.attach(&kind);
            if self.stopped() {
                break;
            }
            if result.connected {
                down_since = None;
                backoff = self.timing.backoff_start;
                alerted = false;
            }
            let target = format!("={}", self.name);
            if self.tmux.run(&["has-session", "-t", &target]).is_err() {
                // The session ended: not a fault, and nothing to watch.
                self.gone.store(true, Ordering::SeqCst);
                let _ = self.tx.send(Event::Clear(kind));
                return;
            }
            let since = *down_since.get_or_insert_with(Instant::now);
            if !alerted && since.elapsed() >= self.timing.alert_after {
                let detail = format!(
                    "tmux -C for {} down {}s ({})",
                    self.name,
                    since.elapsed().as_secs(),
                    result.why
                );
                alerted = self
                    .tx
                    .send(Event::Fault {
                        kind: kind.clone(),
                        detail,
                    })
                    .is_ok();
            }
            self.pause(backoff);
            backoff = (backoff * 2).min(self.timing.backoff_max);
        }
    }

    /// One client's life: spawn, read until EOF, reap.
    fn attach(&self, kind: &str) -> Attach {
        let target = format!("={}", self.name);
        let args = [
            "-C",
            "attach-session",
            "-f",
            "read-only,ignore-size",
            "-t",
            &target,
        ];
        let mut cmd = self.tmux.command(&args);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return Attach::down(format!("spawn: {e}")),
        };
        let out = child.stdout.take();
        // Control mode ends when its stdin closes; hold it until EOF.
        let stdin = child.stdin.take();
        *self.child.lock().unwrap_or_else(PoisonError::into_inner) = Some(child);
        if self.stopped() {
            kill(&self.child);
        }
        let mut connected = false;
        let mut last: Option<Instant> = None;
        for line in out.into_iter().flat_map(|o| BufReader::new(o).lines()) {
            let Ok(line) = line else { break };
            if !connected {
                connected = true;
                let _ = self.tx.send(Event::Clear(kind.to_owned()));
            }
            if notifies(&line) && last.is_none_or(|t| t.elapsed() >= RATE) {
                last = Some(Instant::now());
                let _ = self.tx.send(Event::Changed(format!("tmux:{}", self.name)));
            }
        }
        drop(stdin);
        kill(&self.child);
        Attach {
            connected,
            why: "client exited".to_owned(),
        }
    }

    /// Sleeps `d` in short slices so a stop is seen promptly.
    fn pause(&self, d: Duration) {
        let end = Instant::now() + d;
        while !self.stopped() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50).min(end - Instant::now()));
        }
    }
}

struct Attach {
    connected: bool,
    why: String,
}

impl Attach {
    fn down(why: String) -> Self {
        Self {
            connected: false,
            why,
        }
    }
}

/// A control-mode notification that may change what a cycle sees.
fn notifies(line: &str) -> bool {
    let word = line.split_whitespace().next().unwrap_or("");
    word.starts_with('%') && !matches!(word, "%begin" | "%end" | "%error" | "%exit")
}
