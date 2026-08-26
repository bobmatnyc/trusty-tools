//! Unit tests for `serve` argument parsing and transport selection
//! (#914 PR4; transport matrix rewritten by #5267).
//!
//! Why: two separate contracts live here. Parsing — the `--http` flag is
//! `Option<Option<SocketAddr>>` so bare `--http` and `--http ADDR` are both
//! valid, and `--stdio` conflicts with both HTTP flags. Selection (#5267) —
//! which server bare `serve` actually runs, asserted via [`super::serve_mode`]
//! rather than by parsing alone, because a parse-only test passes identically
//! before and after the behavior change.
//! These tests guard against clap-level regressions in:
//!
//!   - bare `serve` (no flags)
//!   - `serve --http` (explicit HTTP, no address)
//!   - `serve --http 127.0.0.1:7070` (specific address)
//!   - `serve --stdio` (stdio mode)
//!   - `serve --http --stdio` (must error — mutually exclusive)
//!
//! What: exercises `Cli::try_parse_from` (clap 4 `Parser` derive) and
//! matches on the resulting `Command::Serve` variant.
//!
//! Test: this file.

use super::{Cli, Command};
use clap::Parser;

/// Why: bare `serve` sets no transport flag — that is what #5267 reinterprets.
/// What: parses `["trusty-memory", "serve"]` and asserts all three transport
/// flags are unset. This is the PARSING contract only; which server that
/// selects is `serve_mode_bare_is_stdio`'s job.
/// Test: this function.
#[test]
fn serve_bare_sets_no_transport_flag() {
    let cli = Cli::try_parse_from(["trusty-memory", "serve"]).expect("parse ok");
    let Command::Serve {
        http,
        stdio,
        foreground,
        ..
    } = cli.command
    else {
        panic!("expected Serve");
    };
    assert!(http.is_none(), "bare serve: http must be None");
    assert!(!stdio, "bare serve: stdio must be false");
    assert!(!foreground, "bare serve: foreground must be false");
}

/// Why: `serve --http` (bare, no address value) must select explicit HTTP
/// mode with dynamic port (same runtime behaviour as bare `serve`).
/// What: parses `["trusty-memory", "serve", "--http"]` and asserts
/// http=Some(None), stdio=false.
/// Test: this function.
#[test]
fn serve_http_bare_parses_as_some_none() {
    let cli = Cli::try_parse_from(["trusty-memory", "serve", "--http"]).expect("--http bare ok");
    let Command::Serve { http, stdio, .. } = cli.command else {
        panic!("expected Serve");
    };
    assert_eq!(http, Some(None), "--http (bare) must parse as Some(None)");
    assert!(!stdio, "--http bare: stdio must be false");
    // flatten() must return None so run_serve takes the dynamic-port path.
    assert!(
        http.flatten().is_none(),
        "--http bare flattened must be None (dynamic port)"
    );
}

/// Why: `serve --http 127.0.0.1:7070` must bind that specific address.
/// What: parses the full `--http <ADDR>` form and asserts http=Some(Some(addr)).
/// Test: this function.
#[test]
fn serve_http_with_addr_parses_as_some_some() {
    let cli = Cli::try_parse_from(["trusty-memory", "serve", "--http", "127.0.0.1:7070"])
        .expect("--http ADDR ok");
    let Command::Serve { http, .. } = cli.command else {
        panic!("expected Serve");
    };
    let addr: std::net::SocketAddr = "127.0.0.1:7070".parse().unwrap();
    assert_eq!(
        http,
        Some(Some(addr)),
        "--http ADDR must parse as Some(Some(addr))"
    );
    assert_eq!(
        http.flatten(),
        Some(addr),
        "--http ADDR flattened must return the address"
    );
}

/// Why: `--http` and `--stdio` are mutually exclusive — clap must reject
/// the combination before any dispatch logic runs.
/// What: parses `["trusty-memory", "serve", "--http", "--stdio"]` and
/// asserts that `try_parse_from` returns an Err.
/// Test: this function.
#[test]
fn serve_http_and_stdio_together_is_error() {
    let result = Cli::try_parse_from(["trusty-memory", "serve", "--http", "--stdio"]);
    assert!(
        result.is_err(),
        "--http and --stdio together must be rejected by clap"
    );
}

