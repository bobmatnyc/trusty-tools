//! Runtime port discovery for trusty sidecar services.
//!
//! Why: trusty-memory and trusty-search do not use fixed ports — each service
//! picks its port from its own config.toml and writes the resolved address to a
//! well-known file (`~/.trusty-{service}/http_addr`) once the listener is bound.
//! Call sites must read this file at runtime; hardcoding port numbers breaks when
//! the service operator changes the config.
//!
//! What: `discover_addr` implements the three-step resolution: env override →
//! port file → fallback default.  `TrustyAddrs` groups the two resolved addresses
//! for convenient daemon startup.
//!
//! Test: `cargo test -p trusty-mpm-daemon discover` exercises file-present,
//! file-absent, malformed-file, and env-override cases without hitting the
//! network.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Default address for trusty-memory when `~/.trusty-memory/http_addr` is absent.
///
/// Why: must match the trusty-memory daemon's own `DEFAULT_HTTP_PORT` (7070,
/// see `crates/trusty-memory/src/lib.rs`) — otherwise clients that fall through
/// to this default hit a port nothing is listening on (issue: dream cycle log
/// flood). The trusty-memory daemon picks dynamically from 7070..=7079 and
/// writes the bound address to the lock file; the historical 3038 default did
/// not match any port the daemon ever binds.
/// What: `127.0.0.1:7070` — matches the trusty-memory daemon default.
/// Only used as a last resort — never embed this literal at call sites.
pub const TRUSTY_MEMORY_DEFAULT_ADDR: &str = "127.0.0.1:7070";

/// Default address for trusty-search when `~/.trusty-search/http_addr` is absent.
/// Only used as a last resort — never embed this literal at call sites.
pub const TRUSTY_SEARCH_DEFAULT_ADDR: &str = "127.0.0.1:7878";

const TRUSTY_MEMORY_DATA_DIR: &str = ".trusty-memory";
const TRUSTY_SEARCH_DATA_DIR: &str = ".trusty-search";
const HTTP_ADDR_FILE: &str = "http_addr";

/// Resolved addresses for both trusty sidecar services.
///
/// Why: groups the two addresses so the daemon's startup code passes them
/// through a single function call rather than two.
/// What: produced by `discover_all`; stored in daemon config.
/// Test: construct directly in unit tests to inject fake addresses.
#[derive(Debug, Clone)]
pub struct TrustyAddrs {
    /// Resolved HTTP address for trusty-memory.
    pub memory: SocketAddr,
    /// Resolved HTTP address for trusty-search.
    pub search: SocketAddr,
}

/// Resolves the HTTP address for a trusty sidecar service.
///
/// Why: trusty services write their bound address to a well-known file rather
/// than exposing a fixed port, so callers must discover the address at runtime.
/// What: reads `{data_dir}/http_addr`, falls back to `default_addr`; an
///       optional env var string overrides both.
/// Test: supply a temp dir with a known http_addr file; assert the returned
///       SocketAddr matches its contents.  Supply an absent file; assert the
///       default is returned.
pub async fn discover_addr(
    data_dir: &Path,
    default_addr: SocketAddr,
    env_override: Option<&str>,
) -> SocketAddr {
    // 1. Environment variable wins (set by integration tests or operator override).
    if let Some(raw) = env_override
        && let Ok(addr) = raw.trim().parse::<SocketAddr>()
    {
        return addr;
        // Malformed env var falls through to file.
    }

    // 2. Read the service-written port file.
    let port_file = data_dir.join(HTTP_ADDR_FILE);
    if let Ok(contents) = tokio::fs::read_to_string(&port_file).await
        && let Ok(addr) = contents.trim().parse::<SocketAddr>()
    {
        return addr;
        // Malformed file falls through to default.
    }

    // 3. Last resort: the compiled-in default.
    default_addr
}

/// Discovers both trusty service addresses in parallel.
///
/// Why: the daemon needs both addresses at startup; running discovery in parallel
/// avoids serializing two file reads.
/// What: reads `~/.trusty-memory/http_addr` and `~/.trusty-search/http_addr`
///       concurrently, applying env overrides and compiled defaults as fallbacks.
/// Test: see `tests::discover_all_with_files`.
pub async fn discover_all(home: &Path) -> TrustyAddrs {
    let memory_dir = home.join(TRUSTY_MEMORY_DATA_DIR);
    let search_dir = home.join(TRUSTY_SEARCH_DATA_DIR);

    let memory_default: SocketAddr = TRUSTY_MEMORY_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");
    let search_default: SocketAddr = TRUSTY_SEARCH_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");

    let memory_env = std::env::var("TRUSTY_MEMORY_ADDR").ok();
    let search_env = std::env::var("TRUSTY_SEARCH_ADDR").ok();

    let (memory, search) = tokio::join!(
        discover_addr(&memory_dir, memory_default, memory_env.as_deref()),
        discover_addr(&search_dir, search_default, search_env.as_deref()),
    );

    TrustyAddrs { memory, search }
}

