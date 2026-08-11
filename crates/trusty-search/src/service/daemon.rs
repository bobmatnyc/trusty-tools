//! Background HTTP daemon: PID lockfile + auto-port + graceful shutdown.
//!
//! Why: `trusty-search daemon` is the long-lived process that owns every
//! index for a machine. Two invariants matter:
//!
//! 1. **Singleton.** Only one daemon may run per machine. We enforce this
//!    via an OS-level advisory exclusive lock on a lockfile in the user's
//!    data-local dir. If the lock is held, `run_daemon` returns
//!    [`DaemonError::AlreadyRunning`] and `main` exits 1.
//!
//! 2. **Discoverable port.** The MCP server (and `trusty-search status`)
//!    needs to know what port the daemon picked. We bind a `TcpListener`
//!    starting at the requested port and walking forward until something
//!    is free, then write the chosen port to a file siblings to the lock.
//!
//! Graceful shutdown: axum's `with_graceful_shutdown` is wired to a tokio
//! signal future that resolves on SIGTERM or SIGINT. On exit we delete the
//! port file (the lockfile is unlinked by drop semantics on Unix; on
//! Windows the `Drop` of `File` releases the lock).
//!
//! What:
//! - [`daemon_lock_path`] / [`daemon_port_path`] resolve XDG-style paths.
//! - [`run_daemon`] is the one-shot entry point used by `main`.
//! - [`DaemonHandle`] returned for tests/embedding.
//!
//! Test: `cargo test -p trusty-search-service` covers (a) port-file
//! round-trip, (b) lockfile contention (second `try_lock_exclusive` on the
//! same path errors), (c) auto-port selection when the requested port is
//! taken.

use crate::service::server::{build_router_with_self_origins, SearchAppState};
use fs4::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
};
use thiserror::Error;
use tokio::net::TcpListener;

/// Errors raised by [`run_daemon`].
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("another trusty-search daemon is already running (lock held at {0})")]
    AlreadyRunning(PathBuf),
    #[error("could not determine data-local directory")]
    NoDataDir,
    #[error("could not find a free port starting at {0}")]
    NoFreePort(u16),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("server error: {0}")]
    Server(String),
}

/// Path to the advisory PID lockfile (`~/.local/share/trusty-search/daemon.lock`
/// on Linux, the platform equivalent elsewhere).
pub fn daemon_lock_path() -> Result<PathBuf, DaemonError> {
    Ok(daemon_dir()?.join("daemon.lock"))
}

/// Path to the file that records the listening port.
pub fn daemon_port_path() -> Result<PathBuf, DaemonError> {
    Ok(daemon_dir()?.join("daemon.port"))
}

/// Path to `daemon.env` — persisted memory-limit env vars written by
/// `trusty-search start` so launchd restarts inherit them.
///
/// Why: launchd re-spawns the daemon without the operator's shell environment,
/// causing `TRUSTY_MEMORY_LIMIT_MB` and friends to be lost after a restart.
/// Writing them to a file at `start`-time lets the daemon re-apply them on
/// every boot, regardless of how it was launched. When `TRUSTY_DATA_DIR` is
/// set the file lands in that directory so isolated daemons keep their own
/// env snapshot distinct from the production daemon.
/// What: returns `<daemon_dir>/daemon.env` (respecting `TRUSTY_DATA_DIR`).
/// Test: path ends in `daemon.env`; the parent directory is the same as the
/// lockfile directory so both are writable under the same permission set.
pub fn daemon_env_path() -> Option<PathBuf> {
    daemon_dir().ok().map(|d| d.join("daemon.env"))
}

/// The env-var keys that `trusty-search start` persists and the daemon sources
/// on startup. Ordered from most critical to least so log output is predictable.
pub const PERSISTED_ENV_VARS: &[&str] = &[
    "TRUSTY_MEMORY_LIMIT_MB",
    "TRUSTY_MAX_CHUNKS",
    "TRUSTY_EMBEDDING_CACHE",
    "TRUSTY_MAX_BATCH_SIZE",
    "TRUSTY_BM25_CORPUS_CAP",
    // Persist the device selection so launchd/systemd restarts (which run
    // without the user's shell env) keep honouring `--device cpu`. This is
    // load-bearing on Apple Silicon: CoreML inflates virtual RSS to ~100 GB
    // and triggers macOS jetsam kill on large repos, so operators who pin
    // CPU must have that pin survive every restart.
    "TRUSTY_DEVICE",
    // Issue #2845: persist the fan-out concurrency cap (from `--serial` /
    // `--fanout-concurrency`) so launchd/systemd restarts — which run without
    // the operator's shell env — keep bounding the cross-project search
    // fan-out and don't silently revert to the compiled-in default.
    "TRUSTY_SEARCH_FANOUT_CONCURRENCY",
];

