//! Which process is the trusty-mpm daemon — the single entry point.
//!
//! Why: two defects had the same cause, that nothing in the codebase could
//! authoritatively answer "which process is the daemon". #1731: the lock file
//! at `~/.trusty-mpm/daemon.lock` carried no identity, so any process could
//! delete it and any same-named file from another product could be read as
//! ours. #5951: `tm services list` picked the daemon's PID with
//! `pgrep -f trusty-mpm`, whose first hit on this machine is a
//! `trusty-mpm serve --stdio` MCP bridge, not the daemon.
//!
//! What: owns the lock-file record (write, parse, ownership-checked removal)
//! and the port-owner lookup. Every reader and writer of the lock file routes
//! through here — before this module there were three independent hand-rolled
//! parsers in this crate alone (`core::discovery`, `daemon::lock`, the `tm`
//! welcome panel), each with its own idea of what a valid lock file is.
//!
//! Test: `parse_lock_*`, `read_lock_*`, `remove_lock_owned_by_*`,
//! `parse_lsof_pid_*` below.

use serde::{Deserialize, Serialize};

use super::discovery::lock_file_path;

/// Value of the lock file's `product` field written by this daemon.
///
/// Why (#1731): the filename `daemon.lock` is not unique to trusty-mpm.
/// Claude Code writes `daemon.lock` too — one lives at
/// `~/.trusty-tools/trusty-mpm/claude-config/daemon.lock` naming an unrelated
/// PID, inside a path that contains "trusty-mpm". A reader that trusts the
/// path alone can hand an operator someone else's PID to inspect or signal.
/// What: [`parse_lock`] rejects any file whose `product` is absent or
/// different, so trusting the contents requires the file to say it is ours.
/// Test: `parse_lock_rejects_foreign_product`, `parse_lock_rejects_missing_product`.
pub const LOCK_PRODUCT: &str = "trusty-mpm";

/// The daemon's on-disk identity record.
///
/// Why: clients discover the daemon's actual bound address from this file —
/// the daemon falls back to an ephemeral port when 7880 is busy, so the
/// compiled-in default is not always right.
/// What: serialised as TOML at [`lock_file_path`]. `product` is the
/// [`LOCK_PRODUCT`] magic; `pid` identifies the writing process so removal can
/// be ownership-checked; `addr` is the base URL clients connect to;
/// `socket_path` is the Unix socket the same daemon serves (#6288).
/// Test: `parse_lock_round_trips`, `parse_lock_reads_a_pre_6288_record`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonLock {
    /// Product magic — must equal [`LOCK_PRODUCT`] for the record to be trusted.
    pub product: String,
    /// PID of the daemon process that wrote this file.
    pub pid: u32,
    /// Base URL the daemon bound, e.g. `http://127.0.0.1:7880`.
    pub addr: String,
    /// RFC-3339 timestamp of the write, for operator diagnostics only.
    #[serde(default)]
    pub started_at: String,
    /// Unix socket the daemon serves alongside `addr` (#6288, ADR-0032).
    ///
    /// Why record a path that [`crate::daemon::socket::socket_path`] derives
    /// anyway: a consumer reading this file learns whether the daemon that
    /// wrote it serves a socket AT ALL. Empty means a pre-#6288 daemon is
    /// running — HTTP only — which is the fact a client picking a transport
    /// needs and cannot get from a path it computed itself.
    ///
    /// `#[serde(default)]` for the same reason `started_at` carries it: a lock
    /// written by the previous version has no such key, and refusing to parse
    /// it would make every running daemon invisible across an upgrade.
    #[serde(default)]
    pub socket_path: String,
}

/// Parse a lock-file body, rejecting anything that does not claim to be ours.
///
/// Why (#1731): this is the check that makes the lock file safe to act on.
/// Without it the reader's only evidence is the path, and a same-named file
/// from another product parses far enough to yield a PID.
/// What: TOML-decodes into [`DaemonLock`] and returns `None` unless `product`
/// equals [`LOCK_PRODUCT`]. Malformed TOML, a missing `product`, and a foreign
/// `product` are all the same answer: not ours, do not act on it.
/// Test: `parse_lock_round_trips`, `parse_lock_rejects_foreign_product`,
/// `parse_lock_rejects_missing_product`, `parse_lock_rejects_claude_code_lock`.
pub fn parse_lock(text: &str) -> Option<DaemonLock> {
    let lock: DaemonLock = toml::from_str(text).ok()?;
    (lock.product == LOCK_PRODUCT).then_some(lock)
}

