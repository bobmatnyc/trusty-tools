//! Bind-address resolution for trusty-console.
//!
//! Why: The console needs to be reachable from tailnet clients across restarts
//! without manual `--http 0.0.0.0:7788` overrides. This module centralises all
//! bind-address logic — env-var defaults, `--tailscale` flag handling, and
//! Tailscale-IPv4 detection — behind a clean, mockable boundary so the
//! resolution logic can be unit-tested without a real tailnet.
//! What: Exports `resolve_bind_addrs`, which returns the ordered list of
//! `SocketAddr`s the server should bind; and `detect_tailscale_ipv4`, which
//! shells out to `tailscale ip -4` (or accepts an injected command for tests).
//! Test: `tests` module below; no real tailnet required.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

use anyhow::{Context, Result};
use tracing::{info, warn};

// ─── public types ────────────────────────────────────────────────────────────

/// How the server should bind its listeners.
///
/// Why: Captures the three meaningful bind modes so `resolve_bind_addrs` can
/// return the right `SocketAddr` list for each without tangled string parsing.
/// What: Three variants — local-only (default), tailscale (dual listener), or
/// explicit (whatever the `--http` flag / `TRUSTY_CONSOLE_BIND` env var says).
/// Test: Constructed by `BindMode::from_env_and_flags`; exercised below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindMode {
    /// Bind only `127.0.0.1:<port>` — default.
    Local,
    /// Bind `127.0.0.1:<port>` AND the detected Tailscale IPv4 `<ts-ip>:<port>`.
    Tailscale,
    /// Bind the explicit address string (e.g. `0.0.0.0:7788` or `127.0.0.1:9000`).
    Explicit(String),
}

impl BindMode {
    /// Determine the bind mode from the env var and CLI flags.
    ///
    /// Why: Encodes the precedence rule — explicit `--http` wins, then
    /// `TRUSTY_CONSOLE_BIND`, then `--tailscale`, then local default — in one
    /// place so callers never need to re-implement it.
    /// What: Reads `TRUSTY_CONSOLE_BIND` from the process environment and
    /// delegates to `from_flags_and_bind_env`; see that function for full
    /// precedence rules.
    ///
    /// Test: `test_bind_mode_*` tests call `from_flags_and_bind_env` directly
    /// with injected values to avoid parallel-test env-var races.
    pub fn from_env_and_flags(
        explicit_http: &str,
        default_http: &str,
        tailscale_flag: bool,
    ) -> Self {
        let bind_env = std::env::var("TRUSTY_CONSOLE_BIND").ok();
        Self::from_flags_and_bind_env(
            explicit_http,
            default_http,
            tailscale_flag,
            bind_env.as_deref(),
        )
    }

    /// Pure, deterministic core of bind-mode resolution (no env reads).
    ///
    /// Why: Separating the env read from the logic makes this function fully
    /// testable without process-global env mutation — important because tests
    /// run in parallel threads and `set_var`/`remove_var` races cause flakiness.
    ///
    /// What: Applies a four-level precedence rule — `--http` override > env
    /// `"tailscale"` > env non-empty addr > `--tailscale` flag > local default.
    ///
    /// Test: `test_bind_mode_*` below call this function directly.
    pub fn from_flags_and_bind_env(
        explicit_http: &str,
        default_http: &str,
        tailscale_flag: bool,
        bind_env: Option<&str>,
    ) -> Self {
        // 1. Explicit `--http` override (user changed it from the default).
        if explicit_http != default_http {
            return BindMode::Explicit(explicit_http.to_owned());
        }

        // 2. TRUSTY_CONSOLE_BIND env var.
        if let Some(val) = bind_env {
            let val = val.trim().to_lowercase();
            if val == "tailscale" {
                return BindMode::Tailscale;
            }
            if !val.is_empty() {
                return BindMode::Explicit(val);
            }
        }

        // 3. --tailscale flag.
        if tailscale_flag {
            return BindMode::Tailscale;
        }

        // 4. Default: local-only.
        BindMode::Local
    }
}

