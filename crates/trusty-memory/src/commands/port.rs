//! Handler for `trusty-memory port` — report the daemon's listening port.
//!
//! Why: operators and agents need to know which port the running trusty-memory
//! daemon is listening on. The daemon selects a port dynamically from the
//! 7070–7079 range (plus OS fallback) and writes it to `http_addr`; this
//! command reads that file and exposes the live address as a first-class CLI
//! surface (issue #526).
//!
//! What: reads the daemon's persisted address via
//! `trusty_common::read_daemon_addr("trusty-memory")` and prints one of three
//! formats to stdout:
//!   - default: bare port number  →  `7070\n`
//!   - `--addr`: `host:port`      →  `127.0.0.1:7070\n`
//!   - `--json`: JSON object      →  `{"addr":"127.0.0.1","port":7070}\n`
//!
//! Every intentional port/JSON output goes to **stdout**. Error messages go to
//! **stderr**. When no daemon is running the command exits non-zero so shell
//! substitution (`$(trusty-memory port)`) fails cleanly.
//!
//! Test: unit tests in this module cover all three output formats plus the
//! missing-daemon error path using a fake address string.

use anyhow::Result;

/// Output format requested by the caller.
///
/// Why: the three output shapes have distinct audiences — bare port for shell
/// substitution, host:port for direct `curl`, JSON for scripted consumers.
/// Encoding the choice as an enum keeps `handle_port` a thin dispatcher and
/// makes each formatter independently testable.
/// What: one variant per flag; `Default` is the bare-port case.
/// Test: `format_port_output_*` unit tests exercise all three variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortFormat {
    /// Bare port number (default).
    Port,
    /// `host:port` string.
    Addr,
    /// `{"addr":"…","port":…}` JSON object.
    Json,
}

/// Parse a `host:port` string and return the port as `u16`.
///
/// Why: `read_daemon_addr` returns the full `host:port` string; extracting the
/// port number for the `Port` and `Json` output modes requires splitting on
/// the last `:` (to handle IPv6 addresses where the host itself contains `:`).
/// What: splits on the final `:`, parses the port, returns `None` on any parse
/// failure so the caller can emit a helpful error rather than panicking.
/// Test: `parse_port_from_addr_*` unit tests cover normal, IPv6, and malformed inputs.
pub fn parse_port_from_addr(addr: &str) -> Option<u16> {
    let colon = addr.rfind(':')?;
    addr[colon + 1..].parse::<u16>().ok()
}

/// Format the daemon address for output based on the requested `PortFormat`.
///
/// Why: separating the formatting logic from the I/O lets unit tests assert
/// the output string without spinning up a daemon or touching a lockfile.
/// What: takes a validated `host:port` string plus the desired format and
/// returns the string to print. Returns `None` when the port cannot be
/// parsed from the address (which would indicate a corrupt address file).
/// Test: `format_port_output_*` unit tests cover all three variants.
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
            Some(format!(r#"{{"addr":"{host}","port":{port}}}"#))
        }
    }
}

/// Entry point for `trusty-memory port [--json | --addr]`.
///
/// Why: exposes the daemon's listening port as a first-class CLI command so
/// shell substitutions like `curl http://127.0.0.1:$(trusty-memory port)/api/v1/health`
/// work without guessing. Issue #526.
/// What: reads the address from the `http_addr` discovery file via
/// `trusty_common::read_daemon_addr("trusty-memory")`, formats it per the
/// caller's flags, and prints to stdout. On any error (no daemon, missing file,
/// corrupt address) the message goes to stderr and the function returns `Err`
/// so `main` exits non-zero.
/// Test: unit tests cover all format variants; the live path is exercised
/// manually via `trusty-memory start && trusty-memory port`.
pub fn handle_port(format: PortFormat) -> Result<()> {
    let addr = match trusty_common::read_daemon_addr("trusty-memory") {
        Ok(Some(a)) if !a.is_empty() => a,
        Ok(Some(_)) | Ok(None) => {
            eprintln!(
                "trusty-memory: no daemon running (address file not found). \
                 Start with `trusty-memory start`."
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("trusty-memory: could not read daemon address: {e:#}");
            std::process::exit(1);
        }
    };

    match format_output(&addr, format) {
        Some(out) => {
            println!("{out}");
            Ok(())
        }
        None => {
            eprintln!(
                "trusty-memory: daemon address file contains an unrecognised \
                 address `{addr}` (expected host:port). \
                 Re-start the daemon with `trusty-memory start`."
            );
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_output ──────────────────────────────────────────────────────

    /// Default format emits the bare port number.
    ///
    /// Why: the primary use case is shell substitution; anything other than
    /// a bare integer in stdout would break `curl http://…:$(trusty-memory port)/…`.
    #[test]
    fn format_port_output_default() {
        assert_eq!(
            format_output("127.0.0.1:7070", PortFormat::Port),
            Some("7070".to_string())
        );
    }

    /// `--addr` format emits the full `host:port` string unchanged.
    ///
    /// Why: callers using `curl http://$(trusty-memory port --addr)/api/v1/health`
    /// need the host included.
    #[test]
    fn format_port_output_addr() {
        assert_eq!(
            format_output("127.0.0.1:7070", PortFormat::Addr),
            Some("127.0.0.1:7070".to_string())
        );
    }

    /// `--json` format emits a JSON object with `addr` and `port` fields.
    ///
    /// Why: scripted consumers may want both fields in a structured payload
    /// without shelling out twice or parsing the port themselves.
    #[test]
    fn format_port_output_json() {
        assert_eq!(
            format_output("127.0.0.1:7070", PortFormat::Json),
            Some(r#"{"addr":"127.0.0.1","port":7070}"#.to_string())
        );
    }

    /// IPv6 addresses use the last `:` as the port separator.
    ///
    /// Why: on dual-stack hosts the daemon might bind `[::1]:7070`; `rfind`
    /// correctly splits on the final `:` rather than the first.
    #[test]
    fn parse_port_ipv6() {
        assert_eq!(parse_port_from_addr("[::1]:7070"), Some(7070));
    }

    /// A corrupt address (no `:`) returns `None` instead of panicking.
    ///
    /// Why: the caller converts `None` to a human-readable error rather than
    /// crashing — this validates the safety net.
    #[test]
    fn format_port_output_malformed_returns_none() {
        assert_eq!(format_output("not-an-addr", PortFormat::Port), None);
        assert_eq!(format_output("", PortFormat::Port), None);
    }

    /// `--json` with a port that doesn't parse returns `None`.
    ///
    /// Why: same safety-net; a non-numeric port must not produce garbage JSON.
    #[test]
    fn format_port_output_json_malformed_returns_none() {
        assert_eq!(format_output("127.0.0.1:notaport", PortFormat::Json), None);
    }

    // ── parse_port_from_addr ───────────────────────────────────────────────

    /// Standard IPv4 address parses correctly.
    #[test]
    fn parse_port_standard() {
        assert_eq!(parse_port_from_addr("127.0.0.1:7071"), Some(7071));
    }

    /// Port 0 is valid (OS-assigned).
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
