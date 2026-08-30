//! Low-level detection helpers shared by all service connectors.
//!
//! Why: Centralising the binary-probe, TCP-probe, and HTTP-health helpers
//! avoids duplicating them across every connector and makes them independently
//! testable.
//! What: Five free functions — `binary_on_path`, `read_addr_file`, `tcp_probe`,
//! `fetch_health_version`, `binary_version` — plus the shared `detect_service`
//! orchestrator.
//! Test: Unit tests at the bottom of this file. Run with
//! `cargo test -p trusty-console`.

use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::connector::{ServiceInfo, ServiceLifecycle, ServiceStatus};

// ─── primitive helpers ────────────────────────────────────────────────────────

/// Return true if `binary` is found on PATH.
///
/// Why: The binary presence check is the outermost gate — if the binary isn't
/// installed, no daemon can ever be running.
/// What: Delegates to the `which` crate.
/// Test: Covered indirectly by every connector test; mocked via PATH override.
pub(super) fn binary_on_path(binary: &str) -> bool {
    which::which(binary).is_ok()
}

/// Read an `http_addr` file and return the trimmed address string.
///
/// Why: All three daemons write the bound address (e.g. `127.0.0.1:7879`) to a
/// well-known file on successful bind. Reading that file is cheaper than a
/// TCP probe and gives us the exact port without parsing config files.
/// What: Reads `path`, trims whitespace. Returns `None` if the file is absent,
/// empty, or unreadable.
/// Test: `test_read_addr_file_*` below.
pub(super) fn read_addr_file(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// TCP-probe `host:port` with a 300 ms connect timeout.
///
/// Why: The discovery file may be stale (daemon crashed without cleanup). A
/// fast TCP probe confirms the port is actually open before we call it Running.
/// What: Attempts a non-blocking TCP connect with a 300 ms timeout. Returns
/// `true` only on a successful connection. Returns `false` immediately if
/// `addr` cannot be parsed as a `SocketAddr` (no fallback to a random port,
/// which could produce a misleading `false` connection result).
/// Test: `test_tcp_probe_unreachable` and `test_tcp_probe_malformed_addr` below.
pub(super) fn tcp_probe(addr: &str) -> bool {
    let Ok(socket_addr) = addr.parse() else {
        return false;
    };
    TcpStream::connect_timeout(&socket_addr, Duration::from_millis(300))
        .map(|s| {
            drop(s);
            true
        })
        .unwrap_or(false)
}

/// Fetch `/health` from `host:port` and extract the `version` field.
///
/// Why: When the daemon is Running we surface the version in the card so
/// operators can see at a glance which build is deployed.
/// What: Issues a minimal HTTP/1.0 GET over a raw `TcpStream` with a 1 s
/// read timeout. HTTP/1.0 is used deliberately — servers do not send
/// `Transfer-Encoding: chunked` in HTTP/1.0 responses, so the raw body can
/// be parsed without a chunked-transfer decoder. Uses no external HTTP
/// client so detect() stays sync-only without pulling in reqwest blocking.
/// Returns `None` on any error (network, parse, missing field) — the caller
/// degrades gracefully.
/// Test: Not tested against a real daemon in unit tests; the server
/// integration test in `server.rs` verifies the overall JSON shape.
pub(super) fn fetch_health_version(addr: &str) -> Option<String> {
    use std::io::{Read, Write};

    let mut stream =
        TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(800)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(800)))
        .ok()?;

    // HTTP/1.0: server must not use Transfer-Encoding: chunked, so we can
    // read the body directly after the blank-line separator.
    let request = format!("GET /health HTTP/1.0\r\nHost: {addr}\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;

    let raw = String::from_utf8_lossy(&buf);
    // Split headers from body on blank line.
    let body_start = raw.find("\r\n\r\n").map(|i| i + 4)?;
    let body = &raw[body_start..];

    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    json.get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ─── version probe (on-demand members) ────────────────────────────────────────

/// How long `<binary> --version` may take before the probe gives up.
///
/// A cold `trusty-review` start was measured at 191 ms (#5028) and `--version`
/// does strictly less than that — clap prints and exits before any config,
/// credential or network work. Two seconds is far past any honest answer and
/// still short enough that one wedged binary cannot stall the console's whole
/// detection pass.
const VERSION_TIMEOUT: Duration = Duration::from_secs(2);

/// What running `<binary> --version` established.
///
/// Why: "no version string" and "the binary cannot execute" are opposite facts
/// that a bare `Option<String>` collapses into the same `None` (#6290). The
/// second is the exact state the spawn exists to catch, and reporting it as
/// `Available` also puts the console at odds with `tctl`, whose
/// `probe_presence` calls it `ProbeFailed`.
pub(super) enum VersionProbe {
    /// `--version` ran to completion; the payload is the parsed version, which
    /// is `None` only when the output did not have clap's `<name> <version>`
    /// shape. The binary works either way.
    Ran(Option<String>),
    /// On `PATH` but not runnable — a broken signature, a truncated download, a
    /// hang. The payload is the operator-facing reason.
    CannotExecute(String),
}

/// Run `<binary> --version` and report what happened.
///
/// Why: for an on-demand member the binary IS the service, so presence plus a
/// clean run is the whole health question — and the run yields the version the
/// card renders. Presence alone would report a binary that cannot execute (a
/// broken signature, a truncated download) as usable.
/// What: spawns `<binary> --version`, waits up to [`VERSION_TIMEOUT`], and takes
/// the second whitespace-separated token of the first line (clap's
/// `<name> <version>` shape). A non-zero exit, a spawn failure, or a hang is
/// [`VersionProbe::CannotExecute`], never a silent `None`.
/// Test: `a_binary_that_cannot_execute_is_not_available`,
/// `binary_version_reads_the_version_off_a_real_binary`.
pub(super) fn binary_version(binary: &str) -> VersionProbe {
    let mut child = match std::process::Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return VersionProbe::CannotExecute(format!("spawn `{binary} --version`: {e}")),
    };

    // `wait_timeout` is not in std; poll the child instead of blocking forever
    // on a binary that hangs before printing.
    let deadline = std::time::Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                let _ = child.wait();
                return VersionProbe::CannotExecute(format!(
                    "`{binary} --version` exited {status}"
                ));
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return VersionProbe::CannotExecute(format!(
                    "`{binary} --version` did not exit within {VERSION_TIMEOUT:?}"
                ));
            }
            Err(e) => {
                return VersionProbe::CannotExecute(format!(
                    "waiting on `{binary} --version`: {e}"
                ));
            }
        }
    }

    match child.wait_with_output() {
        Ok(output) => VersionProbe::Ran(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .map(str::to_owned),
        ),
        Err(e) => VersionProbe::CannotExecute(format!("reading `{binary} --version`: {e}")),
    }
}

