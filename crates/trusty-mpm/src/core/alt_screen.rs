//! Start every tm-managed Claude Code session on the classic renderer, with
//! its mouse capture off (issues #6495, #7160).
//!
//! Why: Claude Code's fullscreen renderer takes the alternate screen and
//! captures the mouse wheel, so a managed pane scrolls only the one live window
//! — the terminal's native scrollback and tmux's copy-mode history both stop
//! responding, which operators report as "everything is one window that will not
//! scroll". Claude Code names the escape hatch in its own failure text:
//! `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 forces that any time`. #7160 found
//! the fix incomplete: even under the classic renderer, Claude Code still sets
//! tmux's per-pane `mouse_any_flag` via its own escape sequence, so the wheel
//! still reaches the app instead of tmux copy-mode — `CLAUDE_CODE_DISABLE_MOUSE`
//! is the documented opt-out for that half. Setting either by hand in
//! `~/.claude/settings.json` fixes a session; tm now provisions both so nobody
//! has to find them.
//!
//! What: [`MANAGED_DEFAULTS`] is the one table — variable name, default value,
//! and pinned shell operand — that drives every carrier below, so a third
//! variable is a one-line addition to the table rather than a change repeated
//! at each launch site:
//!   * [`managed_shell_assignments`] composes the `env NAME="${NAME-default}"`
//!     operands, space-joined in table order, for the builders that emit an
//!     `env …` prefix string.
//!   * [`apply_default_to_command`] (and its hermetic core,
//!     [`apply_default_when_unset`]) mutates a [`std::process::Command`] for the
//!     builders that exec `claude` directly, applying each table entry
//!     independently.
//!
//! Both carriers YIELD to a value the launch already carries; neither forces
//! one, and each variable is decided independently — an operator who exported
//! only one of the two still gets the tm default for the other.
//!
//! Operator precedence, and where it comes from:
//!   * launch environment — the shell operand expands `${NAME-default}`, so the
//!     pane shell substitutes the default only when the variable is UNSET. A
//!     value the pane already exports reaches `claude` unchanged, including an
//!     empty one (`-`, not `:-`). [`apply_default_when_unset`] makes the same
//!     decision in Rust for the exec paths, reading the environment the child
//!     would otherwise inherit.
//!   * settings `env` block — Claude Code applies an allowlisted settings `env`
//!     entry ON TOP of the process environment, and both variables are on that
//!     allowlist, so an operator entry in any settings tier already outranks
//!     whatever tm exports. tm writes neither variable into any settings file,
//!     so there is nothing here for an operator entry to fight.
//!
//! Switching renderers from inside a session still works: Claude Code's own
//! switch re-execs with these variables dropped.
//!
//! Test: this module's `tests`, plus one per launch path for each variable —
//! `spawn_command_defaults_the_alternate_screen_off` /
//! `spawn_command_defaults_the_mouse_capture_off`,
//! `resume_command_defaults_the_alternate_screen_off` /
//! `resume_command_defaults_the_mouse_capture_off`,
//! `claude_command_defaults_the_alternate_screen_off` /
//! `claude_command_defaults_the_mouse_capture_off`,
//! `inplace_session_command_defaults_the_alternate_screen_off` /
//! `inplace_session_command_defaults_the_mouse_capture_off`,
//! `client_session_command_defaults_the_alternate_screen_off` /
//! `client_session_command_defaults_the_mouse_capture_off`,
//! `test_build_launch_command_defaults_the_alternate_screen_off` /
//! `test_build_launch_command_defaults_the_mouse_capture_off`,
//! `inplace_exec_command_defaults_the_alternate_screen_off` /
//! `inplace_exec_command_defaults_the_mouse_capture_off`.

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

/// The Claude Code variable that opts the pane out of the app's own mouse
/// capture (#7160).
///
/// Why: tmux's per-pane `mouse_any_flag` is set by the application running in
/// the pane, via an escape sequence — independent of tmux's own `mouse`
/// server option and independent of [`ALT_SCREEN_ENV_VAR`]. Claude Code sets
/// it even under the classic renderer, so #6495's fix alone leaves the wheel
/// captured. Claude Code documents this variable as the opt-out.
pub const MOUSE_ENV_VAR: &str = "CLAUDE_CODE_DISABLE_MOUSE";

