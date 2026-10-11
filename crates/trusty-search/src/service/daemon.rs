//! The trusty-search daemon: PID lockfile, RPC socket, graceful shutdown.
//!
//! Why: `trusty-search start` is the long-lived process that owns every
//! index for a machine. Two invariants matter:
//!
//! 1. **Singleton.** Only one daemon may run per machine. We enforce this
//!    via an OS-level advisory exclusive lock on a lockfile in the user's
//!    data-local dir. If the lock is held, `run_daemon` returns
//!    [`DaemonError::AlreadyRunning`] and `main` exits 1. The exact
//!    invariant, and the only sanctioned unlink of the lockfile, live in
//!    `daemon_lock.rs` (#8760).
//!
//! 2. **One door.** The daemon serves its Unix socket only (#9214,
//!    ADR-0032). It binds no TCP listener and publishes no HTTP address; it
//!    removes the `daemon.port` / `http_addr` files an older build of the
//!    same data dir left, so no client finds a dead address.
//!
//! Graceful shutdown: a signal watcher cancels one drain token on SIGTERM,
//! SIGINT or an in-process admin stop; the socket's serve loop ends on it,
//! then every index is flushed. Dropping the lockfile's `File` releases the
//! lock; the file itself stays on disk and is reused by the next daemon.
//!
//! What:
//! - [`daemon_lock_path`] / [`daemon_port_path`] resolve XDG-style paths.
//! - [`run_daemon`] is the one-shot entry point used by `main`.
//!
//! Test: `daemon_tests.rs` — lockfile contention, the socket-only boot, and
//! the stale-announcement withdrawal.

use crate::service::server::SearchAppState;
// #9214: the UDS listener, the daemon's only one.
use crate::service::socket;
use fs4::FileExt;
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Errors raised by [`run_daemon`].
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("another trusty-search daemon is already running (lock held at {0})")]
    AlreadyRunning(PathBuf),
    #[error("could not determine data-local directory")]
    NoDataDir,
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

/// Path to the legacy port file a pre-#9214 daemon wrote; read only to
/// remove it.
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

