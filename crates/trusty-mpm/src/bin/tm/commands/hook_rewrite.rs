//! `tm hook` `PreToolUse` Bash command-rewrite logic — Option 0 spike (#1956).
//!
//! Why: `docs/specs/tool-output-interception-seam.md` §Option 0 documents a
//! pre-context-insertion compression seam for Bash: rewrite the *command*
//! Claude Code is about to execute so the subprocess's own stdout is already
//! filtered by the time the tool result is captured — the same trick
//! claude-mpm's `ztk` hook uses (`hooks/ztk_hook.py`), but piping through
//! `tm compress` instead of an external Zig binary. That doc's Tradeoffs
//! section calls out the exact incident this must defend against from day
//! one: an earlier orchestrator command (a SAM build) broke when wrapped
//! unconditionally, because appending a pipe can change exit-code semantics
//! (`set -o pipefail` interactions) for build tools that spawn long
//! subprocess chains.
//! What: [`rewrite_bash_command_for_compression`] is the single entry point —
//! it returns `None` (meaning: do not rewrite, forward the command
//! unmodified) for empty commands, day-one orchestrator exclusions
//! (`make`/`sam`/`rake`/`gradle`/`gradlew`/`mvn`/`ant`/`cdk`/`terraform`,
//! mirroring claude-mpm's `_ORCHESTRATOR_EXCLUSIONS`), any command that
//! already contains pipe/chain/redirection/background composition (`|`,
//! `&`, `;`, `>`, `<`) since appending another pipe to those is the same class of
//! risk (redirection in particular: piping `tm compress` after an
//! already-redirected stdout gets it empty input for no benefit — a
//! trusty-review finding, PR #1968), and any command whose derived tool
//! name would embed shell metacharacters (see
//! [`is_safe_tool_name`]). Otherwise it returns `Some(rewritten)` with
//! `| tm compress --tool "<effective tool name>"` appended — see
//! [`effective_tool_name`] for why the tool value is derived from the
//! command rather than a hardcoded `"bash"`.
//! [`build_pretooluse_rewrite_response`] builds the
//! `hookSpecificOutput.updatedInput` JSON body `tm hook` prints to stdout so
//! Claude Code substitutes the rewritten command before executing it — this
//! exact shape is confirmed against the live Claude Code hooks reference
//! (<https://code.claude.com/docs/en/hooks>, confirmed 2026-07-03; see that
//! function's doc comment for the citation).
//! Test: `rewrite_*`, `is_orchestrator_command_*`, `first_command_token_*`,
//! `is_safe_tool_name_*`, `build_pretooluse_rewrite_response_*` below.

/// Day-one orchestrator-command exclusion list.
///
/// Why: These tools spawn long, multi-stage subprocess chains where an
/// unconditional `| tm compress --tool bash` wrap has already caused a real
/// incident (a SAM build silently truncated at a later stage — see the
/// design doc's Tradeoffs section and `docs/specs/SPEC-INSTALLER-01.md:150`).
/// Mirrors claude-mpm's `_ORCHESTRATOR_EXCLUSIONS` (`hooks/ztk_hook.py`
/// ~lines 49-69) so this spike inherits the same day-one safety margin
/// rather than rediscovering the failure mode.
/// What: Matched against the command's first whitespace-delimited token
/// (after stripping env-var-assignment prefixes, `sudo`/`env`, and any
/// leading path) by [`is_orchestrator_command`].
/// Test: `is_orchestrator_command_matches_known_exclusions`.
const ORCHESTRATOR_EXCLUSIONS: &[&str] = &[
    "make",
    "sam",
    "rake",
    "gradle",
    "gradlew",
    "mvn",
    "ant",
    "cdk",
    "terraform",
];

