//! Daemon HTTP-address file helpers.
//!
//! Why: Both trusty-search and trusty-memory persist their bound `host:port`
//! to disk so MCP clients and follow-up CLI invocations can discover where
//! the daemon ended up after auto-port-walking. Centralising the path layout
//! keeps the two projects in sync and prevents a third trusty-* daemon from
//! inventing yet another location.

use anyhow::{Context, Result};
use std::path::Path;

/// Filename used inside each app's data directory to record the daemon's
/// bound HTTP address. Kept as a module-level constant so writers and readers
/// can't drift.
///
/// Why: a single agreed-upon name means any consumer (CLI, MCP bridge) finds
/// the address file without per-daemon configuration.
/// What: the constant value `"http_addr"` — a plain UTF-8 filename.
/// Test: `daemon_addr_round_trips` relies on this name indirectly.
const DAEMON_ADDR_FILENAME: &str = "http_addr";

/// Write the daemon's bound HTTP address to the app's data directory.
///
/// Why: Both trusty-search and trusty-memory persist their bound `host:port`
/// to disk so MCP clients (and follow-up CLI invocations) can discover where
/// the daemon ended up after auto-port-walking. Centralising the path layout
/// keeps the two projects in sync and prevents a third trusty-* daemon from
/// inventing yet another location.
/// What: writes `addr` verbatim (no trailing newline) to
/// `{resolve_data_dir(app_name)}/http_addr`, creating the directory if it
/// doesn't yet exist. Atomic-overwrite semantics aren't required — the file
/// is rewritten on every daemon start.
/// Test: `daemon_addr_round_trips` writes then reads under a stubbed HOME and
/// confirms equality.
pub fn write_daemon_addr(app_name: &str, addr: &str) -> Result<()> {
    let dir = crate::data_dir::resolve_data_dir(app_name)?;
    let path = dir.join(DAEMON_ADDR_FILENAME);
    std::fs::write(&path, addr).with_context(|| format!("write daemon addr to {}", path.display()))
}

/// Read the daemon's HTTP address from the app's data directory.
///
/// Why: CLI commands and MCP clients need to discover the running daemon's
/// bound port. Returning `Option` lets callers distinguish "daemon never
/// started" (file absent) from "filesystem error" (permission denied, etc.)
/// without resorting to string matching on error messages.
/// What: reads `{resolve_data_dir(app_name)}/http_addr`, trims surrounding
/// whitespace, and returns `Some(addr)`. Returns `Ok(None)` iff the file
/// does not exist; any other I/O error propagates as `Err`.
/// Test: `daemon_addr_round_trips`, `read_daemon_addr_missing_returns_none`,
/// `contract_read_daemon_addr_separates_absent_from_failed`.
///
/// # Code Contract
/// Preconditions:
/// - None. `app_name` need not name a daemon that has ever run.
///
/// Postconditions:
/// - `Ok(None)` means the address file DOES NOT EXIST, and only that. It never
///   stands in for a read that failed.
/// - `Err` means the address is UNKNOWN — a permission denial, a corrupt data
///   directory, or any other I/O failure. Distinguishing it from `Ok(None)` is
///   this function's reason to be fallible, and is what lets callers avoid
///   string-matching on error messages.
/// - `Ok(Some(s))` returns the file's contents with surrounding whitespace
///   trimmed. `s` may be empty if the file is empty or blank; this function
///   does not validate that `s` is a well-formed address.
///
/// Invariants:
/// - Read-only: unlike `check_already_running`, it never deletes a stale file.
/// - The path it reads is the one `write_daemon_addr` writes and
///   `remove_daemon_addr` deletes; the three share one layout so a third
///   trusty-* daemon cannot invent another location.
pub fn read_daemon_addr(app_name: &str) -> Result<Option<String>> {
    let dir = crate::data_dir::resolve_data_dir(app_name)?;
    let path = dir.join(DAEMON_ADDR_FILENAME);
    match std::fs::read_to_string(&path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("read daemon addr from {}", path.display())),
    }
}

