//! Client for the out-of-process extraction worker (SPEC-OKF-01 §2.16,
//! Appendix B). Feature `extract-worker`.
//!
//! Why: every format but UTF-8 text and Markdown goes to a separate worker
//! binary, so this crate never links an extraction backend (B9). The worker
//! reads untrusted bytes, so the host enforces every limit and deadline
//! itself and trusts nothing the worker reports about itself that it can
//! check: the echoed input hash, the output sizes, the sandbox.
//! What: [`WorkerExtractor`] is DOC-55's `Extractor` over the Appendix B
//! protocol. It finds the worker only at [`WorkerConfig::path`] (no `PATH`
//! search); a missing file is `worker_absent`. It starts the worker lazily
//! on the first item (or the first fingerprint), and runs the `hello`
//! handshake under the handshake deadline: no common protocol, a
//! non-deterministic worker, a `sandbox` claim that disagrees with the
//! host's spawn evidence, or a different identity from an earlier spawn in
//! the run is `worker_incompatible` for the run; an expired deadline is
//! `handshake_timeout`; a worker that dies first is `crashed`. None of
//! these is retried in the run. Each item: over the input limit is
//! `too_large` and not sent; then one `extract` frame and one reply under
//! the per-item wall time. A timeout, a crash, a frame over the cap, a bad
//! reply, oversize output or a cancel kills and reaps the worker and fails
//! only that item (`timeout`, `crashed`, `protocol_error`,
//! `output_too_large`, `cancelled`); the next item gets a fresh worker. A
//! worker is recycled after [`Limits::items_per_worker`] items. A
//! connector-store item (any store that is not a local document tree) is
//! sent only to a worker whose spawn evidence shows the sandbox, else it is
//! skipped `sandbox_required`. Each extracted item carries the §2.16
//! provenance; [`Extractor::version`] is `<protocol>.<worker version>.<config
//! hash>`, so the DOC-55 fingerprint changes with any of them. Run-level
//! failures are returned by [`Extractor::end_run`], which also shuts the
//! worker down. Slice S10 of #10746.
//! Test: `crates/trusty-okf/tests/worker_criteria.rs` (criteria 99-105,
//! 121-123, 145, 147), `c124_forced_reap_timeout_returns_within_the_deadline`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.16, §4.4 `WorkerError`, Appendix B.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};

use super::{ExtractError, Extracted, Extractor, RunOutcome, TypeHint};
use crate::WorkerError;
use crate::build::registry::{Locator, SourceSpec};

mod cpu;
mod process;
mod proto;
mod sandbox;
#[cfg(test)]
mod worker_tests;

use process::{Hooks, IoFail, WorkerProcess};
pub use proto::{HOST_VERSION, PROTOCOL_MAX, PROTOCOL_MIN};
use proto::{OutputLimits, Reply, ReplyFault};
pub use sandbox::{SandboxPolicy, SpawnEvidence};

/// The stderr excerpt cap (§2.16 limits).
pub const STDERR_CAP: usize = 64 * 1024;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

/// Ceilings no configuration may exceed (Appendix B "Default limits").
pub const MAX_INPUT_BYTES: u64 = 64 * MIB;
/// The output ceiling.
pub const MAX_OUTPUT_BYTES: u64 = 32 * MIB;
/// The wall-time ceiling for any one deadline.
pub const MAX_DEADLINE: Duration = Duration::from_secs(300);
/// #10746 S10 fix: the `RLIMIT_CPU` ceiling, in seconds.
pub const MAX_CPU_SECONDS: u64 = 3600;
/// #10746 S10 fix: the `RLIMIT_AS` ceiling, in bytes.
pub const MAX_ADDRESS_SPACE: u64 = 16 * 1024 * MIB;