// ─── address resolution ───────────────────────────────────────────────────────

/// Resolve the list of `SocketAddr`s to bind based on `mode` and `port`.
///
/// Why: Separating address resolution from bind/listen lets tests verify the
/// correct addresses are produced without opening real sockets.
/// What: Returns 1-2 `SocketAddr`s. In `Tailscale` mode runs
/// `detect_tailscale_ipv4` — if that fails, logs a warning and falls back to
/// local-only.  In `Explicit` mode parses the string directly.
/// Test: `test_resolve_bind_addrs_*` below; `detect_tailscale_ipv4` is
/// injected via the `ip_detector` closure for unit-test isolation.
pub fn resolve_bind_addrs(
    mode: &BindMode,
    default_port: u16,
    ip_detector: impl FnOnce() -> Option<IpAddr>,
) -> Vec<SocketAddr> {
    match mode {
        BindMode::Local => {
            let addr = SocketAddr::from(([127, 0, 0, 1], default_port));
            vec![addr]
        }

        BindMode::Tailscale => {
            let loopback = SocketAddr::from(([127, 0, 0, 1], default_port));
            match ip_detector() {
                Some(ts_ip) => {
                    let ts_addr = SocketAddr::new(ts_ip, default_port);
                    info!("tailscale mode: binding loopback and {ts_addr}");
                    vec![loopback, ts_addr]
                }
                None => {
                    warn!(
                        "tailscale mode requested but could not detect Tailscale IPv4 — \
                         falling back to localhost-only"
                    );
                    vec![loopback]
                }
            }
        }

        BindMode::Explicit(addr_str) => match SocketAddr::from_str(addr_str) {
            Ok(addr) => vec![addr],
            Err(e) => {
                warn!("could not parse bind address {addr_str:?}: {e}; falling back to localhost");
                vec![SocketAddr::from(([127, 0, 0, 1], default_port))]
            }
        },
    }
}

// ─── Tailscale IP detection ───────────────────────────────────────────────────

/// Detect the machine's Tailscale IPv4 address by running `tailscale ip -4`.
///
/// Why: The canonical way to find the tailnet IP without parsing routing tables
/// or iterating interfaces. The `tailscale` CLI is already required to use the
/// tailnet, so it is a safe runtime dependency.
/// What: Spawns `tailscale ip -4`, trims the output, parses it as an `IpAddr`.
/// Returns `None` (with a tracing warning) when Tailscale is not installed,
/// not running, or the output is unparseable.
/// Test: Not called in unit tests — replaced by the `ip_detector` closure in
/// `resolve_bind_addrs`; integration-tested via `--tailscale` on a live machine.
pub fn detect_tailscale_ipv4() -> Option<IpAddr> {
    let output = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output();

    match output {
        Err(e) => {
            warn!("could not run `tailscale ip -4`: {e}");
            None
        }
        Ok(out) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(
                "tailscale ip -4 exited with status {}: {stderr}",
                out.status
            );
            None
        }
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let raw = stdout.trim();
            match IpAddr::from_str(raw) {
                Ok(ip) => {
                    info!("detected Tailscale IPv4: {ip}");
                    Some(ip)
                }
                Err(e) => {
                    warn!("could not parse Tailscale IP {raw:?}: {e}");
                    None
                }
            }
        }
    }
}

/// Parse the port from an explicit bind address string, falling back to `default`.
///
/// Why: In Tailscale mode we need the port from the `--http` default when
/// constructing dual-listener addresses; this helper avoids duplicating the
/// parsing in `run_serve`.
/// What: Attempts `addr.parse::<SocketAddr>().port()`; returns `default` on
/// failure.
/// Test: `test_port_from_addr` below.
pub fn port_from_addr(addr: &str, default: u16) -> u16 {
    addr.parse::<SocketAddr>()
        .map(|a| a.port())
        .unwrap_or(default)
}

