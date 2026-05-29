//! /tm REPL slash-command dispatcher.
//!
//! Why: Issue #316 — give users a single `/tm <subcommand>` interface from
//! the REPL to inspect, create, and control tmux-managed sessions without
//! leaving the chat. Centralizing the dispatcher here keeps `repl/mod.rs`
//! focused on UI plumbing and lets the same code be reused by tests / other
//! frontends.
//! What: `handle_tm_command` parses the first whitespace-delimited token as a
//! subcommand name and routes to the corresponding `cmd_*` helper, each of
//! which writes user-facing output into a shared `String` buffer. All
//! subcommands return `Result<()>`; helper formatting errors are bubbled up
//! via `?`.
//! Test: `tests` module below covers help + unknown-command rendering and
//! the parse paths that don't require a live tmux server. Live-tmux flows
//! are exercised by the integration tests.

// Module layout (see #366 split): dispatcher + help text here; the per-verb
// `cmd_*` handlers in `handlers.rs`; tests in `tests.rs`.
mod handlers;

#[cfg(test)]
mod tests;

use std::fmt::Write;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use handlers::{
    cmd_attach, cmd_capture, cmd_detect, cmd_kill, cmd_list, cmd_new, cmd_pause, cmd_projects,
    cmd_reconcile, cmd_resume, cmd_send, cmd_status,
};

use crate::tm::manager::TmManager;

/// Dispatch a `/tm <subcommand> [args]` invocation.
///
/// Why: Single entry point so the REPL only has to forward the raw arg
/// string; this function owns the parse of the first token + delegation.
/// What: Splits `args` on the first whitespace, matches the subcommand name,
/// and calls the matching helper. Unknown subcommands produce a friendly
/// hint pointing at `/tm help`.
/// Test: `tests::dispatch_help` and `tests::dispatch_unknown` below.
pub async fn handle_tm_command(
    manager: &Arc<Mutex<TmManager>>,
    args: &str,
    out: &mut String,
) -> Result<()> {
    let args = args.trim();
    let (sub, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
    let rest = rest.trim();

    match sub {
        "list" | "ls" => cmd_list(manager, out).await,
        "new" => cmd_new(manager, rest, out).await,
        "attach" | "a" => cmd_attach(manager, rest, out).await,
        "pause" => cmd_pause(manager, rest, out).await,
        "resume" => cmd_resume(manager, rest, out).await,
        "kill" => cmd_kill(manager, rest, out).await,
        "send" => cmd_send(manager, rest, out).await,
        "capture" | "cap" => cmd_capture(manager, rest, out).await,
        "detect" => cmd_detect(manager, rest, out).await,
        "reconcile" | "sync" => cmd_reconcile(manager, out).await,
        "status" | "st" => cmd_status(manager, rest, out).await,
        "projects" | "proj" => cmd_projects(manager, out).await,
        "help" | "" => {
            write_tm_help(out);
            Ok(())
        }
        _ => {
            writeln!(out, "Unknown /tm subcommand: '{sub}'. Try /tm help")?;
            Ok(())
        }
    }
}

// ==================== /tm help ====================

/// Write the static help text into `out`.
///
/// Why: Centralizing the help text avoids drift between `/help` summary and
/// `/tm help`'s detail page.
/// What: One usage line per subcommand.
/// Test: `tests::dispatch_help` asserts the buffer is non-empty and lists
/// all the verbs.
pub fn write_tm_help(out: &mut String) {
    let _ = writeln!(
        out,
        "/tm commands:
  /tm list                                 List all TM sessions
  /tm new [name] [-p path] [-a adapter]    Create new session
  /tm attach <name>                        Show attach command
  /tm pause <name>                         Pause session (adapter-dependent)
  /tm resume <name>                        Resume session (adapter-dependent)
  /tm kill <name>                          Kill tmux session
  /tm send <name> <message>                Send message to session
  /tm capture <name> [lines]               Show pane output (default 50)
  /tm detect <name>                        Auto-detect adapter type
  /tm reconcile                            Sync registry with live tmux
  /tm status [name]                        Session detail or summary
  /tm projects                             List projects with frameworks
  /tm help                                 Show this help"
    );
}

// ==================== shared helpers ====================

/// Truncate a string to `n` display chars, appending `…` when cut.
///
/// Why: Several `cmd_*` renderers clamp names/paths to a fixed column width;
/// centralizing keeps the ellipsis behavior consistent.
/// What: Returns `s` unchanged when it fits, else the first `n-1` chars + `…`.
/// Test: `truncate_*` in `tests`.
pub(super) fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Extract the leading session name from `rest`, or write a usage line.
///
/// Why: Most subcommands need a session name; centralize the "missing arg"
/// message so the wording stays consistent.
/// What: Returns `Ok(Some(name))` on success, writes a usage line and
/// returns `Ok(None)` when `rest` is empty.
/// Test: `require_name_*` in `tests`.
pub(super) fn require_name(rest: &str, sub: &str, out: &mut String) -> Result<Option<String>> {
    let name = rest.split_whitespace().next().unwrap_or("").to_string();
    if name.is_empty() {
        writeln!(out, "usage: /tm {sub} <name>")?;
        return Ok(None);
    }
    Ok(Some(name))
}
