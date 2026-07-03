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
//! mirroring claude-mpm's `_ORCHESTRATOR_EXCLUSIONS`), and any command that
//! already contains pipe/chain composition (`|`, `&&`, `;`) since appending
//! another pipe to those is the same class of risk. Otherwise it returns
//! `Some(rewritten)` with `| tm compress --tool "<effective tool name>"`
//! appended — see [`effective_tool_name`] for why the tool value is derived
//! from the command rather than a hardcoded `"bash"`.
//! [`build_pretooluse_rewrite_response`] builds the
//! `hookSpecificOutput.updatedInput` JSON body `tm hook` prints to stdout so
//! Claude Code substitutes the rewritten command before executing it.
//! Test: `rewrite_*`, `is_orchestrator_command_*`, `first_command_token_*`,
//! `build_pretooluse_rewrite_response_*` below.

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
/// (after stripping env-var-assignment prefixes, `sudo`, and any leading
/// path) by [`is_orchestrator_command`].
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
/// [`is_orchestrator_command`], or already contains pipe/chain composition
/// (see [`has_unsafe_pipe_composition`]). Otherwise returns
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
/// `rewrite_skips_chained_commands`, `rewrite_skips_empty_command`.
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
/// What: Strips the same env-assignment/`sudo`/path noise
/// [`first_command_token`] strips, then returns `"<program> <subcommand>"`
/// when a second whitespace-delimited token follows, else just `<program>`.
/// Test: `effective_tool_name_joins_program_and_subcommand`,
/// `effective_tool_name_single_token_command`,
/// `effective_tool_name_strips_env_and_sudo_noise`.
fn effective_tool_name(command: &str) -> String {
    let mut tokens = command.split_whitespace();
    let Some(mut first) = tokens.next() else {
        return String::new();
    };
    while is_env_assignment(first) {
        let Some(next) = tokens.next() else {
            return String::new();
        };
        first = next;
    }
    if first == "sudo" {
        first = tokens.next().unwrap_or(first);
    }
    let program = first.rsplit('/').next().unwrap_or(first);
    match tokens.next() {
        Some(subcommand) => format!("{program} {subcommand}"),
        None => program.to_string(),
    }
}

/// Whether `command`'s first token matches a known orchestrator exclusion.
///
/// Why: Split out from [`rewrite_bash_command_for_compression`] for direct
/// unit testing of the matching rule independent of the rewrite string
/// format.
/// What: Delegates token extraction to [`first_command_token`], then does a
/// case-sensitive exact match against [`ORCHESTRATOR_EXCLUSIONS`].
/// Test: `is_orchestrator_command_matches_known_exclusions`,
/// `is_orchestrator_command_ignores_unrelated_commands`.
fn is_orchestrator_command(command: &str) -> bool {
    match first_command_token(command) {
        Some(token) => ORCHESTRATOR_EXCLUSIONS.contains(&token),
        None => false,
    }
}

/// Extract the effective first command token, stripping noise prefixes.
///
/// Why: A raw `command.split_whitespace().next()` would miss
/// `FOO=bar make build` (env-var prefix), `sudo make install`, and
/// `/usr/bin/make` (absolute path) — all of which should still match
/// `make` for exclusion purposes. Being permissive here trades a little
/// precision for staying on the safe (under-rewrite) side.
/// What: Skips any number of leading `KEY=value`-shaped tokens, then a
/// single leading `sudo`, then returns the basename (post-`/`) of whatever
/// token remains.
/// Test: `first_command_token_strips_env_assignment`,
/// `first_command_token_strips_sudo`, `first_command_token_strips_path`,
/// `first_command_token_plain_command`.
fn first_command_token(command: &str) -> Option<&str> {
    let mut tokens = command.split_whitespace();
    let mut tok = tokens.next()?;
    while is_env_assignment(tok) {
        tok = tokens.next()?;
    }
    if tok == "sudo" {
        tok = tokens.next()?;
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

/// Whether `command` already contains pipe/chain composition.
///
/// Why: Appending a second pipe to a command that already pipes/chains
/// (`|`, `&&`, `;`) is the same class of risk the orchestrator exclusion
/// guards against — the design doc explicitly calls out "don't rewrite
/// something that already contains a `|` or `&&`/`;` chain that could break
/// under an additional pipe".
/// What: Simple substring checks — deliberately conservative (a command
/// with `|`/`&&`/`;` inside a quoted string literal will also be skipped;
/// under-rewriting is the safe failure mode here).
/// Test: `has_unsafe_pipe_composition_detects_pipe`,
/// `has_unsafe_pipe_composition_detects_and_chain`,
/// `has_unsafe_pipe_composition_detects_semicolon`,
/// `has_unsafe_pipe_composition_false_for_plain_command`.
fn has_unsafe_pipe_composition(command: &str) -> bool {
    command.contains('|') || command.contains("&&") || command.contains(';')
}

/// Build the `hookSpecificOutput.updatedInput` JSON body for a `PreToolUse`
/// Bash command rewrite.
///
/// Why: This is the JSON shape `tm hook` prints to stdout so Claude Code
/// substitutes `rewritten_command` for the original before executing it.
/// **Unverified against a live Claude Code instance** — implemented per the
/// shape sketched in `docs/specs/tool-output-interception-seam.md` §Option 0
/// (itself derived from claude-mpm's `ztk_hook.py::_build_ztk_response_impl`
/// convention), since no reference to `hookSpecificOutput`/`updatedInput`
/// exists elsewhere in this repo's docs. Needs live validation before this
/// spike is considered production-ready.
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
