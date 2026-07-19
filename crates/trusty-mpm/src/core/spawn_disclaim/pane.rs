//! Disclaim the pane-launched `claude` off the shared tmux server (issue #2997).
//!
//! Why: unlike the three spawn shapes in [`super`] (capture/status/piped),
//! trusty-mpm's DEFAULT managed session does not `posix_spawn` `claude`
//! directly — it types a shell command into a tmux pane via `send-keys`, and
//! the pane's shell (forked by the ONE long-lived, shared tmux server) execs
//! `claude`. So the disclaim attribute the parent would set at `posix_spawn`
//! never applies to that `claude`: tccd walks its responsibility chain
//! `claude → zsh → tmux server` and blames the shared server. #3037 already
//! disclaims the `tmux new-session` CLIENT ([`crate::core::tmux::run_tmux_with_bin`]),
//! which moved attribution off the signed `trusty-mpm` binary and ONTO the
//! tmux server — but a `RemovableVolumes`/App-Data prompt still surfaces,
//! now subject-lined "tmux", because the server is still SOME process's
//! responsible ancestor for every pane (issue #2997, 2026-07-19 evidence).
//! What: [`disclaim_pane_command`] rewrites the pane's `claude` invocation to
//! route it through the running `tm`/`trusty-mpm` binary's hidden
//! [`PANE_DISCLAIM_SUBCOMMAND`], which `posix_spawn`s `claude` WITH the
//! disclaim attribute set (reusing [`super::disclaimed_status`], the #3037
//! inherited-stdio shape). That makes `claude` its OWN responsible process, so
//! tccd stops the walk at `claude` (a stably-signed `com.anthropic.claude-code`
//! identity) instead of the tmux server. On non-macOS, or when
//! [`super::DISABLE_ENV`] is set, the invocation is returned unchanged.
//! Test: `wraps_invocation_when_wrapper_present`,
//! `passes_through_when_no_wrapper`, `single_quotes_wrapper_with_space`,
//! `preserves_flags_after_program` (all cross-platform, pure).

/// The hidden `tm`/`trusty-mpm` subcommand a managed pane invokes to launch
/// its `claude` disclaimed (issue #2997).
///
/// Why: the pane cannot set a `posix_spawn` attribute itself — it can only run
/// a shell command. Routing `claude` through this subcommand puts a tm-owned
/// process between the pane shell and `claude` that DOES set the disclaim
/// attribute. Both `[[bin]]` targets (`tm` and `trusty-mpm`) build from the
/// same `main.rs`, so whichever binary built the pane command can service it.
/// What: the literal subcommand name; the argv that follows is `<program>
/// <args...>`, spawned via [`super::disclaimed_status`].
/// Test: `wraps_invocation_when_wrapper_present`.
pub const PANE_DISCLAIM_SUBCOMMAND: &str = "internal-spawn-disclaimed";

/// POSIX single-quote `s` so a path with a space survives the pane shell's
/// word-splitting intact.
///
/// Why: `current_exe()` under a home like `/Users/John Doe/.cargo/bin/tm`
/// would otherwise word-split and break the pane launch — the same failure
/// [`crate::runtime::claude_code`]'s `shell_single_quote` guards against.
/// What: wraps `s` in single quotes, escaping any embedded `'` with the
/// canonical `'\''` close-reopen sequence.
/// Test: `single_quotes_wrapper_with_space`.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Build the pane command that launches `claude_invocation` disclaimed, given
/// the wrapper binary to route through (pure — the seam that makes the
/// wrapping unit-testable without a real `current_exe`/OS).
///
/// Why: separating the string construction from the [`current_wrapper_bin`]
/// resolution lets a hermetic test assert both the wrapped shape (with a fake
/// wrapper) and the pass-through (with `None`) deterministically on any
/// platform.
/// What: `Some(bin)` → `<'bin'> <PANE_DISCLAIM_SUBCOMMAND> <claude_invocation>`
/// (the wrapper path single-quoted, the invocation appended verbatim so its
/// existing flags follow the program token as separate shell words). `None`
/// → `claude_invocation` unchanged.
/// Test: `wraps_invocation_when_wrapper_present`,
/// `passes_through_when_no_wrapper`, `preserves_flags_after_program`.
fn disclaim_pane_command_with(wrapper_bin: Option<&str>, claude_invocation: &str) -> String {
    match wrapper_bin {
        Some(bin) => format!(
            "{} {PANE_DISCLAIM_SUBCOMMAND} {claude_invocation}",
            shell_single_quote(bin)
        ),
        None => claude_invocation.to_string(),
    }
}

