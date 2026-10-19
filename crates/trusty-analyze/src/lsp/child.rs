//! One language-server child process over stdio (#9078).
//!
//! Why: a leaked `rust-analyzer` is a 1.7 GB orphan, and it spawns helpers of
//! its own (DOC-72 §7). The child must die on every exit path, descendants
//! included, and a server that cannot start must say why.
//! What: [`LspChild::spawn`] starts the program in its own process group with
//! stdin/stdout piped to an [`LspClient`] and stderr drained into a bounded
//! tail, then runs `initialize`/`initialized`. A monitor task owns the process
//! and reaps it the moment it exits, so a crash leaves no zombie and its exit
//! status is known. Every kill is a `killpg(SIGKILL)` on the group;
//! [`LspChild::shutdown`] tries `shutdown`/`exit` first, and `Drop` kills.
//! Test: `spawned_server_initializes_and_answers`,
//! `failed_initialize_carries_the_stderr_tail`,
//! `dropping_the_manager_kills_the_child_and_its_descendants`,
//! `shutdown_all_kills_a_child_that_ignores_exit`,
//! `a_panicking_owner_still_kills_the_child`.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr};
use tokio::sync::{watch, Notify};
use tokio::task::JoinHandle;

use super::client::LspClient;
use super::config::{LspCommand, LspConfig};
use super::documents::file_uri;
use super::{lock, LspError};

const STDERR_TAIL_LINES: usize = 40;
const STDERR_LINE_CHARS: usize = 512;
/// How long a kill may take to be observed as an exit.
const REAP_BUDGET: Duration = Duration::from_secs(5);

/// A running language server.
///
/// Why/What: see the module doc.
/// Test: `spawned_server_initializes_and_answers`.
pub struct LspChild {
    client: LspClient,
    pid: u32,
    program: String,
    /// `Some(exit status)` once the monitor has reaped the process.
    exit: watch::Receiver<Option<String>>,
    kill: Arc<Notify>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    stderr_task: Mutex<Option<JoinHandle<()>>>,
    /// Non-zero once killed as hung: the timeout count that condemned it.
    hung: AtomicU32,
    /// Set once `shutdown` has swept the group; `Drop` then has nothing to do.
    finished: AtomicBool,
}

