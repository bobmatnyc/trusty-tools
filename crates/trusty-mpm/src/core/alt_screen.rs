//! Start every tm-managed Claude Code session on the classic renderer (issue
//! #6495).
//!
//! Why: Claude Code's fullscreen renderer takes the alternate screen and
//! captures the mouse wheel, so a managed pane scrolls only the one live window
//! — the terminal's native scrollback and tmux's copy-mode history both stop
//! responding, which operators report as "everything is one window that will not
//! scroll". Claude Code names the escape hatch in its own failure text:
//! `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 forces that any time`. Setting it by
//! hand in `~/.claude/settings.json` fixes the session; tm now provisions it so
//! nobody has to find it.
//!
//! What: one variable name, one default value, and the two carriers the launch
//! paths need — a shell `NAME=VALUE` operand for the builders that emit an
//! `env …` prefix string, and a [`std::process::Command`] mutation for the
//! builders that exec `claude` directly. Both carriers YIELD to a value the
//! launch already carries; neither forces one.
//!
//! Operator precedence, and where it comes from:
//!   * launch environment — the shell operand expands `${NAME-1}`, so the pane
//!     shell substitutes the default only when the variable is UNSET. A value
//!     the pane already exports reaches `claude` unchanged, including an empty
//!     one (`-`, not `:-`). [`apply_default_when_unset`] makes the same decision
//!     in Rust for the exec paths, reading the environment the child would
//!     otherwise inherit.
//!   * settings `env` block — Claude Code applies an allowlisted settings `env`
//!     entry ON TOP of the process environment, and this variable is on that
//!     allowlist, so an operator entry in any settings tier already outranks
//!     whatever tm exports. tm writes this variable into no settings file, so
//!     there is nothing here for an operator entry to fight.
//!
//! Switching renderers from inside a session still works: Claude Code's own
//! switch re-execs with this variable dropped.
//!
//! Test: this module's `tests`, plus one per launch path —
//! `spawn_command_defaults_the_alternate_screen_off`,
//! `resume_command_defaults_the_alternate_screen_off`,
//! `claude_command_defaults_the_alternate_screen_off`,
//! `inplace_session_command_defaults_the_alternate_screen_off`,
//! `client_session_command_defaults_the_alternate_screen_off`,
//! `test_build_launch_command_defaults_the_alternate_screen_off`,
//! `inplace_exec_command_defaults_the_alternate_screen_off`.

/// The Claude Code variable that selects the classic (non-fullscreen) renderer.
pub const ALT_SCREEN_ENV_VAR: &str = "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN";

/// The value tm provisions when the launch carries none.
pub const ALT_SCREEN_DEFAULT: &str = "1";

/// The `env` operand every shell-string launch line carries (#6495).
///
/// Why: written as a literal rather than composed from
/// [`ALT_SCREEN_ENV_VAR`]/[`ALT_SCREEN_DEFAULT`] so a test can pin the exact
/// shell text. A composed constant compared against a composed expectation
/// would stay green if the `${NAME-default}` form itself were broken — and the
/// form IS the operator-precedence mechanism, not decoration around it.
/// What: `NAME="${NAME-1}"`, which the pane shell expands to the operator's
/// exported value when the variable is set and to `1` when it is not. The
/// `-` form (rather than `:-`) preserves an explicitly-empty operator value.
/// Double quotes are required: the single quotes the sibling assignments use
/// would suppress the expansion and hand `claude` the literal text.
///
/// POSIX `env` grammar puts this AFTER every `-u` flag on the line — see
/// [`crate::core::claude_env_scrub::env_unset_flags`].
/// Test: `shell_assignment_pins_the_defaulting_form`,
/// `shell_assignment_names_the_variable_and_the_default`.
pub const ALT_SCREEN_SHELL_ASSIGNMENT: &str =
    "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=\"${CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN-1}\"";