/// Serialise a lock record to the TOML body written to disk.
///
/// Test: `parse_lock_round_trips`.
pub fn render_lock(lock: &DaemonLock) -> String {
    toml::to_string(lock).unwrap_or_else(|e| {
        // A fixed set of scalar fields cannot fail TOML encoding; if it somehow
        // does, an empty body is rejected by `parse_lock` rather than written
        // as a half-record that a reader might trust.
        tracing::warn!("failed to encode daemon lock: {e}");
        String::new()
    })
}

/// Is `pid` a live process?
///
/// What: `kill(pid, 0)` on Unix; assumes alive on other platforms, where the
/// lock file's PID cannot be checked.
/// Test: `pid_alive_true_for_self`, `read_lock_rejects_dead_pid`.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `kill` with signal 0 performs the permission/existence check
        // only; it delivers no signal and cannot affect the target process.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Read the daemon's lock record, if one belongs to us and its PID is alive.
///
/// Why: the single read path for every consumer — URL resolution, the `tm`
/// welcome panel, and the daemon's own duplicate-instance guard.
/// What: reads [`lock_file_path`], rejects a foreign record via [`parse_lock`],
/// then verifies the PID. A stale record (ours, dead PID) is deleted; a
/// foreign file is left untouched, because deleting another product's file is
/// the same mistake as reading it.
/// Test: `read_lock_returns_written_record`, `read_lock_rejects_dead_pid`,
/// `read_lock_leaves_foreign_file_in_place`.
pub fn read_lock_at(path: &std::path::Path) -> Option<DaemonLock> {
    let lock = parse_lock(&std::fs::read_to_string(path).ok()?)?;
    if pid_alive(lock.pid) {
        return Some(lock);
    }
    let _ = std::fs::remove_file(path);
    None
}

/// [`read_lock_at`] against the real `~/.trusty-mpm/daemon.lock`.
pub fn read_lock() -> Option<DaemonLock> {
    read_lock_at(&lock_file_path())
}

/// Write the lock file naming `addr`, `socket`, and the current process.
///
/// What: creates parent directories, then writes the TOML record. Best-effort:
/// a failure is logged, never fatal — the daemon still serves. `socket` is the
/// Unix socket path this daemon serves alongside `addr` (#6288).
/// Test: `read_lock_returns_written_record`.
pub fn write_lock_at(path: &std::path::Path, addr: &str, socket: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let lock = DaemonLock {
        product: LOCK_PRODUCT.to_string(),
        pid: std::process::id(),
        addr: addr.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        // #6288: recorded so a reader can tell a socket-serving daemon from a
        // pre-#6288 one without dialling a path it guessed.
        socket_path: socket.to_string(),
    };
    match std::fs::write(path, render_lock(&lock)) {
        Ok(()) => tracing::debug!("lock file written: {}", path.display()),
        Err(e) => tracing::warn!("failed to write daemon lock file {}: {e}", path.display()),
    }
}

/// [`write_lock_at`] against the real `~/.trusty-mpm/daemon.lock`.
pub fn write_lock(addr: &str, socket: &str) {
    write_lock_at(&lock_file_path(), addr, socket);
}

