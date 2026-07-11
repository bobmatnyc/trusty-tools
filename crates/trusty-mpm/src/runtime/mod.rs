//! Runtime adapter trait and concrete implementations.
//!
//! Why: the session manager must be able to swap different runtime backends
//! (Claude Code CLI via OAuth, or trusty-code via the direct Anthropic API)
//! without changing its own code. A trait seam here means new adapters slot in
//! without touching the manager.
//! What: defines [`RuntimeAdapter`] trait, [`RuntimeError`] error type, the
//! [`RuntimeKind`] selector + its [`build_adapter`] factory, re-exports the
//! two concrete adapters ([`ClaudeCodeAdapter`], [`TcodeAdapter`]), and
//! re-exports [`build_inplace_resume_command`]/[`InPlaceResumeCommand`] — the
//! `tm` CLI's building block for the bare-`tm` in-pane relaunch path (#2023 C).
//! Test: each adapter carries its own unit tests; both are testable without a
//! real tmux binary via a fake tmux driver. `RuntimeKind` parsing/serde is
//! covered by `runtime_kind_*` tests in this module.

mod claude_code;
mod tcode;

#[cfg(test)]
pub(crate) mod test_helpers;

pub use claude_code::{ClaudeCodeAdapter, InPlaceResumeCommand, build_inplace_resume_command};
pub use tcode::TcodeAdapter;

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::session_manager::ManagedTmuxDriver;

/// Errors produced by a runtime adapter during spawning.
///
/// Why: callers (HTTP handlers, the session manager) need structured errors
/// to map to the right HTTP status and log message.
/// What: one variant per failure class: spawn command failed, tmux unavailable,
/// or required binary not found on PATH.
/// Test: exercised by `ClaudeCodeAdapter` unit tests.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The spawn command could not be executed in the target pane.
    #[error("spawn failed: {0}")]
    Spawn(String),

    /// tmux was unavailable or a tmux operation failed.
    #[error("tmux unavailable: {0}")]
    TmuxUnavailable(String),

    /// A required binary (e.g. `claude`) was not found on PATH.
    #[error("binary not found: {0}")]
    BinaryNotFound(String),
}

/// Trait for launching an agent runtime inside a named tmux session.
///
/// Why: the session manager calls `spawn` without knowing or caring which
/// runtime backend is in use; the trait is the contract that makes that work.
/// What: one `spawn` method that takes the tmux session name, working directory,
/// and task description and starts the runtime inside the pane. `identify`
/// returns a human-readable backend name for logging.
/// Test: `ClaudeCodeAdapter` is the only MVP implementor; a `FakeTmuxDriver`
/// can substitute the tmux layer for unit tests.
pub trait RuntimeAdapter: Send + Sync {
    /// Start the runtime inside the already-created tmux session `tmux_name`.
    ///
    /// Why: the session manager creates the tmux session first, then calls
    /// `spawn` so the adapter can send the start command into the pane.
    /// What: sends the appropriate shell command(s) to start the runtime in the
    /// named tmux pane; returns `RuntimeError` on any failure. `session_id` is
    /// the managed session's UUID (#2023 component B) — it is exported as
    /// `TM_MANAGED_SESSION_ID` into the pane shell so a command run in that pane
    /// AFTER the runtime exits can identify which managed session it belongs to
    /// (needed by the in-place-relaunch path, #2023 component C).
    /// Test: `claude_code_adapter_spawn_sends_env_scrub_command`.
    fn spawn(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        session_id: &str,
    ) -> Result<(), RuntimeError>;