// ─── orchestrator ─────────────────────────────────────────────────────────────

/// Strip all leading `http://` or `https://` scheme prefixes from a bare
/// address string.
///
/// Why: Discovery addr files should contain bare `host:port` (e.g.
/// `127.0.0.1:7788`), but a malformed or misconfigured file might include one
/// or more scheme prefixes.  Building `format!("http://{addr}")` with a
/// scheme-prefixed addr produces a double-scheme URL
/// (`http://http://127.0.0.1:7788`) that reqwest fails to parse.
/// Stripping all leading schemes before composing guarantees exactly one
/// well-formed `http://host:port` URL.
/// What: Delegates to `crate::url_util::strip_schemes` — the single shared
/// implementation backing both this function and `proxy::routes::normalize_base_url`
/// so the loop logic cannot drift between the two sites.  Returns a slice of
/// `addr`, allocation-free on the hot path.
/// Test: `test_normalize_addr_*` below; `url_util::tests` cover the core loop.
fn normalize_addr(addr: &str) -> &str {
    crate::url_util::strip_schemes(addr)
}

/// Run the standard P0 detection sequence for a service.
///
/// Why: All three connectors use the same three-step sequence; extracting it
/// avoids triplicating the logic.
/// What: Returns a fully-populated `ServiceInfo`. When the binary is absent
/// the function returns early with `Absent`. When the addr file is present and
/// the TCP probe succeeds it returns `Running` + optional version. Otherwise
/// `Available`. The lifecycle is always `Daemon` (#6416): this sequence reads a
/// discovery file a resident daemon writes on bind, so only daemon members
/// (mpm, agents) route through it.
/// Test: Each connector's unit test calls the connector's `detect()` method
/// with a custom `addr_file` path pointing into a tmpdir.
pub(super) fn detect_service(
    id: &'static str,
    display_name: &'static str,
    binary: &str,
    addr_file: PathBuf,
) -> ServiceInfo {
    if !binary_on_path(binary) {
        return ServiceInfo {
            id: id.to_string(),
            display_name: display_name.to_string(),
            status: ServiceStatus::Absent,
            version: None,
            url: None,
            hint: None,
            lifecycle: ServiceLifecycle::Daemon,
        };
    }

    if let Some(addr) = read_addr_file(&addr_file)
        && tcp_probe(&addr)
    {
        // Normalize the addr to strip any accidental scheme prefix before
        // building the base URL — prevents a double-scheme like
        // `http://http://127.0.0.1:7788` if the addr file was written with
        // the scheme included (#1849 Phase 2 hardening).
        let base_url = format!("http://{}", normalize_addr(&addr));
        let version = fetch_health_version(&addr);
        return ServiceInfo {
            id: id.to_string(),
            display_name: display_name.to_string(),
            status: ServiceStatus::Running,
            version,
            url: Some(base_url),
            hint: None,
            lifecycle: ServiceLifecycle::Daemon,
        };
    }

    ServiceInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        status: ServiceStatus::Available,
        version: None,
        url: None,
        hint: None,
        lifecycle: ServiceLifecycle::Daemon,
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ── read_addr_file ──────────────────────────────────────────────────────

    /// Why: round-trips a valid address file.
    /// What: writes `127.0.0.1:9999` to a temp file; asserts read_addr_file returns it.
    /// Test: this test itself.
    #[test]
    fn test_read_addr_file_returns_trimmed_content() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("http_addr");
        fs::write(&path, "  127.0.0.1:9999\n").expect("write");
        assert_eq!(read_addr_file(&path), Some("127.0.0.1:9999".to_string()));
    }

    /// Why: absent file must yield None so callers degrade to Available.
    /// What: calls read_addr_file on a non-existent path.
    /// Test: this test itself.
    #[test]
    fn test_read_addr_file_absent_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        assert_eq!(read_addr_file(&tmp.path().join("no_file")), None);
    }

    /// Why: empty file must yield None (can happen if daemon wrote then crashed).
    /// What: writes an empty file and calls read_addr_file.
    /// Test: this test itself.
    #[test]
    fn test_read_addr_file_empty_returns_none() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("http_addr");
        fs::write(&path, "   \n").expect("write");
        assert_eq!(read_addr_file(&path), None);
    }

    // ── tcp_probe ───────────────────────────────────────────────────────────

    /// Why: a closed port must yield false (not panic or block).
    /// What: binds port 0 (OS assigns a free port), captures the addr, drops
    /// the listener so the port closes, then asserts tcp_probe returns false.
    /// Using an OS-assigned port avoids CI flap from hardcoded port numbers.
    /// Test: this test itself.
    #[test]
    fn test_tcp_probe_unreachable() {
        use std::net::TcpListener;
        // Bind to get a free OS port, then drop so the port is closed.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
        let addr = listener.local_addr().expect("local_addr").to_string();
        drop(listener);
        assert!(!tcp_probe(&addr), "closed port must return false");
    }

    /// Why: a listening port must yield true.
    /// What: binds port 0, keeps the listener alive, asserts tcp_probe returns
    /// true against that addr, then drops the listener to release the port.
    /// Test: this test itself.
    #[test]
    fn test_tcp_probe_reachable() {
        use std::net::TcpListener;
        // Keep the listener alive while probing — port must be open.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
        let addr = listener.local_addr().expect("local_addr").to_string();
        assert!(tcp_probe(&addr), "listening port must return true");
        drop(listener);
    }

    /// Why: malformed address must not panic.
    /// What: calls tcp_probe with a non-SocketAddr string.
    /// Test: this test itself.
    #[test]
    fn test_tcp_probe_malformed_addr() {
        assert!(!tcp_probe("not-an-addr"));
    }

    // ── normalize_addr ──────────────────────────────────────────────────────

    /// Why: a bare `host:port` must pass through unchanged.
    /// What: calls normalize_addr with `127.0.0.1:7788`; asserts no change.
    /// Test: this test itself.
    #[test]
    fn test_normalize_addr_bare_unchanged() {
        assert_eq!(normalize_addr("127.0.0.1:7788"), "127.0.0.1:7788");
    }

    /// Why: an addr file that was erroneously written with an `http://` scheme
    /// (e.g. `http://127.0.0.1:7788`) must have the scheme stripped so that
    /// `format!("http://{}", normalize_addr(...))` produces exactly one scheme.
    /// What: calls normalize_addr with `http://127.0.0.1:7788`; asserts scheme
    /// is stripped, returning `127.0.0.1:7788`.
    /// Test: this test itself (double-scheme regression guard for #1849 Phase 2).
    #[test]
    fn test_normalize_addr_strips_http_scheme() {
        assert_eq!(normalize_addr("http://127.0.0.1:7788"), "127.0.0.1:7788");
    }

    /// Why: an https-prefixed addr must also have the scheme stripped.
    /// What: calls normalize_addr with `https://127.0.0.1:7788`; asserts `127.0.0.1:7788`.
    /// Test: this test itself.
    #[test]
    fn test_normalize_addr_strips_https_scheme() {
        assert_eq!(normalize_addr("https://127.0.0.1:7788"), "127.0.0.1:7788");
    }

    // ── binary_version ──────────────────────────────────────────────────────

    /// REGRESSION (#6290): a binary that cannot execute is not `Available`.
    ///
    /// Why: `binary_version` returned `None` for BOTH "no version in the output"
    /// and "the binary exited non-zero", and `detect` reported `Available`
    /// regardless — the exact "cannot execute" state the spawn exists to catch,
    /// reported as usable. It also disagreed with `tctl`, whose `probe_presence`
    /// calls the same host `ProbeFailed`.
    /// What: probes a binary that exists and always exits 1 (`/usr/bin/false`),
    /// and asserts the outcome carries the reason.
    #[cfg(unix)]
    #[test]
    fn a_binary_that_cannot_execute_is_not_available() {
        match binary_version("/usr/bin/false") {
            VersionProbe::CannotExecute(why) => {
                assert!(
                    why.contains("exited"),
                    "the operator needs the reason: {why}"
                );
            }
            VersionProbe::Ran(v) => {
                panic!("a binary exiting 1 must not read as a clean run, got {v:?}")
            }
        }
    }

    /// Why (#6416): both on-demand connectors render the card's version off this
    /// one probe now, so the clap `<name> <version>` parse has to keep working.
    /// What: probes `cargo`, which every machine running this test has.
    /// Test: this test itself.
    #[test]
    fn binary_version_reads_the_version_off_a_real_binary() {
        if which::which("cargo").is_err() {
            eprintln!("skip: no cargo on PATH to probe as a stand-in binary");
            return;
        }
        match binary_version("cargo") {
            VersionProbe::Ran(version) => {
                assert!(version.is_some(), "`cargo --version` prints a version");
            }
            VersionProbe::CannotExecute(why) => panic!("cargo must run: {why}"),
        }
    }
}