/// Remove the lock file only when it names one of `owners`.
///
/// Why (#1731): unconditional removal is how the write was lost. A daemon
/// going down removed whatever lock file was present, so a restart in which
/// the incoming daemon bound and wrote its record before the outgoing daemon
/// finished its shutdown handler left a running daemon with no lock file —
/// exactly the state the reopened issue reports. The same applies to `tm stop`,
/// which cleans up after the PIDs it signalled, not after a daemon that
/// started since.
/// What: reads the record, and deletes the file only if its `pid` is in
/// `owners`. A foreign or newer record is left alone.
/// Test: `remove_lock_owned_by_removes_own_record`,
/// `remove_lock_owned_by_keeps_newer_daemons_record`,
/// `remove_lock_owned_by_keeps_foreign_file`.
pub fn remove_lock_owned_by_at(path: &std::path::Path, owners: &[u32]) {
    let Some(lock) = std::fs::read_to_string(path)
        .ok()
        .as_deref()
        .and_then(parse_lock)
    else {
        // Absent, unreadable, or not ours — nothing this process may delete.
        return;
    };
    if !owners.contains(&lock.pid) {
        tracing::debug!(
            "leaving daemon lock in place: it names pid {}, not {owners:?}",
            lock.pid
        );
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => tracing::debug!("lock file removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("failed to remove lock file: {e}"),
    }
}

/// [`remove_lock_owned_by_at`] against the real `~/.trusty-mpm/daemon.lock`.
pub fn remove_lock_owned_by(owners: &[u32]) {
    remove_lock_owned_by_at(&lock_file_path(), owners);
}

/// Extract the first PID from `lsof -t` output.
///
/// What: `-t` prints one bare PID per line; several appear when a process has
/// forked while holding the listening descriptor. The first is the listener.
/// Test: `parse_lsof_pid_reads_first`, `parse_lsof_pid_empty`,
/// `parse_lsof_pid_ignores_garbage`.
pub fn parse_lsof_pid(stdout: &str) -> Option<u32> {
    stdout.split_whitespace().next()?.parse().ok()
}

/// PID of the process listening on `port`, if any.
///
/// Why (#5951): the process bound to the port IS the service. Matching a
/// process name instead reports whichever same-named sibling the process table
/// happens to yield first — on this machine `pgrep -f trusty-mpm` returns a
/// `trusty-mpm serve --stdio` MCP bridge ahead of the daemon.
/// What: `lsof -t -nP -iTCP:<port> -sTCP:LISTEN`, first PID. Returns `None`
/// when nothing listens, when `lsof` is absent, or when the port is held by a
/// process this user cannot see — callers fall back to name matching.
/// Test: `parse_lsof_pid_reads_first` covers the parse; the spawn is exercised
/// live by `tests/services_integration.rs`.
pub fn pid_listening_on(port: u16) -> Option<u32> {
    let out = std::process::Command::new("lsof")
        .args(["-t", "-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_lsof_pid(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DaemonLock {
        DaemonLock {
            product: LOCK_PRODUCT.to_string(),
            pid: std::process::id(),
            addr: "http://127.0.0.1:7880".to_string(),
            started_at: "2026-08-18T03:45:07.813744+00:00".to_string(),
            socket_path: "/tmp/trusty-mpm/trusty-mpm.sock".to_string(),
        }
    }

    #[test]
    fn parse_lock_round_trips() {
        let lock = sample();
        assert_eq!(parse_lock(&render_lock(&lock)), Some(lock));
    }

    /// #6288, the backward half: this daemon must still read a lock written by
    /// the previous version.
    ///
    /// Why: an upgrade does not restart a running daemon. If a record without
    /// `socket_path` failed to parse, `read_lock` would return `None` and every
    /// consumer — URL resolution, the `tm` welcome panel, the daemon's own
    /// duplicate-instance guard — would report no daemon while one was serving,
    /// and the guard would go on to start a second.
    /// What: the exact pre-#6288 body, with every other field asserted intact
    /// and `socket_path` empty rather than absent.
    /// Test: this function IS the test.
    #[test]
    fn parse_lock_reads_a_pre_6288_record() {
        let body = "product = \"trusty-mpm\"\npid = 84890\n\
                    addr = \"http://127.0.0.1:7880\"\n\
                    started_at = \"2026-08-18T03:45:07.813744+00:00\"\n";
        let lock = parse_lock(body).expect("a pre-#6288 record must still parse");
        assert_eq!(lock.pid, 84890);
        assert_eq!(lock.addr, "http://127.0.0.1:7880");
        assert_eq!(
            lock.socket_path, "",
            "an absent socket_path means HTTP-only, not a parse failure"
        );
    }

    /// #6288, the forward half: a record this daemon writes stays readable by
    /// the parsers that predate the new field.
    ///
    /// Why: `trusty-console` (`detect/mpm.rs`) and the `tm` welcome panel read
    /// `addr` out of this file. A new key must not displace or shadow it.
    /// What: renders a record carrying a socket path, then asserts the TOML
    /// still carries `addr` on its own line with the URL intact, and that a
    /// reader keyed on the exact `addr` key finds it.
    /// Test: this function IS the test.
    #[test]
    fn render_lock_keeps_addr_readable_alongside_socket_path() {
        let body = render_lock(&sample());
        let addr_line = body
            .lines()
            .find(|l| l.starts_with("addr ="))
            .expect("addr must still be a top-level key");
        assert!(
            addr_line.contains("http://127.0.0.1:7880"),
            "addr line lost its value: {addr_line}"
        );
        assert!(
            body.lines().any(|l| l.starts_with("socket_path =")),
            "socket_path must be recorded: {body}"
        );
    }

    #[test]
    fn parse_lock_rejects_foreign_product() {
        let body =
            "product = \"some-other-daemon\"\npid = 84890\naddr = \"http://127.0.0.1:7880\"\n";
        assert_eq!(parse_lock(body), None);
    }

    #[test]
    fn parse_lock_rejects_missing_product() {
        // The pre-#1731 format. It is indistinguishable from any other
        // product's TOML lock, so it is no longer trusted.
        let body = "pid = 84890\naddr = \"http://127.0.0.1:7880\"\n";
        assert_eq!(parse_lock(body), None);
    }

    /// #1731: the only `daemon.lock` on the reporting machine belongs to
    /// Claude Code, not trusty-mpm, and sits inside a path containing
    /// "trusty-mpm". Reading it and reporting its `pid` would send an operator
    /// to an unrelated process.
    #[test]
    fn parse_lock_rejects_claude_code_lock() {
        let body = r#"{
  "pid": 84890,
  "version": "2.1.225",
  "launchTarget": "/Users/masa/.local/share/claude/versions/2.1.225"
}"#;
        assert_eq!(parse_lock(body), None);
    }

    #[test]
    fn pid_alive_true_for_self() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn read_lock_returns_written_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        write_lock_at(
            &path,
            "http://127.0.0.1:7880",
            "/tmp/trusty-mpm/trusty-mpm.sock",
        );
        let lock = read_lock_at(&path).expect("own lock file must be readable");
        assert_eq!(lock.product, LOCK_PRODUCT);
        assert_eq!(lock.pid, std::process::id());
        assert_eq!(lock.addr, "http://127.0.0.1:7880");
        assert!(!lock.started_at.is_empty());
        // #6288: `addr` is unchanged and `socket_path` rides alongside it.
        assert_eq!(lock.socket_path, "/tmp/trusty-mpm/trusty-mpm.sock");
    }

    #[test]
    fn read_lock_rejects_dead_pid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        // PID 0 is never a live user process; `kill(0, 0)` addresses the
        // caller's process group, so use a high PID that cannot be running.
        std::fs::write(
            &path,
            "product = \"trusty-mpm\"\npid = 4194303\naddr = \"http://127.0.0.1:7880\"\nstarted_at = \"\"\n",
        )
        .expect("write");
        assert_eq!(read_lock_at(&path), None);
        assert!(!path.exists(), "a stale record of ours is cleaned up");
    }

    #[test]
    fn read_lock_leaves_foreign_file_in_place() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        let body = "product = \"claude-code\"\npid = 4194303\naddr = \"http://127.0.0.1:7880\"\n";
        std::fs::write(&path, body).expect("write");
        assert_eq!(read_lock_at(&path), None);
        assert!(path.exists(), "another product's file must not be deleted");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), body);
    }

    #[test]
    fn remove_lock_owned_by_removes_own_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        write_lock_at(
            &path,
            "http://127.0.0.1:7880",
            "/tmp/trusty-mpm/trusty-mpm.sock",
        );
        remove_lock_owned_by_at(&path, &[std::process::id()]);
        assert!(!path.exists());
    }

    /// #1731: the regression mechanism. An outgoing daemon must not delete the
    /// record an incoming daemon has already written, or a restart leaves a
    /// live daemon with no lock file.
    #[test]
    fn remove_lock_owned_by_keeps_newer_daemons_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        write_lock_at(
            &path,
            "http://127.0.0.1:7880",
            "/tmp/trusty-mpm/trusty-mpm.sock",
        );
        let outgoing = std::process::id() + 1;
        remove_lock_owned_by_at(&path, &[outgoing]);
        assert!(path.exists(), "the incoming daemon's record must survive");
        assert_eq!(
            read_lock_at(&path).expect("still ours").pid,
            std::process::id()
        );
    }

    #[test]
    fn remove_lock_owned_by_keeps_foreign_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("daemon.lock");
        let body = "product = \"claude-code\"\npid = 4194303\n";
        std::fs::write(&path, body).expect("write");
        remove_lock_owned_by_at(&path, &[4194303]);
        assert!(
            path.exists(),
            "a PID match is not ownership without the magic"
        );
    }

    #[test]
    fn parse_lsof_pid_reads_first() {
        assert_eq!(parse_lsof_pid("48689\n48690\n"), Some(48689));
    }

    #[test]
    fn parse_lsof_pid_empty() {
        assert_eq!(parse_lsof_pid(""), None);
        assert_eq!(parse_lsof_pid("\n  \n"), None);
    }

    #[test]
    fn parse_lsof_pid_ignores_garbage() {
        assert_eq!(parse_lsof_pid("lsof: not found"), None);
    }
}