/// Decide whether `command` is safe to rewrite, and build the rewrite.
///
/// Why: Centralises the "is this rewrite safe?" decision so `tm hook` can
/// stay a thin caller — better to under-rewrite (skip a compressible
/// command) than to break a working one, per the design doc's explicit
/// tradeoff framing.
/// What: Returns `None` (no-op — forward the original command unmodified)
/// when `command` is empty/whitespace-only, matches
/// [`is_orchestrator_command`], already contains pipe/chain composition
/// (see [`has_unsafe_pipe_composition`]), or derives a tool name containing
/// shell metacharacters (see [`is_safe_tool_name`] — a
/// trusty-review-flagged injection risk, PR #1968: the derived name is
/// embedded verbatim inside a double-quoted shell argument below, and
/// double quotes do not stop POSIX command substitution). Otherwise returns
/// `Some("<command> | tm compress --tool \"<effective tool name>\"")` —
/// the `--tool` value comes from [`effective_tool_name`], **not** a
/// hardcoded `"bash"`: `compress_tool_output`'s dispatch table
/// (`trusty-agents-common::compress::tool_output`) matches filters by
/// substring against the tool name (`"cargo"`, `"diff"`, `"log"`, …), so a
/// literal `"bash"` would never match any filter branch and every rewritten
/// command would silently pass through with 0% compression — defeating the
/// entire point of this spike. Quoted because the derived name may contain
/// a space (e.g. `"cargo test"`).
/// Test: `rewrite_appends_compress_pipe_for_plain_command`,
/// `rewrite_appends_compress_pipe_with_subcommand_tool_name`,
/// `rewrite_skips_orchestrator_commands`, `rewrite_skips_piped_commands`,
/// `rewrite_skips_chained_commands`, `rewrite_skips_empty_command`,
/// `rewrite_skips_command_substitution_in_tool_name`.
pub(crate) fn rewrite_bash_command_for_compression(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return None;
    }
    if is_orchestrator_command(trimmed) {
        return None;
    }
    if has_unsafe_pipe_composition(trimmed) {
        return None;
    }
    let tool = effective_tool_name(trimmed);
    if !is_safe_tool_name(&tool) {
        return None;
    }
    Some(format!("{trimmed} | tm compress --tool \"{tool}\""))
}

/// Derive the `compress_tool_output` dispatch key from a Bash command.
///
/// Why: The dispatch table in `trusty-agents-common::compress::tool_output`
/// matches per-tool filters by substring against a `tool_name` string shaped
/// like `"cargo test"` / `"git diff"` (see that module's `cargo`/`git`
/// branches) — it has no branch for the literal string `"bash"`. Passing
/// the command's actual program + subcommand lets the existing `cargo
/// test`/`cargo check`/`git diff`/`git log` filters fire for the domains
/// they already cover; commands outside that coverage (e.g. `grep`, `ls`)
/// still pass through unchanged, which matches this repo's own documented
/// filter-coverage gap (`docs/specs/tool-output-interception-seam.md`
/// §Spike) rather than a new regression.
/// What: Strips the same env-assignment/`sudo`/`env`/path noise
/// [`first_command_token`] strips (looping so `env FOO=bar cargo test` and
/// similar multi-prefix combinations resolve correctly — a trusty-review
/// finding, PR #1968: a one-shot `if first == "sudo"` check left the literal
/// `env` command unhandled, e.g. `env FOO=bar cargo test` previously
/// resolved to the wrong tool name `"env FOO=bar"` instead of `"cargo
/// test"`), then returns `"<program> <subcommand>"` when a second
/// whitespace-delimited token follows, else just `<program>`.
/// Test: `effective_tool_name_joins_program_and_subcommand`,
/// `effective_tool_name_single_token_command`,
/// `effective_tool_name_strips_env_and_sudo_noise`,
/// `effective_tool_name_strips_env_command_prefix`.
pub(crate) fn effective_tool_name(command: &str) -> String {
    let mut tokens = command.split_whitespace();
    let Some(mut first) = tokens.next() else {
        return String::new();
    };
    loop {
        if is_env_assignment(first) {
            let Some(next) = tokens.next() else {
                return String::new();
            };
            first = next;
            continue;
        }
        if first == "sudo" || first == "env" {
            match tokens.next() {
                Some(next) if !next.starts_with('-') => {
                    first = next;
                    continue;
                }
                // Either nothing follows `sudo`/`env`, or the next token is
                // itself a flag (e.g. `sudo -u root make`, `env -i cmd`).
                // Resolving the real program name in that case needs
                // argument-aware parsing this function doesn't do; rather
                // than guess and derive a bogus tool name (trusty-review
                // finding, PR #1968: `sudo -u root make build` previously
                // resolved to `"-u root"` instead of recognising `make`),
                // degrade to "unknown" — `is_safe_tool_name` already
                // rejects an empty string, so the caller safely skips the
                // rewrite.
                _ => return String::new(),
            }
        }
        break;
    }
    let program = first.rsplit('/').next().unwrap_or(first);
    match tokens.next() {
        Some(subcommand) => format!("{program} {subcommand}"),
        None => program.to_string(),
    }
}