/// Write memory-limit env vars from the current process environment to
/// `daemon.env` so launchd restarts inherit them.
///
/// Why: called by `trusty-search start` to snapshot whatever the operator set
/// in their shell; the file is sourced by `load_daemon_env` at daemon startup.
/// What: iterates `PERSISTED_ENV_VARS`; writes only vars that are currently
/// set so the file stays minimal and the daemon's compiled-in defaults win for
/// anything absent. Uses `key=value\n` lines (POSIX dotenv subset).
/// Test: call `save_daemon_env()` after setting `TRUSTY_MEMORY_LIMIT_MB=1024`
/// in the process env; then read the file and assert it contains that line.
pub fn save_daemon_env() {
    let Some(path) = daemon_env_path() else {
        tracing::warn!("could not resolve daemon.env path — memory limits will not persist");
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut lines = Vec::new();
    for key in PERSISTED_ENV_VARS {
        if let Ok(val) = std::env::var(key) {
            lines.push(format!("{key}={val}\n"));
        }
    }
    // Only write the file when at least one memory-limit var is present.
    // This prevents a launchd restart (which inherits no shell vars) from
    // overwriting a previously-saved daemon.env with an empty file, which
    // would lose the operator's configured limits on the next restart.
    if lines.is_empty() {
        tracing::debug!("no memory-limit env vars set — daemon.env unchanged");
        return;
    }
    let content = lines.concat();
    match std::fs::write(&path, &content) {
        Ok(()) => tracing::debug!("wrote memory limits to {}", path.display()),
        Err(e) => tracing::warn!("could not write daemon.env: {e}"),
    }
}

/// Source `daemon.env` into the current process environment, skipping vars
/// that are already set (env > file > compiled-in default precedence).
///
/// Why: launchd restarts the daemon without the operator's shell env; this
/// function restores memory-limit knobs from the file written by `save_daemon_env`.
/// What: reads `daemon.env` (silently ignores missing file), parses `key=value`
/// lines, calls `std::env::set_var` only when the var is not already present.
/// Test: write `daemon.env` with `TRUSTY_MEMORY_LIMIT_MB=512`; unset the var;
/// call `load_daemon_env()`; assert `std::env::var("TRUSTY_MEMORY_LIMIT_MB") == "512"`.
pub fn load_daemon_env() {
    apply_daemon_env(&[]);
}

/// Keys the EARLY (pre-`clap`) `daemon.env` pass must never apply.
///
/// Why (#4827): moving the whole file ahead of argument parsing would invert
/// precedence for the settings a CLI flag imperatively stamps into the env
/// AFTER parsing. `--device` and `--fanout-concurrency` are both applied as
/// "set the var if it is unset", so a file value present before the flag runs
/// would silently beat the flag. `TRUSTY_DATA_DIR` is worse than that: it
/// decides WHERE `daemon.env` itself lives, so applying it early would let the
/// production data dir's file redirect a `--data-dir /tmp/isolated` run back at
/// production data. These three keep their existing post-parse timing, where
/// the flag still wins.
/// What: the exact key set skipped by [`load_daemon_env_early`] and applied by
/// the later [`load_daemon_env`] as before.
/// Test: `early_load_skips_flag_derived_keys`.
pub const EARLY_LOAD_EXCLUDED_ENV: &[&str] = &[
    "TRUSTY_DATA_DIR",
    "TRUSTY_DEVICE",
    "TRUSTY_SEARCH_FANOUT_CONCURRENCY",
];

/// Source `daemon.env` BEFORE `clap` parses the command line.
///
/// Why (#4827): `load_daemon_env` ran after `Cli::try_parse()`, so every
/// variable in the file that backs a `#[arg(long, env = "…")]` was read too
/// late to change the already-computed value and was silently ignored. An
/// operator could write `TRUSTY_NO_AUTO_DISCOVER=1`, see no error, and get the
/// opposite behaviour — which is why #767's suppression never actually took
/// effect on the reporter's machine. The file advertised itself as a working
/// configuration mechanism and was not one for that whole class of setting.
/// What: applies every `daemon.env` key except [`EARLY_LOAD_EXCLUDED_ENV`], and
/// only where the process env has not already set it, so shell env still
/// outranks the file. Called from `main` on the `start` path only — `daemon.env`
/// is the daemon's environment, and a client subcommand must not inherit it.
/// Test: `early_load_skips_flag_derived_keys`, and the end-to-end
/// `tests/daemon_env_precedence.rs`, which drives the real `clap` env path
/// through a spawned binary.
pub fn load_daemon_env_early() {
    apply_daemon_env(EARLY_LOAD_EXCLUDED_ENV);
}

/// Source `daemon.env` early, but only when `argv` selects the daemon.
///
/// Why (#4827): the file has to be applied before `clap` parses, which is
/// before any subcommand exists to dispatch on — so the decision has to come
/// from raw argv. Gating it keeps `daemon.env`'s blast radius at the one path
/// it was written for: a client subcommand such as `query` or `status` must not
/// silently inherit the daemon's `TRUSTY_INDEX` or memory caps.
/// What: calls [`load_daemon_env_early`] when the first token that is neither
/// the program name nor a leading global flag is `start`. Global flags taking a
/// value (`-i` / `--index`) consume the token after them, so `-i start query`
/// is a query against index `start`, not the daemon path.
/// Test: `argv_selects_daemon_start_*` in `daemon_tests.rs`.
pub fn load_daemon_env_early_for(argv: &[String]) {
    if argv_selects_daemon_start(argv) {
        load_daemon_env_early();
    }
}

/// Apply every file-backed environment source before `clap` parses `argv`.
///
/// Why (#4827): clap resolves `#[arg(long, env = "…")]` by reading the REAL
/// process environment during the parse, so any source applied afterwards is a
/// silent no-op for that whole class of setting — which is exactly how
/// `daemon.env` came to advertise itself as a working configuration mechanism
/// while ignoring `TRUSTY_NO_AUTO_DISCOVER`. Keeping both loads in one function
/// keeps their order (and the reason for it) in one place instead of two loose
/// calls in `main`.
/// What: loads `.env.local` through the shared `trusty_common` loader (#2405),
/// then `daemon.env` via [`load_daemon_env_early_for`] — which is a no-op
/// unless `argv` selects `start`. Neither ever overrides an already-set process
/// env var, so shell env keeps outranking both files.
/// Test: `tests/daemon_env_precedence.rs` drives the whole chain through a
/// spawned binary; `argv_selects_daemon_start_for_the_daemon_path` covers the
/// gate.
pub fn bootstrap_process_env(argv: &[String]) {
    trusty_common::credentials::load_env_local_once();
    load_daemon_env_early_for(argv);
}

/// Whether `argv` invokes `trusty-search start`. See [`load_daemon_env_early_for`].
pub(crate) fn argv_selects_daemon_start(argv: &[String]) -> bool {
    let mut skip_next = false;
    for arg in argv.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "-i" || arg == "--index" {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return arg == "start";
    }
    false
}

/// Shared body of [`load_daemon_env`] and [`load_daemon_env_early`].
///
/// Why: one parser, one precedence rule, one set of diagnostics — two copies
/// would drift on exactly the ordering question #4827 is about.
/// What: reads `daemon.env`, applies each `key=value` line whose key is absent
/// from `skip` and unset in the process env. A read failure that is NOT
/// "file absent" is reported at `warn` rather than swallowed, and so is any
/// malformed line; before #4827 both degraded silently to compiled-in defaults
/// while startup carried on, so an unreadable file looked exactly like no file.
/// Test: `parse_daemon_env_reports_malformed_lines`,
/// `early_load_skips_flag_derived_keys`.
fn apply_daemon_env(skip: &[&str]) {
    let Some(path) = daemon_env_path() else {
        return;
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            // #4827: an unreadable file is a configuration failure, not an
            // absent one. Silently returning made a permission error
            // indistinguishable from first-run.
            tracing::warn!(
                "daemon.env at {} exists but could not be read ({e}); \
                 every setting in it is being ignored",
                path.display()
            );
            return;
        }
    };
    let (pairs, malformed) = parse_daemon_env(&content);
    for (lineno, line) in &malformed {
        // #4827: a line with no `=` was dropped without a word, so a typo cost
        // the operator the setting and told them nothing. The line itself is
        // never logged — see `malformed_line_summary`.
        tracing::warn!(
            "daemon.env {}:{lineno}: ignoring malformed line (expected key=value): {}",
            path.display(),
            malformed_line_summary(line)
        );
    }
    let mut loaded = Vec::new();
    for (key, val) in pairs {
        if skip.contains(&key.as_str()) {
            continue;
        }
        // env var takes priority: only apply file value when var is unset
        if std::env::var(&key).is_err() {
            // SAFETY: only called at startup before any threads read these vars;
            // `set_var` is not async-signal-safe but we are on the main thread here.
            unsafe { std::env::set_var(&key, &val) };
            loaded.push(key);
        }
    }
    if !loaded.is_empty() {
        tracing::info!("sourced settings from daemon.env: {}", loaded.join(", "));
    }
}

