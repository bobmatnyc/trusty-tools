//! Shell dialect argument for `tm shell-init`.
//!
//! Why: `tm shell-init <shell>` emits a wrapper function whose syntax differs
//! per shell family, so the dialect has to be a closed, validated set — a free
//! `String` would let a typo reach the emitter and print a snippet for the
//! wrong shell.
//! What: a three-variant clap `ValueEnum`.
//! Test: `cli_parses_shell_init` in `tests.rs`.

/// Shell dialect accepted by `tm shell-init`.
///
/// Why: clap needs a `ValueEnum` to validate and to render the choices in
/// `--help`.
/// What: `Zsh` | `Bash` | `Fish`. `Zsh` and `Bash` share one POSIX-compatible
/// function body; `Fish` needs genuinely different syntax.
/// Test: `cli_parses_shell_init`, and the per-dialect golden tests in
/// `commands/shell_init_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum ShellArg {
    /// Z shell (`~/.zshrc`).
    Zsh,
    /// Bourne-again shell (`~/.bashrc`).
    Bash,
    /// fish (`~/.config/fish/config.fish`).
    Fish,
}