/// The value tm provisions when the launch carries none.
pub const MOUSE_DEFAULT: &str = "1";

/// The `env` operand every shell-string launch line carries (#7160) — the
/// mouse-capture counterpart to [`ALT_SCREEN_SHELL_ASSIGNMENT`], same form and
/// same reason for being a pinned literal rather than a composed one.
/// Test: `mouse_shell_assignment_pins_the_defaulting_form`,
/// `mouse_shell_assignment_names_the_variable_and_the_default`.
pub const MOUSE_SHELL_ASSIGNMENT: &str =
    "CLAUDE_CODE_DISABLE_MOUSE=\"${CLAUDE_CODE_DISABLE_MOUSE-1}\"";

/// One variable tm provisions a default for on every managed launch.
///
/// Why: pairs each variable's name, default, and pinned shell operand so
/// [`MANAGED_DEFAULTS`] can drive every carrier from one table instead of the
/// table's shape being re-derived at each carrier (#7160).
/// What: three fields, all `&'static str` so the table itself can be a `const`.
struct ManagedDefault {
    /// The environment variable name.
    env_var: &'static str,
    /// The value tm provisions when the launch carries none.
    default: &'static str,
    /// The pinned `NAME="${NAME-default}"` shell operand — see
    /// [`ALT_SCREEN_SHELL_ASSIGNMENT`]'s doc for why this is a literal rather
    /// than composed from the other two fields.
    shell_assignment: &'static str,
}

/// Every variable tm provisions a default for on a managed launch, in the
/// order every carrier emits them (#6495, #7160).
///
/// Why: the single source of truth [`managed_shell_assignments`] and
/// [`apply_default_when_unset`] both drive off, so a third variable is a
/// one-line addition here rather than a change repeated at every launch site
/// this module has callers in.
/// Test: `managed_shell_assignments_joins_both_operands_in_order`.
const MANAGED_DEFAULTS: &[ManagedDefault] = &[
    ManagedDefault {
        env_var: ALT_SCREEN_ENV_VAR,
        default: ALT_SCREEN_DEFAULT,
        shell_assignment: ALT_SCREEN_SHELL_ASSIGNMENT,
    },
    ManagedDefault {
        env_var: MOUSE_ENV_VAR,
        default: MOUSE_DEFAULT,
        shell_assignment: MOUSE_SHELL_ASSIGNMENT,
    },
];

