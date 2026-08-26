//! Handler for `trusty-memory port` — report where the daemon can be reached.
//!
//! Why: operators and agents need the daemon's address as a first-class CLI
//! surface (#526). Until #6286 that address was a TCP port picked out of
//! 7070–7079 and published in an `http_addr` file, and this command read that
//! file. ADR-0032 retired both: there is no listener to hold a port, nothing
//! writes the file, and `read_daemon_addr("trusty-memory")` therefore answers
//! `None` on every machine forever — so the command exited 1 reporting "no
//! daemon running" whether or not one was.
//!
//! What: reports the Unix socket the daemon binds — derived by
//! `trusty_common::daemon_socket_path`, the same call the daemon itself makes,
//! so caller and daemon compute the same path with nothing published between
//! them. Three formats, keeping the flags' audiences:
//!   - default: the socket path  →  `/…/trusty-memory.sock\n`
//!   - `--addr`: the same path, for a client that dials it
//!   - `--json`: `{"socket":"/…/trusty-memory.sock","serving":true}\n`
//!
//! **There is no port to print, and the command does not invent one.** The
//! `--port` shape a shell substitution used to interpolate into a URL has no
//! honest value now; printing the socket is what a caller can actually use.
//!
//! Every intentional output goes to **stdout**; errors go to **stderr**. The
//! exit contract is unchanged: when nothing is serving the socket, the command
//! exits non-zero so `$(trusty-memory port)` fails cleanly rather than
//! interpolating a path to a dead endpoint.
//!
//! Test: `format_socket_output_renders_each_format`,
//! `format_socket_output_escapes_an_awkward_path`. The exit arms call
//! `process::exit` and are covered by the manual `trusty-memory port` run
//! rather than in-process — a test cannot observe an exit it shares.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;

/// How long to wait for the socket to prove it is being served.
///
/// A local dial either connects or is refused immediately; the budget only
/// covers a loaded machine.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Output format requested by the caller.
///
/// Why: the shapes have distinct audiences — a bare path for shell
/// substitution, and JSON for a scripted consumer that also wants the liveness
/// verdict without re-probing.
/// What: `Port` and `Addr` both render the socket path, because since #6286
/// there is one address and it is not a port. They are kept distinct so the
/// existing flags stay accepted rather than becoming an unknown-argument error
/// in someone's script.
/// Test: `format_socket_output_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortFormat {
    /// The socket path (default).
    Port,
    /// The socket path, for a caller that passes it to a client.
    Addr,
    /// `{"socket":"…","serving":bool}` JSON object.
    Json,
}

/// Render the socket for output in the requested format.
///
/// Why: separating the formatting from the I/O lets a unit test assert the
/// output without binding a socket.
/// What: the path for `Port`/`Addr`; a JSON object carrying the path and the
/// liveness verdict for `Json`. `serde_json` does the escaping, so a data
/// directory containing a quote or a backslash cannot produce invalid JSON.
/// Test: `format_socket_output_renders_each_format`.
pub fn format_output(socket: &Path, serving: bool, format: PortFormat) -> String {
    match format {
        PortFormat::Port | PortFormat::Addr => socket.display().to_string(),
        PortFormat::Json => serde_json::json!({
            "socket": socket.display().to_string(),
            "serving": serving,
        })
        .to_string(),
    }
}

/// Entry point for `trusty-memory port [--json | --addr]`.
///
/// Why: exposes the daemon's address so a caller does not have to re-derive the
/// socket path from the data directory.
///
/// # Errors
///
/// Never returns `Err` — an unresolvable data directory and a socket nothing is
/// serving both print to stderr and exit 1, which is what a shell substitution
/// has to see.
///
/// Test: the formatting is covered by `format_socket_output_renders_each_format`;
/// the exit arms are exercised manually (see the module doc).
pub async fn handle_port(format: PortFormat) -> Result<()> {
    let socket = match crate::transport::uds::socket_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("trusty-memory: could not resolve the daemon socket path: {e:#}");
            std::process::exit(1);
        }
    };

    let serving = trusty_common::uds::socket_is_serving(&socket, PROBE_TIMEOUT).await;
    if !serving {
        eprintln!(
            "trusty-memory: no daemon serving {}. Start it with `trusty-memory start`.",
            socket.display()
        );
        std::process::exit(1);
    }

    println!("{}", format_output(&socket, serving, format));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Why: the three flags are a documented CLI contract, and a caller piping
    /// `--json` into `jq` needs the object shape to hold.
    /// Test: itself.
    #[test]
    fn format_socket_output_renders_each_format() {
        let socket = PathBuf::from("/tmp/trusty/trusty-memory.sock");
        assert_eq!(
            format_output(&socket, true, PortFormat::Port),
            "/tmp/trusty/trusty-memory.sock"
        );
        assert_eq!(
            format_output(&socket, true, PortFormat::Addr),
            "/tmp/trusty/trusty-memory.sock"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&format_output(&socket, false, PortFormat::Json))
                .expect("--json must emit parseable JSON");
        assert_eq!(parsed["socket"], "/tmp/trusty/trusty-memory.sock");
        assert_eq!(parsed["serving"], false);
    }

    /// Why: a path is operator-supplied by way of `TRUSTY_DATA_DIR_OVERRIDE`,
    /// and hand-formatting it into JSON would emit an unparseable object for a
    /// directory containing a quote or a backslash.
    /// Test: itself.
    #[test]
    fn format_socket_output_escapes_an_awkward_path() {
        let socket = PathBuf::from(r#"/tmp/a"b\c/trusty-memory.sock"#);
        let parsed: serde_json::Value =
            serde_json::from_str(&format_output(&socket, true, PortFormat::Json))
                .expect("an awkward path must still emit parseable JSON");
        assert_eq!(parsed["socket"], r#"/tmp/a"b\c/trusty-memory.sock"#);
    }
}