/// The `http_addr` file a pre-#9214 daemon of this instance published.
///
/// Why (#9214): the daemon binds no HTTP listener, but an older build wrote
/// its `host:port` here, and clients that predate the socket still read it.
/// A stale file would send them to a dead port, or to whatever holds it now,
/// so the daemon removes it at boot and never writes it.
/// What: `$TRUSTY_DATA_DIR/http_addr` when the env var is set, otherwise
/// `$HOME/.trusty-search/http_addr` — the path that build wrote (#3545).
/// Test: `run_daemon_removes_a_stale_http_addr`.
fn legacy_http_addr_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TRUSTY_DATA_DIR") {
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
/// Why: the lockfile records the daemon PID for `running_daemon_pid` and
/// operator diagnostics. It is never an input to lock acquisition (#8760):
/// between a holder's flock and its pid write the file can still name a dead
/// predecessor.
fn read_lockfile_pid(lock_path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(lock_path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse::<u32>().ok()
}

/// Check whether a process with the given PID is currently alive.
///
/// Why: the crate has three separate places that must tell a live process from
/// a dead one, and all three carry the same consequence if they get it wrong —
/// something belonging to a live process gets treated as abandoned. A stale
/// daemon lockfile (from a SIGKILL'd daemon) records a PID that no longer
/// exists and must be removable so the next daemon starts on the preferred
/// port; `commands::stop` must not report a reaped child as a straggler; and
/// `core::store::staging_reap` must not delete a `.tmp` snapshot another live
/// daemon is mid-write on (#2936). One implementation, three callers — the
/// EPERM subtlety below is exactly the kind of detail that silently drifts
/// between copies.
///
/// What: on Unix, `kill(pid, 0)` returns 0 if the process exists, ESRCH if
/// not, EPERM if it exists but is owned by another user (still alive). On
/// non-Unix targets we conservatively assume the PID is alive — every caller
/// treats "alive" as the do-nothing answer, so this fails safe.
///
/// `pub` rather than `pub(crate)` because `commands::stop` lives in the BINARY
/// crate, which reaches this through the library's public `service` module; a
/// crate-private item is invisible from there and the duplicate would have had
/// to stay. Additive only — no existing signature changes.
/// Test: `pid_alive_current_process_is_alive` (covers both the live and the
/// clearly-dead pid).
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
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
pub fn pid_alive(_pid: u32) -> bool {
    true
}

// #8760: acquisition and the only sanctioned unlink of `daemon.lock` live in
// their own module, which states the singleton invariant.
#[path = "daemon_lock.rs"]
mod lock;
use lock::acquire_lock;
pub use lock::{remove_daemon_files_if_unheld, StaleLockRemoval};

// #9214: the stop wait of the socket-only daemon.
#[path = "daemon_socket_only.rs"]
mod socket_only;

// Why: the shared `shutdown_signal` helper in trusty-common provides identical
// SIGTERM + SIGINT handling for all trusty-* daemons (issue #534). Delegating
// to it removes local duplication while keeping behaviour identical.
// What: re-export as a module-private alias so call sites below are unchanged.
// Test: trusty-common's own unit test confirms compilation; the integration
// tests here exercise the full `with_graceful_shutdown` path.
use trusty_common::shutdown_signal;

/// Start the daemon: acquire the lock, bind the RPC socket, withdraw any HTTP
/// announcement an older build left, serve until SIGTERM/SIGINT or in-process
/// admin stop, then clean up.
///
/// Why (#9214, ADR-0032): the socket is the daemon's only listener. Every
/// in-workspace client reaches it there, so the `:7878` bind is gone and the
/// daemon announces no HTTP address.
/// What: binds no TCP listener and writes no port or `http_addr` file. It
/// removes stale ones and clears the default instance's shared discovery
/// registry entry; `search.health` reports `transport.http_addr: null`. The
/// tickers start here, since no router starts them. The socket bind is
/// fatal; a stale entry it cannot remove is only warned. `rpc_socket`
/// (`start --socket`) replaces [`socket::socket_path`] as the socket to bind.
/// Test: `run_daemon_serves_only_the_socket`,
/// `run_daemon_removes_a_stale_http_addr`,
/// `run_daemon_serves_when_a_stale_http_addr_cannot_be_removed`,
/// `run_daemon_health_reports_the_transport_it_bound`.
pub async fn run_daemon(
    state: SearchAppState,
    rpc_socket: Option<std::path::PathBuf>,
) -> Result<(), DaemonError> {
    let lock_path = daemon_lock_path()?;

    // Lock first — a second daemon must error before binding the socket.
    // #8760: `acquire_lock` writes our pid itself and fails closed; a
    // contended lock is `AlreadyRunning` whatever pid the file names.
    let lock_file = acquire_lock(&lock_path)?;

    // #6285: the socket binds before anything is announced, and a bind
    // failure is fatal. #9214: `start --socket` names the socket; otherwise
    // the data-dir one. A named socket's parent is never narrowed unless
    // start created it.
    let rpc_socket = rpc_socket.map_or_else(socket::socket_path, socket::prepare_named_socket);
    let rpc_socket = rpc_socket.map_err(|e| DaemonError::Server(e.to_string()))?;
    let rpc = socket::bind(&rpc_socket)
        .await
        .map_err(|e| DaemonError::Server(format!("{e:#}")))?;

    // #9214: no HTTP address exists to announce; withdraw an older build's.
    // A stale entry it cannot remove is warned, never fatal.
    withdraw_http_discovery();

    // Startup banner (stderr only — stdout is JSON-RPC transport).
    eprintln!(
        "trusty-search v{} — rpc socket: {}",
        env!("CARGO_PKG_VERSION"),
        rpc.path.display(),
    );

    // #9030: `search.health` reports the listeners actually bound.
    let state = state.with_transport(crate::service::server::DaemonTransport {
        socket_path: Some(rpc.path.to_string_lossy().into_owned()),
        http_addr: None,
    });
    // Issue #85: clone for the post-shutdown flush.
    let flush_state = state.clone();
    // Issue #829: subscribe before the state moves into its `Arc`.
    let mut shutdown_rx = state.shutdown_tx.subscribe();
    // #6285: ONE `Arc<SearchAppState>`, shared by the socket and the tickers.
    let state_arc = std::sync::Arc::new(state);
    let rpc_state = std::sync::Arc::clone(&state_arc);
    // #9214: no router starts the tickers as a side effect.
    crate::service::server::spawn_daemon_tickers(&state_arc);

    tracing::info!(
        "daemon serving the rpc socket {} (lock {})",
        rpc.path.display(),
        lock_path.display()
    );

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
    // the flush starts — the drain and watcher teardown happen in between and
    // spend real time out of the same window. Recording the instant here and
    // building the `ShutdownBudget` from it is what makes the flush charge
    // that time to itself.
    let sigterm_at: std::sync::Arc<std::sync::OnceLock<std::time::Instant>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let sigterm_at_signal = std::sync::Arc::clone(&sigterm_at);
    // #8600: drained at the signal — an embed pass must stop writing before
    // the flush.
    let signal_state = flush_state.clone();

    // #6285: the stop condition is awaited ONCE and fanned out through a
    // token. Issue #829: OS signal OR in-process admin_stop channel.
    let drain = tokio_util::sync::CancellationToken::new();
    let signal_watcher = drain.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = shutdown_signal() => {}
            _ = shutdown_rx.changed() => {
                tracing::warn!("daemon: in-process stop via admin_stop");
            }
        }
        let _ = sigterm_at_signal.set(std::time::Instant::now());
        drain_paused_embedders(&signal_state);
        signal_watcher.cancel();
    });

    let rpc_drain = drain.clone();
    let mut rpc_task = tokio::spawn(async move {
        socket::serve_until_shutdown(rpc, rpc_state, rpc_drain.cancelled_owned()).await;
    });

    // #9214: the socket is the only door; see `await_socket_only_stop`.
    let (serve_result, rpc_joined) =
        socket_only::await_socket_only_stop(&drain, &mut rpc_task).await;

    // #6285: cancel unconditionally, so the socket is unlinked even when the
    // serve loop ended for a reason that was never a shutdown signal. The join
    // happens BEFORE the flush below — an RPC connection still in a handler
    // must not race the flush.
    drain.cancel();
    // #6524: release every parked embedding stage. A pause is an unbounded wait
    // on an operator action, so a paused index would otherwise hold a background
    // task — and the flush behind it — open for a resume that is never coming.
    drain_paused_embedders(&flush_state);
    let joined = match rpc_joined {
        Some(joined) => joined,
        None => rpc_task.await,
    };
    if let Err(e) = joined {
        tracing::warn!("the rpc serve task did not exit cleanly: {e}");
    }
    // #9214: the tickers hold a `Weak`; this is the last strong reference once
    // the rpc task is gone, so they stop before the flush.
    drop(state_arc);

    // Issue #1621: stop every filesystem watcher before flushing so no save
    // event races the shutdown flush by mutating an index mid-write.
    let stopped_watchers = flush_state.watcher_manager.stop_all().await;
    if stopped_watchers > 0 {
        tracing::info!("shutdown: stopped {stopped_watchers} file watcher(s)");
    }

    // Issue #85 — flush HNSW + chunk corpus for every registered index so
    // the next daemon boot warm-starts instead of paying a full re-index.
    // Best-effort: log on failure, don't abort cleanup.
    //
    // #4393: bounded by the real SIGTERM→SIGKILL window. `unwrap_or_else`
    // covers a serve loop that ended for a reason other than shutdown.
    let budget = crate::service::shutdown_budget::ShutdownBudget::started_at(
        sigterm_at
            .get()
            .copied()
            .unwrap_or_else(std::time::Instant::now),
    );
    flush_all_indexes_on_shutdown(&flush_state, budget).await;
    // #9459, #9477: the caller exits through `process::exit(0)`, which skips
    // redb's `Drop`, and the corpus reopen sweep keeps a strong clone of the
    // state, so close every corpus here or each one is left needing repair.
    // The report holds the closed indexes' permits until this function returns,
    // so no reindex starts on a corpus-less indexer before the exit.
    let _closed = crate::service::shutdown_close::close_corpora_on_shutdown(
        &flush_state,
        crate::service::shutdown_close::SHUTDOWN_CORPUS_CLOSE_BUDGET,
    )
    .await;

    serve_result?;
    drop(lock_file);
    Ok(())
}

