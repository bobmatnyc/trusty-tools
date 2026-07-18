//! trusty-code (`tcode`) runtime adapter.
//!
//! Why: alongside the OAuth-based Claude Code runtime, operators need a
//! lightweight, API-key-based backend for cost-sensitive deployments. `tcode`
//! (the trusty-code CLI) talks directly to the Anthropic API using the
//! `ANTHROPIC_API_KEY` from the environment — the opposite of `ClaudeCodeAdapter`,
//! which *scrubs* that key so Claude Code falls back to OAuth.
//! What: [`TcodeAdapter`] wraps a [`ManagedTmuxDriver`] and implements
//! [`RuntimeAdapter`]; `spawn` builds a `tcode run-task` command rooted at the
//! workspace and sends it to the named tmux pane after verifying the `tcode`
//! binary is on PATH. The `ANTHROPIC_API_KEY` is intentionally preserved.
//! Test: `tcode_adapter_identifies`, `tcode_build_spawn_command_*`,
//! `tcode_adapter_spawn_sends_run_task` (see the module test block).

use std::path::Path;
use std::sync::Arc;

use tracing::debug;

use crate::session_manager::ManagedTmuxDriver;

use super::RuntimeAdapter;
use super::RuntimeError;

/// The `tcode` binary name expected on PATH.
///
/// Why: centralises the binary name so the availability check and the spawn
/// command never drift apart.
/// What: literal `"tcode"` — the binary produced by the `trusty-code` crate
/// (`[[bin]] name = "tcode"`).
/// Test: `tcode_build_spawn_command_starts_with_binary`.
const TCODE_BINARY: &str = "tcode";

/// Default agent name `tcode run-task` is invoked with when the operator does
/// not pin one.
///
/// Why: `tcode run-task <agent> <task> --project <cwd>` requires an agent name;
/// `engineer` is the general-purpose implementation agent shipped in every
/// `.claude/agents/` directory, making it the safe, predictable default for a
/// freshly provisioned managed session.
/// What: literal `"engineer"`.
/// Test: `tcode_build_spawn_command_uses_default_agent`.
const DEFAULT_AGENT: &str = "engineer";

/// Runtime adapter that launches the `tcode` CLI inside a tmux session.
///
/// Why: provides the direct-Anthropic-API alternative to `ClaudeCodeAdapter`;
/// coupling the launch sequence (binary check, command construction, tmux send)
/// to a typed adapter keeps the session manager free of runtime-specific logic.
/// What: holds a [`ManagedTmuxDriver`] reference; `spawn` verifies the `tcode`
/// binary exists, builds a `tcode run-task <agent> <task> --project <cwd>`
/// command, and sends it to the named pane. Unlike the Claude Code adapter it
/// does NOT scrub `ANTHROPIC_API_KEY` — `tcode` needs it for the API-key path.
/// Test: `tcode_adapter_identifies`, `tcode_adapter_spawn_sends_run_task`.
pub struct TcodeAdapter {
    tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>,
}

impl TcodeAdapter {
    /// Construct an adapter backed by the given tmux driver.
    ///
    /// Why: the session manager injects the tmux driver via `Arc<dyn …>` so the
    /// adapter is testable without a real tmux binary.
    /// What: stores the driver reference.
    /// Test: used in every `TcodeAdapter` test.
    pub fn new(tmux: Arc<dyn ManagedTmuxDriver + Send + Sync>) -> Self {
        Self { tmux }
    }

    /// Return `true` if the `tcode` binary can be found on `PATH`.
    ///
    /// Why: `spawn` must return `RuntimeError::BinaryNotFound` rather than
    /// sending a command that silently fails inside the pane. The previous
    /// implementation shelled out to `which`, which is absent on some targets
    /// (notably Windows) and adds a subprocess; the `which` crate performs the
    /// same PATH/PATHEXT resolution portably and in-process (#1213).
    /// What: resolves `tcode` against `PATH` via [`which::which`] and reports
    /// whether a matching executable was found (any lookup error → `false`).
    /// Test: `tcode_adapter_binary_check_returns_bool`.
    fn tcode_available() -> bool {
        which::which(TCODE_BINARY).is_ok()
    }

