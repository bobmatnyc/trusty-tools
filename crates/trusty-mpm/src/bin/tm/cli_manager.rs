//! The `tm manager <action>` subcommand tree (DOC-36 §3.2/§6, epic #2109).
//!
//! Why: extracted from `cli.rs` (which is at its SLOC cap) so the Layer-3
//! manager verb set has a cohesive home it can grow into across phases without
//! pushing the monolithic `cli.rs` over the line-cap. Re-exported from `cli` so
//! `crate::cli::ManagerAction` stays the stable reference every dispatcher and
//! parse test already uses.
//! What: [`ManagerAction`] — the clap subcommand enum, one variant per daemon
//! verb `commands::manager` wraps.
//! Test: `cli_parses_manager_*` in `tests_manager.rs`.

use clap::Subcommand;

/// Actions for `tm manager <action>` (DOC-36 §3.2/§6, epic #2109).
///
/// Why: mirrors `ProjectsAction`'s shape — one variant per daemon verb this
/// surface wraps. Phase 1 ships the read-only triad (`Status`/`Digest`/`Chat`);
/// phase 2 adds advisory task routing (`Route`, #2585).
/// What: each variant is a thin client over the matching
/// `DaemonClient::manager_*` method (`client/http_client/manager.rs`).
/// Test: `cli_parses_manager_*` in `tests_manager.rs`.
#[derive(Debug, Subcommand)]
pub(crate) enum ManagerAction {
    /// Deterministic cross-project portfolio rollup — no LLM call.
    Status {
        /// Emit the raw status JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// LLM-authored portfolio (or single-project) narrative, with a
    /// deterministic fallback when no inference provider is configured.
    Digest {
        /// `portfolio` (default) or `project:<name>` to scope to one project.
        #[arg(long, default_value = "portfolio")]
        scope: String,
        /// Emit the raw digest JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// One-shot chat turn against the portfolio manager persona.
    ///
    /// Why (conversation-key convention, #2583): defaults to a stable
    /// per-user key (`cli:$USER`, see `commands::manager::default_conversation_key`)
    /// so repeated one-shot invocations on the same machine accumulate as one
    /// ongoing conversation server-side (matching `SessionProxy`'s focus-map
    /// keying, DOC-36 §3.2) — `--conversation` overrides it for scripts/tests
    /// that need an isolated key. Interactive REPL mode (multi-turn within a
    /// single process) is deferred to a follow-up; this ships the one-shot
    /// form the issue's acceptance criteria require.
    Chat {
        /// The message to send. When omitted, reads the full message from
        /// stdin (supports both piping and a terminal Ctrl-D-terminated
        /// single message).
        message: Option<String>,
        /// Override the conversation key (defaults to a stable per-user key).
        #[arg(long)]
        conversation: Option<String>,
        /// Emit the raw chat-response JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Route a free-text task to the project it belongs to (advisory).
    ///
    /// Why (§7 Q5, RESOLVED 2026-07-14): a thin, API-first wrapper over
    /// `POST /api/v1/manager/route-task` (WI-8, #2585) for headless/scriptable
    /// use — it surfaces the resolved `{ project, confidence, rationale }` and
    /// NEVER launches or mutates a session (acting on a route is the separate
    /// explicit `/manager/act` proposal-and-confirm flow, WI-9). No separate
    /// CLI-only disambiguation prompt is introduced for v1; disambiguation lives
    /// entirely in the daemon's routing endpoint. Needs no channel/bot token.
    Route {
        /// The free-text task to route (e.g. "fix the flaky auth test").
        text: String,
        /// Emit the raw route-task JSON instead of the human view.
        #[arg(long)]
        json: bool,
    },
}
