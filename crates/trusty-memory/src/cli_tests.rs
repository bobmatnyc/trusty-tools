//! Unit tests for `serve` subcommand argument parsing (#914 PR4).
//!
//! Why: the `--http` flag was changed from `Option<SocketAddr>` to
//! `Option<Option<SocketAddr>>` so bare `--http` (explicit HTTP mode, dynamic
//! port) is valid in addition to `--http 127.0.0.1:7070` (specific address).
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

/// Why: bare `serve` must keep HTTP as default (no flags set).
/// What: parses `["trusty-memory", "serve"]` and asserts http=None, stdio=false.
/// Test: this function.
#[test]
fn serve_bare_is_http_default() {
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
