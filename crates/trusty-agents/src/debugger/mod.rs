//! `trusty-agents debug [session]` — interactive REPL debugger TUI (#237).
//!
//! Why: Investigating REPL/ctrl-socket behaviour by reading log files is
//! slow. This subcommand spawns the REPL inside a detached tmux session
//! and renders a live two-pane TUI in the invoking terminal so operators
//! can observe scrollback, inject commands, and watch socket liveness in
//! one window.
//! What: Parses CLI args, ensures tmux is present, optionally launches a
//! fresh tmux session running the current `trusty-agents` executable in default
//! REPL mode, then hands off to the ratatui event loop in `tui::run_tui`.
//! On exit (normal or panic) it tears the tmux session down.
//! Test: `parse_args_defaults` and `parse_args_overrides` cover argument
//! parsing; full TUI behaviour is verified manually via `cargo run --
//! debug` and through the deterministic `DebugApp` unit tests.

pub mod socket_monitor;
pub mod tmux;
pub mod tui;

use anyhow::{Context, Result, anyhow};
use clap::Parser;

use socket_monitor::SocketMonitor;
use tmux::TmuxAdapter;

/// Default tmux session name. Documented constant so tests can assert the
/// CLI's behaviour without hard-coding the literal in two places.
pub const DEFAULT_SESSION: &str = "ompm-debug";

/// Default scrollback line count for `tmux capture-pane`.
pub const DEFAULT_LINES: u32 = 500;

#[derive(Debug, Parser)]
#[command(name = "tagent debug", about = "Interactive REPL debugger TUI")]
struct DebugArgs {
    /// tmux session name to launch / attach.
    #[arg(long, default_value = DEFAULT_SESSION)]
    session: String,

    /// Number of scrollback lines to capture per tick.
    #[arg(long, default_value_t = DEFAULT_LINES)]
    lines: u32,

    /// Attach to an existing tmux session instead of launching a new one.
    #[arg(long)]
    no_launch: bool,
}

impl DebugArgs {
    fn from_args(args: &[String]) -> Result<Self> {
        // Prepend the binary name so clap's argv conventions are respected.
        let mut argv = vec!["trusty-agents-debug".to_string()];
        argv.extend_from_slice(args);
        DebugArgs::try_parse_from(&argv).map_err(|e| anyhow!("{e}"))
    }
}

/// Subcommand entry point invoked from `main.rs`.
///
/// Why: Keeps the prefix-dispatch in `main.rs` thin and lets all the
/// debugger logic stay in this module.
/// What: Parses args, validates tmux availability, prepares the session
/// (kill stale + create new, unless `--no-launch`), then runs the TUI.
/// Test: integration via `cargo run -- debug --no-launch ...`.
pub async fn run_debug_subcommand(args: &[String]) -> Result<()> {
    let parsed = DebugArgs::from_args(args)?;

    if !TmuxAdapter::is_available() {
        return Err(anyhow!(
            "tmux not found in PATH; install tmux to use `tagent debug`"
        ));
    }
    let tmux = TmuxAdapter::new().context("initialise tmux adapter")?;

    if !parsed.no_launch {
        // Best-effort cleanup of a prior session with the same name so the
        // user always gets a fresh REPL when they invoke `debug`.
        if tmux.session_exists(&parsed.session) {
            tracing::info!(session = %parsed.session, "killing existing tmux session");
            let _ = tmux.kill_session(&parsed.session);
        }

        // Launch the current executable in default REPL mode.
        let exe = std::env::current_exe().context("locate current executable")?;
        let cmd = exe
            .to_str()
            .ok_or_else(|| anyhow!("current_exe path is not valid UTF-8"))?
            .to_string();
        tmux.create_session(&parsed.session, &cmd)
            .with_context(|| format!("create tmux session {}", parsed.session))?;
    } else if !tmux.session_exists(&parsed.session) {
        return Err(anyhow!(
            "--no-launch given but session `{}` does not exist",
            parsed.session
        ));
    }

    let pane_id = tmux
        .get_pane_id(&parsed.session)
        .with_context(|| format!("read pane id for session {}", parsed.session))?;

    let project_dir = tui::resolve_project_dir();
    let socket_monitor = SocketMonitor::new(&project_dir);

    // Run the blocking TUI on a thread so we don't hold the tokio runtime
    // worker. The TUI itself is synchronous (crossterm + ratatui).
    let session = parsed.session.clone();
    let pane_id_clone = pane_id.clone();
    let lines = parsed.lines;
    let tmux_clone = tmux.clone();

    let handle = tokio::task::spawn_blocking(move || {
        tui::run_tui(tmux_clone, socket_monitor, session, pane_id_clone, lines)
    });

    let result = handle.await.context("debug TUI thread panicked")?;

    // Tear the session down on exit so we don't leave orphan REPLs.
    if !parsed.no_launch && tmux.session_exists(&parsed.session) {
        let _ = tmux.kill_session(&parsed.session);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_defaults() {
        let parsed = DebugArgs::from_args(&[]).expect("default args parse");
        assert_eq!(parsed.session, DEFAULT_SESSION);
        assert_eq!(parsed.lines, DEFAULT_LINES);
        assert!(!parsed.no_launch);
    }

    #[test]
    fn parse_args_overrides() {
        let parsed = DebugArgs::from_args(&[
            "--session".into(),
            "custom".into(),
            "--lines".into(),
            "200".into(),
            "--no-launch".into(),
        ])
        .expect("override args parse");
        assert_eq!(parsed.session, "custom");
        assert_eq!(parsed.lines, 200);
        assert!(parsed.no_launch);
    }
}