/// Describe a malformed `daemon.env` line without reproducing its contents.
///
/// Why: `daemon.env` is an operator file holding live credentials, and a typo'd
/// credential assignment — a space where the `=` belongs, as in
/// `OPENROUTER_API_KEY sk-or-v1-…` — is precisely the shape that reaches the
/// malformed arm. Logging the raw line wrote that secret to the daemon log in
/// cleartext. The success path has always logged loaded key NAMES and never
/// values; this brings the failure path to the same discipline.
/// What: reports the line's character count, plus its leading token when that
/// token has the shape of a conventional environment-variable name
/// (`[A-Z_][A-Z0-9_]*`) — enough to name the setting the operator fumbled. A
/// bare secret pasted on its own line does not match that shape (real tokens
/// carry lowercase, `-`, `.` or `/`), so it degrades to the length alone.
/// Test: `malformed_line_summary_redacts_a_typod_credential`,
/// `malformed_line_summary_redacts_a_bare_secret`,
/// `malformed_line_summary_names_a_conventional_key`.
fn malformed_line_summary(line: &str) -> String {
    let len = line.chars().count();
    let leading = line.split_whitespace().next().unwrap_or_default();
    let looks_like_env_key = !leading.is_empty()
        && leading.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && leading
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if looks_like_env_key {
        format!("{len} chars, starting with `{leading}`")
    } else {
        format!("{len} chars")
    }
}

