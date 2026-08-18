//! Shared `gh` invocation, schema, and argument-validation helpers (#4170).
//!
//! Why: Every tool in this module shells out to the SAME binary with a FIXED
//! subcommand and a handful of LLM-supplied operands. Two properties have to
//! hold identically for all of them, so they live here once rather than being
//! re-derived per tool: (1) no operand the model produces may ever be read by
//! `gh` as a FLAG or smuggle a second argument — `gh` has no shell involved
//! (argv is passed directly), so metacharacter escaping is a non-issue, but a
//! value like `--json` or `-R attacker/repo` absolutely would be honoured; and
//! (2) `gh`'s absence or a missing `gh auth login` must degrade into a legible
//! tool error the model can act on, never a panic.
//! What: [`fn_schema`] (the OpenAI envelope, mirroring
//! `git_tools::helpers::fn_schema`), [`plain_arg`] / [`repo_arg`] / [`limit_arg`]
//! / [`enum_arg`] (fail-closed operand validation), and [`run_gh`] (argv-only
//! subprocess execution rooted at a working directory).
//! Test: `plain_arg_rejects_flag_shaped_values`,
//! `plain_arg_rejects_whitespace_and_control_characters`,
//! `repo_arg_requires_owner_slash_repo`, `limit_arg_clamps_to_the_supported_range`,
//! `enum_arg_rejects_unlisted_values`.

use std::path::Path;

use serde_json::{Value, json};
use trusty_common::gh::{GhCommand, GhOutput};

/// Maximum accepted length for any single LLM-supplied operand.
///
/// Why: A branch name or PR selector is short. An unbounded string reaching
/// argv is a denial-of-service shape (and an obvious sign the model is
/// confused), so it is refused rather than forwarded.
const MAX_ARG_LEN: usize = 200;

/// Build an OpenAI-style function schema envelope.
///
/// Why: Identical in shape to `git_tools::helpers::fn_schema`; duplicated
/// rather than shared because that one is `pub(super)` to the git module and
/// widening its visibility to reach a second tool family would couple the two
/// surfaces for three lines of JSON.
/// What: Returns `{type, function:{name, description, parameters}}`.
/// Test: `gh_pr_view_schema_has_the_expected_envelope`.
pub(super) fn fn_schema(name: &str, description: &str, params: Value) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": params
        }
    })
}

/// Validate one LLM-supplied operand destined for `gh`'s argv.
///
/// Why: THE flag-injection gate. `gh` parses its own argv, so an operand of
/// `--repo other/repo`, `-q .`, or `--template {{...}}` would be honoured as
/// an option rather than the PR/branch/run the model meant to name. Because
/// L0 is deliberately NOT sandboxed (epic #4167), a tool in this module is
/// the last line of defence between model output and a real GitHub API call:
/// it must refuse anything ambiguous instead of forwarding it and hoping.
/// What: Accepts a non-empty value of at most [`MAX_ARG_LEN`] chars drawn from
/// `[A-Za-z0-9._/#:@-]` that does NOT begin with `-`. Everything else — a
/// leading dash, whitespace, control characters, quotes, `$`, `;`, `&`, `|`,
/// non-ASCII — is rejected with a message naming the offending field.
/// Test: `plain_arg_rejects_flag_shaped_values`,
/// `plain_arg_rejects_whitespace_and_control_characters`,
/// `plain_arg_accepts_ordinary_selectors`.
pub(super) fn plain_arg(field: &str, raw: &str) -> Result<String, String> {
    if raw.is_empty() {
        return Err(format!("'{field}' must not be empty"));
    }
    if raw.len() > MAX_ARG_LEN {
        return Err(format!(
            "'{field}' is too long ({} chars, max {MAX_ARG_LEN})",
            raw.len()
        ));
    }
    if raw.starts_with('-') {
        return Err(format!(
            "'{field}' must not start with '-' (it would be parsed by gh as an option, not a value)"
        ));
    }
    let ok = raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '#' | ':' | '@' | '-'));
    if !ok {
        return Err(format!(
            "'{field}' may only contain letters, digits, and . _ / # : @ - \
             (got {raw:?})"
        ));
    }
    Ok(raw.to_string())
}

/// Validate an `owner/repo` selector.
///
/// Why: `--repo` is how an orchestration persona reaches a repository other
/// than the one it happens to be rooted in — the cross-project half of the L0
/// grant. It is also the single operand whose shape `gh` will not check for us
/// in a useful way, so a malformed value produces a confusing remote error
/// instead of an actionable one.
/// What: Requires exactly one `/`, with a non-empty [`plain_arg`]-clean owner
/// and repo on either side.
/// Test: `repo_arg_requires_owner_slash_repo`.
pub(super) fn repo_arg(raw: &str) -> Result<String, String> {
    let value = plain_arg("repo", raw)?;
    let mut parts = value.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(format!("'repo' must be exactly 'owner/repo' (got {raw:?})"));
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(format!("'repo' must be exactly 'owner/repo' (got {raw:?})"));
    }
    Ok(value)
}