/// Provision the classic-renderer default on a `claude` [`std::process::Command`].
///
/// Why: the exec launch paths (the bare-`tm` in-place relaunch, `tm run`) build
/// a `Command` with no shell in between, so [`ALT_SCREEN_SHELL_ASSIGNMENT`] and
/// its `${NAME-1}` expansion cannot apply to them — but they inherit the same
/// pane environment and need the same default and the same operator precedence.
/// What: delegates to [`apply_default_when_unset`] with a real-environment
/// lookup. The parent's environment is what the child inherits, so "already set
/// here" is exactly "the launch already carries a value".
/// Test: covered via [`apply_default_when_unset`], which takes the lookup as a
/// parameter — mirroring
/// [`crate::core::claude_env_scrub::markers_present_in_env`], and for the same
/// reason: `std::env::set_var` is `unsafe` in edition 2024 and mutates state
/// every test in the binary shares.
pub fn apply_default_to_command(cmd: &mut std::process::Command) {
    apply_default_when_unset(cmd, |name| std::env::var_os(name).is_some());
}

/// Pure core of [`apply_default_to_command`]: set the default only when `is_set`
/// reports the variable absent.
///
/// Why: [`std::process::Command::env`] overrides the inherited value
/// unconditionally, so an unguarded call would defeat an operator who exported
/// the variable in the pane — the exact override #6495 must not perform.
/// What: no-op when `is_set(ALT_SCREEN_ENV_VAR)` is true; otherwise
/// `cmd.env(ALT_SCREEN_ENV_VAR, ALT_SCREEN_DEFAULT)`.
/// Test: `command_default_applies_when_the_variable_is_unset`,
/// `command_default_yields_to_an_operator_value`.
pub fn apply_default_when_unset(cmd: &mut std::process::Command, is_set: impl Fn(&str) -> bool) {
    if is_set(ALT_SCREEN_ENV_VAR) {
        return;
    }
    cmd.env(ALT_SCREEN_ENV_VAR, ALT_SCREEN_DEFAULT);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The literal shell text, pinned. `:-` here would silently rewrite an
    /// operator's explicitly-empty value, and single quotes would hand `claude`
    /// the unexpanded text.
    #[test]
    fn shell_assignment_pins_the_defaulting_form() {
        assert_eq!(
            ALT_SCREEN_SHELL_ASSIGNMENT,
            "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=\"${CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN-1}\""
        );
        assert!(
            !ALT_SCREEN_SHELL_ASSIGNMENT.contains(":-"),
            "`:-` would override an operator value of empty string"
        );
    }

    /// The literal above and the two constants must describe the same variable,
    /// or a caller reading the constants would look at the wrong name.
    #[test]
    fn shell_assignment_names_the_variable_and_the_default() {
        assert_eq!(
            ALT_SCREEN_SHELL_ASSIGNMENT,
            format!("{ALT_SCREEN_ENV_VAR}=\"${{{ALT_SCREEN_ENV_VAR}-{ALT_SCREEN_DEFAULT}}}\"")
        );
    }

    /// Read the explicit overrides a `Command` carries: `get_envs` reports a set
    /// value as `(key, Some(value))` and an `env_remove` as `(key, None)`.
    fn override_for(cmd: &std::process::Command, name: &str) -> Option<Option<String>> {
        cmd.get_envs()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    }

    #[test]
    fn command_default_applies_when_the_variable_is_unset() {
        let mut cmd = std::process::Command::new("claude");
        apply_default_when_unset(&mut cmd, |_| false);
        assert_eq!(
            override_for(&cmd, ALT_SCREEN_ENV_VAR),
            Some(Some(ALT_SCREEN_DEFAULT.to_string())),
            "an unset variable must be provisioned with the tm default"
        );
    }

    #[test]
    fn command_default_yields_to_an_operator_value() {
        let mut cmd = std::process::Command::new("claude");
        apply_default_when_unset(&mut cmd, |name| name == ALT_SCREEN_ENV_VAR);
        assert_eq!(
            override_for(&cmd, ALT_SCREEN_ENV_VAR),
            None,
            "a value the launch already carries must reach claude untouched"
        );
    }
}