/// One accepted `daemon.env` assignment: `(key, value)`, both trimmed.
pub type DaemonEnvPair = (String, String);

/// One rejected `daemon.env` line: `(1-based line number, the offending text)`.
pub type DaemonEnvReject = (usize, String);

/// Split `daemon.env` content into `key=value` pairs and rejected lines.
///
/// Why: extracting the parser makes the malformed-line arm testable without
/// touching a shared process env from a parallel test binary — the same reason
/// `service_unit::resolve_persisted_env` is pure.
/// What: returns `(pairs, malformed)`, where `malformed` carries the 1-based
/// line number and the offending text for every non-blank, non-comment line
/// with no `=`. Keys and values are trimmed.
/// Test: `parse_daemon_env_reports_malformed_lines`.
pub fn parse_daemon_env(content: &str) -> (Vec<DaemonEnvPair>, Vec<DaemonEnvReject>) {
    let mut pairs = Vec::new();
    let mut malformed = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once('=') {
            Some((key, val)) => pairs.push((key.trim().to_owned(), val.trim().to_owned())),
            None => malformed.push((i + 1, line.to_owned())),
        }
    }
    (pairs, malformed)
}

/// Path to the canonical address-discovery file used by `trusty-search
/// dashboard` and other client tools to locate the running daemon. Distinct
/// from the legacy `daemon.port` file (which stores only the port number
/// under the platform-specific data-local dir).
///
/// Why: aligns trusty-search with the trusty-memory address-discovery
/// contract — both daemons write a fully-qualified `host:port` line to a
/// well-known file. Clients can read it to discover the daemon without DNS or
/// service registration.
///
/// Issue #3545: when `TRUSTY_DATA_DIR` is set (by `--data-dir` or the env
/// var), the file lives *inside* that directory instead of the fixed
/// `$HOME/.trusty-search/` location, mirroring [`daemon_dir`]'s precedence.
/// Before this fix, an isolated daemon still wrote its address to the one
/// shared `$HOME/.trusty-search/http_addr` file regardless of `TRUSTY_DATA_DIR`,
/// clobbering the production daemon's discovery file with the isolated
/// instance's port (and vice versa on the next production restart).
/// What: returns `$TRUSTY_DATA_DIR/http_addr` when the env var is set,
/// otherwise `$HOME/.trusty-search/http_addr` (unchanged default, preserving
/// the existing cross-crate discovery contract for the production daemon).
/// Test: `http_addr_path_respects_trusty_data_dir` below; with `HOME=/tmp/xyz`
/// and no override → returns "/tmp/xyz/.trusty-search/http_addr".
pub fn http_addr_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("TRUSTY_DATA_DIR") {
        return Some(PathBuf::from(dir).join("http_addr"));
    }
    dirs::home_dir().map(|h| h.join(".trusty-search").join("http_addr"))
}