/// Resolve and clamp a result-count operand.
///
/// Why: `gh`'s `--limit` accepts arbitrarily large values; an LLM asking for
/// 100000 PRs would page the API for minutes and produce output no context
/// window can hold. Clamping (rather than erroring) keeps the tool usable when
/// the model over-asks.
/// What: `None` -> `default`; otherwise clamped to `1..=100`.
/// Test: `limit_arg_clamps_to_the_supported_range`.
pub(super) fn limit_arg(raw: Option<u64>, default: u64) -> u64 {
    raw.unwrap_or(default).clamp(1, 100)
}

/// Validate an operand against a closed set of accepted values.
///
/// Why: `--state`/`--status` operands map onto `gh`'s own enums. Passing an
/// unrecognized value through would make `gh` fail with a message about flags
/// the model never saw; rejecting it here names the accepted set instead.
/// What: Case-sensitive membership test against `accepted`.
/// Test: `enum_arg_rejects_unlisted_values`.
pub(super) fn enum_arg(field: &str, raw: &str, accepted: &[&str]) -> Result<String, String> {
    if accepted.contains(&raw) {
        return Ok(raw.to_string());
    }
    Err(format!(
        "'{field}' must be one of {} (got {raw:?})",
        accepted.join(", ")
    ))
}

/// Run `gh` with a fully pre-validated argv and return its output verbatim.
///
/// Why: #4170's acceptance criterion is "tool output is legible and unfiltered
/// (full gh output format)" — so stdout is returned as-is, with no JSON
/// re-shaping. `tolerate_nonzero` exists because `gh pr checks` uses its EXIT
/// CODE to report check state (non-zero when any check failed or is still
/// pending); treating that as a tool failure would make the single most
/// useful CI primitive report `is_error` on every red or in-flight PR, which
/// is precisely the state an orchestrator calls it to observe.
/// What: spawns via trusty-common's single `gh` entry point (#5475) rooted at
/// `root`, then maps the outcome with [`map_gh_outcome`].
/// Test: `run_gh_surfaces_the_cli_state_legibly` (environment-tolerant); the
/// outcome mapping itself is pinned deterministically by `map_gh_outcome_*`.
pub(super) async fn run_gh(
    root: &Path,
    args: &[String],
    tolerate_nonzero: bool,
) -> crate::tools::traits::ToolResult {
    use crate::tools::traits::ToolResult;

    match GhCommand::new(args).cwd(root).output().await {
        Ok(out) => map_gh_outcome(&out, tolerate_nonzero),
        // #5475: the entry point already classifies a missing binary and its
        // Display carries the `gh auth login` hint, so no local hint string.
        Err(e) => ToolResult::err(format!("failed to run 'gh {}': {e}", args.join(" "))),
    }
}

/// Map a completed `gh` run onto a `ToolResult` (pure).
///
/// Why: split from the spawn so the two behaviours that actually matter —
/// non-zero-exit TOLERANCE and the empty-output note — are pinned
/// deterministically, with no `gh` install and no subprocess at all. A test
/// that can only ever skip proves nothing, and the tolerance rule is the one
/// piece of logic whose regression would be invisible (every red PR would
/// silently start reading as a tool failure).
/// What: zero exit with output -> that stdout; zero exit with no output -> a
/// "no output" note; non-zero -> an error carrying stderr (or stdout when
/// stderr is blank), unless `tolerate_nonzero`, in which case the exit status
/// and both streams are returned as a SUCCESS payload.
/// Test: `map_gh_outcome_tolerates_a_nonzero_exit_when_asked`,
/// `map_gh_outcome_reports_a_nonzero_exit_as_an_error_by_default`,
/// `map_gh_outcome_notes_empty_output`.
pub(super) fn map_gh_outcome(
    out: &GhOutput,
    tolerate_nonzero: bool,
) -> crate::tools::traits::ToolResult {
    use crate::tools::traits::ToolResult;

    let exit = out
        .code
        .map_or_else(|| "signal".to_string(), |c| c.to_string());
    if out.success {
        if out.stdout.trim().is_empty() {
            return ToolResult::ok(format!("gh {}: no output", out.args));
        }
        return ToolResult::ok(out.stdout.clone());
    }
    if tolerate_nonzero {
        // The exit status IS the answer here; report it alongside the output
        // rather than discarding either.
        return ToolResult::ok(format!(
            "gh {} (exit {exit})\n{}{}",
            out.args, out.stdout, out.stderr
        ));
    }
    ToolResult::err(format!(
        "gh {} failed (exit {exit}): {}",
        out.args,
        if out.stderr.trim().is_empty() {
            out.stdout.trim()
        } else {
            out.stderr.trim()
        }
    ))
}