/// Resolve the wrapper binary (the running `tm`/`trusty-mpm` executable) when
/// pane disclaim is active, else `None`.
///
/// Why: the wrapper must be a binary that services [`PANE_DISCLAIM_SUBCOMMAND`]
/// — that is exactly the process building the pane command, so `current_exe()`
/// is always the right absolute path (and absolute matters: the pane inherits
/// the daemon's minimal launchd `PATH`, so a bare name could fail to resolve —
/// the #1298 rationale for the resolved `claude_bin`).
/// What: `Some(path)` on macOS when [`super::DISABLE_ENV`] is unset and
/// `current_exe()` resolves to valid UTF-8; `None` on every other OS, when the
/// escape hatch is set, or when the path cannot be read/encoded (in which case
/// the caller falls back to launching `claude` directly — no regression).
/// Test: exercised end-to-end by the live managed-session path; the pure
/// wrapping logic is covered via [`disclaim_pane_command_with`].
fn current_wrapper_bin() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if std::env::var_os(super::DISABLE_ENV).is_none()
            && let Ok(exe) = std::env::current_exe()
            && let Some(s) = exe.to_str()
        {
            return Some(s.to_string());
        }
    }
    None
}

/// Rewrite a pane-launched `claude` invocation so the pane's `claude` child is
/// spawned disclaimed (its own TCC responsible process) instead of inheriting
/// the shared tmux server's identity (issue #2997).
///
/// Why: see the module docs — every managed session types this command into a
/// tmux pane, and without the wrapper tccd attributes the pane's `claude`
/// `RemovableVolumes`/App-Data access up to the shared tmux server.
/// What: on macOS (and with [`super::DISABLE_ENV`] unset) prefixes
/// `claude_invocation` with `<current_exe> <PANE_DISCLAIM_SUBCOMMAND>`; on any
/// other platform, or with the escape hatch set, returns it unchanged.
/// `claude_invocation` may be a bare program token (the daemon's resolved
/// `claude_bin`, with its flags appended downstream) or a full `claude …`
/// command string (the CLI paths) — prefixing works identically for both.
/// Test: the pure shape is covered by [`disclaim_pane_command_with`]'s tests;
/// the resolution is exercised by the live managed-session launch path.
pub fn disclaim_pane_command(claude_invocation: &str) -> String {
    disclaim_pane_command_with(current_wrapper_bin().as_deref(), claude_invocation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_invocation_when_wrapper_present() {
        let out = disclaim_pane_command_with(Some("/Users/masa/.cargo/bin/tm"), "claude --flag");
        assert_eq!(
            out,
            "'/Users/masa/.cargo/bin/tm' internal-spawn-disclaimed claude --flag"
        );
    }

    #[test]
    fn passes_through_when_no_wrapper() {
        // No wrapper (non-macOS, or the escape hatch) → the invocation is
        // launched directly, byte-identical to the pre-#2997 pane command.
        let out = disclaim_pane_command_with(None, "claude --flag");
        assert_eq!(out, "claude --flag");
    }

    #[test]
    fn single_quotes_wrapper_with_space() {
        // A home with a space must not word-split the wrapper token.
        let out = disclaim_pane_command_with(Some("/Users/John Doe/.cargo/bin/tm"), "claude");
        assert_eq!(
            out,
            "'/Users/John Doe/.cargo/bin/tm' internal-spawn-disclaimed claude"
        );
    }

    #[test]
    fn preserves_flags_after_program() {
        // The daemon passes a resolved absolute claude_bin; its downstream
        // flags must remain after the program token so the wrapper forwards
        // them to claude unchanged.
        let out = disclaim_pane_command_with(
            Some("/w/tm"),
            "/abs/claude --setting-sources project,local --dangerously-skip-permissions",
        );
        assert_eq!(
            out,
            "'/w/tm' internal-spawn-disclaimed /abs/claude \
             --setting-sources project,local --dangerously-skip-permissions"
        );
    }

    #[test]
    fn subcommand_name_is_stable() {
        // The wrapper name is a wire contract between the pane command and the
        // CLI dispatcher — pin it so a rename can't silently break the pane.
        assert_eq!(PANE_DISCLAIM_SUBCOMMAND, "internal-spawn-disclaimed");
    }
}