/// Resolve the root data directory for this daemon instance.
///
/// Why: `dirs::data_local_dir()` calls macOS NSFileManager and ignores HOME
/// overrides, which means only one daemon can run per Mac (issue #281). When
/// `TRUSTY_DATA_DIR` is set (by `--data-dir` or directly in the environment),
/// we use that path instead so isolated daemons (e.g. cert/benchmark runs) can
/// coexist with the production daemon.
///
/// What: returns `$TRUSTY_DATA_DIR` when set, otherwise
/// `<data_local_dir>/trusty-search`. Creates the directory if absent.
///
/// Test: set `TRUSTY_DATA_DIR=/tmp/ts-test` before calling; assert the returned
/// path equals `/tmp/ts-test` and the directory exists.
fn daemon_dir() -> Result<PathBuf, DaemonError> {
    let dir = resolve_daemon_dir(std::env::var_os("TRUSTY_DATA_DIR").as_deref())
        .ok_or(DaemonError::NoDataDir)?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Resolve which data directory a trusty-search daemon uses, given the value of
/// its `TRUSTY_DATA_DIR` (equivalently, its `--data-dir` flag).
///
/// Why (#4395): the orphan reaper has to answer this question about OTHER
/// processes, not just itself — "is that daemon looking at my data, or someone
/// else's?" is the only trustworthy basis for deciding whether it is an orphan
/// of mine or a healthy stranger. Taking the override as a parameter rather than
/// reading the environment makes the rule one function that the reaper can apply
/// to a candidate's observed argv/environ and to its own, so the two answers
/// cannot be computed by different logic.
///
/// What: `Some(override)` when one is supplied, otherwise
/// `<data_local_dir>/trusty-search`. Pure — creates nothing. `None` only when
/// the platform data-local dir is unresolvable, which [`daemon_dir`] reports as
/// [`DaemonError::NoDataDir`].
///
/// NB: We use `data_local_dir()` (not the shared `trusty_common::resolve_data_dir`
/// which uses `data_dir()`) because the lockfile path is replicated in `main.rs`
/// (`Stop`, `daemon_port_path`) against `data_local_dir()`. They must agree;
/// diverging would break daemon discovery on Windows where the two paths differ
/// (Roaming vs Local). If/when `trusty-common` grows a `resolve_data_local_dir`
/// helper, switch both sides at once.
///
/// Test: `commands::start::reap_orphans` tests drive it through
/// `DaemonIdentity`; `daemon_tests` cover the override precedence.
pub fn resolve_daemon_dir(override_dir: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    // TRUSTY_DATA_DIR wins over the platform default so callers can run
    // isolated daemons for cert/benchmark work without conflicting with the
    // production lockfile (issue #281).
    if let Some(dir) = override_dir {
        return Some(PathBuf::from(dir));
    }
    dirs::data_local_dir().map(|d| d.join("trusty-search"))
}

/// Handle returned by [`run_daemon`] (mostly for tests).
pub struct DaemonHandle {
    pub port: u16,
    pub addr: SocketAddr,
}

/// Try to bind a `TcpListener` starting at `start_port`, walking forward up
/// to `max_attempts` ports. `0` means "let the OS pick" — handled directly.
///
/// Why: thin wrapper around `trusty_common::bind_with_auto_port` so the
/// daemon and the rest of the trusty-* family share the same port-walk
/// behaviour. We keep the wrapper to (a) preserve the `NoFreePort` typed
/// error this crate exposes and (b) translate the shared async helper into
/// a `DaemonError` boundary.
async fn bind_with_auto_port(
    start_port: u16,
    max_attempts: u16,
) -> Result<TcpListener, DaemonError> {
    let addr: SocketAddr =
        format!("127.0.0.1:{start_port}")
            .parse()
            .map_err(|e: std::net::AddrParseError| {
                DaemonError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e))
            })?;
    trusty_common::bind_with_auto_port(addr, max_attempts)
        .await
        .map_err(|e| {
            tracing::warn!("auto-port exhausted from {start_port}: {e:#}");
            DaemonError::NoFreePort(start_port)
        })
}

/// Check whether a daemon is already running without starting one.
///
/// Why: callers that need to fail-fast (e.g. before loading a 86 MB embedding
/// model) can call this before doing any expensive work. Returns the lock-file
/// path when a running daemon is detected, `None` when the lock is free.
///
/// What: opens the lockfile (if it exists) and attempts a non-blocking
/// exclusive lock. If the attempt fails the lock is held by another process.
pub fn is_already_running() -> Option<PathBuf> {
    let lock_path = daemon_lock_path().ok()?;
    // If the lockfile doesn't exist there is definitely no daemon.
    if !lock_path.exists() {
        return None;
    }
    let file = OpenOptions::new()
        .create(false)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    if file.try_lock_exclusive().is_err() {
        // Lock is held — another daemon is alive.
        Some(lock_path)
    } else {
        // We acquired it; release immediately (lock drops here).
        None
    }
}

