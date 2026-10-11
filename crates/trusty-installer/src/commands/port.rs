//! `tctl port [<member>]` — report a MANAGED member daemon's bound port/address.
//!
//! Why: Operators and scripts need to discover where a running trusty-* daemon
//! bound (its auto-walked port) without parsing logs. `tctl` does NOT host its
//! own HTTP server (DOC-7); it reports a *member* daemon's address by reading
//! that member's `http_addr` discovery file via
//! `trusty_common::read_daemon_addr`, mirroring `trusty-search port`. A member
//! that serves a Unix socket — trusty-search since #9214 — has no port, so
//! `tctl port` names its socket and exits 1 rather than reading a stale file.
//!
//! ## Flag precedence
//!
//! `--json-port` and the global `--json` both produce JSON on stdout but with
//! different shapes. Precedence:
//!   1. `--json-port` (highest) — `{"addr":"…","port":N}`
//!   2. `--addr` — `host:port` string
//!   3. (default) — bare port integer
//!
//! `--json-port` wins when both it and `--json` are present (enforced at runtime
//! because clap does not apply `conflicts_with` across subcommand boundaries for
//! `global = true` flags).
//!
//! Test: `tests` covers the pure `parse_port_from_addr` / `format_output`
//! helpers (copied from `trusty-search port` per the #1332 plan) and the
//! `PortFormat` precedence selection; the address read is side-effecting.

/// Output format requested by the caller.
///
/// Why: the three output shapes have distinct audiences — bare port for shell
/// substitution, host:port for direct `curl`, JSON for scripted consumers.
/// Encoding the choice as an enum keeps `run` a thin dispatcher and makes each
/// formatter independently testable.
/// What: one variant per flag; `Port` is the bare-port default.
/// Test: `format_output_*` unit tests exercise all three variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortFormat {
    /// Bare port number (default).
    Port,
    /// `host:port` string.
    Addr,
    /// `{"addr":"…","port":…}` JSON object.
    Json,
}

/// Choose the output format from the `addr` / `json_port` flags.
///
/// Why: The precedence (`json_port` > `addr` > default) must be decided in one
/// pure place so it is testable and the handler stays a thin shell.
/// What: `Json` when `json_port`; else `Addr` when `addr`; else `Port`.
/// Test: `tests::format_precedence`.
pub fn select_format(addr: bool, json_port: bool) -> PortFormat {
    if json_port {
        PortFormat::Json
    } else if addr {
        PortFormat::Addr
    } else {
        PortFormat::Port
    }
}

/// Parse a `host:port` string and return the port as `u16`.
///
/// Why: `read_daemon_addr` returns the full `host:port` string; extracting the
/// port for the `Port`/`Json` modes requires splitting on the last `:` to handle
/// IPv6 hosts that themselves contain `:`.
/// What: splits on the final `:`, parses the port, returns `None` on any parse
/// failure so the caller emits a helpful error rather than panicking.
/// Test: `tests::parse_port_*` (copied from `trusty-search port`).
pub fn parse_port_from_addr(addr: &str) -> Option<u16> {
    let colon = addr.rfind(':')?;
    addr[colon + 1..].parse::<u16>().ok()
}

/// Format the daemon address for output based on the requested `PortFormat`.
///
/// Why: separating formatting from I/O lets unit tests assert the output string
/// without touching a lockfile or spawning a daemon.
/// What: takes a `host:port` string + the desired format, returns the string to
/// print, or `None` when the port cannot be parsed (corrupt address file).
/// Test: `tests::format_output_*` (copied from `trusty-search port`).
pub fn format_output(addr: &str, format: PortFormat) -> Option<String> {
    match format {
        PortFormat::Port => {
            let port = parse_port_from_addr(addr)?;
            Some(port.to_string())
        }
        PortFormat::Addr => Some(addr.to_string()),
        PortFormat::Json => {
            let port = parse_port_from_addr(addr)?;
            let colon = addr.rfind(':')?;
            let host = &addr[..colon];
            // Serialise via serde_json so a host needing JSON escaping (quotes,
            // backslashes) can't break the output. Field names/shape are
            // identical to the prior hand-rolled form, so existing tests hold.
            Some(serde_json::json!({ "addr": host, "port": port }).to_string())
        }
    }
}

/// Default member when none is named on the command line.
///
/// Why: `tctl port` with no member should answer the most common question —
/// "what port is search on?" — matching the muscle memory of `trusty-search port`.
/// What: the binary name `trusty-search`.
/// Test: `tests::default_member_is_search`.
const DEFAULT_MEMBER: &str = "trusty-search";

/// The refusal `tctl port` prints for a member that binds no TCP port.
///
/// Why (#9214): trusty-search, like trusty-analyze and trusty-memory before
/// it, serves a Unix socket only. Its `http_addr` file can outlive the build
/// that wrote it, and printing that port sends a script to a dead port or to
/// a stranger holding it.
/// What: `Some(message naming the socket)` when `binary` serves a socket
/// (`probe_http::uds_socket_for`); `None` for a member still on TCP.
/// Test: `tests::socket_members_name_their_socket_instead_of_a_port`.
pub fn socket_only_refusal(binary: &str) -> Option<String> {
    let socket = super::probe_http::uds_socket_for(binary)?;
    Some(format!(
        "tctl port: `{binary}` serves a Unix socket only and binds no TCP port; \
         its socket is {}",
        socket.display()
    ))
}

