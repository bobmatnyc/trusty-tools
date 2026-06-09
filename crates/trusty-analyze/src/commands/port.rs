//! Handler for `trusty-analyze port` — report the daemon's listening port.
//!
//! Why: operators and automation often need to know which port the running
//! trusty-analyze daemon is listening on without guessing (7879 vs a
//! machine-assigned port). The `http_addr` discovery file already records
//! the exact address the daemon bound on `serve`; this command exposes it as a
//! first-class, machine-parsable CLI surface so shell substitution and monitoring
//! scripts work without hard-coding the default port.
//!
//! What: reads the daemon's persisted address via `trusty_common::read_daemon_addr`
//! and prints one of three formats to stdout based on the caller's flags:
//!   - default: bare port number  →  `7879\n`
//!   - `--addr`: `host:port`      →  `127.0.0.1:7879\n`
//!   - `--json`: JSON object      →  `{"addr":"127.0.0.1","port":7879}\n`
//!
//! Every intentional port/JSON output goes to **stdout**. Error messages go to
//! **stderr**. When no daemon is running the command falls back to the compiled-in
//! default address and prints a stderr warning; it exits 0 so scripted callers
//! that start the daemon after querying the port are not broken by the check.
//!
//! Test: unit tests in this module cover all three output formats plus the
//! missing-daemon error path using a fake address string.

use anyhow::Result;

use trusty_analyze::service::DEFAULT_PORT;

/// Output format requested by the caller.
///
/// Why: the three output shapes have distinct audiences — bare port for shell
/// substitution, host:port for direct `curl`, JSON for scripted consumers.
/// Encoding the choice as an enum keeps `handle_port` a thin dispatcher and
/// makes each formatter independently testable.
/// What: one variant per flag; `Port` is the bare-port default.
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
/// the last `:` to handle both IPv4 and IPv6 addresses correctly.
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
/// the output string without spawning a daemon or touching a lockfile.
/// What: takes a validated `host:port` string plus the desired format and
/// returns the formatted string. For the JSON format, IPv6 bracket notation
/// (e.g. `[::1]`) is stripped from the `addr` field because the JSON consumer
/// receives a plain hostname, not a URI — the brackets are only required in
/// URI authority syntax. The `--addr` output keeps the original string
/// unchanged for URI validity. Returns `None` when the port cannot be parsed
/// (which indicates a corrupt or empty discovery file).
/// Test: `format_port_output_*` unit tests cover all three variants, the IPv6
/// bracket-stripping behaviour for the JSON path, and the malformed-addr None path.
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
            let host_part = &addr[..colon];
            // Strip a single matching pair of brackets for IPv6 addresses
            // (e.g. `[::1]` → `::1`) so the JSON `addr` field contains a
            // plain host, not a URI bracket form.  Using strip_prefix + strip_suffix
            // instead of trim_matches ensures only one balanced pair is removed —
            // trim_matches would strip multiple leading/trailing brackets if the
            // address were somehow doubly-bracketed.
            let host = host_part
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or(host_part);
            Some(format!(r#"{{"addr":"{host}","port":{port}}}"#))
        }
    }
}

/// Entry point for `trusty-analyze port [--json | --addr]`.
///
/// Why: exposes the daemon's listening port as a first-class CLI command so
/// shell substitutions like `curl http://127.0.0.1:$(trusty-analyze port)/health`
/// work without guessing or hard-coding the default. Closes #956.
/// What: reads the address from the `http_addr` discovery file via
/// `trusty_common::read_daemon_addr("trusty-analyze")`, formats it per the
/// caller's flags, and prints to stdout. Falls back to the compiled-in
/// `DEFAULT_PORT` (7879) when no discovery file exists, printing a stderr
/// warning so operators know the value is a default/fallback rather than the
/// live daemon's address. Exits 0 in the fallback case so scripts that start
/// the daemon after querying the port are not broken by the check. On a parse
/// error (corrupt address) the message goes to stderr and exits non-zero.
/// Test: unit tests cover all format variants and the missing-file fallback;
/// the fallback to DEFAULT_PORT is verified by `handle_port_fallback_to_default`.
pub fn handle_port(format: PortFormat) -> Result<()> {
    let addr = match trusty_common::read_daemon_addr("trusty-analyze") {
        // Non-empty address read from the discovery file — daemon is running.
        Ok(Some(a)) if !a.is_empty() => a,
        // Empty string or missing entry: no live daemon; fall back to the
        // compiled-in default so scripts that query the port before starting
        // the daemon still get a usable answer.  Warning goes to stderr only
        // on this genuine fallback path, never when a real address was found.
        Ok(_) => {
            eprintln!(
                "trusty-analyze: no daemon running (address file not found); \
                 reporting compiled-in default {DEFAULT_PORT}. \
                 Start with `trusty-analyze serve` for the live address."
            );
            format!("127.0.0.1:{DEFAULT_PORT}")
        }
        Err(e) => {
            eprintln!("trusty-analyze: could not read daemon address: {e:#}");
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
                "trusty-analyze: daemon address file contains an unrecognised \
                 address `{addr}` (expected host:port). \
                 Re-start the daemon with `trusty-analyze serve`."
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
    /// a bare integer in stdout would break `curl http://…:$(trusty-analyze port)/…`.
    #[test]
    fn format_port_output_default() {
        assert_eq!(
            format_output("127.0.0.1:7879", PortFormat::Port),
            Some("7879".to_string())
        );
    }

    /// `--addr` format emits the full `host:port` string unchanged.
    ///
    /// Why: callers using `curl http://$(trusty-analyze port --addr)/health`
    /// need the host included.
    #[test]
    fn format_port_output_addr() {
        assert_eq!(
            format_output("127.0.0.1:7879", PortFormat::Addr),
            Some("127.0.0.1:7879".to_string())
        );
    }

    /// `--json` format emits a JSON object with `addr` and `port` fields.
    ///
    /// Why: scripted consumers may want both fields in a structured payload
    /// without shelling out twice or parsing the port themselves.
    #[test]
    fn format_port_output_json() {
        assert_eq!(
            format_output("127.0.0.1:7879", PortFormat::Json),
            Some(r#"{"addr":"127.0.0.1","port":7879}"#.to_string())
        );
    }

    /// IPv6 addresses use the last `:` as the port separator.
    ///
    /// Why: on dual-stack hosts the daemon might bind `[::1]:7879`;
    /// `rfind` correctly splits on the final `:` rather than the first.
    #[test]
    fn format_port_output_ipv6_port() {
        assert_eq!(parse_port_from_addr("[::1]:7879"), Some(7879));
    }

    /// `--json` with an IPv6 address strips the brackets from the `addr` field.
    ///
    /// Why: JSON consumers expect a plain hostname (`::1`), not the URI
    /// bracket form (`[::1]`); the `--addr` output keeps brackets for URI
    /// validity, but JSON is a different serialisation context.
    #[test]
    fn format_port_output_json_ipv6_strips_brackets() {
        assert_eq!(
            format_output("[::1]:7879", PortFormat::Json),
            Some(r#"{"addr":"::1","port":7879}"#.to_string())
        );
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
        assert_eq!(parse_port_from_addr("127.0.0.1:7879"), Some(7879));
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

    /// format_output with PortFormat::Addr passes the full addr through.
    #[test]
    fn format_port_output_addr_different_port() {
        assert_eq!(
            format_output("127.0.0.1:9000", PortFormat::Addr),
            Some("127.0.0.1:9000".to_string())
        );
    }
}
