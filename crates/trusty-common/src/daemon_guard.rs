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
//!
//! No TCP address resolver lives here. #9957 removed `DaemonAddrLayout`, its
//! one layout `TRUSTY_SEARCH` and that layout's `default_port: 7878`, because
//! trusty-search binds no TCP port (#9214). This import must not compile:
//!
//! ```compile_fail
//! use trusty_common::daemon_guard::DaemonAddrLayout;
//! ```
//!
//! STDOUT hygiene: like `mcp::daemon_bridge`, this module NEVER writes to
//! stdout. All user-visible output (spinner, ready/timeout messages) goes to
//! stderr so stdout stays clean for JSON piping and MCP framing.
//!
//! Test: `probe_once_returns_false_for_refused_port` and
//! `spin_until_ready_returns_ok_for_live_server` exercise the core paths
//! without requiring a real daemon binary.

use std::io::Write;
use std::path::Path;
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
// Address file write (#5670)
// ---------------------------------------------------------------------------

/// Write a `host:port` discovery line to `path`, atomically.
///
/// Why: the address file is read by other processes at arbitrary moments, so a
/// partially-written file must never be observable (#3602 review, #5670).
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

/// Probe the given health URL once; returns `true` on any 2xx HTTP response.
///
/// Why: a fresh `reqwest::Client` per probe avoids carrying connection-pool
/// state from a failed probe to a later successful one, keeping the logic
/// simple and predictable across cold/warm starts.
/// What: builds a one-shot reqwest client with `PROBE_TIMEOUT`, issues a GET,
/// returns `true` on any 2xx status and `false` on any error or non-2xx.
/// Test: `probe_once_returns_false_for_refused_port` (async unit test below);
/// the proxy immunity is proven in `http_client::tests`.
pub async fn probe_once(health_url: &str) -> bool {
    // #4392: the target is a loopback daemon, so the client must not honour an
    // exported HTTP_PROXY.
    let client = match crate::http_client::loopback_client_builder()
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
    spawn_built(detached_command(program, args, None))
}

/// [`spawn_detached`], but the child keeps this process's parent-death stamp.
///
/// Why (#7085): a detached daemon started ON BEHALF of a stamped process must
/// die with that process, not with the short-lived CLI that forked it — a stdio
/// bridge auto-starting a daemon is exactly that shape, and it produced six
/// orphans per full `trusty-memory` test run. Forwarding is a separate entry
/// point rather than the default so the linkage is a decision a call site makes
/// and a reader can grep for, and so a stray `TRUSTY_EXIT_WITH_PARENT` in an
/// operator's environment can never arm a daemon nobody asked to link.
/// What: identical to [`spawn_detached`] except that
/// [`crate::parent_death::ENV_EXIT_WITH_PARENT`] is copied from this process
/// when it is set, rather than scrubbed. No stamp here means no stamp on the
/// child, which is every production invocation.
///
/// # Errors
///
/// As [`spawn_detached`].
///
/// Test: `detached_command_forwards_an_explicit_parent_stamp`.
pub fn spawn_detached_forwarding_parent_link(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
) -> Result<u32> {
    let stamp = crate::parent_death::inherited_stamp();
    spawn_built(detached_command(program, args, stamp.as_deref()))
}

/// Build the detached `Command` both entry points spawn.
///
/// Why it is separate: the env decision is the whole of #7085's review finding,
/// and a `Command` can be inspected in a unit test where a spawned daemon
/// cannot.
/// What: null stdio, a new session ([`start_in_new_session`]), plus
/// [`crate::parent_death::ENV_EXIT_WITH_PARENT`] either removed (`stamp` is
/// `None`, the default) or set to `stamp`.
/// Test: `detached_command_scrubs_the_parent_stamp`,
/// `detached_command_forwards_an_explicit_parent_stamp`,
/// `a_detached_daemon_survives_a_sigkill_of_its_spawners_group`.
fn detached_command(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    stamp: Option<&str>,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // #8783: out of the spawner's process group, so a group kill aimed at the
    // spawner (trusty-audit's budget kill, a terminal Ctrl-C) spares the daemon.
    start_in_new_session(&mut cmd);
    // #7085: scrub by default so an inherited stamp cannot silently arm a
    // daemon; a caller that wants the link asks for it by name.
    match stamp {
        Some(value) => cmd.env(crate::parent_death::ENV_EXIT_WITH_PARENT, value),
        None => cmd.env_remove(crate::parent_death::ENV_EXIT_WITH_PARENT),
    };
    cmd
}