/// The `env` operand text carrying every managed default, space-joined in
/// [`MANAGED_DEFAULTS`] order — what a shell-command launch line splices in
/// ahead of `claude` (#6495 alternate-screen, #7160 mouse capture).
///
/// Why: every shell-string builder previously spliced in
/// [`ALT_SCREEN_SHELL_ASSIGNMENT`] alone; adding the mouse default without a
/// second call at every one of those sites would silently miss one the next
/// time a variable is added. One function, driven by the table, is the single
/// place a launch line's defaulted-env text is assembled.
/// What: each entry's `shell_assignment`, joined with a single space — the
/// same separator every builder already places between the scrub flags and
/// the assignment, and between one assignment and the next (see
/// [`crate::core::model_inject::build_claude_command_with`]'s per-assignment
/// `cmd.push(' ')`).
/// Test: `managed_shell_assignments_joins_both_operands_in_order`.
pub fn managed_shell_assignments() -> String {
    MANAGED_DEFAULTS
        .iter()
        .map(|d| d.shell_assignment)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Provision every managed default (#6495, #7160) on a `claude`
/// [`std::process::Command`].
///
/// Why: the exec launch paths (the bare-`tm` in-place relaunch, `tm run`) build
/// a `Command` with no shell in between, so [`managed_shell_assignments`] and
/// its `${NAME-default}` expansion cannot apply to them — but they inherit the
/// same pane environment and need the same defaults and the same operator
/// precedence, decided independently per variable.
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

/// Pure core of [`apply_default_to_command`]: for every table entry, set its
/// default only when `is_set` reports that variable absent.
///
/// Why: [`std::process::Command::env`] overrides the inherited value
/// unconditionally, so an unguarded call would defeat an operator who exported
/// the variable in the pane — the exact override #6495/#7160 must not perform.
/// Each variable is decided independently: an operator who exported only one
/// of the two must still get the tm default for the other, never both-or-
/// neither.
/// What: for each entry in [`MANAGED_DEFAULTS`], no-op when
/// `is_set(entry.env_var)` is true; otherwise
/// `cmd.env(entry.env_var, entry.default)`.
/// Test: `command_defaults_apply_when_every_variable_is_unset`,
/// `command_defaults_yield_to_an_operator_value`,
/// `command_defaults_yield_per_variable_independently`.
pub fn apply_default_when_unset(cmd: &mut std::process::Command, is_set: impl Fn(&str) -> bool) {
    for entry in MANAGED_DEFAULTS {
        if is_set(entry.env_var) {
            continue;
        }
        cmd.env(entry.env_var, entry.default);
    }
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

    /// #7160: the mouse-capture counterpart of `shell_assignment_pins_the_defaulting_form`.
    #[test]
    fn mouse_shell_assignment_pins_the_defaulting_form() {
        assert_eq!(
            MOUSE_SHELL_ASSIGNMENT,
            "CLAUDE_CODE_DISABLE_MOUSE=\"${CLAUDE_CODE_DISABLE_MOUSE-1}\""
        );
        assert!(
            !MOUSE_SHELL_ASSIGNMENT.contains(":-"),
            "`:-` would override an operator value of empty string"
        );
    }

    /// #7160: the mouse-capture counterpart of
    /// `shell_assignment_names_the_variable_and_the_default`.
    #[test]
    fn mouse_shell_assignment_names_the_variable_and_the_default() {
        assert_eq!(
            MOUSE_SHELL_ASSIGNMENT,
            format!("{MOUSE_ENV_VAR}=\"${{{MOUSE_ENV_VAR}-{MOUSE_DEFAULT}}}\"")
        );
    }

    /// #7160: one function must carry both operands, in table order, so a
    /// caller that splices its result in ahead of `claude` gets both defaults
    /// from a single call site.
    #[test]
    fn managed_shell_assignments_joins_both_operands_in_order() {
        assert_eq!(
            managed_shell_assignments(),
            format!("{ALT_SCREEN_SHELL_ASSIGNMENT} {MOUSE_SHELL_ASSIGNMENT}")
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
    fn command_defaults_apply_when_every_variable_is_unset() {
        let mut cmd = std::process::Command::new("claude");
        apply_default_when_unset(&mut cmd, |_| false);
        assert_eq!(
            override_for(&cmd, ALT_SCREEN_ENV_VAR),
            Some(Some(ALT_SCREEN_DEFAULT.to_string())),
            "an unset alternate-screen variable must be provisioned with the tm default"
        );
        assert_eq!(
            override_for(&cmd, MOUSE_ENV_VAR),
            Some(Some(MOUSE_DEFAULT.to_string())),
            "an unset mouse-capture variable must be provisioned with the tm default"
        );
    }

    #[test]
    fn command_defaults_yield_to_an_operator_value() {
        let mut cmd = std::process::Command::new("claude");
        apply_default_when_unset(&mut cmd, |_| true);
        assert_eq!(
            override_for(&cmd, ALT_SCREEN_ENV_VAR),
            None,
            "a value the launch already carries must reach claude untouched"
        );
        assert_eq!(
            override_for(&cmd, MOUSE_ENV_VAR),
            None,
            "a value the launch already carries must reach claude untouched"
        );
    }

    /// #7160: the two variables must be decided INDEPENDENTLY — an operator who
    /// exported only the alternate-screen variable must still get the tm
    /// default for mouse capture, never both-or-neither.
    #[test]
    fn command_defaults_yield_per_variable_independently() {
        let mut cmd = std::process::Command::new("claude");
        apply_default_when_unset(&mut cmd, |name| name == ALT_SCREEN_ENV_VAR);
        assert_eq!(
            override_for(&cmd, ALT_SCREEN_ENV_VAR),
            None,
            "the variable the launch already carries must reach claude untouched"
        );
        assert_eq!(
            override_for(&cmd, MOUSE_ENV_VAR),
            Some(Some(MOUSE_DEFAULT.to_string())),
            "the variable the launch does NOT carry must still be provisioned"
        );
    }
}
