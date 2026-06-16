//! `tctl ui` — launch trusty-console or print its URL (#1316, DOC-7).
//!
//! Why: The "option to launch the console" — the single HTTP front door for the
//! stack. Post-de-bundle (#1318) `trusty-console` is a standalone binary on
//! PATH; `tctl ui` discovers a running console (or starts one) and either opens
//! the URL or, with `--print`, just emits it.
//!
//! What: Resolves the console URL via the console's own `port --json` contract
//! (reusing [`super::up::os_env::parse_console_port`]). With `--print`, prints
//! the resolved URL (or the default) and exits. Otherwise spawns
//! `trusty-console serve` detached and prints the URL it will serve on. Returns
//! a non-zero exit when the console binary is absent.
//!
//! Test: `tests` covers the URL resolution (`resolve_url`) against canned
//! `port --json` bytes and the absent-binary path indirectly.

use std::process::Command;

use super::up::os_env::parse_console_port;
use crate::output::render_json;

/// The console's default URL when no running instance reports a port.
///
/// Why: A sane fallback so `--print` always yields a usable URL even before the
/// console has started (it binds `127.0.0.1:7788` by default — see
/// `trusty_console::DEFAULT_HTTP`).
const DEFAULT_CONSOLE_URL: &str = "http://127.0.0.1:7788";

/// Resolve the console URL from a `trusty-console port --json` invocation.
///
/// Why: Isolating the resolution (spawn + parse + fallback) from the command
/// flow keeps the URL logic testable via canned JSON.
///
/// What: Returns the parsed `http://addr:port` URL, or `None` when the console
/// binary is absent / errors / emits unparseable output (caller applies the
/// default).
///
/// Test: `resolve_url_from_bytes` covers the parse; the spawn is side-effecting.
fn query_console_url() -> Option<String> {
    let out = Command::new("trusty-console")
        .args(["port", "--json"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_console_port(&out.stdout)
}

/// Whether the `trusty-console` binary is resolvable on PATH.
///
/// Why: `tctl ui` (launch mode) must fail clearly when the console is not
/// installed rather than silently doing nothing.
/// What: `which trusty-console`.
/// Test: Side-effect-only (PATH); the absent path is reported as exit 4.
fn console_present() -> bool {
    which::which("trusty-console").is_ok()
}

/// Handle `tctl ui [--print]`.
///
/// Why: Phase-1 entry point for the console launch/URL command (#1316 priority 4).
///
/// What:
/// - `--print`: resolve and print the URL (live port if a console is running,
///   else the default), in human or `--json` form. Exit 0.
/// - default: require the console binary; spawn `trusty-console serve` detached,
///   print the URL it will serve on, and exit 0. If the binary is absent, print
///   an install hint and exit 4.
///
/// Test: `tests::resolve_url_from_bytes`; the spawn path is side-effecting.
pub fn run(print: bool, json: bool) -> i32 {
    let live_url = query_console_url();
    let url = live_url
        .clone()
        .unwrap_or_else(|| DEFAULT_CONSOLE_URL.to_owned());

    if print {
        if json {
            let _ = render_json(&serde_json::json!({
                "command": "ui",
                "url": url,
                "running": live_url.is_some(),
            }));
        } else {
            println!("{url}");
        }
        return 0;
    }

    // Launch mode: the console binary must be present.
    if !console_present() {
        let hint = "trusty-console not found on PATH — install it with `tctl install trusty-console` or `cargo install trusty-console`.";
        if json {
            let _ = render_json(&serde_json::json!({"command":"ui","error":hint}));
        } else {
            eprintln!("tctl ui: {hint}");
        }
        return 4;
    }

    // If a console is already running, do not spawn a second one — just report.
    if live_url.is_some() {
        report_launch(&url, true, json);
        return 0;
    }

    match Command::new("trusty-console").arg("serve").spawn() {
        Ok(_child) => {
            report_launch(&url, false, json);
            0
        }
        Err(e) => {
            let msg = format!("failed to launch trusty-console serve: {e}");
            if json {
                let _ = render_json(&serde_json::json!({"command":"ui","error":msg}));
            } else {
                eprintln!("tctl ui: {msg}");
            }
            4
        }
    }
}

/// Report a console launch (or already-running) result.
///
/// Why: Both the "already running" and "just spawned" paths emit the same shape.
/// What: Prints the URL (human) or a `{command,url,already_running}` JSON object.
/// Test: Side-effect-only.
fn report_launch(url: &str, already_running: bool, json: bool) {
    if json {
        let _ = render_json(&serde_json::json!({
            "command": "ui",
            "url": url,
            "already_running": already_running,
        }));
    } else if already_running {
        println!("trusty-console already running at {url}");
    } else {
        println!("trusty-console starting at {url}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The URL resolution from the console's `port --json` contract is the
    /// load-bearing piece of `tctl ui`; verify it against canned bytes.
    /// What: Parses a `{addr,port}` envelope and asserts the composed URL.
    /// Test: This is the test.
    #[test]
    fn resolve_url_from_bytes() {
        let json = br#"{"addr":"127.0.0.1","port":7788}"#;
        assert_eq!(
            parse_console_port(json),
            Some("http://127.0.0.1:7788".to_owned())
        );
    }

    /// Why: A missing `port` field must yield `None` so the default applies.
    /// What: Parses an envelope without `port`; asserts `None`.
    /// Test: This is the test.
    #[test]
    fn resolve_url_missing_port_is_none() {
        let json = br#"{"addr":"127.0.0.1"}"#;
        assert_eq!(parse_console_port(json), None);
    }

    /// Why: The default URL is the fallback contract; pin it.
    /// What: Asserts the default constant shape.
    /// Test: This is the test.
    #[test]
    fn default_url_is_localhost_7788() {
        assert_eq!(DEFAULT_CONSOLE_URL, "http://127.0.0.1:7788");
    }
}