/// Remove every HTTP announcement an older build of this instance left.
///
/// Why (#9214): an older daemon wrote `daemon.port`, `http_addr` and the
/// shared registry entry, and older clients still resolve the daemon from
/// them. Left in place, they send those clients to a dead port — or to
/// whatever now holds it.
/// What: removes the port file and the `http_addr` file and clears the
/// default instance's shared registry entry. A missing entry is fine. Every
/// arm shares one failure policy, [`warn_stale_discovery`]: a WARN naming the
/// stale path, never an error. The socket is already bound, so a stale entry
/// must not stop the daemon serving it — under launchd `KeepAlive` a fatal
/// error relaunches the daemon into the same failure forever.
/// Safe because the caller holds the daemon lock, so no live instance of this
/// data dir owns them.
/// Test: `run_daemon_removes_a_stale_http_addr`,
/// `run_daemon_serves_when_a_stale_http_addr_cannot_be_removed`,
/// `deregister_shared_discovery_warns_when_the_entry_cannot_be_removed`.
fn withdraw_http_discovery() {
    // #9214: a path that cannot resolve is warned like one that cannot go.
    match daemon_port_path() {
        Ok(port_path) => remove_stale_discovery_file(&port_path),
        Err(e) => warn_stale_discovery("daemon.port (path unresolved)", &e),
    }
    if let Some(addr_path) = legacy_http_addr_path() {
        remove_stale_discovery_file(&addr_path);
    }
    deregister_shared_discovery();
}