/// Inspect the lockfile and return the PID of a *running* daemon, if any.
///
/// Why: launchd treats any non-zero exit from `trusty-search start` as a
/// crash and re-spawns it after `ThrottleInterval` — producing an infinite
/// crash-loop when the daemon is already up and the second invocation
/// exits 1 with "already running". Callers (notably the `start` command)
/// need to distinguish "another live daemon is running, exit cleanly"
/// from "stale lockfile, recover and start".
///
/// What: returns `Some(pid)` if a lockfile exists, contains a parseable
/// PID, and that PID is currently alive. Returns `None` if the lockfile
/// is absent, unparseable, or records a dead PID (stale).
///
/// Test: with no lockfile, returns None. With a lockfile containing
/// `std::process::id()`, returns Some(current_pid). With a lockfile
/// containing a known-dead PID (e.g. u32::MAX), returns None.
pub fn running_daemon_pid() -> Option<u32> {
    let lock_path = daemon_lock_path().ok()?;
    if !lock_path.exists() {
        return None;
    }
    let pid = read_lockfile_pid(&lock_path)?;
    if pid_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

/// Read the PID stored in the lockfile (if any). Returns `None` on parse failure.
///
/// Why: the lockfile records the daemon PID so callers can detect stale
/// lockfiles left over from SIGKILL'd or crashed daemons (where the OS may
/// not have released the advisory lock cleanly, or the file persisted with
/// a dead PID written inside).
fn read_lockfile_pid(lock_path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(lock_path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse::<u32>().ok()
}

/// Check whether a process with the given PID is currently alive.
///
/// Why: a stale lockfile (from a SIGKILL'd or crashed daemon) records a PID
/// that no longer exists. Treat such lockfiles as removable so the next
/// daemon can start on the preferred port instead of bumping.
///
/// What: on Unix, `kill(pid, 0)` returns 0 if the process exists, ESRCH if
/// not, EPERM if it exists but is owned by another user (still alive). On
/// non-Unix targets we conservatively assume the PID is alive.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // Use nix's safe wrapper over kill(pid, 0); signal None performs no
    // action, only error checking. We accept i32 narrowing — PIDs always
    // fit on platforms we support.
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        // EPERM means the process exists but we cannot signal it.
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    true
}

/// Acquire an exclusive advisory lock on the daemon lockfile. The returned
/// `File` must outlive the daemon — drop releases the lock.
///
/// Why stale-lock handling: when a daemon is SIGKILL'd mid-run, the file
/// may persist with the dead PID recorded inside. On some platforms or
/// filesystems the advisory lock can also outlive the process. Before
/// reporting `AlreadyRunning`, we check whether the PID stored in the file
/// is still alive — if not, we remove the stale file and retry once.
fn acquire_lock(lock_path: &PathBuf) -> Result<File, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    if file.try_lock_exclusive().is_ok() {
        return Ok(file);
    }

    // Lock is held — but is it stale? Inspect the PID written by the previous
    // daemon. If the recorded PID is dead, treat the lockfile as abandoned
    // and recreate it.
    if let Some(prev_pid) = read_lockfile_pid(lock_path) {
        if !pid_alive(prev_pid) {
            tracing::warn!(
                "stale lockfile at {} (pid {prev_pid} is dead) — removing and retrying",
                lock_path.display()
            );
            drop(file);
            let _ = std::fs::remove_file(lock_path);
            let retry = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(lock_path)?;
            if retry.try_lock_exclusive().is_ok() {
                return Ok(retry);
            }
        }
    }

    Err(DaemonError::AlreadyRunning(lock_path.clone()))
}

// Why: the shared `shutdown_signal` helper in trusty-common provides identical
// SIGTERM + SIGINT handling for all trusty-* daemons (issue #534). Delegating
// to it removes local duplication while keeping behaviour identical.
// What: re-export as a module-private alias so call sites below are unchanged.
// Test: trusty-common's own unit test confirms compilation; the integration
// tests here exercise the full `with_graceful_shutdown` path.
use trusty_common::shutdown_signal;

