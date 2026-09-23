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
//!   * [`configured_shell_assignments`] composes the `env` operands,
//!     space-joined in table order, for the builders that emit an `env …`
//!     prefix string.
//!   * [`apply_default_to_command`] (and its hermetic core,
//!     [`apply_default_when_unset`]) mutates a [`std::process::Command`] for the
//!     builders that exec `claude` directly, applying each table entry
//!     independently.
//!
//! Both carriers YIELD to a value the launch already carries; neither forces
//! one, and each variable is decided independently — an operator who exported
//! only one of the two still gets the tm default for the other.
//!
//! The renderer is the exception (#8405): config `tmux.alternate_screen`
//! decides it through [`configured_env`] — `=0` for `true`, `=1` for `false` —
//! as an explicit assignment on the daemon's launch spec and every shell launch
//! line, so the tmux server's inherited environment cannot override it.
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
//! the daemon's spawn/resume/attach paths by
//! `spec_command_yields_the_alt_screen_default_to_the_pane` (#8233 turned the
//! `${NAME-1}` shell operand into `apply_default_to_command`), and
//! `claude_command_assigns_the_configured_renderer` /
//! `claude_command_defaults_the_mouse_capture_off`,
//! `inplace_session_command_assigns_the_configured_renderer` /
//! `inplace_session_command_defaults_the_mouse_capture_off`,
//! `client_session_command_assigns_the_configured_renderer` /
//! `client_session_command_defaults_the_mouse_capture_off`,
//! `test_build_launch_command_defaults_the_alternate_screen_off` /
//! `test_build_launch_command_defaults_the_mouse_capture_off`,
//! `inplace_exec_command_defaults_the_alternate_screen_off` /
//! `inplace_exec_command_defaults_the_mouse_capture_off`.

/// The Claude Code variable that selects the classic (non-fullscreen) renderer.
pub const ALT_SCREEN_ENV_VAR: &str = "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN";

/// The value tm provisions when the launch carries none.
pub const ALT_SCREEN_DEFAULT: &str = "1";

/// The value that selects Claude Code's fullscreen renderer — what a managed
/// launch carries when config `tmux.alternate_screen` is `true` (#8405).
pub const ALT_SCREEN_ENABLED: &str = "0";

/// The assignments a managed launch must carry for the configured
/// `tmux.alternate_screen` (#8405).
///
/// Why: the `${NAME-1}` default is decided by the pane's environment, and a
/// pane inherits the tmux SERVER's global environment — a snapshot of whatever
/// process first started the server, never the daemon's own env. With config
/// `alternate_screen: true`, tmux lets panes use the alternate screen, yet a
/// server that inherited `=1` (or no value at all, which defaults to `1`) still
/// told `claude` to stay on the classic renderer. The reverse leaks too: with
/// `alternate_screen: false`, a server that inherited `=0` put `claude` on the
/// fullscreen renderer while tmux discarded its alternate screen. Config is the
/// operator's statement of intent, so it decides in both directions.
/// What: `[(ALT_SCREEN_ENV_VAR, "0")]` when `alternate_screen` is `true`,
/// `[(ALT_SCREEN_ENV_VAR, "1")]` when `false`. `false` assigns `1` rather than
/// leaving the variable unset: unset lets an inherited `=0` through, and lets
/// Claude Code pick its own renderer default, which #6495 found to be
/// fullscreen. The caller puts these into an explicit assignment, which
/// [`apply_default_when_unset`] never overrides. A settings-tier `env` entry
/// still outranks it, as the module doc explains.
/// Test: `configured_env_forces_the_fullscreen_renderer_when_enabled`,
/// `configured_env_forces_the_classic_renderer_when_disabled`.
pub fn configured_env(alternate_screen: bool) -> Vec<(String, String)> {
    let value = if alternate_screen {
        ALT_SCREEN_ENABLED
    } else {
        ALT_SCREEN_DEFAULT
    };
    vec![(ALT_SCREEN_ENV_VAR.to_owned(), value.to_owned())]
}

