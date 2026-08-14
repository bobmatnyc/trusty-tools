//! Shared "ensure daemon running" helper for trusty-* CLI commands.
//!
//! Why: trusty-search, trusty-memory, and trusty-analyze all have a CLI
//! daemon_guard module that probes a health endpoint, optionally spawns a
//! detached daemon process, then polls with a spinner until the daemon is
//! ready or a timeout is exceeded. The spin/probe/timeout logic was identical
//! across all three crates. This module is the single shared implementation;
//! each crate's daemon_guard.rs is reduced to a thin shim that fills in the
//! service-specific knobs (health path, timeout, spawn args) and delegates
//! here. See issue #985.
//!
//! What: `DaemonGuardConfig` carries all service-specific parameters;
//! `probe_once` and `spin_until_ready` together implement the full guard loop.
//! `spawn_current_exe` is the shared process-spawn helper.
//! `DaemonAddrLayout` answers the question that comes BEFORE the guard loop —
//! which `host:port` is this service listening on? — for any service, from its
//! on-disk discovery files (#5670).
//!
//! STDOUT hygiene: like `mcp::daemon_bridge`, this module NEVER writes to
//! stdout. All user-visible output (spinner, ready/timeout messages) goes to
//! stderr so stdout stays clean for JSON piping and MCP framing.
//!
//! Test: `probe_once_returns_false_for_refused_port` and
//! `spin_until_ready_returns_ok_for_live_server` exercise the core paths
//! without requiring a real daemon binary.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use colored::Colorize;

/// Per-probe HTTP timeout.
///
/// Why: a hung or half-started daemon must not exhaust the spinner budget on a
/// single stalled TCP connect. 750 ms matches the value used by all three
/// daemon_guard copies.
/// What: connect + read deadline applied to each `probe_once` call.
/// Test: probe tests assert completion within 6s (generous for filtered ports).
const PROBE_TIMEOUT: Duration = Duration::from_millis(750);

/// Polling interval between health probes during the spinner loop.
///
/// Why: 500 ms keeps the spinner feeling responsive without hammering the
/// daemon during its own boot sequence.
/// What: sleep duration in `spin_until_ready` between probe attempts.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Default hard-error budget for the daemon to become ready after spawning.
///
/// Why: 30s is the value used by both trusty-memory and trusty-analyze; the
/// search crate historically used 60s but that was for ONNX model loading
/// which is no longer on the critical-start path. Callers can override via
/// `DaemonGuardConfig::startup_timeout`.
pub const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Spinner animation frames cycled while waiting for the daemon.
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Configuration for a service's CLI daemon-guard startup check.
///
/// Why: each service has its own health path, timeout, and error hint. Encoding
/// those differences in a config struct lets `spin_until_ready` be a single
/// tested function rather than three near-identical copies.
/// What: holds the full health URL (so the caller can handle dynamic-port
/// resolution), the ready budget, and the error hint shown on timeout.
/// Test: `spin_until_ready_returns_ok_for_live_server` constructs one and
/// exercises the happy path.
pub struct DaemonGuardConfig {
    /// Full `http://host:port/path` URL to probe for health.
    pub health_url: String,
    /// Human-readable service name used in spinner messages.
    pub service_name: String,
    /// Wall-clock budget before the guard hard-errors.
    pub startup_timeout: Duration,
    /// Polling interval between probes.
    pub poll_interval: Duration,
    /// One-line hint appended to the timeout error message.
    pub timeout_hint: String,
}