/// The worker's limits and deadlines (§2.16 limits table). Configuration
/// may lower any of them; [`Limits::check`] refuses one over its ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Input bytes per item (default 8 MiB).
    pub input_bytes: u64,
    /// Output text bytes per item (default 5 MiB).
    pub output_bytes: u64,
    /// Output `fields` bytes as JSON (default 64 KiB).
    pub fields_bytes: u64,
    /// Output notes per item (default 100).
    pub notes: usize,
    /// Wall time per item (default 30 s).
    pub item: Duration,
    /// Items before the worker is recycled (default 500).
    pub items_per_worker: usize,
    /// Spawn to a valid `hello` (default 10 s).
    pub handshake: Duration,
    /// `shutdown` to exit (default 5 s).
    pub shutdown: Duration,
    /// SIGKILL to a reaped child (default 5 s).
    pub reap: Duration,
    /// #10746 S10 fix: `RLIMIT_CPU` per worker process, in seconds
    /// (default 300). The host recycles a worker that has spent half of it
    /// (#10746 S10 last change), so an item is killed (`crashed`, fresh
    /// worker next) only when it alone spends at least half.
    pub cpu_seconds: u64,
    /// #10746 S10 fix: `RLIMIT_AS` per worker process, in bytes (default
    /// 4 GiB). Not applied on macOS, which does not honour it.
    pub address_space: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            input_bytes: 8 * MIB,
            output_bytes: 5 * MIB,
            fields_bytes: 64 * KIB,
            notes: 100,
            item: Duration::from_secs(30),
            items_per_worker: 500,
            handshake: Duration::from_secs(10),
            shutdown: Duration::from_secs(5),
            reap: Duration::from_secs(5),
            cpu_seconds: 300,
            address_space: 4 * 1024 * MIB,
        }
    }
}

impl Limits {
    /// The largest worker frame: six bytes for each text byte up to one past
    /// the output limit, plus the fields limit and 64 KiB (§2.16 "Frame
    /// size"; the JSON header carries both). Saturates instead of
    /// overflowing.
    pub fn frame_cap(&self) -> u64 {
        // #10746: JSON escapes one text byte as up to six (`\u0001`), and a
        // worker cuts oversize text one byte past the limit. A cap on the
        // escaped size refused in-limit text and turned oversize text into
        // `protocol_error`; the decoded length check in `parse_reply` is the
        // real output limit.
        self.output_bytes
            .saturating_add(1)
            .saturating_mul(6)
            .saturating_add(self.fields_bytes)
            .saturating_add(64 * KIB)
    }

    /// Refuse a limit over its compiled-in ceiling, or a zero count.
    pub fn check(&self) -> Result<(), String> {
        let deadlines = [self.item, self.handshake, self.shutdown, self.reap];
        if self.input_bytes > MAX_INPUT_BYTES
            || self.output_bytes > MAX_OUTPUT_BYTES
            || deadlines.iter().any(|d| *d > MAX_DEADLINE)
            || self.cpu_seconds > MAX_CPU_SECONDS
            || self.address_space > MAX_ADDRESS_SPACE
        {
            return Err("a worker limit is over its compiled-in ceiling".into());
        }
        if self.items_per_worker == 0 {
            return Err("items_per_worker must be at least 1".into());
        }
        Ok(())
    }

    fn output(&self) -> OutputLimits {
        OutputLimits {
            output_bytes: self.output_bytes,
            fields_bytes: self.fields_bytes,
            notes: self.notes,
        }
    }
}

/// Where the worker is and how to run it.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// The worker binary; never resolved through `PATH`.
    pub path: PathBuf,
    /// The directory that holds each spawn's fresh, empty working
    /// directory. The caller supplies it (B3).
    pub scratch: PathBuf,
    /// The OS sandbox policy.
    pub sandbox: SandboxPolicy,
    /// Limits and deadlines.
    pub limits: Limits,
    pub(crate) hooks: Hooks,
}

impl WorkerConfig {
    /// A worker at `path`, working directories under `scratch`, the
    /// platform sandbox and the default limits.
    pub fn new(path: impl Into<PathBuf>, scratch: impl Into<PathBuf>) -> Self {
        WorkerConfig {
            path: path.into(),
            scratch: scratch.into(),
            sandbox: SandboxPolicy::Platform,
            limits: Limits::default(),
            hooks: Hooks::default(),
        }
    }
}

/// What the handshake established about the worker.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    protocol: u32,
    worker_version: String,
    xberg_version: String,
    config_hash: String,
}

#[derive(Default)]
struct State {
    proc: Option<WorkerProcess>,
    served: usize,
    next_id: u64,
    /// The code every later worker item fails with; set once per run.
    dead: Option<&'static str>,
    errors: Vec<WorkerError>,
    stderr: Vec<u8>,
    /// `None` until `begin_run`: treated as a connector store (fail closed).
    local_store: Option<bool>,
    pgids: Vec<i32>,
}