/// The `env` operand text a shell-string launch line carries for the
/// configured `tmux.alternate_screen` (#8405).
///
/// Why: `tm launch`, `tm connect`, `tm session start` and the client
/// `/connect` path type an `env … claude` line into a tmux pane, so they need
/// the same config-decided value the daemon's launch spec carries.
/// What: one operand per [`MANAGED_DEFAULTS`] entry, in table order: a plain
/// `NAME=value` for each variable [`configured_env`] decides, the pinned
/// `NAME="${NAME-default}"` operand for the rest (the mouse default, #7160).
/// The values are `0`/`1`, so no quoting is needed.
/// Test: `configured_shell_assignments_assign_the_renderer_in_both_directions`.
pub fn configured_shell_assignments(alternate_screen: bool) -> String {
    let configured = configured_env(alternate_screen);
    MANAGED_DEFAULTS
        .iter()
        .map(
            |d| match configured.iter().find(|(name, _)| name == d.env_var) {
                Some((name, value)) => format!("{name}={value}"),
                None => d.shell_assignment.to_owned(),
            },
        )
        .collect::<Vec<_>>()
        .join(" ")
}

/// [`configured_alternate_screen_at`] against the operator's own state home
/// (`~/.trusty-tools/trusty-mpm`) — the CLI launch paths' entry point (#8405).
///
/// Why: the `tm` CLI launches run in the operator's home, not under a daemon
/// framework root. An unresolvable home means no config file exists to read,
/// the same answer [`trusty_common::crate_config::load`] gives.
/// What: resolves the directory, then delegates; `Ok(false)`-by-default when the
/// home is unknown.
/// Test: covered via `configured_alternate_screen_reads_the_named_root`.
pub fn configured_alternate_screen() -> Result<bool, trusty_common::crate_config::ConfigError> {
    match trusty_common::crate_config::crate_config_dir(
        crate::core::trusty_tools_config::CRATE_NAME,
    ) {
        Some(root) => configured_alternate_screen_at(&root),
        None => Ok(
            crate::core::trusty_tools_config::resolve_tmux_options(&Default::default())
                .alternate_screen,
        ),
    }
}

/// Read `tmux.alternate_screen` from the `config.yaml` under
/// `crate_config_root` (#8405).
///
/// Why: a managed launch must know the configured value to carry it, and must
/// read it from the state home the caller named (#8233), not `$HOME`. A config
/// that cannot be read or parsed is an error, never a silent fall back to
/// the default: a launch that reports success while dropping the operator's
/// `alternate_screen: true` is the fail-open this issue is about.
/// What: [`trusty_common::crate_config::load_at`] on
/// `<crate_config_root>/config.yaml`, resolved by
/// [`crate::core::trusty_tools_config::resolve_tmux_options`]. An absent file
/// resolves to the built-in default (`false`, #5364).
/// Test: `configured_alternate_screen_reads_the_named_root`,
/// `configured_alternate_screen_defaults_off_without_a_config`,
/// `configured_alternate_screen_errors_on_a_malformed_config`.
pub fn configured_alternate_screen_at(
    crate_config_root: &std::path::Path,
) -> Result<bool, trusty_common::crate_config::ConfigError> {
    use crate::core::trusty_tools_config::{TrustyToolsConfig, resolve_tmux_options};
    let path = crate_config_root.join(trusty_common::crate_config::CONFIG_FILE);
    let config = trusty_common::crate_config::load_at::<TrustyToolsConfig>(&path)?;
    Ok(resolve_tmux_options(&config.unwrap_or_default()).alternate_screen)
}

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
/// Why: the single source of truth [`configured_shell_assignments`] and
/// [`apply_default_when_unset`] both drive off, so a third variable is a
/// one-line addition here rather than a change repeated at every launch site
/// this module has callers in.
/// Test: `configured_shell_assignments_assign_the_renderer_in_both_directions`.
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