    /// Start the runtime in a RESUME context — prefer conversation continuity.
    ///
    /// Why (#1744): when resuming a managed session that exited ungracefully
    /// (tmux pane killed, terminal closed without `/quit`), the operator wants
    /// the prior conversation restored. Passing `--resume <claude_session_id>`
    /// restores the exact Claude Code conversation; `--continue` resumes the
    /// most-recent one when the id is unknown; plain `spawn` starts fresh.
    /// The default implementation delegates to [`Self::spawn`] (no resume flag),
    /// which is correct for the `tcode` adapter and any other runtime that does
    /// not support conversation continuity.
    /// What: same contract as `spawn` but with an optional `claude_session_id` for
    /// the `--resume` flag, an optional `pane_id` (the record's stored
    /// tmux pane identity, sibling-window hijack fix, follow-up to #2456) so
    /// the resume command lands in the SPECIFIC pane the session is bound to
    /// rather than whichever pane tmux considers active, plus the same
    /// `session_id` (managed session UUID, #2023 component B) forwarded to
    /// [`Self::spawn`] for the `TM_MANAGED_SESSION_ID` pane-shell export.
    /// Called by `resume_managed` instead of `spawn`.
    /// Test: `spawn_resume_with_id_uses_resume_flag`,
    /// `spawn_resume_without_id_uses_continue_flag`,
    /// `spawn_resume_targets_stored_pane_id_when_known`,
    /// `spawn_resume_falls_back_to_session_target_when_pane_id_unknown` in
    /// `claude_code` tests.
    fn spawn_resume(
        &self,
        tmux_name: &str,
        pane_id: Option<&str>,
        cwd: &Path,
        task: &str,
        claude_session_id: Option<&str>,
        session_id: &str,
    ) -> Result<(), RuntimeError> {
        let _ = (pane_id, claude_session_id);
        self.spawn(tmux_name, cwd, task, session_id)
    }

    /// Return a short human-readable name for this runtime backend.
    ///
    /// Why: logs and status responses need to identify which runtime is in use
    /// so operators can distinguish `claude-code` from `tcode` sessions.
    /// What: returns a static string like `"claude-code"` or `"tcode"`.
    /// Test: `claude_code_adapter_identifies`.
    fn identify(&self) -> &str;
}

/// Selector for which runtime backend a managed session uses.
///
/// Why: the spawn endpoint and `tm session new` let the operator pick a backend
/// (`runtime=tcode` / `--runtime tcode`); the choice must survive on the
/// persisted [`crate::session_manager::SessionRecord`] so `resume` re-spawns the
/// SAME backend rather than silently reverting to the default.
/// What: a two-variant enum with `Default` = [`RuntimeKind::ClaudeCode`] (so
/// existing behavior is unchanged), stable serde wire strings (`"claude-code"`,
/// `"tcode"`), `FromStr` for HTTP parsing, and a `clap::ValueEnum` impl so the
/// `--runtime` CLI flag rejects bad values at parse time rather than at the HTTP
/// layer (#1213). The `ValueEnum` value names are pinned to the same kebab-case
/// spellings as the serde wire form so the CLI and HTTP surfaces never diverge.
/// Test: `runtime_kind_default_is_claude_code`, `runtime_kind_from_str_*`,
/// `runtime_kind_serde_round_trip`, `runtime_kind_value_enum_matches_wire`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    /// Claude Code CLI over OAuth (the default; `ANTHROPIC_API_KEY` is scrubbed).
    #[value(name = "claude-code")]
    ClaudeCode,
    /// trusty-code (`tcode`) over the direct Anthropic API (`ANTHROPIC_API_KEY`).
    #[value(name = "tcode")]
    Tcode,
}

impl Default for RuntimeKind {
    /// Why: existing callers that omit a runtime selector must keep getting the
    /// Claude Code path so behavior is unchanged (#1203 acceptance criterion).
    /// What: returns [`RuntimeKind::ClaudeCode`].
    /// Test: `runtime_kind_default_is_claude_code`.
    fn default() -> Self {
        RuntimeKind::ClaudeCode
    }
}

impl RuntimeKind {
    /// Return the stable wire/log string for this kind.
    ///
    /// Why: logs, the persisted record, and the HTTP/CLI surface all need one
    /// canonical spelling per backend so they never drift.
    /// What: `"claude-code"` or `"tcode"` (matches the adapters' `identify()`).
    /// Test: `runtime_kind_as_str_matches_identify`.
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeKind::ClaudeCode => "claude-code",
            RuntimeKind::Tcode => "tcode",
        }
    }
}

impl FromStr for RuntimeKind {
    type Err = RuntimeError;

    /// Parse a runtime selector from an HTTP field or CLI flag value.
    ///
    /// Why: `runtime=tcode` (HTTP) and `--runtime tcode` (CLI) arrive as free
    /// strings that must map to a known backend or be rejected with a clear
    /// error rather than silently defaulting.
    /// What: accepts `claude-code`/`claude`/`claude_code` → `ClaudeCode` and
    /// `tcode`/`trusty-code` → `Tcode` (case-insensitive); any other value is a
    /// `RuntimeError::Spawn` describing the supported values.
    /// Test: `runtime_kind_from_str_accepts_known`, `runtime_kind_from_str_rejects_unknown`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude_code" | "claude" => Ok(RuntimeKind::ClaudeCode),
            "tcode" | "trusty-code" => Ok(RuntimeKind::Tcode),
            other => Err(RuntimeError::Spawn(format!(
                "unknown runtime '{other}'; supported values: claude-code, tcode"
            ))),
        }
    }
}