/// DOC-55's `Extractor` over the worker protocol; see the module doc.
pub struct WorkerExtractor {
    cfg: WorkerConfig,
    state: RefCell<State>,
    identity: OnceCell<Identity>,
    version: OnceCell<String>,
    cancel: Arc<AtomicBool>,
}

fn err(code: &str, message: impl Into<String>) -> ExtractError {
    ExtractError::new(code, &message.into())
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl WorkerExtractor {
    /// A client for the worker `cfg` names. Nothing starts until the first
    /// item. Errors: a limit over its ceiling.
    pub fn new(cfg: WorkerConfig) -> Result<Self, String> {
        cfg.limits.check()?;
        Ok(WorkerExtractor {
            cfg,
            state: RefCell::new(State::default()),
            identity: OnceCell::new(),
            version: OnceCell::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The flag that cancels the run: the in-flight item fails `cancelled`,
    /// its worker is killed and reaped, and no later item is sent.
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// #10746 S10 fix: use `flag` as the cancel flag (the CLI's SIGINT
    /// watcher sets it).
    pub fn with_cancel(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = flag;
        self
    }

    /// The process group of every worker this client spawned, in order.
    pub fn spawned_groups(&self) -> Vec<i32> {
        self.state.borrow().pgids.clone()
    }

    /// Stop `proc` with the kill sequence (or `shutdown` when `graceful`),
    /// keep its stderr, and record run-level failures.
    fn retire(&self, st: &mut State, mut proc: WorkerProcess, graceful: bool) -> Option<String> {
        let errs = if graceful {
            proc.shutdown()
        } else {
            proc.kill()
        };
        let room = STDERR_CAP.saturating_sub(st.stderr.len());
        let kept = proc.stderr();
        st.stderr.extend_from_slice(&kept[..kept.len().min(room)]);
        if errs.contains(&WorkerError::ReapTimeout) {
            // A child we could not reap: start no further worker this run.
            st.dead.get_or_insert("reap_timeout");
        }
        st.errors.extend(errs);
        proc.status().map(|s| s.to_string())
    }

    /// Fail the run's worker items with `code` from now on.
    fn kill_run(
        &self,
        st: &mut State,
        proc: WorkerProcess,
        code: &'static str,
        e: Option<WorkerError>,
    ) {
        st.dead = Some(code);
        st.errors.extend(e);
        self.retire(st, proc, false);
    }

    /// Spawn and handshake a worker, unless one is running or the run is
    /// dead. Errors carry the item code.
    fn ensure(&self, st: &mut State) -> Result<(), ExtractError> {
        if let Some(code) = st.dead {
            return Err(err(code, "the worker is unavailable for this run"));
        }
        if st.proc.is_some() {
            return Ok(());
        }
        let path = self
            .cfg
            .path
            .canonicalize()
            .ok()
            .filter(|p| p.is_file())
            .ok_or_else(|| err("worker_absent", "no worker at the configured path"))?;
        let started = Instant::now();
        let mut proc = match WorkerProcess::spawn(
            &path,
            &self.cfg.scratch,
            self.cfg.sandbox,
            self.cfg.limits,
            self.cfg.hooks,
        ) {
            Ok(p) => p,
            Err(e) => {
                st.dead = Some("crashed");
                st.errors.push(e);
                return Err(err("crashed", "the worker could not be started"));
            }
        };
        st.pgids.push(proc.pgid());
        let deadline = started + self.cfg.limits.handshake;
        let reply = proc
            .send(proto::host_hello(), deadline, &self.cancel)
            .and_then(|()| proc.recv(deadline, &self.cancel));
        let frame = match reply {
            Ok(f) => f,
            Err(IoFail::Timeout) => {
                self.kill_run(
                    st,
                    proc,
                    "handshake_timeout",
                    Some(WorkerError::HandshakeTimeout),
                );
                return Err(err("handshake_timeout", "no hello within the deadline"));
            }
            Err(IoFail::Cancelled) => {
                self.kill_run(st, proc, "cancelled", None);
                return Err(err("cancelled", "the import was cancelled"));
            }
            Err(IoFail::TooLarge(_)) => {
                self.kill_run(st, proc, "protocol_error", None);
                return Err(err("protocol_error", "hello frame over the cap"));
            }
            Err(IoFail::Closed | IoFail::Io(_)) => {
                st.dead = Some("crashed");
                let status = self.retire(st, proc, false).unwrap_or_default();
                st.errors
                    .push(WorkerError::Spawn(format!("exited before hello: {status}")));
                return Err(err("crashed", format!("exited before hello: {status}")));
            }
        };
        let hello = match proto::parse_hello(&frame) {
            Ok(h) => h,
            Err(v) => {
                self.kill_run(st, proc, "protocol_error", None);
                return Err(err("protocol_error", v.0));
            }
        };
        let applied = proc.evidence().is_some();
        let claims = hello.sandbox != "none";
        let id = Identity {
            protocol: hello.protocol,
            worker_version: hello.worker_version,
            xberg_version: hello.xberg_version,
            config_hash: hello.config_hash,
        };
        let why = if !(PROTOCOL_MIN..=PROTOCOL_MAX).contains(&id.protocol) {
            Some("no common protocol version")
        } else if !hello.deterministic {
            Some("the worker reports non-deterministic output")
        } else if applied != claims {
            Some("the hello sandbox field disagrees with the spawn evidence")
        } else if self.identity.get().is_some_and(|known| *known != id) {
            Some("the worker identity changed within the run")
        } else {
            None
        };
        if let Some(why) = why {
            self.kill_run(
                st,
                proc,
                "worker_incompatible",
                Some(WorkerError::Incompatible),
            );
            return Err(err("worker_incompatible", why));
        }
        let _ = self.version.set(format!(
            "{}.{}.{}",
            id.protocol, id.worker_version, id.config_hash
        ));
        let _ = self.identity.set(id);
        st.proc = Some(proc);
        st.served = 0;
        Ok(())
    }

    /// #10746 S10 last change: whether the worker may take another item.
    /// `RLIMIT_CPU` counts its whole life, so it is recycled once its spent
    /// CPU leaves less than half the budget: every item gets at least half
    /// of `cpu_seconds`, and only an item that alone spends that much is
    /// killed. An unreadable spend recycles it (fails closed).
    fn budget_left(&self, proc: &WorkerProcess) -> bool {
        let budget = self.cfg.limits.cpu_seconds as f64;
        // #10746: the group ID is the spawned PID, the root of the process
        // tree `spent` reads; under `bwrap --new-session` the worker itself
        // sits two levels below it in a group of its own.
        cpu::spent(proc.pgid()).is_some_and(|used| used + budget / 2.0 <= budget)
    }

    fn provenance(&self, sha: &str) -> Mapping {
        let mut m = Mapping::new();
        if let Some(id) = self.identity.get() {
            let s = |v: &str| Value::String(v.to_string());
            m.insert(
                s("extracted_by"),
                s(&format!("xberg-worker@{}", id.worker_version)),
            );
            m.insert(s("extract_protocol"), Value::Number(id.protocol.into()));
            m.insert(s("xberg_version"), s(&id.xberg_version));
            m.insert(s("extract_config"), s(&id.config_hash));
            m.insert(s("extract_input_sha256"), s(&format!("sha256:{sha}")));
        }
        m
    }

    /// One request and its reply on the running worker.
    fn exchange(
        &self,
        st: &mut State,
        bytes: &[u8],
        hint: &TypeHint,
        sha: &str,
    ) -> Result<Extracted, ExtractError> {
        let Some(mut proc) = st.proc.take() else {
            return Err(err("crashed", "no worker is running"));
        };
        // #10746 S10 fix: anything the worker sent between requests is a
        // protocol violation; the worker is killed before this item.
        if let Some((code, why)) = proc.unsolicited() {
            self.retire(st, proc, false);
            return Err(err(code, why));
        }
        st.next_id += 1;
        let rid = st.next_id;
        let lim = self.cfg.limits.output();
        let deadline = Instant::now() + self.cfg.limits.item;
        let frame = proto::extract_request(rid, hint, bytes, sha, lim);
        let got = proc
            .send(frame, deadline, &self.cancel)
            .and_then(|()| proc.recv(deadline, &self.cancel));
        let frame = match got {
            Ok(f) => f,
            Err(fail) => {
                let (code, msg) = match fail {
                    IoFail::Timeout => ("timeout", "wall time exceeded".to_string()),
                    IoFail::Cancelled => {
                        st.dead = Some("cancelled");
                        ("cancelled", "the import was cancelled".into())
                    }
                    IoFail::TooLarge(n) => {
                        ("protocol_error", format!("frame of {n} bytes over the cap"))
                    }
                    IoFail::Closed | IoFail::Io(_) => ("crashed", String::new()),
                };
                let status = self.retire(st, proc, false);
                let msg = match (code, status) {
                    ("crashed", Some(s)) => format!("worker exited mid-request: {s}"),
                    ("crashed", None) => "worker exited mid-request".into(),
                    _ => msg,
                };
                return Err(err(code, msg));
            }
        };
        match proto::parse_reply(&frame, rid, sha, lim) {
            Ok(reply) => {
                st.served += 1;
                if st.served >= self.cfg.limits.items_per_worker || !self.budget_left(&proc) {
                    self.retire(st, proc, true);
                } else {
                    st.proc = Some(proc);
                }
                match reply {
                    Reply::Ok {
                        text,
                        fields,
                        notes,
                    } => Ok(Extracted {
                        text,
                        fields,
                        notes,
                        provenance: self.provenance(sha),
                    }),
                    // Appendix B: a note with empty text, not a failure.
                    Reply::Error { code, .. } if code == "no_text_layer" => Ok(Extracted {
                        notes: vec![code],
                        provenance: self.provenance(sha),
                        ..Extracted::default()
                    }),
                    Reply::Error { code, message } => Err(err(&code, message)),
                }
            }
            Err(fault) => {
                self.retire(st, proc, false);
                Err(match fault {
                    ReplyFault::Protocol(m) => err("protocol_error", m),
                    ReplyFault::TooLarge(m) => err("output_too_large", m),
                })
            }
        }
    }
}

impl Extractor for WorkerExtractor {
    fn id(&self) -> &str {
        "xberg-worker"
    }

    /// `<protocol>.<worker version>.<config hash>` once a handshake
    /// succeeded (starting the worker if needed), else `unavailable`.
    fn version(&self) -> &str {
        if self.version.get().is_none() {
            let mut st = self.state.borrow_mut();
            let _ = self.ensure(&mut st);
        }
        self.version.get().map_or("unavailable", String::as_str)
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn available(&self) -> Result<(), ExtractError> {
        if self.cfg.path.is_file() {
            Ok(())
        } else {
            Err(err("worker_absent", "no worker at the configured path"))
        }
    }

    fn begin_run(&self, store: &SourceSpec) {
        self.state.borrow_mut().local_store =
            Some(matches!(store.locator, Locator::DocStore { .. }));
    }

    fn end_run(&self) -> RunOutcome {
        let mut st = self.state.borrow_mut();
        if let Some(proc) = st.proc.take() {
            self.retire(&mut st, proc, true);
        }
        let stderr = std::mem::take(&mut st.stderr);
        RunOutcome {
            errors: std::mem::take(&mut st.errors),
            stderr_excerpt: (!stderr.is_empty())
                .then(|| String::from_utf8_lossy(&stderr).into_owned()),
        }
    }

    fn extract(&self, bytes: &[u8], hint: &TypeHint) -> Result<Extracted, ExtractError> {
        if self.cancel.load(Ordering::SeqCst) {
            return Err(err("cancelled", "the import was cancelled"));
        }
        if bytes.len() as u64 > self.cfg.limits.input_bytes {
            return Err(err("too_large", "input over the limit; not sent"));
        }
        let mut st = self.state.borrow_mut();
        self.ensure(&mut st)?;
        let sandboxed = st.proc.as_ref().is_some_and(|p| p.evidence().is_some());
        if st.local_store != Some(true) && !sandboxed {
            return Err(err(
                "sandbox_required",
                "connector content needs the OS sandbox",
            ));
        }
        let sha = hex_sha256(bytes);
        self.exchange(&mut st, bytes, hint, &sha)
    }
}

impl Drop for WorkerExtractor {
    fn drop(&mut self) {
        // The process's own Drop runs the kill sequence.
        self.state.get_mut().proc.take();
    }
}