impl DaemonGuardConfig {
    /// Build a `DaemonGuardConfig` with `DEFAULT_STARTUP_TIMEOUT` and
    /// `DEFAULT_POLL_INTERVAL`.
    ///
    /// Why: the three call sites that replace their inline guards only need to
    /// specify the service-specific parts (URL, name, hint); sensible defaults
    /// handle the rest.
    /// What: fills `startup_timeout` and `poll_interval` with the module
    /// defaults; callers can override those fields afterwards if needed.
    /// Test: exercised by every test that constructs a `DaemonGuardConfig`.
    pub fn new(
        health_url: impl Into<String>,
        service_name: impl Into<String>,
        timeout_hint: impl Into<String>,
    ) -> Self {
        Self {
            health_url: health_url.into(),
            service_name: service_name.into(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
            timeout_hint: timeout_hint.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Address resolution (#5670)
// ---------------------------------------------------------------------------

/// Host paired with a port read from a port-only discovery file.
///
/// Why: a port file records `12345`, not `127.0.0.1:12345`, so the host has to
/// come from somewhere. Every trusty-* daemon binds loopback only
/// (ADR-0018), so loopback is the only host that fallback can mean.
const ADDR_FALLBACK_HOST: &str = "127.0.0.1";

/// Deadline for the TCP reachability probe in [`DaemonAddrLayout::resolve_base_url`].
///
/// Why: a discovery file can outlive the process that wrote it — a SIGKILL'd
/// daemon never runs its cleanup — so the address it names must be proven live
/// before it is trusted. 200 ms is short enough to stay invisible in CLI
/// startup and long enough to tolerate a busy machine. See #117.
const ADDR_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Where one service records the address it is listening on, and how a client
/// resolves it back.
///
/// Why (#5670): resolving a daemon's address was private to `trusty-search`'s
/// CLI, so `tga` — which must probe that daemon but does not depend on
/// `trusty-search` — had no way to ask. The alternative was a second copy of
/// the resolution rules in `tga`, and a second copy drifts: #117 (dead
/// discovery file) and #3545 (a stale non-isolated cache outranking a
/// `TRUSTY_DATA_DIR` instance) were both fixed in the one copy that existed.
/// What: names the six things that vary per service — the isolation env var,
/// the two directory fallbacks, the two filenames, and the compiled-in default
/// port. [`Self::TRUSTY_SEARCH`] is the layout `trusty-search` writes and every
/// client of it reads.
///
/// The two directory fallbacks are DIFFERENT on purpose. When the isolation env
/// var is unset the address file falls back under `$HOME`, while the port file
/// falls back under the platform data-local dir. That asymmetry is what
/// `trusty-search` shipped; it is preserved here rather than normalised,
/// because the two files have separate readers and moving either one breaks a
/// reader that predates this change.
///
/// Test: `trusty_search_layout_matches_shipped_paths`,
/// `layout_paths_honour_the_isolation_env_var`.
#[non_exhaustive]
pub struct DaemonAddrLayout {
    /// Env var that, when set, holds both discovery files in one isolated
    /// directory (`TRUSTY_DATA_DIR` for trusty-search).
    pub data_dir_env: &'static str,
    /// `$HOME`-relative directory holding the address file when the isolation
    /// env var is unset.
    pub home_subdir: &'static str,
    /// Platform-data-local-relative directory holding the port file when the
    /// isolation env var is unset.
    pub data_local_subdir: &'static str,
    /// Filename of the canonical `host:port` address file.
    pub addr_file_name: &'static str,
    /// Filename of the legacy port-only file.
    pub port_file_name: &'static str,
    /// Port assumed when no port file can be read.
    pub default_port: u16,
}

impl DaemonAddrLayout {
    /// The layout `trusty-search` writes (#3545) and its clients read.
    pub const TRUSTY_SEARCH: Self = Self {
        data_dir_env: "TRUSTY_DATA_DIR",
        home_subdir: ".trusty-search",
        data_local_subdir: "trusty-search",
        addr_file_name: "http_addr",
        port_file_name: "daemon.port",
        default_port: 7878,
    };

    /// Path to the canonical `host:port` address file.
    ///
    /// Why: an isolated instance must never read or refresh the shared
    /// instance's file, so the isolation env var wins over the `$HOME` default
    /// (#3545).
    /// What: `$<data_dir_env>/<addr_file_name>` when that env var is set,
    /// otherwise `$HOME/<home_subdir>/<addr_file_name>`. `None` when the env
    /// var is unset and the home directory cannot be resolved.
    /// Test: `layout_paths_honour_the_isolation_env_var`.
    pub fn discovery_file_path(&self) -> Option<PathBuf> {
        if let Ok(dir) = std::env::var(self.data_dir_env) {
            return Some(PathBuf::from(dir).join(self.addr_file_name));
        }
        dirs::home_dir().map(|h| h.join(self.home_subdir).join(self.addr_file_name))
    }

    /// Path to the legacy port-only file.
    ///
    /// What: `$<data_dir_env>/<port_file_name>` when that env var is set,
    /// otherwise `<data_local_dir>/<data_local_subdir>/<port_file_name>`.
    /// `None` when the env var is unset and the platform data-local directory
    /// cannot be resolved.
    /// Test: `layout_paths_honour_the_isolation_env_var`.
    pub fn port_file_path(&self) -> Option<PathBuf> {
        if let Ok(dir) = std::env::var(self.data_dir_env) {
            return Some(PathBuf::from(dir).join(self.port_file_name));
        }
        dirs::data_local_dir().map(|d| d.join(self.data_local_subdir).join(self.port_file_name))
    }

    /// Resolve the daemon's base URL from its discovery files.
    ///
    /// Why: stdio MCP servers, CLI subcommands, and now sibling crates need to
    /// find a running daemon with no configuration. The address file is
    /// preferred, the port file is the backward-compatible fallback, and the
    /// compiled-in default is the last resort. Both files honour the isolation
    /// env var, so an isolated instance's clients never fall through to the
    /// shared instance (#3545).
    /// What: returns `http://{host}:{port}`, no trailing slash. The address
    /// file is trusted only when it is readable, non-blank, AND TCP-reachable
    /// within [`ADDR_PROBE_TIMEOUT`] — a file left behind by a SIGKILL'd daemon
    /// names a dead address (#117). On every other outcome the port file
    /// decides the port, paired with [`ADDR_FALLBACK_HOST`], and when THAT
    /// address is reachable the address file is refreshed so the next caller
    /// skips the probe. The refresh is best-effort: no `$HOME`, or a read-only
    /// filesystem, is not an error here.
    ///
    /// This never fails and never returns `None`. An unreachable daemon yields
    /// the default-port URL, and the caller learns the daemon is down from the
    /// request it then makes — which is what
    /// [`spin_until_ready`] is for. Note that this differs from
    /// [`crate::resolve_daemon_base_url`], which returns `None` rather than
    /// guessing a port; the two answer different questions and both callers
    /// exist.
    ///
    /// Test: `resolve_base_url_prefers_a_live_discovery_file`,
    /// `resolve_base_url_falls_back_when_discovery_file_is_dead`,
    /// `resolve_base_url_falls_back_when_discovery_file_is_absent`,
    /// `resolve_base_url_falls_back_when_discovery_file_is_malformed`,
    /// `resolve_base_url_uses_default_port_when_nothing_is_readable`.
    pub fn resolve_base_url(&self) -> String {
        // #5670: promoted verbatim from trusty-search's private
        // `commands::daemon_utils::daemon_base_url`.
        // A missing, unreadable, blank, or unreachable file falls through to
        // the port-file fallback and refresh below.
        if let Some(path) = self.discovery_file_path()
            && let Ok(raw) = std::fs::read_to_string(&path)
        {
            let addr = raw.trim();
            if !addr.is_empty() && address_reachable_blocking(addr) {
                return format!("http://{addr}");
            }
        }
        let port = self
            .port_file_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| s.trim().parse::<u16>().ok())
            .unwrap_or(self.default_port);

        let live_addr = format!("{ADDR_FALLBACK_HOST}:{port}");
        // #3602 review: the refresh writes through the atomic tmp+rename path,
        // never a bare `std::fs::write` — a false-positive probe must not tear
        // a concurrent reader's view of the file that daemon discovery trusts.
        if address_reachable_blocking(&live_addr)
            && let Some(path) = self.discovery_file_path()
        {
            let _ = write_addr_file_atomic(&path, &live_addr);
        }
        format!("http://{live_addr}")
    }
}

/// Write a `host:port` discovery line to `path`, atomically.
///
/// Why: the address file is read by other processes at arbitrary moments, so a
/// partially-written file must never be observable. Writers and the
/// reachability-probe refresh in [`DaemonAddrLayout::resolve_base_url`] share
/// this one implementation (#3602 review, #5670).
/// What: creates the parent directory if absent, writes `addr` plus a trailing
/// newline to a sibling temp file, `fsync`s it, then renames it over `path`.
/// Readers trim, so the newline is invisible to them.
///
/// # Errors
///
/// Any I/O failure along that sequence — unresolvable parent, permission
/// denied, read-only filesystem.
///
/// Test: `write_addr_file_atomic_round_trips`,
/// `write_addr_file_atomic_replaces_existing_content`.
pub fn write_addr_file_atomic(path: &Path, addr: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("addr.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "{addr}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Synchronous, time-boxed TCP reachability check.
///
/// Why: [`DaemonAddrLayout::resolve_base_url`] is called from sync contexts
/// (CLI dispatch) and cannot `.await`. A blocking `connect_timeout` is the
/// simplest correct primitive.
/// What: parses `host:port`, attempts a TCP connect with an
/// [`ADDR_PROBE_TIMEOUT`] deadline, returns `true` on success. Any parse or
/// connect error returns `false`.
/// Test: `address_reachable_returns_false_for_dead_port`,
/// `address_reachable_returns_false_for_garbage_input`,
/// `address_reachable_returns_true_for_live_listener`.
fn address_reachable_blocking(host_port: &str) -> bool {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    let Ok(mut iter) = host_port.to_socket_addrs() else {
        return false;
    };
    let Some(addr): Option<SocketAddr> = iter.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, ADDR_PROBE_TIMEOUT).is_ok()
}

/// Probe the given health URL once; returns `true` on any 2xx HTTP response.
///
/// Why: a fresh `reqwest::Client` per probe avoids carrying connection-pool
/// state from a failed probe to a later successful one, keeping the logic
/// simple and predictable across cold/warm starts.
/// What: builds a one-shot reqwest client with `PROBE_TIMEOUT`, issues a GET,
/// returns `true` on any 2xx status and `false` on any error or non-2xx.
/// Test: `probe_once_returns_false_for_refused_port` (async unit test below).
pub async fn probe_once(health_url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .connect_timeout(PROBE_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    matches!(
        client.get(health_url).send().await,
        Ok(r) if r.status().is_success()
    )
}

/// Spawn `program` with the given arguments as a detached background process
/// (all stdio fds null-ed).
///
/// Why (#5670): a guard does not always boot its OWN binary. `tga audit` has to
/// start `trusty-analyze`, a sibling it resolves by env override or PATH, and
/// the alternative to naming that capability here is a second
/// `Command::new(…).stdin(null)…` in another crate — the duplication the
/// common-entry-point rule exists to prevent.
/// What: spawns `program` with `args` and stdin/stdout/stderr all redirected to
/// null, so the child outlives the parent terminal and writes nothing into the
/// caller's streams, and returns the child PID.
///
/// The PID is proof the process STARTED, never proof it stayed up — a daemon
/// that exits a millisecond later still yields one. Callers must follow this
/// with [`spin_until_ready`] and treat that verdict as the real answer.
///
/// # Errors
///
/// Any spawn failure, including the not-installed case, rendered with the
/// program and arguments that were tried.
///
/// Test: `spawn_detached_reports_a_missing_program`; the live path is exercised
/// by `tga::audit`'s guard tests, which spawn a stub executable.
pub fn spawn_detached(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Result<u32> {
    let program = program.as_ref();
    let child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            anyhow!(
                "could not spawn `{} {}`: {e}",
                program.to_string_lossy(),
                args.join(" "),
            )
        })?;
    Ok(child.id())
}

/// Spawn `current_exe()` with the given arguments as a detached background
/// process (all stdio fds null-ed).
///
/// Why: every daemon_guard copy spawns `<current_exe> <args>` with stdin,
/// stdout, and stderr redirected to null so the daemon outlives the parent
/// terminal / shell and does not pollute the user's output. Using
/// `current_exe()` ensures a `cargo run` session boots its own debug daemon
/// and a production install boots the production binary.
/// What: resolves `current_exe()` and hands it to [`spawn_detached`].
/// Test: compile-only (spawning a real process in unit tests risks port/FS
/// side-effects; the live path is exercised by integration tests).
pub fn spawn_current_exe(args: &[&str]) -> Result<u32> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("could not resolve current_exe: {e}"))?;
    spawn_detached(&exe, args)
}

/// Poll `config.health_url` until the daemon is ready, printing a spinner to
/// stderr. The daemon is assumed to have already been spawned (or been
/// confirmed already running) by the caller.
///
/// Why: the spinner loop was copy-pasted verbatim across the three daemon_guard
/// files. This function is the single tested implementation; see issue #985.
/// What: polls `probe_once(config.health_url)` every `config.poll_interval`,
/// renders a braille spinner and elapsed-second counter to stderr, clears the
/// line on success, and hard-errors with `config.timeout_hint` after
/// `config.startup_timeout`.
/// Test: `spin_until_ready_returns_ok_for_live_server` (async integration test).
pub async fn spin_until_ready(config: &DaemonGuardConfig) -> Result<()> {
    let deadline = Instant::now() + config.startup_timeout;
    let start = Instant::now();
    let mut frame = 0usize;
    loop {
        let elapsed = start.elapsed().as_secs();
        let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
        eprint!(
            "\r{} Waiting for {} to become ready… ({}s) ",
            glyph.cyan(),
            config.service_name,
            elapsed
        );
        let _ = std::io::stderr().flush();
        frame = frame.wrapping_add(1);

        tokio::time::sleep(config.poll_interval).await;
        if probe_once(&config.health_url).await {
            // Erase the spinner line so subsequent output starts fresh.
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            eprintln!(
                "{} {} ready ({}s)",
                "✓".green(),
                config.service_name,
                start.elapsed().as_secs()
            );
            return Ok(());
        }
        if Instant::now() >= deadline {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
            return Err(anyhow!(
                "{} did not become ready within {}s — {}",
                config.service_name,
                config.startup_timeout.as_secs(),
                config.timeout_hint,
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod addr_tests {
    //! Address-resolution coverage (#5670). Every test drives a layout whose
    //! `data_dir_env` is unique to that test and points at a tempdir, so no
    //! test reads `$HOME`, the platform data-local dir, or another test's
    //! state. `#[serial]` still guards the process-global `set_var` itself.

    use super::*;
    use serial_test::serial;

    /// Build a layout isolated to `dir` via the named env var, and set that var.
    ///
    /// # Safety
    ///
    /// `set_var` is process-global; every caller is `#[serial]`.
    fn isolated_layout(env_var: &'static str, dir: &Path) -> DaemonAddrLayout {
        unsafe { std::env::set_var(env_var, dir) };
        DaemonAddrLayout {
            data_dir_env: env_var,
            ..DaemonAddrLayout::TRUSTY_SEARCH
        }
    }

    fn clear(env_var: &str) {
        unsafe { std::env::remove_var(env_var) };
    }

    /// Why: the promoted resolver must address the same files `trusty-search`
    /// has always written, or every existing client silently stops finding the
    /// daemon.
    /// What: asserts the shipped constant's six fields.
    /// Test: this test.
    #[test]
    fn trusty_search_layout_matches_shipped_paths() {
        let l = DaemonAddrLayout::TRUSTY_SEARCH;
        assert_eq!(l.data_dir_env, "TRUSTY_DATA_DIR");
        assert_eq!(l.home_subdir, ".trusty-search");
        assert_eq!(l.data_local_subdir, "trusty-search");
        assert_eq!(l.addr_file_name, "http_addr");
        assert_eq!(l.port_file_name, "daemon.port");
        assert_eq!(l.default_port, 7878);
    }

    /// Why (#3545): both files must move into the isolated directory together.
    /// If only one did, an isolated instance would read its own address file
    /// and the shared instance's port file.
    /// What: sets the env var and asserts both paths land under it.
    /// Test: this test.
    #[test]
    #[serial]
    fn layout_paths_honour_the_isolation_env_var() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = isolated_layout("TRUSTY_TEST_ADDR_DIR_PATHS", tmp.path());
        let discovery = layout.discovery_file_path().unwrap();
        let port = layout.port_file_path().unwrap();
        clear("TRUSTY_TEST_ADDR_DIR_PATHS");
        assert_eq!(discovery, tmp.path().join("http_addr"));
        assert_eq!(port, tmp.path().join("daemon.port"));
    }

    /// Why: the fast path — a current address file — must be returned without
    /// consulting the port file at all.
    /// What: writes a live listener's address to the discovery file and a
    /// DIFFERENT live listener's port to the port file; asserts the discovery
    /// file wins.
    /// Test: this test.
    #[test]
    #[serial]
    fn resolve_base_url_prefers_a_live_discovery_file() {
        let discovery_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let discovery_addr = discovery_listener.local_addr().unwrap().to_string();
        let port_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let other_port = port_listener.local_addr().unwrap().port();

        let tmp = tempfile::tempdir().unwrap();
        let layout = isolated_layout("TRUSTY_TEST_ADDR_DIR_LIVE", tmp.path());
        std::fs::write(tmp.path().join("http_addr"), &discovery_addr).unwrap();
        std::fs::write(tmp.path().join("daemon.port"), other_port.to_string()).unwrap();

        let url = layout.resolve_base_url();
        clear("TRUSTY_TEST_ADDR_DIR_LIVE");
        assert_eq!(url, format!("http://{discovery_addr}"));
    }

    /// Why (#117): a discovery file outlives a SIGKILL'd daemon, so an
    /// unreachable address in it must not be returned — and once the port file
    /// resolves a live address, the stale file must be corrected in place so
    /// the next caller skips the probe.
    /// What: writes a dead address (reserved port 1) to the discovery file and
    /// a live listener's port to the port file; asserts the live address is
    /// returned AND written back.
    /// Test: this test.
    #[test]
    #[serial]
    fn resolve_base_url_falls_back_when_discovery_file_is_dead() {
        let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live_port = live.local_addr().unwrap().port();

        let tmp = tempfile::tempdir().unwrap();
        let layout = isolated_layout("TRUSTY_TEST_ADDR_DIR_DEAD", tmp.path());
        let discovery = tmp.path().join("http_addr");
        std::fs::write(&discovery, "127.0.0.1:1").unwrap();
        std::fs::write(tmp.path().join("daemon.port"), live_port.to_string()).unwrap();

        let url = layout.resolve_base_url();
        let refreshed = std::fs::read_to_string(&discovery).unwrap();
        clear("TRUSTY_TEST_ADDR_DIR_DEAD");

        assert_eq!(url, format!("http://127.0.0.1:{live_port}"));
        assert_eq!(refreshed.trim(), format!("127.0.0.1:{live_port}"));
    }

    /// Why: a daemon that has never run leaves no discovery file, and the port
    /// file alone must still resolve.
    /// What: writes only the port file; asserts the port-file address is
    /// returned and the discovery file is created by the refresh.
    /// Test: this test.
    #[test]
    #[serial]
    fn resolve_base_url_falls_back_when_discovery_file_is_absent() {
        let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live_port = live.local_addr().unwrap().port();

        let tmp = tempfile::tempdir().unwrap();
        let layout = isolated_layout("TRUSTY_TEST_ADDR_DIR_ABSENT", tmp.path());
        std::fs::write(tmp.path().join("daemon.port"), live_port.to_string()).unwrap();

        let url = layout.resolve_base_url();
        let created = std::fs::read_to_string(tmp.path().join("http_addr")).unwrap();
        clear("TRUSTY_TEST_ADDR_DIR_ABSENT");

        assert_eq!(url, format!("http://127.0.0.1:{live_port}"));
        assert_eq!(created.trim(), format!("127.0.0.1:{live_port}"));
    }

    /// Why: a torn write, a hand-edit, or a zero-byte file must degrade to the
    /// port file rather than panic or return a nonsense URL.
    /// What: drives an empty discovery file, a whitespace-only one, and a
    /// non-address one; each must resolve from the port file. The port file is
    /// itself garbage in the last case, proving the default-port floor holds
    /// under two simultaneous bad reads.
    /// Test: this test.
    #[test]
    #[serial]
    fn resolve_base_url_falls_back_when_discovery_file_is_malformed() {
        let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let live_port = live.local_addr().unwrap().port();

        let tmp = tempfile::tempdir().unwrap();
        let layout = isolated_layout("TRUSTY_TEST_ADDR_DIR_BAD", tmp.path());
        let discovery = tmp.path().join("http_addr");
        std::fs::write(tmp.path().join("daemon.port"), live_port.to_string()).unwrap();

        for junk in ["", "   \n", "not-a-host:port", "127.0.0.1"] {
            std::fs::write(&discovery, junk).unwrap();
            assert_eq!(
                layout.resolve_base_url(),
                format!("http://127.0.0.1:{live_port}"),
                "a discovery file containing {junk:?} must fall through to the port file"
            );
        }

        // Both files unusable: the compiled-in default is the floor.
        std::fs::write(&discovery, "not-a-host:port").unwrap();
        std::fs::write(tmp.path().join("daemon.port"), "not-a-port").unwrap();
        let url = layout.resolve_base_url();
        clear("TRUSTY_TEST_ADDR_DIR_BAD");
        assert_eq!(url, format!("http://127.0.0.1:{}", layout.default_port));
    }

    /// Why: with nothing on disk the resolver still returns a URL rather than
    /// an error — callers learn the daemon is down from the request they make.
    /// What: points the layout at an empty tempdir; asserts the default port
    /// and asserts NO discovery file was written (the default address is not
    /// reachable, so there is nothing to cache).
    /// Test: this test.
    #[test]
    #[serial]
    fn resolve_base_url_uses_default_port_when_nothing_is_readable() {
        let tmp = tempfile::tempdir().unwrap();
        // A port nothing is listening on, so the refresh branch stays cold.
        let layout = DaemonAddrLayout {
            data_dir_env: "TRUSTY_TEST_ADDR_DIR_EMPTY",
            default_port: 1,
            ..DaemonAddrLayout::TRUSTY_SEARCH
        };
        unsafe { std::env::set_var("TRUSTY_TEST_ADDR_DIR_EMPTY", tmp.path()) };

        let url = layout.resolve_base_url();
        let wrote_cache = tmp.path().join("http_addr").exists();
        clear("TRUSTY_TEST_ADDR_DIR_EMPTY");

        assert_eq!(url, "http://127.0.0.1:1");
        assert!(
            !wrote_cache,
            "an unreachable fallback address must not be cached as if it were live"
        );
    }

    /// Why: readers trim, so the trailing newline is invisible — but a torn
    /// file never is, which is why the write goes through tmp+rename.
    /// What: writes into a fresh nested directory and reads it back.
    /// Test: this test.
    #[test]
    fn write_addr_file_atomic_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("http_addr");
        write_addr_file_atomic(&path, "127.0.0.1:7878").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "127.0.0.1:7878\n",
            "the file carries exactly one trailing newline"
        );
    }

    /// Why: the refresh path overwrites a stale file, so a shorter new address
    /// must not leave a tail of the longer old one behind.
    /// What: writes a long address, then a short one, and reads back.
    /// Test: this test.
    #[test]
    fn write_addr_file_atomic_replaces_existing_content() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("http_addr");
        write_addr_file_atomic(&path, "127.0.0.1:65535").unwrap();
        write_addr_file_atomic(&path, "127.0.0.1:80").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "127.0.0.1:80\n");
    }

    /// Why (#117): the probe is what stops a dead discovery file being
    /// returned, and it must decide fast.
    /// What: port 1 is reserved and unbound; asserts `false` well inside the
    /// deadline.
    /// Test: this test.
    #[test]
    fn address_reachable_returns_false_for_dead_port() {
        let start = Instant::now();
        assert!(!address_reachable_blocking("127.0.0.1:1"));
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "probe took too long: {:?}",
            start.elapsed()
        );
    }

    /// Why: a corrupted discovery file must not panic the resolver.
    /// What: three shapes that are not `host:port`.
    /// Test: this test.
    #[test]
    fn address_reachable_returns_false_for_garbage_input() {
        assert!(!address_reachable_blocking("not-a-host:port"));
        assert!(!address_reachable_blocking(""));
        assert!(!address_reachable_blocking("127.0.0.1"));
    }

    /// Why: positive control — a real bound port must register as reachable so
    /// the resolver does not fall back unnecessarily.
    /// What: binds an ephemeral listener and probes it.
    /// Test: this test.
    #[test]
    fn address_reachable_returns_true_for_live_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(address_reachable_blocking(&addr.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Why: `probe_once` against an unbound localhost port must return `false`
    /// without panicking, within a generous wall-clock bound.
    /// What: binds a `TcpListener` to port 0 to let the OS assign a free
    /// ephemeral port, reads that port, drops the listener to free it, then
    /// probes the now-guaranteed-unbound address. This avoids hard-coding port
    /// 65535 which can be bound on busy CI hosts.
    /// Test: this test.
    #[tokio::test]
    async fn probe_once_returns_false_for_refused_port() {
        // Bind port 0 to get a free OS-assigned port, then release it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let started = Instant::now();
        let ok = probe_once(&format!("http://127.0.0.1:{port}/health")).await;
        assert!(!ok, "probe must fail against an unbound port");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "probe took too long: {:?}",
            started.elapsed()
        );
    }

    /// Why (#5670): the not-installed case is the one `spawn_detached` failure a
    /// caller acts on differently from "it started and died", so the error has
    /// to name the program rather than a generic OS code.
    /// What: spawns a path that cannot exist and asserts the message quotes it.
    /// Test: this test.
    #[test]
    fn spawn_detached_reports_a_missing_program() {
        let err = spawn_detached("/nonexistent/trusty-nothing-here", &["serve"])
            .expect_err("a program that does not exist cannot be spawned");
        let msg = err.to_string();
        assert!(
            msg.contains("/nonexistent/trusty-nothing-here") && msg.contains("serve"),
            "the error must name the program and its arguments; got: {msg}"
        );
    }

    /// Why: a malformed URL must not panic — reqwest converts it to an error and
    /// `probe_once` must translate that to `false`.
    /// What: passes a non-URL string; asserts `false` is returned.
    /// Test: this test.
    #[tokio::test]
    async fn probe_once_returns_false_for_bad_url() {
        let ok = probe_once("not-a-valid-url").await;
        assert!(!ok);
    }

    /// Why: `spin_until_ready` must return `Ok(())` immediately when the
    /// health endpoint is already responsive.
    /// What: binds a real TCP listener that returns `HTTP/1.1 200 OK` on every
    /// connection, then calls `spin_until_ready`. Using a real listener avoids
    /// mocking the reqwest client while keeping the test hermetic.
    /// Test: this test.
    #[tokio::test]
    async fn spin_until_ready_returns_ok_for_live_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    tokio::spawn(async move {
                        use tokio::io::AsyncWriteExt;
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    });
                }
            }
        });
        tokio::time::sleep(Duration::from_millis(20)).await;

        let cfg = DaemonGuardConfig {
            health_url: format!("http://127.0.0.1:{port}/health"),
            service_name: "test-daemon".to_string(),
            startup_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(50),
            timeout_hint: "run `test-daemon start` to debug".to_string(),
        };
        let result = spin_until_ready(&cfg).await;
        assert!(
            result.is_ok(),
            "spin_until_ready must succeed when daemon is up: {result:?}"
        );
    }

    /// Why: when the daemon never starts, `spin_until_ready` must return `Err`
    /// after the timeout rather than looping forever.
    /// What: binds a `TcpListener` to port 0 to get a free OS-assigned port,
    /// drops the listener to free it, then spins against that now-unbound port
    /// with a very short timeout. This avoids using privileged port 1 which
    /// produces ETIMEDOUT instead of ECONNREFUSED on some macOS/BSD configs and
    /// can cause the test to stall for the full `PROBE_TIMEOUT` rather than
    /// returning immediately.
    /// Test: this test.
    #[tokio::test]
    async fn spin_until_ready_times_out_for_down_daemon() {
        // Bind port 0 to get a free OS-assigned port, then release it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let cfg = DaemonGuardConfig {
            health_url: format!("http://127.0.0.1:{port}/health"),
            service_name: "test-daemon".to_string(),
            startup_timeout: Duration::from_millis(200),
            poll_interval: Duration::from_millis(50),
            timeout_hint: "run `test-daemon start` to debug".to_string(),
        };
        let result = spin_until_ready(&cfg).await;
        assert!(
            result.is_err(),
            "spin_until_ready must fail when daemon never starts"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("test-daemon"),
            "error must name the service; got: {msg}"
        );
    }
}