/// Start the daemon: acquire the lock, bind a port, write the port file,
/// serve the axum router until SIGTERM/SIGINT or in-process admin stop, then
/// clean up the port file.
pub async fn run_daemon(state: SearchAppState, requested_port: u16) -> Result<(), DaemonError> {
    let lock_path = daemon_lock_path()?;
    let port_path = daemon_port_path()?;

    // Lock first — second daemon must error before binding a port.
    let mut lock_file = acquire_lock(&lock_path)?;
    let pid_string = std::process::id().to_string();
    // Best-effort: write PID into the lockfile so `ps`/`lsof` can confirm.
    let _ = lock_file.set_len(0);
    let _ = lock_file.write_all(pid_string.as_bytes());

    let listener = bind_with_auto_port(requested_port, 64).await?;
    let addr = listener.local_addr()?;
    let port = addr.port();

    // Atomically write the port file (write + rename).
    write_port_file(&port_path, port)?;

    // Write the http_addr discovery file (host:port) for client discovery.
    // Issue #117: unconditional write corrects stale files from crashed daemons.
    // Issue #3545: this path honors TRUSTY_DATA_DIR so an isolated instance
    // never clobbers the default instance's file (or vice versa).
    let addr_string = addr.to_string();
    let http_addr_written = match http_addr_path() {
        Some(path) => match write_http_addr_file(&path, &addr_string) {
            Ok(()) => Some(path),
            Err(e) => {
                tracing::warn!("could not write {}: {e}", path.display());
                None
            }
        },
        None => None,
    };

    // Issue #3602 review (post-#3545): also populate the generic,
    // TRUSTY_DATA_DIR-oblivious discovery registry that predates
    // http_addr_path()'s TRUSTY_DATA_DIR-awareness -- trusty-common's monitor
    // dashboard client (`resolve_search_url`) and trusty-installer's `ensure`
    // (`resolve_base_url`) still read it via
    // `trusty_common::read_daemon_addr("trusty-search")` and have no other
    // way to discover the daemon. Gated to the default instance only; see
    // `register_shared_discovery`'s doc for why.
    register_shared_discovery(&addr);

    // Startup banner (stderr only — stdout is JSON-RPC transport).
    eprintln!(
        "trusty-search v{} — HTTP admin panel: http://{}",
        env!("CARGO_PKG_VERSION"),
        addr,
    );

    // Stamp port into state so the SPA knows window.__DAEMON_PORT__.
    let state = state.with_daemon_port(port);
    // Issue #85: clone before moving into build_router for post-shutdown flush.
    let flush_state = state.clone();
    // Issue #829: subscribe before moving state into build_router.
    let mut shutdown_rx = state.shutdown_tx.subscribe();
    // #3304: trust the daemon's own resolved bind address as a self-origin so a
    // non-loopback (Tailscale) bind still passes the router-wide write guard;
    // `from_bind_addrs` drops loopback (already trusted), so a plain loopback
    // bind yields the empty default.
    let self_origins = trusty_common::server::SelfOrigins::from_bind_addrs(&[addr]);
    let router = build_router_with_self_origins(state, self_origins);

    tracing::info!("daemon listening on {addr} (lock {})", lock_path.display());

    // Log active memory limits (confirms launchd restarts inherit correct values).
    {
        use crate::core::memguard::{index_memory_limit_mb, memory_limit_mb};
        let max_chunks = std::env::var("TRUSTY_MAX_CHUNKS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(200_000);
        let emb_cache = std::env::var("TRUSTY_EMBEDDING_CACHE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1_000);
        let fmt = |v: Option<u64>| match v {
            Some(mb) => format!("{mb}"),
            None => "unlimited".to_string(),
        };
        tracing::info!(
            "memory limits: max_chunks={max_chunks} embedding_cache={emb_cache} \
             memory_limit_mb={} index_memory_limit_mb={}",
            fmt(memory_limit_mb()),
            fmt(index_memory_limit_mb()),
        );
    }

    // #4393: the termination window starts the moment SIGTERM lands, not when
    // the flush starts — the axum drain and watcher teardown happen in between
    // and spend real time out of the same window. Recording the instant here
    // and building the `ShutdownBudget` from it is what makes the flush charge
    // that time to itself instead of over-planning by however long the drain
    // took.
    let sigterm_at: std::sync::Arc<std::sync::OnceLock<std::time::Instant>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let sigterm_at_signal = std::sync::Arc::clone(&sigterm_at);

    // Issue #829: OS signal OR in-process admin_stop channel.
    let serve_result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = shutdown_signal() => {}
                _ = shutdown_rx.changed() => {
                    tracing::warn!("daemon: in-process stop via admin_stop");
                }
            }
            let _ = sigterm_at_signal.set(std::time::Instant::now());
        })
        .await;

    // Issue #1621: stop every filesystem watcher before flushing so no save
    // event races the shutdown flush by mutating an index mid-write. Aborts the
    // consumer tasks and releases the OS watches as part of the graceful drain.
    let stopped_watchers = flush_state.watcher_manager.stop_all().await;
    if stopped_watchers > 0 {
        tracing::info!("shutdown: stopped {stopped_watchers} file watcher(s)");
    }

    // Issue #85 — flush HNSW + chunk corpus for every registered index so
    // the next daemon boot warm-starts instead of paying a full re-index.
    // Best-effort: log on failure, don't abort cleanup.
    //
    // #4393: bounded by the real SIGTERM→SIGKILL window rather than by the sum
    // of the per-index budgets, which no terminator ever granted. Anchored at
    // the instant the shutdown future resolved; `unwrap_or_else` covers the
    // `serve` returning for a reason other than shutdown (a bind/accept error),
    // where the window has not started ticking against us at all.
    let budget = crate::service::shutdown_budget::ShutdownBudget::started_at(
        sigterm_at
            .get()
            .copied()
            .unwrap_or_else(std::time::Instant::now),
    );
    flush_all_indexes_on_shutdown(&flush_state, budget).await;

    // Best-effort cleanup; ignore errors so the lockfile drop is what frees
    // the next daemon, not our cleanup.
    let _ = std::fs::remove_file(&port_path);
    if let Some(path) = http_addr_written {
        let _ = std::fs::remove_file(&path);
    }
    deregister_shared_discovery();

    serve_result.map_err(|e| DaemonError::Server(e.to_string()))?;
    drop(lock_file);
    Ok(())
}

