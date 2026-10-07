//! Handler for `trusty-search port` — report the daemon's listening port.
//!
//! Why: operators and agents often need to know which port the running
//! trusty-search daemon is listening on without guessing (7878 vs 7879 vs a
//! machine-assigned port). The `port.lock` / `http_addr` mechanism already
//! records the exact address the daemon bound; this command exposes it as a
//! first-class, machine-parsable CLI surface.
//!
//! What: asks the daemon on its socket (`search.health`, #9214) which HTTP
//! address it bound, and prints one of three formats to stdout based on the
//! caller's flags:
//!   - default: bare port number  →  `7879\n`
//!   - `--addr`: `host:port`      →  `127.0.0.1:7879\n`
//!   - `--json`: JSON object      →  `{"addr":"127.0.0.1","port":7879}\n`
//!
//! Every intentional port/JSON output goes to **stdout**. Error messages go to
//! **stderr**. The command exits 1 when no daemon answers on the socket, and
//! also when the daemon answers but serves no HTTP listener (`--no-http`), so
//! shell substitution (`$(trusty-search port)`) fails cleanly. It only probes;
//! it never starts a daemon.
//!
//! Test: unit tests in this module cover the output formats; the daemon-facing
//! paths are covered end to end by `port_names_the_socket_when_the_daemon_is_http_less`,
//! `port_reports_no_daemon_only_when_the_socket_is_dead` and
//! `port_prints_the_http_address_the_daemon_reports`.

use std::path::Path;

use anyhow::Result;
use serde_json::Value;
use trusty_search::service::daemon_client::{DaemonCallError, DaemonClient};

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
/// Why: the discovery file stores the full `host:port` string; extracting the
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
/// the output string without spawning a daemon or touching a lockfile.
/// What: takes a validated `host:port` string plus the desired format and
/// returns the string to print. Returns `None` when the port cannot be
/// parsed from the address (which would indicate a corrupt lockfile).
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

/// What a live daemon says about its HTTP listener (#9214).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HttpListener {
    /// The daemon serves HTTP at this `host:port`.
    Bound(String),
    /// The daemon serves only its socket (`start --no-http`).
    SocketOnly,
    /// A daemon from before #9030 reports no transport, and left no
    /// `http_addr` file to read instead.
    Unreported,
}

/// Ask the daemon on `client`'s socket whether it serves HTTP, and where.
///
/// Why (#9214): the `http_addr` and `daemon.port` files outlive the daemon
/// that wrote them, and a `--no-http` daemon writes neither, so a file read
/// can neither prove a daemon is up nor tell a dead one from a socket-only one.
/// Only the daemon knows which listeners it bound (#9030).
/// What: one `search.health` probe. Reads `transport.http_addr`: a string is
/// [`HttpListener::Bound`], `null` is [`HttpListener::SocketOnly`]. A body
/// with no `transport` key comes from a pre-#9030 daemon, which always bound
/// HTTP and wrote `http_addr`; that file is read instead. Never dials TCP and
/// never starts a daemon.
///
/// # Errors
///
/// The probe's own [`DaemonCallError`] — `Unreachable` when nothing serves the
/// socket.
///
/// Test: `port_names_the_socket_when_the_daemon_is_http_less`,
/// `dashboard_under_no_http_errors_and_opens_nothing`.
pub(crate) async fn probe_http_listener(
    client: &DaemonClient,
) -> Result<HttpListener, DaemonCallError> {
    let health = client.health().await?;
    let Some(transport) = health.get("transport") else {
        // #9214: a pre-#9030 daemon; it wrote the file this CLI used to read.
        return Ok(trusty_search::service::http_addr_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .map_or(HttpListener::Unreported, HttpListener::Bound));
    };
    Ok(match transport.get("http_addr").and_then(Value::as_str) {
        Some(addr) if !addr.trim().is_empty() => HttpListener::Bound(addr.trim().to_string()),
        _ => HttpListener::SocketOnly,
    })
}

/// The one wording every CLI command uses for a socket-only daemon (#9214).
pub(crate) fn socket_only_message(socket: &Path) -> String {
    format!(
        "no HTTP listener (socket-only daemon at {})",
        socket.display()
    )
}

/// Entry point for `trusty-search port [--json | --addr]`.
///
/// Why: exposes the daemon's listening port as a first-class CLI command so
/// shell substitutions like `curl http://127.0.0.1:$(trusty-search port)/health`
/// work without guessing. Issue #526.
/// What: resolves the socket the daemon binds (honouring `TRUSTY_DATA_DIR`,
/// #3545), runs [`port_output`], prints the result to stdout, or prints the
/// error to stderr and exits 1.
/// Test: `port_names_the_socket_when_the_daemon_is_http_less`,
/// `port_reports_no_daemon_only_when_the_socket_is_dead`,
/// `port_prints_the_http_address_the_daemon_reports`.
pub async fn handle_port(format: PortFormat) -> Result<()> {
    let client = DaemonClient::resolve()?;
    match port_output(&client, format).await {
        Ok(out) => {
            println!("{out}");
            Ok(())
        }
        Err(message) => {
            eprintln!("trusty-search: {message}");
            std::process::exit(1);
        }
    }
}

/// The line `port` prints, or the reason it prints none.
///
/// Why (#9214): "no daemon running" is true only when nothing answers on the
/// socket. A daemon that answers but serves no HTTP gets its own message, and
/// a stale discovery file is never reported as the port.
/// What: maps [`probe_http_listener`] onto the output line or an error message
/// naming the socket.
/// Test: as [`handle_port`].
async fn port_output(client: &DaemonClient, format: PortFormat) -> Result<String, String> {
    let socket = client.socket().display();
    let addr = match probe_http_listener(client).await {
        Ok(HttpListener::Bound(addr)) => addr,
        Ok(HttpListener::SocketOnly) => return Err(socket_only_message(client.socket())),
        Ok(HttpListener::Unreported) => {
            return Err(format!(
                "the daemon at socket {socket} reported no HTTP address; restart it"
            ));
        }
        Err(e) if e.is_unreachable() => {
            return Err(format!(
                "no daemon running (nothing answers on socket {socket}). \
                 Start with `trusty-search start`."
            ));
        }
        Err(e) => return Err(format!("the daemon at socket {socket} did not answer: {e}")),
    };
    format_output(&addr, format).ok_or_else(|| {
        format!("the daemon reported an unrecognised address `{addr}` (expected host:port)")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_output ──────────────────────────────────────────────────────

    /// Default format emits the bare port number.
    ///
    /// Why: the primary use case is shell substitution; anything other than
    /// a bare integer in stdout would break `curl http://…:$(trusty-search port)/…`.
    #[test]
    fn format_port_output_default() {
        assert_eq!(
            format_output("127.0.0.1:7879", PortFormat::Port),
            Some("7879".to_string())
        );
    }

    /// `--addr` format emits the full `host:port` string unchanged.
    ///
    /// Why: callers using `curl http://$(trusty-search port --addr)/health`
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
    /// Why: on dual-stack hosts the daemon might bind `[::1]:7879`; `rfind`
    /// correctly splits on the final `:` rather than the first.
    #[test]
    fn format_port_output_ipv6_port() {
        assert_eq!(parse_port_from_addr("[::1]:7879"), Some(7879));
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
        assert_eq!(parse_port_from_addr("127.0.0.1:7878"), Some(7878));
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