    /// Build the `tcode run-task` line and send it to the named tmux pane.
    ///
    /// Why: this is the half of `spawn` AFTER the binary-availability gate —
    /// command construction plus the tmux send. Factoring it out lets the unit
    /// tests exercise the send path deterministically through `FakeTmux` without
    /// a real `tcode` binary on PATH (the availability check is the only part
    /// that depends on the environment), closing the silent-skip gap in #1213.
    /// What: constructs the command via [`build_spawn_command`] (which carries
    /// the `TM_MANAGED_SESSION_ID` export, #2023 component B), logs it, and
    /// forwards it to `send_line`, mapping any tmux failure to
    /// [`RuntimeError::TmuxUnavailable`]. It does NOT check binary availability.
    /// Test: `tcode_adapter_spawn_sends_run_task` (always runs in CI).
    fn send_spawn_command(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        session_id: &str,
    ) -> Result<(), RuntimeError> {
        let command = build_spawn_command(cwd, task, session_id);
        debug!(
            session = %tmux_name,
            cwd = %cwd.display(),
            task = %task,
            "spawning tcode in tmux pane"
        );
        self.tmux
            .send_line(tmux_name, &command)
            .map_err(|e| RuntimeError::TmuxUnavailable(e.to_string()))?;
        // #2157 item 1: durable publish, belt-and-suspenders alongside the
        // pane-shell export baked into build_spawn_command above — see
        // `claude_code::ClaudeCodeAdapter::publish_session_env` for the full
        // rationale (shared verbatim here). Best-effort; never fails the spawn.
        if let Err(e) = self
            .tmux
            .set_environment(tmux_name, "TM_MANAGED_SESSION_ID", session_id)
        {
            debug!(
                session = %tmux_name,
                "tmux set-environment TM_MANAGED_SESSION_ID failed (non-fatal): {e}"
            );
        }
        Ok(())
    }
}

/// Build the shell line sent to the tmux pane to start `tcode` for `task` at `cwd`.
///
/// Why: command construction is the single most regression-prone part of the
/// adapter (quoting, project flag, agent selection), so it is factored into a
/// pure function that can be unit-tested without spawning a process or tmux.
/// An `export TM_MANAGED_SESSION_ID='<session_id>'; ` prefix (#2023 component B)
/// is prepended so a command run in the pane's shell after `tcode` exits can
/// identify the managed session — `tmux set-environment` alone does not reach
/// the pane's already-running login shell, so the export must be part of the
/// literal command line sent via `send_line` (see `claude_code::
/// session_id_export_prefix` for the full rationale, shared verbatim here).
/// What: returns `export TM_MANAGED_SESSION_ID='<id>'; tcode run-task
/// <DEFAULT_AGENT> '<task>' --project '<cwd>'`, single-quoting the session id,
/// task, and project path so spaces and shell metacharacters in any of them are
/// passed literally. Embedded single quotes are escaped using the POSIX `'\''`
/// idiom. The `ANTHROPIC_API_KEY` is deliberately NOT scrubbed — `tcode` uses
/// it for the direct-API path.
/// Test: `tcode_build_spawn_command_*` in the module test block.
fn build_spawn_command(cwd: &Path, task: &str, session_id: &str) -> String {
    format!(
        "export TM_MANAGED_SESSION_ID={}; {TCODE_BINARY} run-task {DEFAULT_AGENT} {} --project {}",
        shell_single_quote(session_id),
        shell_single_quote(task),
        shell_single_quote(&cwd.to_string_lossy()),
    )
}