/// Remove one stale discovery file; a missing file is fine (#9214).
fn remove_stale_discovery_file(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => tracing::info!("removed stale {} (no HTTP listener)", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn_stale_discovery(&path.display().to_string(), &e),
    }
}

/// The one failure policy for a stale HTTP discovery entry (#9214): warn,
/// naming the entry, and carry on serving the socket.
fn warn_stale_discovery(entry: &str, err: &dyn std::fmt::Display) {
    tracing::warn!(
        "cannot remove the stale HTTP discovery entry {entry}: {err}; an older client may \
         still resolve it to a dead port — remove it by hand (#9214)"
    );
}

/// Wake every parked embedding stage so shutdown is never held open by a pause
/// (#6524).
///
/// Why: [`crate::core::embed_pause::EmbeddingPause::wait_while_paused`] waits on
/// an operator, and shutdown cannot. Draining converts every park into a
/// bounded return.
/// What: sets the drain flag on every registered handle's gate. One-way — the
/// process is ending, so nothing clears it.
/// Test: `shutdown_drain_releases_a_parked_embed_pass` in
/// `service::reindex::embed_pause_tests`.
fn drain_paused_embedders(state: &SearchAppState) {
    let mut drained = 0usize;
    for handle in state.registry.list_handles() {
        if handle.embedding_pause.is_paused() {
            drained += 1;
        }
        handle.embedding_pause.drain();
    }
    if drained > 0 {
        tracing::info!("shutdown: released {drained} paused embedding stage(s)");
    }
}

// Shutdown-flush helpers extracted to `service::shutdown_flush` (issue #874 split).
pub use crate::service::shutdown_flush::{
    flush_all_indexes_on_shutdown, shutdown_flush_timeout_override,
};

/// Clear the generic, non-`TRUSTY_DATA_DIR`-aware discovery registry entry
/// (`trusty_common::write_daemon_addr("trusty-search", …)`) an older build
/// wrote for the DEFAULT daemon instance.
///
/// Why (#9214): an older default-instance daemon published its HTTP address
/// in this shared registry for clients that predate `http_addr_path()`'s
/// `TRUSTY_DATA_DIR` awareness (#3602). This build has no address to publish,
/// so a surviving entry can only name a dead port.
/// What: no-ops when `TRUSTY_DATA_DIR` is set — an isolated instance never
/// wrote it and must never clear the default instance's entry; otherwise
/// calls `trusty_common::remove_daemon_addr("trusty-search")`. A failure takes
/// the same policy as the file removals beside it, [`warn_stale_discovery`]:
/// the error's context names the registry path.
/// Test: `deregister_shared_discovery_removes_when_default_instance`,
/// `deregister_shared_discovery_warns_when_the_entry_cannot_be_removed`.
fn deregister_shared_discovery() {
    if std::env::var("TRUSTY_DATA_DIR").is_ok() {
        return;
    }
    // #9214: never swallowed — warned like an unremovable stale file.
    if let Err(e) = trusty_common::remove_daemon_addr("trusty-search") {
        warn_stale_discovery("in the shared registry", &format!("{e:#}"));
    }
}

#[cfg(test)]
#[path = "daemon_tests.rs"]
mod tests;
