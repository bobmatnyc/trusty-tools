//! Daemon discovery + reachability helpers shared across CLI subcommands.
//!
//! Why: every subcommand that talks to the running daemon needs the same
//! "where is it listening?" logic — preferring the canonical
//! `~/.trusty-search/http_addr` file, falling back to the legacy port lockfile,
//! and finally to the compiled-in default port. Centralising it removes
//! duplication and gives `main.rs` a thinner footprint.
//! What: pure path resolvers and one async TCP probe.
//! Test: covered indirectly by every CLI subcommand that calls into the
//! daemon — `status`, `index`, `query`, `doctor`, etc.

use std::time::Duration;

/// Resolve the daemon's base URL.
///
/// Why: stdio MCP servers and CLI subcommands need to find the running daemon
/// without configuration. We check the canonical `~/.trusty-search/http_addr`
/// first (the new address-discovery contract, aligned with trusty-memory),
/// then fall back to the legacy port file
/// (`~/.local/share/trusty-search/daemon.port`) for backward compatibility,
/// and finally to `127.0.0.1:7878` if neither exists.
///
/// Defensive TCP probe (issue #117): if `http_addr` points at a dead address
/// (e.g. left behind by a SIGKILL'd `serve --http` that used to share this
/// file, or by a stopped daemon whose cleanup didn't run), we fall back to
/// `daemon.port` and overwrite `http_addr` with the live address so future
/// callers are fast. The probe is 200 ms — short enough to keep CLI startup
/// snappy when the file is current, long enough to tolerate a busy machine.
///
/// What: returns `http://{host}:{port}` (no trailing slash).
pub fn daemon_base_url() -> String {
    if let Some(addr) = read_http_addr_file() {
        // Quick reachability check so a stale file doesn't strand callers in
        // a 60s probe loop downstream (issue #117).
        if address_reachable_blocking(&addr) {
            return format!("http://{addr}");
        }
        // Stale file — fall through to the port-file fallback and refresh.
    }
    let port = daemon_port_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or(trusty_search::service::DEFAULT_PORT);

    // Refresh `http_addr` so subsequent calls skip the TCP probe entirely.
    // Best-effort: a write failure (no $HOME, read-only fs) is non-fatal.
    let live_addr = format!("127.0.0.1:{port}");
    if address_reachable_blocking(&live_addr) {
        let _ = refresh_http_addr_file(&live_addr);
    }
    format!("http://{live_addr}")
}

/// Synchronous, time-boxed TCP reachability check used by `daemon_base_url()`.
///
/// Why: `daemon_base_url()` is called from sync contexts (e.g. main.rs CLI
/// dispatch) and cannot easily `.await`. A blocking `TcpStream::connect_timeout`
/// is the simplest correct primitive — 200 ms is well below the perceptual
/// threshold for CLI startup.
/// What: parses `host:port`, attempts a TCP connect with a 200 ms deadline,
/// returns true on success. Any parse or connect error returns false.
/// Test: `daemon_base_url_falls_back_when_http_addr_dead` exercises the path.
fn address_reachable_blocking(host_port: &str) -> bool {
    use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
    let Ok(mut iter) = host_port.to_socket_addrs() else {
        return false;
    };
    let Some(addr): Option<SocketAddr> = iter.next() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Overwrite `~/.trusty-search/http_addr` with `host_port`.
///
/// Why: when `daemon_base_url()` discovers a stale `http_addr` and recovers via
/// the port file, we update the discovery file in place so the next caller hits
/// the fast path. Atomic via tmp+rename to avoid partial reads.
/// What: writes `host_port` followed by a newline, then renames over the target.
/// Test: indirectly via `daemon_base_url_falls_back_when_http_addr_dead`.
fn refresh_http_addr_file(host_port: &str) -> std::io::Result<()> {
    use std::io::Write;
    let Some(path) = http_addr_path() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no $HOME",
        ));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("addr.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        writeln!(f, "{host_port}")?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read the canonical address-discovery file. Returns `Some("host:port")`
/// when the daemon has written it; `None` otherwise.
pub fn read_http_addr_file() -> Option<String> {
    let path = http_addr_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Path to `~/.trusty-search/http_addr` — the canonical address-discovery
/// file. Mirrors `crate::service::daemon::http_addr_path` so the CLI doesn't
/// need to depend on the service crate for path resolution.
pub fn http_addr_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".trusty-search").join("http_addr"))
}