/// Build the concrete [`RuntimeAdapter`] for a [`RuntimeKind`].
///
/// Why: the spawn and resume HTTP handlers must turn the persisted/selected
/// runtime kind into an adapter without hard-coding `ClaudeCodeAdapter`; one
/// factory keeps that mapping in a single place so adding a backend touches only
/// this function and the enum.
/// What: returns a boxed adapter wrapping the shared tmux driver — a
/// [`ClaudeCodeAdapter`] or [`TcodeAdapter`].
/// Test: `build_adapter_returns_matching_identify`.
pub fn build_adapter(
    kind: RuntimeKind,
    tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>,
) -> Box<dyn RuntimeAdapter> {
    match kind {
        RuntimeKind::ClaudeCode => Box::new(ClaudeCodeAdapter::new(tmux)),
        RuntimeKind::Tcode => Box::new(TcodeAdapter::new(tmux)),
    }
}

#[cfg(test)]
mod tests {
    use super::test_helpers::FakeTmux;
    use super::*;

    #[test]
    fn runtime_kind_default_is_claude_code() {
        assert_eq!(RuntimeKind::default(), RuntimeKind::ClaudeCode);
    }

    #[test]
    fn runtime_kind_as_str_matches_identify() {
        assert_eq!(RuntimeKind::ClaudeCode.as_str(), "claude-code");
        assert_eq!(RuntimeKind::Tcode.as_str(), "tcode");
    }

    #[test]
    fn runtime_kind_from_str_accepts_known() {
        assert_eq!(
            "claude-code".parse::<RuntimeKind>().unwrap(),
            RuntimeKind::ClaudeCode
        );
        assert_eq!(
            "CLAUDE".parse::<RuntimeKind>().unwrap(),
            RuntimeKind::ClaudeCode
        );
        assert_eq!("tcode".parse::<RuntimeKind>().unwrap(), RuntimeKind::Tcode);
        assert_eq!(
            "trusty-code".parse::<RuntimeKind>().unwrap(),
            RuntimeKind::Tcode
        );
    }

    #[test]
    fn runtime_kind_from_str_rejects_unknown() {
        let err = "gpt".parse::<RuntimeKind>().unwrap_err();
        assert!(err.to_string().contains("unknown runtime"));
    }

    #[test]
    fn runtime_kind_serde_round_trip() {
        for kind in [RuntimeKind::ClaudeCode, RuntimeKind::Tcode] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: RuntimeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
        // The wire form must be the kebab-case spelling.
        assert_eq!(
            serde_json::to_string(&RuntimeKind::ClaudeCode).unwrap(),
            "\"claude-code\""
        );
        assert_eq!(
            serde_json::to_string(&RuntimeKind::Tcode).unwrap(),
            "\"tcode\""
        );
    }

    #[test]
    fn runtime_kind_value_enum_matches_wire() {
        // The clap CLI value names must be byte-identical to the serde wire form
        // and `as_str`, so `--runtime <x>` and `runtime=<x>` accept the same set.
        for kind in [RuntimeKind::ClaudeCode, RuntimeKind::Tcode] {
            let value = kind.to_possible_value().expect("kind has a CLI value");
            assert_eq!(value.get_name(), kind.as_str());
            // Round-trips back through the HTTP-side `FromStr` parser.
            let parsed = kind.as_str().parse::<RuntimeKind>().expect("parses");
            assert_eq!(parsed, kind);
        }
        // The CLI exposes exactly the two supported backends, in order.
        let names: Vec<String> = RuntimeKind::value_variants()
            .iter()
            .map(|k| k.to_possible_value().unwrap().get_name().to_owned())
            .collect();
        assert_eq!(names, vec!["claude-code".to_owned(), "tcode".to_owned()]);
    }

    #[test]
    fn build_adapter_returns_matching_identify() {
        let tmux = FakeTmux::new();
        let claude = build_adapter(RuntimeKind::ClaudeCode, tmux.clone());
        assert_eq!(claude.identify(), "claude-code");
        let tcode = build_adapter(RuntimeKind::Tcode, tmux);
        assert_eq!(tcode.identify(), "tcode");
    }
}