// ---------------------------------------------------------------------------
// Transport selection (#5267) — the behavior change, not just the parse.
// ---------------------------------------------------------------------------

use super::{serve_mode, ServeMode};

/// Parse an argv and return the transport it selects.
///
/// Why: every selection test below needs the same parse-then-decide pipeline;
/// routing them through one helper keeps each test one assertion long.
fn mode_of(args: &[&str]) -> ServeMode {
    let cli = Cli::try_parse_from(args).expect("parse ok");
    let Command::Serve {
        http,
        stdio,
        foreground,
        ..
    } = cli.command
    else {
        panic!("expected Serve");
    };
    serve_mode(&http, foreground, stdio)
}

/// Why: THE #5267 regression test. Before the change bare `serve` selected the
/// HTTP daemon; it now speaks MCP stdio, matching `trusty-search serve`. This
/// assertion fails against the pre-fix binary — a parse-only test would not.
/// What: asserts bare `serve` selects stdio AND requests the human notice.
/// Test: itself.
#[test]
fn serve_mode_bare_is_stdio() {
    assert_eq!(
        mode_of(&["trusty-memory", "serve"]),
        ServeMode::Stdio { notify: true },
        "bare `serve` must speak MCP stdio (#5267)"
    );
}

/// Why: `serve --stdio` was already stdio and must be byte-identical in
/// behavior — but it must NOT emit the notice, because its meaning did not
/// change and its caller is an MCP client.
/// Test: itself.
#[test]
fn serve_mode_explicit_stdio() {
    assert_eq!(
        mode_of(&["trusty-memory", "serve", "--stdio"]),
        ServeMode::Stdio { notify: false },
        "`serve --stdio` must stay stdio and stay silent"
    );
}

/// Why: `serve --http` (bare) selected HTTP before and must still.
/// Test: itself.
#[test]
fn serve_mode_daemon_bare() {
    assert_eq!(
        mode_of(&["trusty-memory", "serve", "--http"]),
        ServeMode::Daemon
    );
}

/// Why: `serve --http ADDR` is how callers bind a specific port; unchanged.
/// Test: itself.
#[test]
fn serve_mode_daemon_addr() {
    assert_eq!(
        mode_of(&["trusty-memory", "serve", "--http", "127.0.0.1:7070"]),
        ServeMode::Daemon
    );
}

/// Why: the launchd plist and `handle_start` both spawn `serve --foreground`.
/// If this regressed, the daemon on every installed machine would come up as a
/// stdio process blocking on a null stdin instead of an HTTP server.
/// Test: itself.
#[test]
fn serve_mode_foreground_is_daemon() {
    assert_eq!(
        mode_of(&["trusty-memory", "serve", "--foreground"]),
        ServeMode::Daemon,
        "`serve --foreground` is the launchd/`start` daemon path"
    );
}

/// Why: `--palace` is a scoping flag, not a transport flag, so it must not
/// change the selection — `serve --palace X` is still the bare form.
/// Test: itself.
#[test]
fn serve_mode_palace_only_is_stdio() {
    assert_eq!(
        mode_of(&["trusty-memory", "serve", "--palace", "demo"]),
        ServeMode::Stdio { notify: true }
    );
}

/// Why: the parser must stay strict. Making bare `serve` permissive must not
/// have loosened anything else — an unknown flag is still an error.
/// Test: itself.
#[test]
fn serve_rejects_unknown_flag() {
    assert!(
        Cli::try_parse_from(["trusty-memory", "serve", "--not-a-real-flag"]).is_err(),
        "unknown flags must still be rejected"
    );
}

/// Why: `--foreground` and `--stdio` are mutually exclusive (`conflicts_with`),
/// and that relationship must survive the #5267 dispatch rewrite.
/// Test: itself.
#[test]
fn serve_foreground_conflicts_with_stdio() {
    assert!(
        Cli::try_parse_from(["trusty-memory", "serve", "--foreground", "--stdio"]).is_err(),
        "--foreground and --stdio must remain mutually exclusive"
    );
}