/// Remove the daemon's HTTP address file from the app's data directory.
///
/// Why: On graceful shutdown the daemon should clean up its discovery file so
/// that the console and CLI do not probe a stale address. Mirroring
/// `write_daemon_addr` keeps the remove symmetric with the write.
/// What: deletes `{resolve_data_dir(app_name)}/http_addr`; ignores
/// `NotFound` (idempotent — already gone or never written). Propagates any
/// other I/O error as `Err`.
/// Test: `daemon_addr_remove_cleans_up` and `daemon_addr_remove_nonexistent_ok`.
pub fn remove_daemon_addr(app_name: &str) -> Result<()> {
    let dir = crate::data_dir::resolve_data_dir(app_name)?;
    let path = dir.join(DAEMON_ADDR_FILENAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("remove daemon addr at {}", path.display())),
    }
}

/// Resolve the Unix socket a UDS-transport daemon binds (#6277, ADR-0032).
///
/// Why: ADR-0032 moves each trusty-* service's own transport onto UDS, and the
/// path is a CROSS-CRATE contract — `trusty-review` binds it while
/// `trusty-console` and `trusty-installer` dial it, and neither of those two
/// has a Cargo edge on `trusty-review` to import a constant from. Deriving the
/// path in each of the three crates is the drift this repo's common-entry-point
/// rule exists to stop: a server and a client that disagree about the path
/// produce a daemon that is up and a probe that reports it down, which is the
/// #4246 false-`down` class all over again.
///
/// What: `{resolve_data_dir(app_name)}/<app_name>.sock`. It sits beside the
/// `http_addr` file this module's other helpers own, because the question is
/// the same one — where does this daemon answer — and there is no reason for
/// two layouts. Deliberately NOT under [`crate::uds::scratch_socket_dir`]: the
/// scratch directory is for sockets a supervisor spawns and reaps
/// (`webhook_relay`), while a resident daemon's own endpoint belongs with the
/// rest of its persistent state, and a data-dir path is redirectable in tests
/// through `TRUSTY_DATA_DIR_OVERRIDE`.
///
/// Unconditional rather than behind the `uds` feature: a caller that only needs
/// the PATH (a doctor command printing it, a test planting a fixture) should
/// not have to compile the socket machinery to ask for it.
///
/// A caller that binds this path should expect `bind_hardened` to narrow the
/// containing data directory to `0700` — the socket is only unreachable to
/// another uid because its directory is.
///
/// # Errors
///
/// Whatever [`crate::data_dir::resolve_data_dir`] returns — the data directory
/// could not be resolved or created.
///
/// Test: `daemon_socket_path_sits_beside_the_addr_file`,
/// `daemon_socket_path_honours_the_data_dir_override`.
pub fn daemon_socket_path(app_name: &str) -> Result<std::path::PathBuf> {
    let dir = crate::data_dir::resolve_data_dir(app_name)?;
    Ok(dir.join(format!("{app_name}.sock")))
}

/// Probe whether an existing daemon recorded at `addr_file` is healthy and,
/// if so, return its base URL so the caller can refuse to start a duplicate.
///
/// Why: every trusty-* daemon (search, memory, mpm) historically port-walked on
/// boot. Invoking the `start` / `serve` command a second time silently spawned
/// a second instance on the next free port — splitting traffic between two
/// stores, doubling RSS, and confusing every client that resolves the address
/// from disk. The CLI must read the recorded address, ask the live process for
/// `/health`, and if both succeed report "already running" and exit 0 rather
/// than racing a duplicate process against the port walker.
/// What: returns `Some("http://<addr>")` only when (a) `addr_file` exists and
/// is readable, (b) its trimmed contents parse as a non-empty `host:port`, and
/// (c) an HTTP `GET http://<addr><health_path>` returns a 2xx within ~1.5 s.
/// Returns `None` on every other outcome. Stale-file cleanup: when the file
/// exists but the probe fails, the function best-effort deletes it so the
/// next caller does not chase the same dead address.
/// Test: `check_already_running_returns_none_when_file_missing`,
/// `check_already_running_returns_none_when_file_empty`,
/// `check_already_running_returns_none_when_address_dead`,
/// `check_already_running_returns_url_when_health_ok`.
pub async fn check_already_running(addr_file: &Path, health_path: &str) -> Option<String> {
    let raw = match std::fs::read_to_string(addr_file) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let addr = raw.trim();
    if addr.is_empty() {
        let _ = std::fs::remove_file(addr_file);
        return None;
    }
    let url = format!("http://{addr}");
    if crate::health_probe::probe_health(&url, health_path).await {
        Some(url)
    } else {
        let _ = std::fs::remove_file(addr_file);
        None
    }
}