/// Path to `~/.trusty-search/mcp_http_addr` — the MCP HTTP/SSE listener's
/// address-discovery file, written by `trusty-search serve --http`.
///
/// Why: distinct from the daemon's `http_addr` so two unrelated processes
/// (the daemon and a `serve --http` MCP transport) cannot clobber each other.
/// Before issue #117 both wrote the same file; a SIGKILL'd `serve --http`
/// would leave a dead-address `http_addr` behind, stranding subsequent
/// `trusty-search dash`/`status` calls in a 60s timeout loop.
/// What: returns `$HOME/.trusty-search/mcp_http_addr`.
pub fn mcp_http_addr_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".trusty-search").join("mcp_http_addr"))
}

/// Path to the daemon port file (`daemon.port` under the resolved data dir).
///
/// Why: the port file records which TCP port the running daemon bound, so CLI
/// subcommands (`status`, `index`, `query`) can discover the daemon without
/// configuration. When `TRUSTY_DATA_DIR` is set (by `--data-dir` or the env
/// var), the port file lives in that directory so an isolated test/cert daemon
/// does not collide with the production daemon's port file (issue #281).
/// What: returns `$TRUSTY_DATA_DIR/daemon.port` when the env var is set,
/// otherwise `<data_local_dir>/trusty-search/daemon.port`.
/// Test: set `TRUSTY_DATA_DIR=/tmp/ts-x`; assert path equals
/// `/tmp/ts-x/daemon.port`.
pub fn daemon_port_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("TRUSTY_DATA_DIR") {
        return Some(std::path::PathBuf::from(dir).join("daemon.port"));
    }
    dirs::data_local_dir().map(|d| d.join("trusty-search").join("daemon.port"))
}

/// Check whether a TCP port is open (non-blocking connect with 500 ms timeout).
pub async fn port_reachable(host: &str, port: u16) -> bool {
    let addr = format!("{}:{}", host, port);
    tokio::time::timeout(
        Duration::from_millis(500),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_reachable_returns_false_for_dead_port() {
        // Why: regression coverage for issue #117 — `daemon_base_url()` must
        // detect a dead address read from `http_addr` so it can fall back to
        // the port file instead of returning a URL nobody can connect to.
        // What: port 1 is reserved and unbound on every developer machine.
        // Test: the probe returns false in well under the 200 ms deadline.
        let start = std::time::Instant::now();
        assert!(!address_reachable_blocking("127.0.0.1:1"));
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "probe took too long: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn address_reachable_returns_false_for_garbage_input() {
        // Why: defence-in-depth — a corrupted `http_addr` (zero bytes,
        // partial write, hand-edited typo) must not panic the resolver.
        assert!(!address_reachable_blocking("not-a-host:port"));
        assert!(!address_reachable_blocking(""));
        assert!(!address_reachable_blocking("127.0.0.1"));
    }

    #[test]
    fn address_reachable_returns_true_for_live_listener() {
        // Why: positive control — a real bound port must register as reachable
        // so we don't fall back unnecessarily.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(address_reachable_blocking(&addr.to_string()));
    }

    #[test]
    fn http_addr_and_mcp_http_addr_paths_are_distinct() {
        // Why: the entire issue #117 fix hinges on these two files being
        // separate. A regression that re-unifies them would re-introduce the
        // 60s timeout bug.
        let http = http_addr_path();
        let mcp = mcp_http_addr_path();
        if let (Some(h), Some(m)) = (http, mcp) {
            assert_ne!(h, m);
            assert!(h.ends_with("http_addr"));
            assert!(m.ends_with("mcp_http_addr"));
        }
    }
}