/// Whether `tool_name` is safe to embed verbatim inside the double-quoted
/// `--tool "<tool_name>"` argument of the rewritten command.
///
/// Why: [`effective_tool_name`] derives its value from whitespace-delimited
/// tokens of the *user-supplied* Bash command with no further sanitisation.
/// Wrapping the value in double quotes only defeats word-splitting — POSIX
/// shells still perform command substitution (`$(...)`, backticks) and
/// variable expansion (`$VAR`) *inside* double-quoted strings, and an
/// embedded `"` would terminate the quoted argument early and let whatever
/// follows be interpreted as new shell syntax. A command whose first two
/// tokens happen to contain any of those characters — e.g.
/// `echo $(touch /tmp/pwned)` — would therefore have that substitution
/// re-evaluated a second time, in a new shell position, as part of building
/// the `--tool` argument (trusty-review-flagged shell-injection risk, PR
/// #1968; confirmed by an independent `review_diff` pass).
/// What: `true` only when every character is ASCII alphanumeric or one of
/// the small set that legitimately appears in tool names/subcommands/paths
/// (space, `-`, `_`, `.`, `/`, `:`) — exactly what `"cargo test"`,
/// `"git diff"`, or `"./scripts/foo.sh"` need. Anything else (`$`,
/// backticks, `"`, `\`, parens, newlines, …) is rejected, and
/// [`rewrite_bash_command_for_compression`] falls back to its safe
/// no-rewrite default rather than risk embedding it.
/// Test: `is_safe_tool_name_allows_typical_tool_names`,
/// `is_safe_tool_name_rejects_shell_metacharacters`.
fn is_safe_tool_name(tool_name: &str) -> bool {
    !tool_name.is_empty()
        && tool_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '/' | ':'))
}

/// Whether `command`'s first token matches a known orchestrator exclusion.
///
/// Why: Split out from [`rewrite_bash_command_for_compression`] for direct
/// unit testing of the matching rule independent of the rewrite string
/// format.
/// What: Delegates token extraction to [`first_command_token`], then does a
/// case-sensitive exact match against [`ORCHESTRATOR_EXCLUSIONS`]. When
/// [`first_command_token`] can't confidently resolve the real program name
/// (returns `None` — e.g. `sudo -u root make`, where the token after `sudo`
/// is itself a flag, not a program), this conservatively returns `true`
/// (treat as orchestrator-like, i.e. don't rewrite) rather than `false`: a
/// trusty-review-flagged fail-open gap (PR #1968) where "can't tell" used
/// to mean "assume it's safe to rewrite" — backwards for a safety check
/// whose entire purpose is avoiding exactly this kind of unrecognised
/// build-orchestrator invocation.
/// Test: `is_orchestrator_command_matches_known_exclusions`,
/// `is_orchestrator_command_ignores_unrelated_commands`,
/// `is_orchestrator_command_conservatively_true_for_sudo_with_flags`.
fn is_orchestrator_command(command: &str) -> bool {
    match first_command_token(command) {
        Some(token) => ORCHESTRATOR_EXCLUSIONS.contains(&token),
        None => true,
    }
}