/// Make the process `cmd` spawns lead a new session: its own process group and
/// no controlling terminal (Unix; a no-op elsewhere).
///
/// Why (#8783): a daemon auto-started by a short-lived CLI otherwise inherits
/// that CLI's process group, so any kill aimed at the group reaches the daemon
/// too — `trusty-audit` SIGKILLing `tga audit`'s group on a budget timeout, or a
/// terminal delivering Ctrl-C to its foreground group. `setsid(2)` is chosen
/// over `process_group(0)` because it also drops the controlling terminal, so a
/// terminal hangup or job-control signal can never reach the daemon either; a
/// daemon has no use for a terminal, and its stdio is null.
/// What: installs a `pre_exec` hook that runs [`become_session_leader`] in the
/// forked child; a failure fails the spawn rather than starting a daemon still
/// in the group. A plain forked child never leads a group, so on its own the
/// hook does not fail. It DOES fail, with `EPERM`, when the same `Command` also
/// sets `process_group` (or a `pre_exec` of its own calls `setpgid`) first:
/// the child then leads a group, and a group leader cannot call `setsid`.
/// Public so a daemon start that needs its own stdio or cwd — and so cannot use
/// [`spawn_detached`] — gets the same treatment instead of a second copy.
/// Test: `a_detached_daemon_survives_a_sigkill_of_its_spawners_group`,
/// `a_new_session_combined_with_process_group_fails_the_spawn_with_eperm`.
pub fn start_in_new_session(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: the hook runs in the forked child before `exec`, where only
        // async-signal-safe calls are allowed; see `become_session_leader`.
        unsafe {
            cmd.pre_exec(become_session_leader);
        }
    }
    cmd
}

/// The `pre_exec` body behind [`start_in_new_session`]: `setsid()`, errno on failure.
///
/// Why: `uds::supervisor` starts its detached children through a tokio
/// `Command`, whose `pre_exec` is a separate method; both share this one body.
/// It runs between `fork` and `exec`, so it makes async-signal-safe calls only:
/// `setsid` is one, and building an `io::Error` from errno does not allocate.
/// Test: `a_new_session_combined_with_process_group_fails_the_spawn_with_eperm`.
#[cfg(unix)]
pub(crate) fn become_session_leader() -> std::io::Result<()> {
    // SAFETY: `setsid` takes no arguments and touches only the calling process.
    if unsafe { libc::setsid() } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Spawn a prepared detached command, rendering a failure with what was tried.
fn spawn_built(mut cmd: std::process::Command) -> Result<u32> {
    let rendered = format!("{cmd:?}");
    let child = cmd
        .spawn()
        .map_err(|e| anyhow!("could not spawn `{rendered}`: {e}"))?;
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
/// What: resolves `current_exe()` and hands it to [`spawn_detached`], so the
/// child carries no parent-death stamp.
/// Test: compile-only (spawning a real process in unit tests risks port/FS
/// side-effects; the live path is exercised by integration tests).
pub fn spawn_current_exe(args: &[&str]) -> Result<u32> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("could not resolve current_exe: {e}"))?;
    spawn_detached(&exe, args)
}

/// [`spawn_current_exe`], forwarding this process's parent-death stamp (#7085).
///
/// Why: the daemon a stdio bridge auto-starts must die with whoever started the
/// bridge. See [`spawn_detached_forwarding_parent_link`] for why forwarding is
/// opt-in rather than the default.
///
/// # Errors
///
/// As [`spawn_current_exe`].
///
/// Test: `detached_command_forwards_an_explicit_parent_stamp` covers the env
/// decision; `crates/trusty-memory/tests/orphan_reap_7085.rs` covers the live
/// grandchild path.
pub fn spawn_current_exe_forwarding_parent_link(args: &[&str]) -> Result<u32> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("could not resolve current_exe: {e}"))?;
    spawn_detached_forwarding_parent_link(&exe, args)
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

#[cfg(all(test, unix))]
#[path = "daemon_guard_session_tests.rs"]
mod session_tests;

#[cfg(test)]
mod addr_tests {
    //! `write_addr_file_atomic` coverage (#5670).

    use super::*;

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

    /// Why: a rewrite overwrites a stale file, so a shorter new address
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

    /// Why (#7085 review): a detached daemon inherits the whole environment, so
    /// a `TRUSTY_EXIT_WITH_PARENT` that happened to be set would silently arm a
    /// watchdog in a daemon nobody asked to link. The default spawn must clear
    /// it, and `env_remove` surfaces in `get_envs` as an explicit `None`.
    /// What: builds the default detached command and asserts the variable is
    /// scheduled for removal rather than merely absent from the override list.
    /// Test: this test.
    #[test]
    fn detached_command_scrubs_the_parent_stamp() {
        let cmd = detached_command("/bin/true", &["serve"], None);
        let entry = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(crate::parent_death::ENV_EXIT_WITH_PARENT));
        assert_eq!(
            entry.map(|(_, v)| v),
            Some(None),
            "the default detached spawn must REMOVE the parent-death stamp"
        );
    }

    /// Why: the forwarding entry point is what keeps a test-started daemon
    /// linked through an intermediate CLI; if it did not carry the value the six
    /// orphans per suite run would come back.
    /// What: builds the forwarding command with an explicit stamp and asserts it
    /// is set to that value.
    /// Test: this test.
    #[test]
    fn detached_command_forwards_an_explicit_parent_stamp() {
        let cmd = detached_command("/bin/true", &["serve"], Some("4242"));
        let value = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(crate::parent_death::ENV_EXIT_WITH_PARENT))
            .and_then(|(_, v)| v)
            .expect("the forwarding spawn must set the parent-death stamp");
        assert_eq!(value, std::ffi::OsStr::new("4242"));
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