/// Resolve a running daemon's discovered HTTP base URL (issue #2033).
///
/// Why: several trusty-mpm call sites (session-launch index registration,
/// decommission-time index removal, the orphan-index sweep) each need "if the
/// named daemon recorded a bound address, give me its `http://`-prefixed base
/// URL, else `None`" — and must NEVER fall back to a hardcoded/guessed port
/// (the #2030 discovery-first rule: a wrong guessed port produces worse
/// failures than a clean skip). Centralising the `read_daemon_addr` +
/// scheme-prefix dance here, instead of repeating it at each call site, keeps
/// every caller in sync and removes the duplication.
/// What: reads `{app_name}`'s recorded address via [`read_daemon_addr`]; when
/// present and non-blank, returns it prefixed with `http://` unless it
/// already carries a `http://`/`https://` scheme. Returns `None` when the
/// daemon has never started (no address file), the file is empty, or is
/// unreadable — callers treat `None` as "skip, daemon not discoverable"
/// rather than guessing a default port.
/// Test: `resolve_daemon_base_url_adds_scheme`,
/// `resolve_daemon_base_url_preserves_existing_scheme`,
/// `resolve_daemon_base_url_none_when_undiscoverable`,
/// `contract_read_daemon_addr_separates_absent_from_failed` (which carries this
/// function's contract too — see the comment mid-body for why the two share one
/// env-guarded block).
///
/// # Code Contract
/// Preconditions:
/// - None. `app_name` need not name a daemon that has ever run.
///
/// Postconditions:
/// - `Some(url)` is returned ONLY from an address the named daemon actually
///   recorded. No port, host, or scheme is ever guessed — the #2030
///   discovery-first rule, because a wrong guessed port fails worse than a
///   clean skip.
/// - A returned url carries exactly one scheme: the recorded address is
///   returned as-is when it already starts with `http://` or `https://`, and
///   prefixed with `http://` otherwise. It is never double-prefixed.
/// - `None` is returned when the daemon never started, when the address file is
///   empty or blank, and when it is unreadable. Callers read `None` as "skip,
///   not discoverable" — it is deliberately NOT distinguishable from an I/O
///   error here, because every one of those cases has the same correct
///   response.
/// - Total: never panics and never returns `Err`; I/O failure collapses to
///   `None`.
///
/// Invariants:
/// - Read-only, and does not probe the address — a returned url means
///   "recorded", never "reachable". Use `check_already_running` for liveness.
pub fn resolve_daemon_base_url(app_name: &str) -> Option<String> {
    match read_daemon_addr(app_name) {
        Ok(Some(addr)) if !addr.trim().is_empty() => Some(
            if addr.starts_with("http://") || addr.starts_with("https://") {
                addr
            } else {
                format!("http://{addr}")
            },
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_dir::{DATA_DIR_OVERRIDE_ENV, ENV_LOCK};
    use std::path::PathBuf;

    fn tempfile_like_dir() -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-common-test-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn daemon_addr_round_trips() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        write_daemon_addr(&app, "127.0.0.1:12345").unwrap();
        let got = read_daemon_addr(&app).unwrap();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(got.as_deref(), Some("127.0.0.1:12345"));
    }

    /// Why (#6277): the socket path is a cross-crate contract that three crates
    /// read and none of them shares a Cargo edge with the other two, so the
    /// layout has to be pinned here rather than re-derived at each call site.
    /// What: the socket is `<app>.sock` in the same directory `write_daemon_addr`
    /// writes `http_addr` into.
    /// Test: itself.
    #[test]
    fn daemon_socket_path_sits_beside_the_addr_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        // SAFETY: guarded by ENV_LOCK, this module's convention for the override.
        unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp) };

        let socket = daemon_socket_path("trusty-review").expect("resolve socket path");
        let dir = crate::data_dir::resolve_data_dir("trusty-review").expect("resolve data dir");

        unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };

        assert_eq!(socket, dir.join("trusty-review.sock"));
        assert_eq!(
            socket.parent(),
            Some(dir.as_path()),
            "the socket must live in the data dir, beside http_addr"
        );
    }

    /// Why (#6277): every consumer test — and the combined-PR integration test —
    /// redirects the daemon's endpoint by setting `TRUSTY_DATA_DIR_OVERRIDE`. A
    /// path helper that ignored the override would make those tests silently
    /// dial the developer's real daemon.
    /// What: the resolved socket sits under the override root.
    /// Test: itself.
    #[test]
    fn daemon_socket_path_honours_the_data_dir_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        // SAFETY: guarded by ENV_LOCK, this module's convention for the override.
        unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp) };

        let socket = daemon_socket_path("trusty-review").expect("resolve socket path");

        unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };

        assert!(
            socket.starts_with(&tmp),
            "{} must sit under the override root {}",
            socket.display(),
            tmp.display()
        );
    }

    // ── Code Contract tests (#5724, ADR-0047) ────────────────────────────────

    /// Why: `Ok(None)` and `Err` mean different things here, and the whole
    /// reason this function is fallible is to keep them apart — a caller that
    /// cannot tell "never started" from "could not read" ends up guessing.
    /// What: an absent file is `Ok(None)`; a present one is `Ok(Some(trimmed))`.
    /// It also carries resolve_daemon_base_url's contract; see the comment mid-body.
    /// Test: itself.
    #[test]
    fn contract_read_daemon_addr_separates_absent_from_failed() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile_like_dir();
        // SAFETY: guarded by ENV_LOCK, the convention this module's other tests
        // already use for the data-dir override.
        unsafe { std::env::set_var(DATA_DIR_OVERRIDE_ENV, &dir) };

        // Postcondition: absent file -> Ok(None), never Err, never Some("").
        assert_eq!(read_daemon_addr("contract-app").unwrap(), None);

        // Postcondition: present file -> Ok(Some(s)) with whitespace trimmed.
        write_daemon_addr("contract-app", "  127.0.0.1:9999\n").unwrap();
        assert_eq!(
            read_daemon_addr("contract-app").unwrap().as_deref(),
            Some("127.0.0.1:9999")
        );

        // Postcondition: no validation — a blank file is Some(""), not None.
        // resolve_daemon_base_url is what collapses that to None.
        write_daemon_addr("contract-app", "   ").unwrap();
        assert_eq!(
            read_daemon_addr("contract-app").unwrap().as_deref(),
            Some("")
        );

        // ── resolve_daemon_base_url's contract, in the SAME guarded block ────
        //
        // Deliberately not a second #[test]. `DATA_DIR_OVERRIDE_ENV` is
        // process-wide and `ENV_LOCK` only serialises the tests that take it —
        // several `credentials::resolver` and `memory_core::dream` tests read
        // the data dir without it. Each additional env-holding test widens the
        // window in which those observe an overridden data dir, and two more
        // was enough to turn that latent race into three reproducible failures
        // in a full `--lib` run. One block keeps this file's footprint as it
        // was. Fixing the race properly means putting every data-dir reader
        // behind the lock, which is not this PR's change.

        // Postcondition: never started -> None. No default port is invented.
        assert_eq!(resolve_daemon_base_url("contract-url-app"), None);

        // Postcondition: blank recorded address -> None.
        write_daemon_addr("contract-url-app", "   ").unwrap();
        assert_eq!(resolve_daemon_base_url("contract-url-app"), None);

        // Postcondition: exactly one scheme is added, never doubled.
        write_daemon_addr("contract-url-app", "127.0.0.1:8080").unwrap();
        let got = resolve_daemon_base_url("contract-url-app").unwrap();
        assert_eq!(got, "http://127.0.0.1:8080");
        assert_eq!(got.matches("http://").count(), 1);

        // Postcondition: an existing scheme is preserved, not prefixed again.
        for recorded in ["http://host:1/", "https://host:2/"] {
            write_daemon_addr("contract-url-app", recorded).unwrap();
            assert_eq!(
                resolve_daemon_base_url("contract-url-app").as_deref(),
                Some(recorded)
            );
        }

        unsafe { std::env::remove_var(DATA_DIR_OVERRIDE_ENV) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_daemon_addr_missing_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let got = read_daemon_addr(&app).unwrap();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(got.is_none(), "expected None when file absent, got {got:?}");
    }

    #[tokio::test]
    async fn check_already_running_returns_none_when_file_missing() {
        let tmp = tempfile_like_dir();
        let missing = tmp.join("does-not-exist");
        let got = check_already_running(&missing, "/health").await;
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn check_already_running_returns_none_when_file_empty() {
        let tmp = tempfile_like_dir();
        let path = tmp.join("http_addr");
        std::fs::write(&path, "   \n  ").unwrap();
        let got = check_already_running(&path, "/health").await;
        assert!(got.is_none());
        assert!(
            !path.exists(),
            "empty address file should be cleaned up by check_already_running"
        );
    }

    #[tokio::test]
    async fn check_already_running_returns_none_when_address_dead() {
        let tmp = tempfile_like_dir();
        let path = tmp.join("http_addr");
        std::fs::write(&path, "127.0.0.1:1\n").unwrap();
        let got = check_already_running(&path, "/health").await;
        assert!(got.is_none(), "dead address should map to None");
        assert!(
            !path.exists(),
            "stale address file should be cleaned up by check_already_running"
        );
    }

    #[tokio::test]
    async fn check_already_running_returns_url_when_health_ok() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await;
                let _ = sock.shutdown().await;
            }
        });
        let tmp = tempfile_like_dir();
        let path = tmp.join("http_addr");
        std::fs::write(&path, format!("{local}\n")).unwrap();
        let got = check_already_running(&path, "/health").await;
        assert_eq!(got.as_deref(), Some(format!("http://{local}").as_str()));
        assert!(
            path.exists(),
            "address file must be preserved when the daemon is healthy"
        );
        let _ = server.await;
    }

    /// Why: write → remove → read must yield None (file cleaned up).
    /// Test: this test.
    #[test]
    fn daemon_addr_remove_cleans_up() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-remove-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        write_daemon_addr(&app, "127.0.0.1:12345").unwrap();
        remove_daemon_addr(&app).unwrap();
        let got = read_daemon_addr(&app).unwrap();
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(
            got.is_none(),
            "addr file should be gone after remove, got {got:?}"
        );
    }

    /// Why: removing an addr file that was never written must succeed (idempotent).
    /// Test: this test.
    #[test]
    fn daemon_addr_remove_nonexistent_ok() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-remove-never-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let result = remove_daemon_addr(&app);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(result.is_ok(), "removing non-existent addr must succeed");
    }

    /// Why (#2033): a bare `host:port` recorded on disk must come back with an
    /// `http://` scheme prefixed so callers can hand it straight to `reqwest`.
    /// Test: this test.
    #[test]
    fn resolve_daemon_base_url_adds_scheme() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-base-url-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        write_daemon_addr(&app, "127.0.0.1:54321").unwrap();
        let got = resolve_daemon_base_url(&app);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(got.as_deref(), Some("http://127.0.0.1:54321"));
    }

    /// Why (#2033): an address that already carries a scheme (rare, but the
    /// contract must be idempotent) must not be double-prefixed.
    /// Test: this test.
    #[test]
    fn resolve_daemon_base_url_preserves_existing_scheme() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-base-url-scheme-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        write_daemon_addr(&app, "https://127.0.0.1:54321").unwrap();
        let got = resolve_daemon_base_url(&app);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert_eq!(got.as_deref(), Some("https://127.0.0.1:54321"));
    }

    /// Why (#2033): the discovery-first rule — an undiscoverable daemon must
    /// resolve to `None`, never a guessed default port.
    /// Test: this test.
    #[test]
    fn resolve_daemon_base_url_none_when_undiscoverable() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile_like_dir();
        unsafe {
            std::env::set_var(DATA_DIR_OVERRIDE_ENV, &tmp);
        }
        let app = format!(
            "trusty-test-daemon-base-url-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let got = resolve_daemon_base_url(&app);
        unsafe {
            std::env::remove_var(DATA_DIR_OVERRIDE_ENV);
        }
        assert!(got.is_none(), "undiscoverable daemon must resolve to None");
    }
}