impl LspChild {
    /// Spawn `command` in `root` and complete the LSP handshake.
    ///
    /// # Errors
    /// [`LspError::NotInstalled`] when the program is not on `PATH` or not
    /// executable (checked before spawning, so it can never hang);
    /// [`LspError::SpawnFailed`] for a bad root or a refused spawn;
    /// [`LspError::InitializeFailed`] with the stderr tail when the handshake
    /// fails, times out, or the server exits (the group is killed first).
    ///
    /// Test: `missing_binary_is_not_installed`,
    /// `failed_initialize_carries_the_stderr_tail`,
    /// `partial_frame_during_initialize_fails_fast`.
    pub async fn spawn(
        command: &LspCommand,
        root: &Path,
        config: &LspConfig,
    ) -> Result<Self, LspError> {
        let program = command.program.clone();
        if which::which(&program).is_err() {
            return Err(LspError::NotInstalled { program });
        }
        if !root.is_dir() {
            return Err(LspError::SpawnFailed {
                program,
                reason: format!("workspace root {} is not a directory", root.display()),
            });
        }
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&command.args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // #9078: a group of its own, so one killpg reaches its helpers too.
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LspError::NotInstalled {
                program: program.clone(),
            },
            _ => LspError::SpawnFailed {
                program: program.clone(),
                reason: e.to_string(),
            },
        })?;
        let pipes = (
            child.id(),
            child.stdin.take(),
            child.stdout.take(),
            child.stderr.take(),
        );
        let (Some(pid), Some(stdin), Some(stdout), Some(stderr)) = pipes else {
            let _ = child.start_kill();
            return Err(LspError::SpawnFailed {
                program,
                reason: "the child has no pid or stdio pipes".into(),
            });
        };

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_task = tokio::spawn(drain_stderr(stderr, Arc::clone(&stderr_tail), pid));
        let (exit_tx, exit) = watch::channel(None);
        let kill = Arc::new(Notify::new());
        tokio::spawn(monitor(child, Arc::clone(&kill), exit_tx));
        let me = Self {
            client: LspClient::new(stdout, stdin),
            pid,
            program,
            exit,
            kill,
            stderr_tail,
            stderr_task: Mutex::new(Some(stderr_task)),
            hung: AtomicU32::new(0),
            finished: AtomicBool::new(false),
        };
        tracing::info!(pid, program = %me.program, root = %root.display(), "language server spawned");
        if let Err(e) = me.initialize(root, config.initialize_timeout).await {
            me.kill_now();
            let status = me.wait_exit(REAP_BUDGET).await;
            let stderr_tail = me.stderr_tail(Duration::from_secs(1)).await;
            let reason = match status {
                Some(status) => format!("{e} ({status})"),
                None => e.to_string(),
            };
            return Err(LspError::InitializeFailed {
                reason,
                stderr_tail,
            });
        }
        Ok(me)
    }

    async fn initialize(&self, root: &Path, timeout: Duration) -> Result<(), LspError> {
        let uri = file_uri(root)?;
        let name = root
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "trusty-analyze", "version": env!("CARGO_PKG_VERSION")},
            "rootUri": uri,
            "workspaceFolders": [{"uri": uri, "name": name}],
            "capabilities": {
                "textDocument": {
                    "synchronization": {"dynamicRegistration": false, "didSave": false},
                    "publishDiagnostics": {"versionSupport": true},
                    "references": {"dynamicRegistration": false}
                },
                "workspace": {"workspaceFolders": true, "configuration": false},
                "window": {"workDoneProgress": false}
            }
        });
        let result = self.client.request("initialize", params, timeout).await?;
        if !result.get("capabilities").is_some_and(Value::is_object) {
            return Err(LspError::Protocol(
                "initialize result has no capabilities".into(),
            ));
        }
        self.client.notify("initialized", json!({})).await
    }

    /// The JSON-RPC client speaking to this server.
    pub fn client(&self) -> &LspClient {
        &self.client
    }

    /// The server's pid, which is also its process-group id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether the process has exited and been reaped.
    pub fn exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    /// Whether calls can still reach a live server.
    pub fn is_usable(&self) -> bool {
        !self.client.is_closed() && !self.exited() && self.hung.load(Ordering::SeqCst) == 0
    }

    /// The error that describes why this server is unusable.
    pub fn failure(&self) -> LspError {
        let hung = self.hung.load(Ordering::SeqCst);
        if hung > 0 {
            return LspError::Hung { consecutive: hung };
        }
        let mut detail = match self.client.closed_reason() {
            Some(LspError::ServerExited { detail }) => detail,
            Some(other) => other.to_string(),
            None => "the server stopped".into(),
        };
        if let Some(status) = self.exit.borrow().clone() {
            detail = format!("{detail} ({status})");
        }
        LspError::ServerExited { detail }
    }

    /// Kill the server as hung after `consecutive` timeouts, and reap it.
    pub(crate) async fn kill_as_hung(&self, consecutive: u32) {
        self.hung.store(consecutive.max(1), Ordering::SeqCst);
        tracing::warn!(
            pid = self.pid,
            consecutive,
            "language server hung; killing it"
        );
        self.kill_now();
        self.wait_exit(REAP_BUDGET).await;
    }

    /// SIGKILL the whole process group now. Never blocks.
    pub fn kill_now(&self) {
        kill_group(self.pid);
        self.kill.notify_one();
    }

    /// Stop the server: `shutdown` then `exit` within `grace`, then kill the
    /// group regardless (descendants, or a server that ignored `exit`), and
    /// wait for the reap.
    ///
    /// Test: `shutdown_all_kills_a_child_that_ignores_exit`.
    pub async fn shutdown(&self, grace: Duration) {
        if !self.client.is_closed() && !self.exited() {
            let _ = self.client.request("shutdown", Value::Null, grace).await;
            let _ = self.client.notify("exit", Value::Null).await;
            self.wait_exit(grace).await;
        }
        self.kill_now();
        if self.wait_exit(REAP_BUDGET).await.is_none() {
            tracing::error!(pid = self.pid, "language server not reaped after SIGKILL");
        }
        self.finished.store(true, Ordering::SeqCst);
    }

    /// Wait up to `budget` for the reap; the exit status, if it came.
    async fn wait_exit(&self, budget: Duration) -> Option<String> {
        let mut exit = self.exit.clone();
        let waited = tokio::time::timeout(budget, exit.wait_for(Option::is_some)).await;
        match waited {
            Ok(Ok(status)) => status.clone(),
            _ => None,
        }
    }

    /// The last stderr lines, after waiting up to `budget` for stderr to close.
    async fn stderr_tail(&self, budget: Duration) -> String {
        let task = lock(&self.stderr_task).take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(budget, task).await;
        }
        lock(&self.stderr_tail)
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for LspChild {
    fn drop(&mut self) {
        // #9078: the panic and plain-drop paths. The monitor task reaps.
        if !self.finished.load(Ordering::SeqCst) {
            self.kill_now();
        }
    }
}

/// Own the process until it exits; a kill request SIGKILLs the leader too, for
/// platforms without process groups.
async fn monitor(mut child: Child, kill: Arc<Notify>, exit: watch::Sender<Option<String>>) {
    let status = tokio::select! {
        status = child.wait() => status,
        () = kill.notified() => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    let status = match status {
        Ok(s) => s.to_string(),
        Err(e) => format!("wait failed: {e}"),
    };
    let _ = exit.send(Some(status));
}

async fn drain_stderr(stderr: ChildStderr, tail: Arc<Mutex<VecDeque<String>>>, pid: u32) {
    let mut reader = BufReader::new(stderr);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let text: String = String::from_utf8_lossy(&line)
            .trim_end()
            .chars()
            .take(STDERR_LINE_CHARS)
            .collect();
        tracing::debug!(pid, line = %text, "language server stderr");
        let mut tail = lock(&tail);
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(text);
    }
}

#[cfg(unix)]
fn kill_group(pid: u32) {
    let Ok(pgid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // Never signal init or our own group: pgid is a child we spawned with
    // `process_group(0)`, so it is a group of its own.
    if pgid > 1 {
        // SAFETY: killpg only sends a signal; ESRCH (group already gone) is
        // the expected outcome after a clean exit and is ignored.
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}
