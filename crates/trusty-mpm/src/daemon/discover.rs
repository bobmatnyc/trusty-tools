//! Runtime port discovery for trusty sidecar services.
//!
//! Why: trusty-search does not use a fixed port — it picks one from its own
//! config.toml and writes the resolved address to a well-known file
//! (`~/.trusty-search/http_addr`) once the listener is bound. Call sites must
//! read this file at runtime; hardcoding a port number breaks when the operator
//! changes the config.
//!
//! **trusty-memory is not here any more (#6286).** ADR-0032 moved it onto a
//! Unix socket, so it writes no `http_addr` and has no port to discover — its
//! path is derived by `trusty_common::memory_rpc::resolve_memory_socket`. The
//! `~/.trusty-memory/http_addr` this module used to read is still on disk on
//! every machine that ran the old daemon, which is exactly why the read had to
//! go rather than be left to fall through to a default.
//!
//! What: `discover_addr` implements the three-step resolution: env override →
//! port file → fallback default. `TrustyAddrs` carries what is left.
//!
//! Test: `cargo test -p trusty-mpm-daemon discover` exercises file-present,
//! file-absent, malformed-file, and env-override cases without hitting the
//! network.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Default address for trusty-search when `~/.trusty-search/http_addr` is absent.
/// Only used as a last resort — never embed this literal at call sites.
pub const TRUSTY_SEARCH_DEFAULT_ADDR: &str = "127.0.0.1:7878";

const TRUSTY_SEARCH_DATA_DIR: &str = ".trusty-search";
const HTTP_ADDR_FILE: &str = "http_addr";

/// Resolved addresses for the trusty sidecar services that still serve HTTP.
///
/// Why: a struct rather than a bare `SocketAddr` so the daemon's startup code
/// keeps one call site as members migrate. #6286 removed the `memory` field:
/// trusty-memory serves a Unix socket, and nothing read that field anyway.
/// What: produced by `discover_all`; stored in daemon config.
/// Test: construct directly in unit tests to inject fake addresses.
#[derive(Debug, Clone)]
pub struct TrustyAddrs {
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
/// Why: the daemon needs the address at startup, and keeping the call here
/// means a future member joins in one place.
/// What: reads `~/.trusty-search/http_addr`, applying the env override and the
/// compiled default as fallbacks.
/// Test: see `tests::discover_all_with_files`.
pub async fn discover_all(home: &Path) -> TrustyAddrs {
    let search_dir = home.join(TRUSTY_SEARCH_DATA_DIR);
    let search_default: SocketAddr = TRUSTY_SEARCH_DEFAULT_ADDR
        .parse()
        .expect("static default is valid");
    let search_env = std::env::var("TRUSTY_SEARCH_ADDR").ok();

    let search = discover_addr(&search_dir, search_default, search_env.as_deref()).await;
    TrustyAddrs { search }
}

/// Returns the path to the `http_addr` file for a given service data directory.
///
/// Why: lets callers log or monitor the file without re-deriving the path.
/// What: joins `data_dir` with the well-known filename `http_addr`.
/// Test: assert the returned path ends with `.trusty-search/http_addr`.
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
        let srch_dir = TempDir::new().unwrap();
        write_addr_file(&srch_dir, "127.0.0.1:4002");

        // We can't call discover_all with a custom dir directly (it uses home),
        // so test the underlying discover_addr call instead.
        let srch_default: SocketAddr = TRUSTY_SEARCH_DEFAULT_ADDR.parse().unwrap();
        let search = discover_addr(srch_dir.path(), srch_default, None).await;
        assert_eq!(search, "127.0.0.1:4002".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn addr_file_path_ends_with_http_addr() {
        let dir = PathBuf::from("/home/user/.trusty-search");
        let p = addr_file(&dir);
        assert!(p.ends_with("http_addr"));
    }

    #[test]
    fn constants_parse_as_socket_addrs() {
        TRUSTY_SEARCH_DEFAULT_ADDR
            .parse::<SocketAddr>()
            .expect("TRUSTY_SEARCH_DEFAULT_ADDR must be a valid SocketAddr");
    }
}