/// Returns the path to the `http_addr` file for a given service data directory.
///
/// Why: lets callers log or monitor the file without re-deriving the path.
/// What: joins `data_dir` with the well-known filename `http_addr`.
/// Test: assert the returned path ends with `.trusty-memory/http_addr`.
#[allow(dead_code)] // Diagnostic helper for operators monitoring the port file.
pub fn addr_file(data_dir: &Path) -> PathBuf {
    data_dir.join(HTTP_ADDR_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_addr_file(dir: &TempDir, addr: &str) {
        let path = dir.path().join(HTTP_ADDR_FILE);
        let mut f = std::fs::File::create(path).unwrap();
        write!(f, "{addr}").unwrap();
    }

    #[tokio::test]
    async fn returns_addr_from_file() {
        let dir = TempDir::new().unwrap();
        write_addr_file(&dir, "127.0.0.1:9999");
        let default: SocketAddr = "127.0.0.1:3038".parse().unwrap();
        let addr = discover_addr(dir.path(), default, None).await;
        assert_eq!(addr, "127.0.0.1:9999".parse::<SocketAddr>().unwrap());
    }

    #[tokio::test]
    async fn falls_back_to_default_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let default: SocketAddr = "127.0.0.1:3038".parse().unwrap();
        let addr = discover_addr(dir.path(), default, None).await;
        assert_eq!(addr, default);
    }

    #[tokio::test]
    async fn falls_back_to_default_when_file_malformed() {
        let dir = TempDir::new().unwrap();
        write_addr_file(&dir, "not-an-address");
        let default: SocketAddr = "127.0.0.1:3038".parse().unwrap();
        let addr = discover_addr(dir.path(), default, None).await;
        assert_eq!(addr, default);
    }

    #[tokio::test]
    async fn env_override_wins_over_file() {
        let dir = TempDir::new().unwrap();
        write_addr_file(&dir, "127.0.0.1:9999");
        let default: SocketAddr = "127.0.0.1:3038".parse().unwrap();
        let addr = discover_addr(dir.path(), default, Some("127.0.0.1:5555")).await;
        assert_eq!(addr, "127.0.0.1:5555".parse::<SocketAddr>().unwrap());
    }

    #[tokio::test]
    async fn malformed_env_override_falls_through_to_file() {
        let dir = TempDir::new().unwrap();
        write_addr_file(&dir, "127.0.0.1:9999");
        let default: SocketAddr = "127.0.0.1:3038".parse().unwrap();
        let addr = discover_addr(dir.path(), default, Some("not-valid")).await;
        assert_eq!(addr, "127.0.0.1:9999".parse::<SocketAddr>().unwrap());
    }

    #[tokio::test]
    async fn discover_all_with_files() {
        let mem_dir = TempDir::new().unwrap();
        let srch_dir = TempDir::new().unwrap();
        write_addr_file(&mem_dir, "127.0.0.1:4001");
        write_addr_file(&srch_dir, "127.0.0.1:4002");

        // We can't call discover_all with custom dirs directly (it uses home),
        // so test the underlying discover_addr calls in parallel instead.
        let mem_default: SocketAddr = TRUSTY_MEMORY_DEFAULT_ADDR.parse().unwrap();
        let srch_default: SocketAddr = TRUSTY_SEARCH_DEFAULT_ADDR.parse().unwrap();
        let (memory, search) = tokio::join!(
            discover_addr(mem_dir.path(), mem_default, None),
            discover_addr(srch_dir.path(), srch_default, None),
        );
        assert_eq!(memory, "127.0.0.1:4001".parse::<SocketAddr>().unwrap());
        assert_eq!(search, "127.0.0.1:4002".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn addr_file_path_ends_with_http_addr() {
        let dir = PathBuf::from("/home/user/.trusty-memory");
        let p = addr_file(&dir);
        assert!(p.ends_with("http_addr"));
    }

    #[test]
    fn constants_parse_as_socket_addrs() {
        TRUSTY_MEMORY_DEFAULT_ADDR
            .parse::<SocketAddr>()
            .expect("TRUSTY_MEMORY_DEFAULT_ADDR must be a valid SocketAddr");
        TRUSTY_SEARCH_DEFAULT_ADDR
            .parse::<SocketAddr>()
            .expect("TRUSTY_SEARCH_DEFAULT_ADDR must be a valid SocketAddr");
    }
}
