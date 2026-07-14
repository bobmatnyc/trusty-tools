//! `tm coordinator` / `tm sm` command group (DOC-14 SM-STDIO #1291).
//!
//! Why: extracted from `cli.rs` (issue #2603) to keep the top-level file
//! under the 500-SLOC production cap.
//! What: [`CoordinatorAction`] — the `serve --stdio` verb.
//! Test: `cli_parses_sm_serve_stdio` in `tests.rs`.

use clap::Subcommand;

/// Modes for the `tm watch` command group.
///
/// Why: `poll` and `listen` share every flag but differ in lifecycle (one-shot
/// vs loop); a sub-subcommand enum keeps them discoverable and individually
/// parseable while reusing one flag set via the flattened [`WatchArgs`].
/// What: `Poll` (discover + dispatch once, then exit) and `Listen` (repeatedly
/// poll on an interval, processing newly-matched issues, until Ctrl-C).
/// Test: `cli_parses_watch_poll`, `cli_parses_watch_listen` in `tests.rs`.
/// Subcommands for `tm coordinator` / `tm sm` (DOC-14 SM-STDIO #1291).
///
/// Why: `tm sm` chats by default (a plain message), but the SM's PRIMARY,
/// API-first interface is the JSON-RPC over STDIO adapter (§1A.1). Exposing it as
/// a `serve` subcommand under the `sm` alias matches the trusty-* `serve --stdio`
/// convention while leaving `tm sm <message>` for ad-hoc chat.
/// What: one variant, `Serve { stdio }`, mirroring the daemon's `serve --stdio`
/// flag shape. `--stdio` selects the newline-delimited JSON-RPC adapter; without
/// it the subcommand prints guidance (the HTTP/TUI surfaces are separate).
/// Test: `cli_parses_sm_serve_stdio` in `tests.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum CoordinatorAction {
    /// Run the SM JSON-RPC 2.0 over STDIO adapter (the headless drive surface).
    Serve {
        /// Speak newline-delimited JSON-RPC 2.0 on stdin/stdout (logs to stderr).
        ///
        /// Why: the SM's API-first surface — a parent `claude-mpm`/PM drives every
        /// `sm.*` method headlessly over stdio (§1A.2). Without this flag there is
        /// no other `serve` mode yet, so it is effectively required.
        #[arg(long)]
        stdio: bool,
    },
}