// Shutdown-flush helpers extracted to `service::shutdown_flush` (issue #874 split).
pub use crate::service::shutdown_flush::{
    flush_all_indexes_on_shutdown, shutdown_flush_timeout_override,
};

/// Write the canonical `host:port` discovery line to the given `http_addr`
/// path, atomically.
///
/// Why: separate from `write_port_file` because the format and location differ
/// — port file stores `12345`, http_addr stores `127.0.0.1:12345`. Both write
/// atomically via tmp-file + rename so partial reads are impossible. Exported
/// (issue #3602 review) so `commands::daemon_utils::daemon_base_url()`'s
/// reachability-probe refresh writes through the same atomic path instead of
/// a bare `std::fs::write`, which could tear a concurrent reader's view of the
/// file that `trusty-console`/`trusty-mpm`'s daemon discovery trust as ground
/// truth.
/// What: creates parent directory if missing; writes via temp + rename.
/// Test: with a fresh tempdir, write addr → read back → matches `host:port`.
pub fn write_http_addr_file(path: &Path, addr: &str) -> Result<(), DaemonError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("addr.tmp");
    {
        let mut f = File::create(&tmp)?;
        writeln!(f, "{addr}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Populate the generic, non-`TRUSTY_DATA_DIR`-aware discovery registry
/// (`trusty_common::write_daemon_addr("trusty-search", …)`) for the DEFAULT
/// daemon instance only.
///
/// Why (issue #3602 review, following #3545): `http_addr_path()` and
/// `daemon_port_path()` are `TRUSTY_DATA_DIR`-aware, but two older consumers
/// predate that and only know how to read the generic, per-app registry at
/// `trusty_common::resolve_data_dir("trusty-search")` via
/// `trusty_common::read_daemon_addr`: the monitor dashboard client
/// (`trusty_common::monitor::search_client::resolve_search_url`, backing
/// `trusty-search monitor status`/`monitor indexes`/`monitor tui`) and
/// trusty-installer's `ensure` (`resolve_base_url`, backing its
/// register-index and readiness-poll stages). #3545's first cut removed the
/// only writer of that file (a CLI-side reachability-probe cache) without
/// replacing it, silently breaking both. This restores a writer -- the
/// daemon itself, which is the one place that unambiguously knows its own
/// bound address -- but ONLY for the default instance: an isolated
/// `TRUSTY_DATA_DIR` instance must never overwrite this shared,
/// non-isolated registry with its own address, which is the exact
/// cross-instance pollution #3545 fixed for `http_addr_path()`. Any I/O
/// failure is logged, not propagated -- this registry is best-effort
/// back-compat, never load-bearing for the daemon's own operation.
/// What: no-ops when `TRUSTY_DATA_DIR` is set; otherwise calls
/// `trusty_common::write_daemon_addr("trusty-search", addr)`.
/// Test: `register_shared_discovery_writes_when_default_instance`,
/// `register_shared_discovery_noop_when_isolated`.
fn register_shared_discovery(addr: &SocketAddr) {
    if std::env::var("TRUSTY_DATA_DIR").is_ok() {
        return;
    }
    if let Err(e) = trusty_common::write_daemon_addr("trusty-search", &addr.to_string()) {
        tracing::warn!("could not write shared discovery registry entry: {e:#}");
    }
}

/// Shutdown-time mirror of [`register_shared_discovery`].
///
/// Why: cleaning up the generic registry on graceful shutdown matches the
/// existing `http_addr_written` cleanup a few lines above, so a stopped
/// default instance never leaves a stale, unreachable address behind for
/// `resolve_search_url`/`resolve_base_url` to hand out.
/// What: no-ops when `TRUSTY_DATA_DIR` is set; otherwise calls
/// `trusty_common::remove_daemon_addr("trusty-search")`, ignoring any error
/// (best-effort, same as the sibling `http_addr_path` removal).
/// Test: `deregister_shared_discovery_removes_when_default_instance`.
fn deregister_shared_discovery() {
    if std::env::var("TRUSTY_DATA_DIR").is_ok() {
        return;
    }
    let _ = trusty_common::remove_daemon_addr("trusty-search");
}

fn write_port_file(path: &PathBuf, port: u16) -> Result<(), DaemonError> {
    let tmp = path.with_extension("port.tmp");
    {
        let mut f = File::create(&tmp)?;
        writeln!(f, "{port}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