/// Extract the effective first command token, stripping noise prefixes.
///
/// Why: A raw `command.split_whitespace().next()` would miss
/// `FOO=bar make build` (env-var prefix), `sudo make install`, and
/// `/usr/bin/make` (absolute path) — all of which should still match
/// `make` for exclusion purposes. Being permissive here trades a little
/// precision for staying on the safe (under-rewrite) side.
/// What: Loops skipping any number of leading `KEY=value`-shaped tokens
/// interleaved with `sudo`/`env` prefixes (so `env FOO=bar make build`
/// resolves to `make` just like `sudo make build` does — see
/// [`effective_tool_name`]'s doc comment for the same `env`-handling gap
/// this mirrors), then returns the basename (post-`/`) of whatever token
/// remains.
/// Returns `None` — "can't confidently resolve" — when the token
/// immediately after `sudo`/`env` is itself flag-shaped (starts with `-`,
/// e.g. `sudo -u root make`, `env -i cmd`): the real program name in that
/// case needs argument-aware parsing this function doesn't do, and
/// [`is_orchestrator_command`] treats `None` as "conservatively assume
/// orchestrator" rather than risk missing a real exclusion (trusty-review
/// finding, PR #1968).
/// Test: `first_command_token_strips_env_assignment`,
/// `first_command_token_strips_sudo`, `first_command_token_strips_path`,
/// `first_command_token_plain_command`,
/// `first_command_token_strips_env_command_prefix`,
/// `first_command_token_none_for_sudo_followed_by_flag`.
pub(crate) fn first_command_token(command: &str) -> Option<&str> {
    let mut tokens = command.split_whitespace();
    let mut tok = tokens.next()?;
    loop {
        if is_env_assignment(tok) {
            tok = tokens.next()?;
            continue;
        }
        if tok == "sudo" || tok == "env" {
            let next = tokens.next()?;
            if next.starts_with('-') {
                return None;
            }
            tok = next;
            continue;
        }
        break;
    }
    Some(tok.rsplit('/').next().unwrap_or(tok))
}