/// Handle `tctl port [<member>] [--addr] [--json-port]`.
///
/// Why: Phase-2 implementation reporting a managed member daemon's bound port.
///
/// What: Resolves the member (defaults to `trusty-search`). A member that
/// serves a Unix socket gets [`socket_only_refusal`] and exit 1 (#9214).
/// Otherwise it reads the member's `http_addr` via `read_daemon_addr` and
/// prints the address in the format chosen by `select_format`. Returns exit
/// code 1 when no address is recorded (daemon not running) or the recorded
/// address is unparseable, 0 on success.
///
/// Test: side-effecting (reads the discovery file); the formatting and
/// precedence are covered by the pure-helper tests.
pub fn run(member: Option<&str>, addr: bool, json_port: bool, _json: bool) -> i32 {
    let binary = member.unwrap_or(DEFAULT_MEMBER);
    let format = select_format(addr, json_port);
    // #9214: a socket-only member has no port; never print a stale one.
    if let Some(refusal) = socket_only_refusal(binary) {
        eprintln!("{refusal}");
        return 1;
    }

    let address = match trusty_common::read_daemon_addr(binary) {
        Ok(Some(a)) if !a.is_empty() => a,
        Ok(_) => {
            eprintln!(
                "tctl port: no address recorded for `{binary}` \
                 (daemon not running?). Start it with `{binary} start`."
            );
            return 1;
        }
        Err(e) => {
            eprintln!("tctl port: could not read `{binary}` daemon address: {e:#}");
            return 1;
        }
    };

    match format_output(&address, format) {
        Some(out) => {
            println!("{out}");
            0
        }
        None => {
            eprintln!(
                "tctl port: `{binary}` address file holds an unrecognised \
                 address `{address}` (expected host:port)."
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#9214): trusty-search serves a socket only, so `tctl port` must
    /// name it and never read a stale `http_addr`; a TCP member is untouched.
    /// What: the refusal for trusty-search names its resolved socket; a TCP
    /// member gets none.
    /// Test: This is the test.
    #[test]
    fn socket_members_name_their_socket_instead_of_a_port() {
        // Both resolutions read `TRUSTY_DATA_DIR_OVERRIDE`, which sibling tests
        // set under this lock.
        let _guard = crate::commands::test_support::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let refusal = socket_only_refusal("trusty-search").expect("search serves a socket");
        let socket = crate::commands::probe_http::uds_socket_for("trusty-search")
            .expect("search resolves a socket");
        assert!(refusal.contains("binds no TCP port"), "{refusal}");
        assert!(refusal.contains(&socket.display().to_string()), "{refusal}");
        assert_eq!(socket_only_refusal("trusty-console"), None);
    }

    /// Why: precedence is load-bearing — `--json-port` must win over `--addr`.
    /// What: asserts each flag combination resolves to the right format.
    /// Test: This is the test.
    #[test]
    fn format_precedence() {
        assert_eq!(select_format(false, false), PortFormat::Port);
        assert_eq!(select_format(true, false), PortFormat::Addr);
        assert_eq!(select_format(false, true), PortFormat::Json);
        // json_port wins over addr.
        assert_eq!(select_format(true, true), PortFormat::Json);
    }

    /// Why: the default member must be search so bare `tctl port` is useful.
    /// What: pins the constant.
    /// Test: This is the test.
    #[test]
    fn default_member_is_search() {
        assert_eq!(DEFAULT_MEMBER, "trusty-search");
    }

    // ── format_output (copied behaviour from trusty-search port) ──────────────

    /// Default format emits the bare port number.
    #[test]
    fn format_output_default() {
        assert_eq!(
            format_output("127.0.0.1:7879", PortFormat::Port),
            Some("7879".to_string())
        );
    }

    /// `--addr` emits the full `host:port` string unchanged.
    #[test]
    fn format_output_addr() {
        assert_eq!(
            format_output("127.0.0.1:7879", PortFormat::Addr),
            Some("127.0.0.1:7879".to_string())
        );
    }

    /// `--json-port` emits a JSON object with `addr` and `port` fields.
    #[test]
    fn format_output_json() {
        assert_eq!(
            format_output("127.0.0.1:7879", PortFormat::Json),
            Some(r#"{"addr":"127.0.0.1","port":7879}"#.to_string())
        );
    }

    /// A corrupt address (no `:`) returns `None` instead of panicking.
    #[test]
    fn format_output_malformed_returns_none() {
        assert_eq!(format_output("not-an-addr", PortFormat::Port), None);
        assert_eq!(format_output("", PortFormat::Port), None);
    }

    // ── parse_port_from_addr ─────────────────────────────────────────────────

    /// Standard IPv4 address parses correctly.
    #[test]
    fn parse_port_standard() {
        assert_eq!(parse_port_from_addr("127.0.0.1:7878"), Some(7878));
    }

    /// IPv6 addresses use the last `:` as the port separator.
    #[test]
    fn parse_port_ipv6() {
        assert_eq!(parse_port_from_addr("[::1]:7879"), Some(7879));
    }

    /// Port 0 (OS-assigned) is valid.
    #[test]
    fn parse_port_zero() {
        assert_eq!(parse_port_from_addr("127.0.0.1:0"), Some(0));
    }

    /// Missing colon returns `None`.
    #[test]
    fn parse_port_no_colon() {
        assert_eq!(parse_port_from_addr("127.0.0.1"), None);
    }

    /// Port too large (>65535) returns `None`.
    #[test]
    fn parse_port_overflow() {
        assert_eq!(parse_port_from_addr("127.0.0.1:99999"), None);
    }
}
