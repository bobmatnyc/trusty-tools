//! Handler for `trusty-review socket` — report the daemon's endpoint.
//!
//! Why: operators and scripts need to discover where the running daemon
//! answers without guessing. This is the migration of `trusty-review port`
//! (#957), which read the `http_addr` discovery file and printed a TCP port.
//! #6277 (ADR-0032) retired both: there is no port, and there is no discovery
//! file — the socket path is DERIVED from the data directory, so the daemon and
//! its consumers cannot disagree about it and there is nothing for a stale file
//! to contradict.
//!
//! That changes what "no daemon running" means here. `port` answered from a
//! file the daemon wrote on bind, so an absent file meant an absent daemon. The
//! path resolves whether or not anything is listening, so this command reports
//! the path AND whether a process is answering on it, and exits non-zero when
//! nothing is — preserving the property that `$(trusty-review socket)` fails
//! cleanly rather than handing a caller a path to a dead socket. See
//! [`handle_socket`] for exactly what each format puts on stdout in that case.
//!
//! What: prints one of two formats to stdout:
//!   - default:  the bare path  →  `/Users/x/…/trusty-review.sock\n`
//!   - `--json`: a JSON object  →  `{"socket":"…","serving":true}\n`
//!
//! Errors go to stderr; stdout carries only the intended output.
//!
//! Test: unit tests cover both formatters; the liveness half is covered by
//! `service::rpc`'s socket tests.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

/// How long to wait for the socket to answer before calling it unserved.
///
/// A local socket answers or refuses in microseconds; this is headroom for a
/// loaded machine, not a latency budget.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Output format requested by the caller.
///
/// Why: two audiences — a bare path for shell substitution, JSON for scripted
/// consumers that also want the liveness answer.
/// What: one variant per flag; `Path` is the default.
/// Test: `format_output_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketFormat {
    /// The bare socket path (default).
    Path,
    /// `{"socket":"…","serving":…}` JSON object.
    Json,
}

/// Format the socket path and liveness verdict for output.
///
/// Why: separating formatting from I/O lets unit tests assert the exact bytes
/// without binding a socket.
/// What: the bare path, or a JSON object carrying the path and whether anything
/// answered. The path is JSON-encoded rather than interpolated, because a home
/// directory may contain a quote or a backslash and a hand-built object would
/// emit invalid JSON for it.
/// Test: `format_output_path`, `format_output_json`,
/// `format_output_json_escapes_a_quoted_path`.
pub fn format_output(socket: &Path, serving: bool, format: SocketFormat) -> String {
    match format {
        SocketFormat::Path => socket.display().to_string(),
        SocketFormat::Json => serde_json::json!({
            "socket": socket.display().to_string(),
            "serving": serving,
        })
        .to_string(),
    }
}

/// Entry point for `trusty-review socket [--json]`.
///
/// Why: exposes the daemon's endpoint as a first-class CLI surface so an
/// operator can check what a consumer would dial.
///
/// What: resolves the path through the shared entry point, probes it, prints the
/// requested format, and returns `Err` when nothing is serving. The probe is
/// `trusty_common::uds::socket_is_serving` — a bare connect — rather than a
/// `review.health` call, because the question here is "is the endpoint live",
/// and a daemon that is up but degraded must not be reported as absent.
///
/// **The two formats differ in what they put on stdout when nothing answers,
/// and the difference is the point.** Path mode writes the bare path to stdout
/// ONLY when the socket is live, so `$(trusty-review socket)` yields an empty
/// string plus a non-zero status rather than a path to a dead socket that an
/// unchecked substitution would go on to use. The path still reaches the
/// operator, on stderr, inside the error. JSON mode always prints, because its
/// `serving` field is the machine-readable answer a caller asked for — printing
/// nothing would withhold the very fact the format exists to report.
///
/// # Errors
///
/// When the data directory cannot be resolved, or when nothing is answering on
/// the socket.
///
/// Test: `format_output_*` cover the formatting; the probe is exercised through
/// `service::rpc`'s socket tests.
pub async fn handle_socket(format: SocketFormat) -> Result<()> {
    let socket = trusty_common::daemon_socket_path("trusty-review")?;
    let serving = trusty_common::uds::socket_is_serving(&socket, PROBE_TIMEOUT).await;

    if serving || format == SocketFormat::Json {
        println!("{}", format_output(&socket, serving, format));
    }

    if !serving {
        anyhow::bail!(
            "trusty-review: nothing is serving {} — start it with \
             `trusty-review serve`.",
            socket.display()
        );
    }
    Ok(())
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Why: the default output is consumed by shell substitution, so anything
    /// other than the bare path breaks `nc -U $(trusty-review socket)`.
    /// Test: this is the test.
    #[test]
    fn format_output_path() {
        let p = PathBuf::from("/tmp/trusty-review.sock");
        assert_eq!(
            format_output(&p, true, SocketFormat::Path),
            "/tmp/trusty-review.sock"
        );
    }

    /// Why: a scripted consumer wants both facts in one call, and `serving` is
    /// the half that replaces "the discovery file exists".
    /// Test: this is the test.
    #[test]
    fn format_output_json() {
        let p = PathBuf::from("/tmp/trusty-review.sock");
        assert_eq!(
            format_output(&p, true, SocketFormat::Json),
            r#"{"serving":true,"socket":"/tmp/trusty-review.sock"}"#
        );
        assert_eq!(
            format_output(&p, false, SocketFormat::Json),
            r#"{"serving":false,"socket":"/tmp/trusty-review.sock"}"#
        );
    }

    /// Why: a home directory can legally contain a quote or a backslash, and a
    /// hand-built JSON object would emit an unparseable document for it. Going
    /// through `serde_json` is what makes this safe; the test is what keeps a
    /// future `format!` from quietly undoing it.
    /// What: a path with an embedded quote round-trips through a JSON parse.
    /// Test: this is the test.
    #[test]
    fn format_output_json_escapes_a_quoted_path() {
        let p = PathBuf::from(r#"/tmp/we"ird/trusty-review.sock"#);
        let out = format_output(&p, true, SocketFormat::Json);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(
            parsed.get("socket").and_then(|v| v.as_str()),
            Some(r#"/tmp/we"ird/trusty-review.sock"#)
        );
    }
}