/// Whether `token` looks like a `KEY=value` shell environment assignment.
///
/// Why: Shared by [`first_command_token`]; split out so the "what counts as
/// an env assignment" rule has one definition.
/// What: `KEY` must be non-empty, start with a letter or underscore, and
/// contain only alphanumerics/underscores up to the first `=`.
fn is_env_assignment(token: &str) -> bool {
    let Some((key, _value)) = token.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `command` already contains pipe/chain/redirection/background
/// composition.
///
/// Why: Appending a second pipe to a command that already pipes/chains
/// (`|`, `&&`, `;`) is the same class of risk the orchestrator exclusion
/// guards against — the design doc explicitly calls out "don't rewrite
/// something that already contains a `|` or `&&`/`;` chain that could break
/// under an additional pipe". Output/input redirection (`>`, `>>`, `<`) is
/// the same class of risk from the opposite direction: a trusty-review
/// finding (PR #1968) noted that `cargo test > out.log`, rewritten to
/// `cargo test > out.log | tm compress --tool "cargo test"`, pipes
/// `tm compress` an *empty* stdin (stdout was already redirected to the
/// file) — the rewrite would silently do nothing useful while altering the
/// command's pipeline/exit-code shape for no benefit. Backgrounding (`&`) is
/// a third variant of the same risk (a further trusty-review finding, PR
/// #1968): `cargo test &` rewritten to `cargo test & | tm compress ...`
/// is invalid shell syntax in most shells, not merely a no-op.
/// What: Simple substring checks — deliberately conservative (a command
/// with `|`/`&`/`;`/`>`/`<` inside a quoted string literal will also be
/// skipped; under-rewriting is the safe failure mode here). A single
/// `contains('&')` check subsumes both the `&&` chain operator and the
/// standalone backgrounding `&` — there's no case where `&` is safe to
/// leave unguarded in this position.
/// Test: `has_unsafe_pipe_composition_detects_pipe`,
/// `has_unsafe_pipe_composition_detects_and_chain`,
/// `has_unsafe_pipe_composition_detects_semicolon`,
/// `has_unsafe_pipe_composition_detects_output_redirection`,
/// `has_unsafe_pipe_composition_detects_input_redirection`,
/// `has_unsafe_pipe_composition_detects_background_operator`,
/// `has_unsafe_pipe_composition_false_for_plain_command`.
fn has_unsafe_pipe_composition(command: &str) -> bool {
    command.contains('|')
        || command.contains('&') // catches both `&&` chaining and background `&`
        || command.contains(';')
        || command.contains('>')
        || command.contains('<')
}

/// Build the `hookSpecificOutput.updatedInput` JSON body for a `PreToolUse`
/// Bash command rewrite.
///
/// Why: This is the JSON shape `tm hook` prints to stdout so Claude Code
/// substitutes `rewritten_command` for the original before executing it.
/// Originally implemented per the shape sketched in
/// `docs/specs/tool-output-interception-seam.md` §Option 0 (itself derived
/// from claude-mpm's `ztk_hook.py::_build_ztk_response_impl` convention) with
/// no in-repo reference to confirm it. **Confirmed against the live Claude
/// Code hooks reference** (<https://code.claude.com/docs/en/hooks>,
/// confirmed 2026-07-03 while addressing a trusty-review finding on PR
/// #1968): "PreToolUse: `updatedInput` directly under `hookSpecificOutput`
/// replaces a tool's arguments before it runs," with the documented example
/// matching this shape exactly. That same reference also confirms (a) the
/// hook's response is only parsed on `exit 0`, and stdout "must contain only
/// the JSON object" for that parse to succeed — see `misc::hook`'s doc
/// comment for why the caller now returns immediately after printing this
/// — and (b) the stdin payload field names this module's caller
/// (`misc::hook`) reads (`hook_event_name`, `tool_name`,
/// `tool_input.command`) match the live protocol. This function itself has
/// no failure path: parsing failures upstream (in `misc::read_stdin_hook_payload`)
/// already degrade to "no stdin payload" before this is ever called, so an
/// invalid/unrecognised hook payload shape fails safe as a silent no-op, not
/// a malformed response.
/// What: `{"hookSpecificOutput": {"hookEventName": "PreToolUse",
/// "updatedInput": {"command": rewritten_command}}}`.
/// Test: `build_pretooluse_rewrite_response_has_expected_shape`.
pub(crate) fn build_pretooluse_rewrite_response(rewritten_command: &str) -> serde_json::Value {
    serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "updatedInput": { "command": rewritten_command }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_appends_compress_pipe_for_plain_command() {
        let out = rewrite_bash_command_for_compression("cargo test");
        assert_eq!(
            out.as_deref(),
            Some("cargo test | tm compress --tool \"cargo test\"")
        );
    }

    #[test]
    fn rewrite_appends_compress_pipe_with_subcommand_tool_name() {
        // The `--tool` value must be the derived "<program> <subcommand>"
        // pair (matching the compress dispatch table's filter keys), not a
        // hardcoded "bash" — see `effective_tool_name`'s doc comment for why
        // a literal "bash" would never match any filter branch.
        let out = rewrite_bash_command_for_compression("git diff HEAD~1");
        assert_eq!(
            out.as_deref(),
            Some("git diff HEAD~1 | tm compress --tool \"git diff\"")
        );
    }

    #[test]
    fn rewrite_skips_orchestrator_commands() {
        for cmd in [
            "make build",
            "sam deploy",
            "terraform apply",
            "gradlew build",
        ] {
            assert_eq!(
                rewrite_bash_command_for_compression(cmd),
                None,
                "expected no rewrite for orchestrator command: {cmd}"
            );
        }
    }

    #[test]
    fn rewrite_skips_piped_commands() {
        assert_eq!(
            rewrite_bash_command_for_compression("cargo test | tee out.log"),
            None
        );
    }

    #[test]
    fn rewrite_skips_chained_commands() {
        assert_eq!(
            rewrite_bash_command_for_compression("cargo build && cargo test"),
            None
        );
        assert_eq!(rewrite_bash_command_for_compression("cd /tmp; ls"), None);
    }

    #[test]
    fn rewrite_skips_empty_command() {
        assert_eq!(rewrite_bash_command_for_compression(""), None);
        assert_eq!(rewrite_bash_command_for_compression("   "), None);
    }

    #[test]
    fn rewrite_skips_command_substitution_in_tool_name() {
        // `$(...)`, backticks, and embedded quotes in the derived tool name
        // must never reach the `--tool "<name>"` shell argument verbatim —
        // see `is_safe_tool_name`'s doc comment for the injection risk.
        assert_eq!(
            rewrite_bash_command_for_compression("echo $(touch /tmp/pwned)"),
            None
        );
        assert_eq!(rewrite_bash_command_for_compression("echo `whoami`"), None);
        assert_eq!(
            rewrite_bash_command_for_compression("echo \"quoted\" arg"),
            None
        );
    }

    #[test]
    fn rewrite_skips_sudo_with_user_flag() {
        // Regression test (trusty-review finding, PR #1968): `sudo -u root
        // make build` must never be rewritten — before the fix, the token
        // after `sudo` (`-u`) was blindly treated as the program name,
        // producing the bogus tool name `"-u root"` and — critically —
        // causing `is_orchestrator_command` to miss that this is a `make`
        // invocation, defeating the orchestrator-exclusion safety net this
        // whole module exists to provide.
        assert_eq!(
            rewrite_bash_command_for_compression("sudo -u root make build"),
            None
        );
    }

    #[test]
    fn is_orchestrator_command_matches_known_exclusions() {
        assert!(is_orchestrator_command("make"));
        assert!(is_orchestrator_command("sudo make install"));
        assert!(is_orchestrator_command("FOO=bar make build"));
        assert!(is_orchestrator_command("/usr/bin/make build"));
    }

    #[test]
    fn is_orchestrator_command_ignores_unrelated_commands() {
        assert!(!is_orchestrator_command("cargo test"));
        assert!(!is_orchestrator_command("git diff"));
    }

    #[test]
    fn is_orchestrator_command_conservatively_true_for_sudo_with_flags() {
        // Fail-safe: when the real program name can't be confidently
        // resolved (`sudo`/`env` followed by a flag rather than a program),
        // treat the command as orchestrator-like rather than risk rewriting
        // an unrecognised `make`/`sam`/etc. invocation.
        assert!(is_orchestrator_command("sudo -u root make build"));
        assert!(is_orchestrator_command("env -i make build"));
    }

    #[test]
    fn first_command_token_strips_env_assignment() {
        assert_eq!(first_command_token("FOO=bar make build"), Some("make"));
    }

    #[test]
    fn first_command_token_strips_sudo() {
        assert_eq!(first_command_token("sudo make install"), Some("make"));
    }

    #[test]
    fn first_command_token_strips_path() {
        assert_eq!(first_command_token("/usr/bin/make build"), Some("make"));
    }

    #[test]
    fn first_command_token_plain_command() {
        assert_eq!(first_command_token("cargo test"), Some("cargo"));
    }

    #[test]
    fn first_command_token_strips_env_command_prefix() {
        // `env FOO=bar make build` (the common `env`-prefixed invocation
        // form) must still resolve to `make` for orchestrator-exclusion
        // matching purposes — a trusty-review-flagged gap where only
        // `sudo` was handled, not the literal `env` command.
        assert_eq!(first_command_token("env FOO=bar make build"), Some("make"));
    }

    #[test]
    fn first_command_token_none_for_sudo_followed_by_flag() {
        // `sudo -u root make` — the token after `sudo` is itself a flag,
        // not a program name; resolving it correctly needs argument-aware
        // parsing this function doesn't do, so it must degrade to `None`
        // (trusty-review finding, PR #1968) rather than returning the
        // bogus token `"-u"`.
        assert_eq!(first_command_token("sudo -u root make build"), None);
        assert_eq!(first_command_token("env -i make build"), None);
    }

    #[test]
    fn effective_tool_name_joins_program_and_subcommand() {
        assert_eq!(effective_tool_name("cargo test --workspace"), "cargo test");
        assert_eq!(effective_tool_name("git diff HEAD~1"), "git diff");
    }

    #[test]
    fn effective_tool_name_single_token_command() {
        assert_eq!(effective_tool_name("ls"), "ls");
        assert_eq!(effective_tool_name("ls -la"), "ls -la");
    }

    #[test]
    fn effective_tool_name_strips_env_and_sudo_noise() {
        assert_eq!(effective_tool_name("FOO=bar cargo test"), "cargo test");
        assert_eq!(effective_tool_name("sudo cargo test"), "cargo test");
        assert_eq!(effective_tool_name("/usr/bin/cargo test"), "cargo test");
    }

    #[test]
    fn effective_tool_name_strips_env_command_prefix() {
        // `env FOO=bar cargo test` (the literal `env` command, not just a
        // `KEY=value` assignment) must still resolve to `"cargo test"`, not
        // the previously-buggy `"env FOO=bar"` — trusty-review finding,
        // PR #1968.
        assert_eq!(effective_tool_name("env FOO=bar cargo test"), "cargo test");
    }

    #[test]
    fn effective_tool_name_returns_empty_for_sudo_followed_by_flag() {
        // `sudo -u root make build` must NOT resolve to a bogus tool name
        // like `"-u root"` — trusty-review finding, PR #1968. An empty
        // string fails `is_safe_tool_name`, so the caller safely skips the
        // rewrite instead of guessing.
        assert_eq!(effective_tool_name("sudo -u root make build"), "");
        assert_eq!(effective_tool_name("env -i cargo test"), "");
    }

    #[test]
    fn is_safe_tool_name_allows_typical_tool_names() {
        assert!(is_safe_tool_name("cargo test"));
        assert!(is_safe_tool_name("git diff"));
        assert!(is_safe_tool_name("./scripts/foo.sh"));
        assert!(is_safe_tool_name("ls-la_v2"));
    }

    #[test]
    fn is_safe_tool_name_rejects_shell_metacharacters() {
        assert!(!is_safe_tool_name("echo $(touch /tmp/pwned)"));
        assert!(!is_safe_tool_name("echo `whoami`"));
        assert!(!is_safe_tool_name("echo \"quoted\""));
        assert!(!is_safe_tool_name("a;b"));
        assert!(!is_safe_tool_name(""));
    }

    #[test]
    fn has_unsafe_pipe_composition_detects_pipe() {
        assert!(has_unsafe_pipe_composition("cargo test | tee out.log"));
    }

    #[test]
    fn has_unsafe_pipe_composition_detects_and_chain() {
        assert!(has_unsafe_pipe_composition("cargo build && cargo test"));
    }

    #[test]
    fn has_unsafe_pipe_composition_detects_semicolon() {
        assert!(has_unsafe_pipe_composition("cd /tmp; ls"));
    }

    #[test]
    fn has_unsafe_pipe_composition_detects_output_redirection() {
        // Regression test (trusty-review finding, PR #1968): a command that
        // already redirects stdout to a file must not be rewritten — piping
        // `tm compress` after the redirect would receive empty stdin and do
        // nothing useful.
        assert!(has_unsafe_pipe_composition("cargo test > out.log"));
        assert!(has_unsafe_pipe_composition("cargo test >> out.log"));
    }

    #[test]
    fn has_unsafe_pipe_composition_detects_input_redirection() {
        assert!(has_unsafe_pipe_composition("cat < input.txt"));
    }

    #[test]
    fn has_unsafe_pipe_composition_detects_background_operator() {
        // Regression test (trusty-review finding, PR #1968): `cargo test &`
        // rewritten to `cargo test & | tm compress ...` is invalid shell
        // syntax in most shells — must never be rewritten.
        assert!(has_unsafe_pipe_composition("cargo test &"));
    }

    #[test]
    fn has_unsafe_pipe_composition_false_for_plain_command() {
        assert!(!has_unsafe_pipe_composition("cargo test"));
    }

    #[test]
    fn build_pretooluse_rewrite_response_has_expected_shape() {
        let response = build_pretooluse_rewrite_response("cargo test | tm compress --tool bash");
        assert_eq!(
            response["hookSpecificOutput"]["hookEventName"],
            "PreToolUse"
        );
        assert_eq!(
            response["hookSpecificOutput"]["updatedInput"]["command"],
            "cargo test | tm compress --tool bash"
        );
    }
}
