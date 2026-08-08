//! `tm shell-init <zsh|bash|fish>` — print the shell wrapper function.
//!
//! Why: `tm run <alias>` launches a session in the alias's repo directory, but
//! when it exits the user's shell is still wherever it started, so a later bare
//! `tm` re-detects the wrong project. A process cannot change its parent
//! shell's cwd; only a function sourced into the shell can. So `tm` prints one
//! and the user installs it themselves.
//!
//! What: emits a `tm` wrapper that runs `command tm "$@"`, and — only for
//! `tm run <alias>` — resolves the alias through the existing `tm path`
//! command and `cd`s there. The repo path is derived from `(alias, root)` and
//! never persisted, so it resolves identically after the session exits. There
//! is no state file, temp-file signal, or env-var channel.
//!
//! PRINT-ONLY. Nothing here (and nothing in any install path) writes to
//! `.zshrc`, `.bashrc`, or a fish config. The user pastes the `eval` line into
//! their own rc file. Do not add an installer hook.
//!
//! Failure is silent by construction: if `tm path` fails, prints nothing, or
//! the directory is gone, the wrapper does nothing and returns the real exit
//! status. A sourced profile that errors noisily is worse than one that does
//! nothing.
//!
//! Test: `shell_init_tests.rs` — golden text per dialect plus a behavioral
//! suite that runs the emitted snippet under a real `bash`.

use crate::cli::ShellArg;

/// POSIX-compatible wrapper function body, shared by zsh and bash.
///
/// Why: the two dialects need no divergence here — `local`, `case`, and
/// `${var#prefix}` behave identically in both — and one body means a fix lands
/// once. Only the installation comment above it differs.
/// What: wraps `tm`, passes the real exit status through unchanged, and `cd`s
/// only when `$1` is `run` AND an alias was found AND `tm path` resolved to an
/// existing directory. `--root`/`--task` values are skipped when scanning for
/// the alias so `tm run --task 'ship it' web` does not mistake the task text
/// for the alias, and a captured `--root` is forwarded to `tm path` so a
/// non-default root resolves against the same root the session used.
/// Test: `zsh_and_bash_share_one_body`,
/// `wrapper_cds_after_tm_run_and_passes_the_exit_status_through`,
/// `wrapper_does_not_cd_when_path_resolution_fails`.
const POSIX_BODY: &str = r#"tm() {
    command tm "$@"
    local _tm_status=$?
    if [ "${1:-}" != "run" ]; then
        return $_tm_status
    fi
    shift
    local _tm_alias='' _tm_root='' _tm_pending='' _tm_arg='' _tm_target=''
    for _tm_arg in "$@"; do
        if [ -n "$_tm_pending" ]; then
            if [ "$_tm_pending" = "--root" ]; then
                _tm_root="$_tm_arg"
            fi
            _tm_pending=''
            continue
        fi
        case "$_tm_arg" in
            --root=*) _tm_root="${_tm_arg#--root=}" ;;
            --root|--task) _tm_pending="$_tm_arg" ;;
            -*) ;;
            *) if [ -z "$_tm_alias" ]; then _tm_alias="$_tm_arg"; fi ;;
        esac
    done
    if [ -z "$_tm_alias" ]; then
        return $_tm_status
    fi
    if [ -n "$_tm_root" ]; then
        _tm_target=$(command tm path "$_tm_alias" --root "$_tm_root" 2>/dev/null) || _tm_target=''
    else
        _tm_target=$(command tm path "$_tm_alias" 2>/dev/null) || _tm_target=''
    fi
    if [ -n "$_tm_target" ] && [ -d "$_tm_target" ]; then
        cd "$_tm_target" || return $_tm_status
    fi
    return $_tm_status
}
"#;

/// Installation header for zsh.
const ZSH_HEADER: &str = r#"# trusty-mpm shell integration (zsh).
# Add to ~/.zshrc:  eval "$(tm shell-init zsh)"
# After `tm run <alias>` exits, your shell is left in that alias's repo.
"#;

/// Installation header for bash.
const BASH_HEADER: &str = r#"# trusty-mpm shell integration (bash).
# Add to ~/.bashrc:  eval "$(tm shell-init bash)"
# After `tm run <alias>` exits, your shell is left in that alias's repo.
"#;

/// fish wrapper — real fish, not bash with the keywords swapped.
///
/// Why: fish has no `local`, no `$?`, no POSIX `case`; it uses `set -l`,
/// `$status`, and `switch`/`case` with glob patterns, and its `function`
/// blocks close with `end`.
/// What: the same contract as [`POSIX_BODY`] — exit status passed through,
/// `cd` only for a resolvable `tm run <alias>`, silent otherwise.
/// Test: `fish_snippet_matches_golden`, plus a real `fish --no-execute` parse.
const FISH_SNIPPET: &str = r#"# trusty-mpm shell integration (fish).
# Add to ~/.config/fish/config.fish:  tm shell-init fish | source
# After `tm run <alias>` exits, your shell is left in that alias's repo.
function tm
    command tm $argv
    set -l _tm_status $status
    if test (count $argv) -eq 0; or test "$argv[1]" != "run"
        return $_tm_status
    end
    set -l _tm_rest $argv
    set -e _tm_rest[1]
    set -l _tm_alias ""
    set -l _tm_root ""
    set -l _tm_pending ""
    for _tm_arg in $_tm_rest
        if test -n "$_tm_pending"
            if test "$_tm_pending" = "--root"
                set _tm_root $_tm_arg
            end
            set _tm_pending ""
            continue
        end
        switch $_tm_arg
            case '--root=*'
                set _tm_root (string replace -r -- '^--root=' '' $_tm_arg)
            case '--root' '--task'
                set _tm_pending $_tm_arg
            case '-*'
            case '*'
                if test -z "$_tm_alias"
                    set _tm_alias $_tm_arg
                end
        end
    end
    if test -z "$_tm_alias"
        return $_tm_status
    end
    set -l _tm_target ""
    if test -n "$_tm_root"
        set _tm_target (command tm path $_tm_alias --root $_tm_root 2>/dev/null)
    else
        set _tm_target (command tm path $_tm_alias 2>/dev/null)
    end
    if test -n "$_tm_target"; and test -d "$_tm_target"
        cd "$_tm_target"
    end
    return $_tm_status
end
"#;

/// Render the wrapper snippet for one shell dialect.
///
/// Why: separated from printing so the tests can assert the exact emitted text
/// and hand it to a real shell for a syntax check.
/// What: header comment plus the function body, newline-terminated.
/// Test: `zsh_snippet_matches_golden`, `bash_snippet_matches_golden`,
/// `fish_snippet_matches_golden`.
pub(crate) fn shell_init_snippet(shell: ShellArg) -> String {
    match shell {
        ShellArg::Zsh => format!("{ZSH_HEADER}{POSIX_BODY}"),
        ShellArg::Bash => format!("{BASH_HEADER}{POSIX_BODY}"),
        ShellArg::Fish => FISH_SNIPPET.to_string(),
    }
}

/// Handle `tm shell-init <shell>` — print the snippet to stdout.
///
/// Why: stdout is the whole interface. The user pipes it into `eval` (or
/// `source` for fish); anything else on stdout would be eval'd too.
/// What: prints the rendered snippet. Never writes a file.
/// Test: `cli_parses_shell_init` covers the parse; the golden tests cover the
/// text.
pub(crate) fn run_shell_init(shell: ShellArg) -> anyhow::Result<()> {
    print!("{}", shell_init_snippet(shell));
    Ok(())
}

#[cfg(test)]
#[path = "shell_init_tests.rs"]
mod tests;