/// Bind a TCP listener, logging and returning a descriptive error on failure.
///
/// Why: Wraps `TcpListener::bind` with uniform context so callers don't need
/// to format their own error messages for each bind attempt.
/// What: Awaits `tokio::net::TcpListener::bind(addr)` and attaches context.
/// Test: Not unit-tested (network I/O); exercised by `run_serve` integration.
pub async fn bind_listener(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    // ── BindMode::from_flags_and_bind_env ────────────────────────────────────
    //
    // All tests call the pure `from_flags_and_bind_env` overload so they are
    // deterministic and free of process-global env mutation (which causes flaky
    // races when tests run in parallel threads).

    /// Why: --http override must produce Explicit regardless of flags/env.
    /// What: passes a non-default http value with tailscale_flag=true; asserts Explicit.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_explicit_http_wins() {
        let mode = BindMode::from_flags_and_bind_env(
            "0.0.0.0:9000",
            "127.0.0.1:7788",
            true,
            Some("tailscale"),
        );
        assert_eq!(mode, BindMode::Explicit("0.0.0.0:9000".to_owned()));
    }

    /// Why: TRUSTY_CONSOLE_BIND=tailscale must produce Tailscale when --http is default.
    /// What: passes bind_env=Some("tailscale"); asserts Tailscale.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_env_tailscale() {
        let mode = BindMode::from_flags_and_bind_env(
            "127.0.0.1:7788",
            "127.0.0.1:7788",
            false,
            Some("tailscale"),
        );
        assert_eq!(mode, BindMode::Tailscale);
    }

    /// Why: TRUSTY_CONSOLE_BIND=TAILSCALE (uppercase) must still produce Tailscale.
    /// What: passes bind_env=Some("TAILSCALE"); asserts case-insensitive match.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_env_tailscale_uppercase() {
        let mode = BindMode::from_flags_and_bind_env(
            "127.0.0.1:7788",
            "127.0.0.1:7788",
            false,
            Some("TAILSCALE"),
        );
        assert_eq!(mode, BindMode::Tailscale);
    }

    /// Why: TRUSTY_CONSOLE_BIND=0.0.0.0:8080 must produce Explicit with that addr.
    /// What: passes bind_env=Some("0.0.0.0:8080"); asserts Explicit.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_env_explicit_addr() {
        let mode = BindMode::from_flags_and_bind_env(
            "127.0.0.1:7788",
            "127.0.0.1:7788",
            false,
            Some("0.0.0.0:8080"),
        );
        assert_eq!(mode, BindMode::Explicit("0.0.0.0:8080".to_owned()));
    }

    /// Why: --tailscale flag must produce Tailscale when env is absent.
    /// What: passes tailscale_flag=true, bind_env=None; asserts Tailscale.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_tailscale_flag() {
        let mode =
            BindMode::from_flags_and_bind_env("127.0.0.1:7788", "127.0.0.1:7788", true, None);
        assert_eq!(mode, BindMode::Tailscale);
    }

    /// Why: env var takes precedence over --tailscale flag.
    /// What: passes bind_env=Some("0.0.0.0:9999") and tailscale_flag=true; asserts Explicit wins.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_env_beats_tailscale_flag() {
        let mode = BindMode::from_flags_and_bind_env(
            "127.0.0.1:7788",
            "127.0.0.1:7788",
            true,
            Some("0.0.0.0:9999"),
        );
        assert_eq!(mode, BindMode::Explicit("0.0.0.0:9999".to_owned()));
    }

    /// Why: no overrides must produce Local.
    /// What: no env, no flag, http = default; asserts Local.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_default_is_local() {
        let mode =
            BindMode::from_flags_and_bind_env("127.0.0.1:7788", "127.0.0.1:7788", false, None);
        assert_eq!(mode, BindMode::Local);
    }

    /// Why: empty bind_env string must fall through to Local (not crash).
    /// What: passes bind_env=Some(""); asserts Local.
    /// Test: this test itself.
    #[test]
    fn test_bind_mode_env_empty_is_local() {
        let mode =
            BindMode::from_flags_and_bind_env("127.0.0.1:7788", "127.0.0.1:7788", false, Some(""));
        assert_eq!(mode, BindMode::Local);
    }

    // ── resolve_bind_addrs ────────────────────────────────────────────────────

    /// Why: Local mode must return exactly one loopback addr on the given port.
    /// What: calls resolve_bind_addrs with Local mode; injected detector is never called.
    /// Test: this test itself.
    #[test]
    fn test_resolve_local() {
        let addrs = resolve_bind_addrs(&BindMode::Local, 7788, || panic!("should not call"));
        assert_eq!(addrs, vec![SocketAddr::from(([127, 0, 0, 1], 7788))]);
    }

    /// Why: Tailscale mode with a valid IP must return loopback + tailscale addr.
    /// What: injects a fixed Tailscale IP; asserts both addrs are returned.
    /// Test: this test itself.
    #[test]
    fn test_resolve_tailscale_with_ip() {
        let ts_ip = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1));
        let addrs = resolve_bind_addrs(&BindMode::Tailscale, 7788, || Some(ts_ip));
        assert_eq!(addrs.len(), 2);
        assert_eq!(addrs[0], SocketAddr::from(([127, 0, 0, 1], 7788)));
        assert_eq!(addrs[1], SocketAddr::new(ts_ip, 7788));
    }

    /// Why: Tailscale mode without a Tailscale IP must fall back to localhost-only.
    /// What: injects None from the detector; asserts single loopback addr.
    /// Test: this test itself.
    #[test]
    fn test_resolve_tailscale_fallback() {
        let addrs = resolve_bind_addrs(&BindMode::Tailscale, 7788, || None);
        assert_eq!(addrs, vec![SocketAddr::from(([127, 0, 0, 1], 7788))]);
    }

    /// Why: Explicit mode with a valid addr must return that addr.
    /// What: passes a valid addr string; asserts it is parsed correctly.
    /// Test: this test itself.
    #[test]
    fn test_resolve_explicit_valid() {
        let mode = BindMode::Explicit("0.0.0.0:9000".to_owned());
        let addrs = resolve_bind_addrs(&mode, 7788, || panic!("should not call"));
        assert_eq!(addrs, vec![SocketAddr::from(([0, 0, 0, 0], 9000))]);
    }

    /// Why: Explicit mode with an unparseable addr must fall back to localhost.
    /// What: passes a garbage string; asserts single loopback addr on default port.
    /// Test: this test itself.
    #[test]
    fn test_resolve_explicit_invalid_fallback() {
        let mode = BindMode::Explicit("not-an-addr".to_owned());
        let addrs = resolve_bind_addrs(&mode, 7788, || panic!("should not call"));
        assert_eq!(addrs, vec![SocketAddr::from(([127, 0, 0, 1], 7788))]);
    }

    // ── port_from_addr ────────────────────────────────────────────────────────

    /// Why: port_from_addr must extract the correct port from a valid addr string.
    /// What: passes "127.0.0.1:7788"; asserts 7788.
    /// Test: this test itself.
    #[test]
    fn test_port_from_addr_valid() {
        assert_eq!(port_from_addr("127.0.0.1:7788", 7788), 7788);
        assert_eq!(port_from_addr("0.0.0.0:9000", 7788), 9000);
    }

    /// Why: port_from_addr must return the default when the string is garbage.
    /// What: passes "garbage"; asserts default 7788.
    /// Test: this test itself.
    #[test]
    fn test_port_from_addr_invalid() {
        assert_eq!(port_from_addr("garbage", 7788), 7788);
    }

    // ── parse tailscale ip output ─────────────────────────────────────────────

    /// Why: we need to verify that the tailscale IP parser handles typical output
    /// (with trailing newline) correctly.
    /// What: simulates the parsing step in detect_tailscale_ipv4 inline.
    /// Test: this test itself.
    #[test]
    fn test_parse_tailscale_output() {
        let raw = "100.64.0.1\n";
        let ip: IpAddr = raw.trim().parse().expect("parse");
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)));
    }
}