/// Provision every managed default (#6495, #7160) on a `claude`
/// [`std::process::Command`].
///
/// Why: the exec launch paths (the bare-`tm` in-place relaunch, `tm run`) build
/// a `Command` with no shell in between, so [`configured_shell_assignments`]
/// and its `${NAME-default}` expansion cannot apply to them — but they inherit the
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
/// An assignment the command already carries explicitly (a [`configured_env`]
/// pair, #8405) also wins: the default fills a gap and never replaces a value
/// the launch chose.
/// What: for each entry in [`MANAGED_DEFAULTS`], no-op when
/// `is_set(entry.env_var)` is true or `cmd` already sets or removes that
/// variable; otherwise `cmd.env(entry.env_var, entry.default)`.
/// Test: `command_defaults_apply_when_every_variable_is_unset`,
/// `command_defaults_yield_to_an_operator_value`,
/// `command_defaults_yield_per_variable_independently`,
/// `command_defaults_never_replace_an_explicit_assignment`.
pub fn apply_default_when_unset(cmd: &mut std::process::Command, is_set: impl Fn(&str) -> bool) {
    for entry in MANAGED_DEFAULTS {
        // #8405: before this guard, an unset pane variable let the default
        // overwrite a configured `=0` the launch had already assigned.
        let explicit = cmd.get_envs().any(|(name, _)| name == entry.env_var);
        if explicit || is_set(entry.env_var) {
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

    /// #8405: a configured `=0` already on the command must survive a pane
    /// that exports nothing. Before the fix the default overwrote it with `1`,
    /// so config `alternate_screen: true` never reached `claude`.
    #[test]
    fn command_defaults_never_replace_an_explicit_assignment() {
        let mut cmd = std::process::Command::new("claude");
        for (name, value) in configured_env(true) {
            cmd.env(name, value);
        }
        apply_default_when_unset(&mut cmd, |_| false);
        assert_eq!(
            override_for(&cmd, ALT_SCREEN_ENV_VAR),
            Some(Some(ALT_SCREEN_ENABLED.to_string())),
            "the default must not overwrite the configured fullscreen renderer"
        );
        assert_eq!(
            override_for(&cmd, MOUSE_ENV_VAR),
            Some(Some(MOUSE_DEFAULT.to_string())),
            "a variable with no explicit assignment still gets the tm default"
        );
    }

    #[test]
    fn configured_env_forces_the_fullscreen_renderer_when_enabled() {
        assert_eq!(
            configured_env(true),
            vec![(ALT_SCREEN_ENV_VAR.to_owned(), "0".to_owned())]
        );
    }

    /// #8405: `false` forces the classic renderer, so a tmux server that
    /// inherited `=0` cannot put `claude` on the fullscreen one.
    #[test]
    fn configured_env_forces_the_classic_renderer_when_disabled() {
        assert_eq!(
            configured_env(false),
            vec![(ALT_SCREEN_ENV_VAR.to_owned(), "1".to_owned())]
        );
    }

    /// #8405: the shell operand is a plain assignment in both directions —
    /// never the `${NAME-1}` form a pane-exported `=0` could override — and
    /// the mouse default keeps its yielding form.
    #[test]
    fn configured_shell_assignments_assign_the_renderer_in_both_directions() {
        assert_eq!(
            configured_shell_assignments(true),
            format!("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=0 {MOUSE_SHELL_ASSIGNMENT}")
        );
        assert_eq!(
            configured_shell_assignments(false),
            format!("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1 {MOUSE_SHELL_ASSIGNMENT}")
        );
    }

    fn write_config(root: &std::path::Path, yaml: &str) {
        std::fs::create_dir_all(root).expect("create root");
        std::fs::write(root.join("config.yaml"), yaml).expect("write config");
    }

    #[test]
    fn configured_alternate_screen_reads_the_named_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_config(dir.path(), "tmux:\n  alternate_screen: true\n");
        assert_eq!(configured_alternate_screen_at(dir.path()).ok(), Some(true));
        write_config(dir.path(), "tmux:\n  alternate_screen: false\n");
        assert_eq!(configured_alternate_screen_at(dir.path()).ok(), Some(false));
    }

    #[test]
    fn configured_alternate_screen_defaults_off_without_a_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(configured_alternate_screen_at(dir.path()).ok(), Some(false));
    }

    /// #8405 fail-open check: a config that cannot be parsed must be an error,
    /// not a silent `false` that drops the operator's `alternate_screen: true`.
    #[test]
    fn configured_alternate_screen_errors_on_a_malformed_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_config(dir.path(), "tmux:\n  alternate_screen: [not, a, bool\n");
        assert!(configured_alternate_screen_at(dir.path()).is_err());
    }
}