/// Wrap `value` in single quotes, escaping any embedded single quotes.
///
/// Why: the task description and workspace path are operator/agent-supplied and
/// may contain spaces or shell metacharacters; single-quoting prevents the tmux
/// pane shell from word-splitting or interpreting them.
/// What: returns `'value'` with each embedded `'` replaced by the POSIX-safe
/// sequence `'\''` (close-quote, escaped-quote, re-open-quote).
/// Test: `tcode_shell_quote_escapes_embedded_quote`.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl RuntimeAdapter for TcodeAdapter {
    /// Launch `tcode` in the named tmux session.
    ///
    /// Why: the session manager calls this after creating the tmux pane so the
    /// `tcode` agent process starts inside it, rooted at the workspace.
    /// What: checks that `tcode` is on PATH (returns `BinaryNotFound` if not),
    /// builds the `tcode run-task` command via [`build_spawn_command`], and
    /// sends it to the pane. `ANTHROPIC_API_KEY` is preserved (API-key path).
    /// `session_id` (the managed session UUID, #2023 component B) is exported
    /// into the pane shell so it survives `tcode` exiting. `gh_env` (#3025)
    /// is currently UNUSED by this adapter — `tcode` sessions are out of
    /// scope for the `GH_TOKEN` spawn-env injection (the issue is scoped to
    /// Claude Code's Bash-tool `gh` calls); accepted only to satisfy the
    /// shared [`RuntimeAdapter`] signature.
    /// Test: `tcode_adapter_spawn_sends_run_task`.
    fn spawn(
        &self,
        tmux_name: &str,
        cwd: &Path,
        task: &str,
        session_id: &str,
        gh_env: &[(String, String)],
    ) -> Result<(), RuntimeError> {
        let _ = gh_env;
        if !Self::tcode_available() {
            return Err(RuntimeError::BinaryNotFound(
                "tcode binary not found on PATH — install trusty-code first".into(),
            ));
        }
        self.send_spawn_command(tmux_name, cwd, task, session_id)
    }

    /// Return `"tcode"` as the adapter's identifier.
    ///
    /// Why: logs and status responses must identify this adapter by name so
    /// operators can distinguish it from the `claude-code` runtime.
    /// What: static string, no I/O.
    /// Test: `tcode_adapter_identifies`.
    fn identify(&self) -> &str {
        "tcode"
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::FakeTmux;
    use super::*;

    /// Fixed managed-session UUID string reused across command-builder tests
    /// (#2023 component B) — a representative id, not a real session.
    const TEST_SESSION_ID: &str = "11111111-2222-3333-4444-555555555555";

    #[test]
    fn tcode_adapter_identifies() {
        let fake = FakeTmux::new();
        let adapter = TcodeAdapter::new(fake);
        assert_eq!(adapter.identify(), "tcode");
    }

    #[test]
    fn tcode_build_spawn_command_starts_with_binary() {
        let cmd = build_spawn_command(Path::new("/tmp/ws"), "do a thing", TEST_SESSION_ID);
        assert!(
            cmd.contains("tcode run-task "),
            "command must invoke the tcode binary: {cmd}"
        );
    }

    #[test]
    fn tcode_build_spawn_command_exports_managed_session_id() {
        // #2023 component B: same mechanism as the claude adapter — the export
        // must precede the tcode invocation so the pane shell retains the id
        // after tcode exits.
        let cmd = build_spawn_command(Path::new("/tmp/ws"), "do a thing", TEST_SESSION_ID);
        let expected_prefix = format!("export TM_MANAGED_SESSION_ID='{TEST_SESSION_ID}'; ");
        assert!(
            cmd.starts_with(&expected_prefix),
            "spawn command must start with the session-id export: {cmd}"
        );
        assert!(
            cmd.find("export TM_MANAGED_SESSION_ID").unwrap() < cmd.find("tcode run-task").unwrap(),
            "export must precede the tcode invocation: {cmd}"
        );
    }

    #[test]
    fn tcode_build_spawn_command_uses_default_agent() {
        let cmd = build_spawn_command(Path::new("/tmp/ws"), "do a thing", TEST_SESSION_ID);
        assert!(
            cmd.contains(&format!("run-task {DEFAULT_AGENT} ")),
            "command must target the default agent: {cmd}"
        );
    }

    #[test]
    fn tcode_build_spawn_command_includes_project_and_task() {
        let cmd = build_spawn_command(Path::new("/work/space dir"), "fix bug #12", TEST_SESSION_ID);
        assert!(
            cmd.contains("--project '/work/space dir'"),
            "command must root tcode at the (quoted) workspace cwd: {cmd}"
        );
        assert!(
            cmd.contains("'fix bug #12'"),
            "command must carry the (quoted) task description: {cmd}"
        );
    }

    #[test]
    fn tcode_build_spawn_command_does_not_scrub_api_key() {
        // The API-key path REQUIRES the key in the child env; the tcode adapter
        // must NOT prepend `env -u ANTHROPIC_API_KEY` like the claude adapter does.
        let cmd = build_spawn_command(Path::new("/tmp/ws"), "task", TEST_SESSION_ID);
        assert!(
            !cmd.contains("ANTHROPIC_API_KEY"),
            "tcode adapter must preserve ANTHROPIC_API_KEY (no env scrub): {cmd}"
        );
        assert!(
            !cmd.contains("env -u"),
            "tcode adapter must not scrub the environment: {cmd}"
        );
    }

    #[test]
    fn tcode_shell_quote_escapes_embedded_quote() {
        // A task containing a single quote must remain a single shell token.
        let quoted = shell_single_quote("it's broken");
        assert_eq!(quoted, "'it'\\''s broken'");
    }

    #[test]
    fn tcode_adapter_binary_check_returns_bool() {
        // Asserts the function returns without panicking; `tcode` may or may not
        // be on PATH in CI so we do not assert the result.
        let _ = TcodeAdapter::tcode_available();
    }

    #[test]
    fn tcode_adapter_spawn_sends_run_task() {
        // The send/command-construction path must be exercised in EVERY CI run,
        // regardless of whether a real `tcode` binary is on PATH. We therefore
        // drive `send_spawn_command` (the post-availability-check half of
        // `spawn`) directly: it needs only the `FakeTmux` driver, never a real
        // binary, so the launch-line assertion below runs unconditionally.
        let fake = FakeTmux::new();
        let adapter = TcodeAdapter::new(fake.clone());
        adapter
            .send_spawn_command("tmpm-test", Path::new("/tmp"), "some task", TEST_SESSION_ID)
            .expect("send_spawn_command");
        let sends = fake.sends.lock().expect("send log mutex");
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].0, "tmpm-test");
        assert_eq!(
            sends[0].1,
            build_spawn_command(Path::new("/tmp"), "some task", TEST_SESSION_ID)
        );
        // #2157 item 1: durable publish alongside the pane-shell export.
        let env_sets = fake.env_sets.lock().expect("env-set log mutex");
        assert!(
            env_sets.iter().any(|(name, key, value)| name == "tmpm-test"
                && key == "TM_MANAGED_SESSION_ID"
                && value == TEST_SESSION_ID),
            "send_spawn_command must call tmux set-environment with TM_MANAGED_SESSION_ID: {env_sets:?}"
        );
    }

    #[test]
    fn tcode_adapter_spawn_errors_when_binary_missing() {
        // The availability gate must short-circuit with `BinaryNotFound` and
        // send NOTHING when `tcode` is absent. We can only assert this branch
        // deterministically when the binary is genuinely off PATH; when it is
        // present (e.g. a dev machine with trusty-code installed) we assert the
        // happy path instead so the test is meaningful in both environments.
        let fake = FakeTmux::new();
        let adapter = TcodeAdapter::new(fake.clone());
        let result = adapter.spawn(
            "tmpm-test",
            Path::new("/tmp"),
            "some task",
            TEST_SESSION_ID,
            &[],
        );
        if TcodeAdapter::tcode_available() {
            assert!(result.is_ok(), "spawn should succeed when tcode is on PATH");
            assert_eq!(fake.sends.lock().expect("send log mutex").len(), 1);
        } else {
            assert!(
                matches!(result, Err(RuntimeError::BinaryNotFound(_))),
                "spawn must report BinaryNotFound when tcode is absent: {result:?}"
            );
            assert!(
                fake.sends.lock().expect("send log mutex").is_empty(),
                "no command may be sent when the binary is missing"
            );
        }
    }
}
