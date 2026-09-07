//! `tm hook --divert-check` — `PreToolUse` bulk-read diversion (#6887).
//!
//! Why: a session that reads a 2000-line file pays for every line in its own
//! context on its own expensive model. This hook is the seam that catches such
//! a read before it happens and steers the agent to `tm divert bulk-read`,
//! which answers the question on a cheap worker instead. The #6882 POC measured
//! -45% cost and -54% output tokens for that swap.
//!
//! What: [`divert_check`] reads the `PreToolUse` stdin payload, classifies the
//! call with [`divert_targets`], counts the target's lines, and — only when a
//! worker credential is plausibly present — prints a `permissionDecision:
//! "deny"` whose reason names the command to run instead. It NEVER calls an
//! LLM: a `PreToolUse` hook has a 10-second budget and a network round trip
//! inside it would stall every read the session makes.
//!
//! Everything about this module fails OPEN, in the same house style as
//! `pm_guard` and `agent_cost`. Three distinct paths reach ALLOW and each is
//! deliberate:
//!
//! 1. Nothing to classify — no stdin, no `tool_name`, an unrecognised tool, a
//!    bounded read (`offset`/`limit`, or `head -n`), an unreadable file.
//! 2. Under threshold — the read is cheap enough to let through.
//! 3. **No worker credential** — the block would be a dead end: the agent would
//!    be told to run a command that cannot succeed, with no way back to the
//!    read it actually wanted. See [`worker_credentials_present`]. This is the
//!    branch that makes the feature safe to leave enabled on a machine that
//!    later loses its API key, and it is tested independently
//!    (`divert_check_allows_when_no_worker_credentials`).
//!
//! Test: the `#[cfg(test)]` suite below.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::manifest::DEFAULT_DIVERT_MIN_LINES;
use trusty_mpm::core::mcp_session_env::{DIVERT_MIN_LINES_ENV, DIVERT_WORKER_PROVIDER_ENV};
use trusty_mpm::core::sm::providers::ProviderRegistry;

use crate::commands::misc::{DISABLE_HOOKS_ENV, read_stdin_hook_payload};
use crate::commands::pm_guard::build_pretooluse_deny_response;

/// Bash commands that read a whole file to stdout.
///
/// Why: these are the routes to the same bytes `Read` would return, so leaving
/// them uncovered makes the feature trivially bypassable — an agent told "don't
/// Read that file" simply `cat`s it.
/// What: the bare command names, matched against the first word of the command
/// (with any directory prefix stripped).
/// Test: `divert_targets_matches_bulk_bash_readers`.
const BULK_READ_COMMANDS: [&str; 5] = ["cat", "head", "tail", "less", "more"];

/// Whether this build can actually reach AWS Bedrock.
///
/// Why (#6887): `aws_credentials_available` says an AWS credential exists, not
/// that `tm` can use it — `ProviderRegistry::construct_bedrock` returns a
/// config error when the `bedrock` cargo feature is off. Counting AWS
/// credentials as a usable worker in a default build would make the hook block
/// into exactly the dead end the fail-open rule exists to prevent.
/// What: `true` only when compiled with `--features bedrock`.
/// Test: `worker_credentials_present_ignores_aws_without_the_bedrock_feature`.
const BEDROCK_BUILT_IN: bool = cfg!(feature = "bedrock");

/// What the hook decided about one tool call.
///
/// Why: separating the decision from the I/O (stdin, the filesystem, stdout)
/// is what lets every branch — especially the fail-open ones — be unit-tested
/// without a live session.
/// What: `Allow` prints nothing (Claude Code's documented "no decision, carry
/// on"); `Block` carries the reason string that becomes
/// `permissionDecisionReason`.
/// Test: the whole suite below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DivertDecision {
    /// Let the tool call proceed untouched.
    Allow,
    /// Deny the call, telling the agent what to run instead.
    Block(String),
}

/// Entry point for `tm hook --divert-check`.
///
/// Why: registered by [`trusty_mpm::core::session_launch`] as the `PreToolUse`
/// hook for `Read` and `Bash` when `[divert] enabled` is on.
/// What: reads the stdin payload, resolves the threshold and worker provider
/// from the session environment, and prints the deny object when
/// [`decide`] blocks. Prints NOTHING on allow — per the hooks reference, exit 0
/// with no output means "no decision to report", which is what we want; an
/// explicit `allow` would bypass the user's own permission flow.
/// Test: `divert_check_allows_when_no_worker_credentials`,
/// `divert_check_blocks_when_worker_available_and_over_threshold`.
pub(crate) async fn divert_check() -> anyhow::Result<()> {
    // Same universal opt-out the PM guard honours, and for the same reason: a
    // shell that cannot edit settings.json still needs a way out.
    if std::env::var_os(DISABLE_HOOKS_ENV).is_some() {
        return Ok(());
    }
    let Some(payload) = read_stdin_hook_payload().await else {
        return Ok(());
    };
    let Some(tool_name) = payload.get("tool_name").and_then(|v| v.as_str()) else {
        return Ok(());
    };

    let cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();

    let min_lines = std::env::var(DIVERT_MIN_LINES_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_DIVERT_MIN_LINES);
    let provider = std::env::var(DIVERT_WORKER_PROVIDER_ENV).unwrap_or_else(|_| "auto".to_string());
    // Cheap and non-async: reads env markers only. NOT `.build()`, which is
    // async and may probe the AWS SDK chain — far past a hook's budget.
    let worker_available = worker_credentials_present(&ProviderRegistry::from_env(), &provider);

    let decision = decide(
        tool_name,
        payload.get("tool_input"),
        min_lines,
        worker_available,
        &|path| count_lines(&resolve_against(&cwd, path)),
    );

    if let DivertDecision::Block(reason) = decision {
        println!("{}", build_pretooluse_deny_response(&reason));
    }
    Ok(())
}

/// Decide whether one tool call should be diverted.
///
/// Why: the pure core. Every fail-open branch is reachable from here with no
/// filesystem, no network, and no process environment, which is the only way
/// the credential-absent branch can be proven to exist rather than asserted to.
/// What: returns [`DivertDecision::Block`] only when ALL of these hold — the
/// tool is an unbounded bulk read ([`divert_targets`] yields at least one
/// path), some target's line count is at or above `min_lines`, and
/// `worker_available` is true. Anything else is [`DivertDecision::Allow`].
/// `line_count` is injected so tests need no fixture files; production passes a
/// closure over [`count_lines`].
///
/// Note the credential check is LAST and is not an optimisation: a build that
/// dropped it would still pass every threshold test, which is why tests 6 and 7
/// assert the two orders of that pair separately.
/// Test: `divert_check_allows_when_no_worker_credentials`,
/// `divert_check_blocks_when_worker_available_and_over_threshold`,
/// `divert_check_allows_a_bounded_read`.
pub(crate) fn decide(
    tool_name: &str,
    tool_input: Option<&Value>,
    min_lines: u32,
    worker_available: bool,
    line_count: &dyn Fn(&str) -> Option<u32>,
) -> DivertDecision {
    let targets = divert_targets(tool_name, tool_input);
    let Some((path, lines)) = targets
        .iter()
        .filter_map(|p| line_count(p).map(|n| (p.clone(), n)))
        .find(|(_, n)| *n >= min_lines)
    else {
        return DivertDecision::Allow;
    };

    if !worker_available {
        // #6887 (fail-open, BLOCKING design precondition): blocking here would
        // send the agent to a command that cannot run. Let the read through and
        // say why once, on stderr, where it does not corrupt the hook protocol.
        tracing::warn!(
            path = %path,
            "divert: no worker credential resolvable; allowing the bulk read"
        );
        return DivertDecision::Allow;
    }

    DivertDecision::Block(block_reason(&path, lines))
}

/// The `permissionDecisionReason` shown to the agent on a block.
///
/// Why: the reason IS the recovery path. It has to name the exact replacement
/// command and the escape hatch, because the agent sees nothing else — a
/// reason that only says "too big" leaves it with no next move.
/// What: names the file, its line count, the `tm divert bulk-read` command, and
/// the two ways out (the fall-through signal, and a bounded re-read).
/// Test: `block_reason_names_the_worker_command`.
pub(crate) fn block_reason(path: &str, lines: u32) -> String {
    format!(
        "This file is {lines} lines. Reading it whole would spend your context on \
         bytes a cheap worker can summarize. Run `tm divert bulk-read {path}` with \
         your question as `--prompt`, and use its answer. If that command prints \
         `divert: fall-through`, no worker is reachable — re-run this Read with \
         `offset`/`limit` instead, which is never diverted."
    )
}

/// The file paths a tool call would bulk-read, if any.
///
/// Why: the classifier decides what the feature even applies to, so it is where
/// over-reach does damage. It is deliberately narrow: a call it cannot lex
/// confidently yields no targets and is allowed.
/// What: for `Read`, the `file_path` — unless `offset` or `limit` is present,
/// which already bounds the read and is the documented escape hatch. For
/// `Bash`, the file operands of a SINGLE simple [`BULK_READ_COMMANDS`]
/// invocation: a command containing a pipe, redirect, or separator is left
/// alone (it is not a plain read), and a `head`/`tail` carrying an explicit
/// count (`-n`, `-c`, `-100`) is already bounded. Every other tool yields
/// nothing.
/// Test: `divert_targets_matches_bulk_bash_readers`,
/// `divert_targets_skips_bounded_reads`, `divert_targets_ignores_other_tools`.
pub(crate) fn divert_targets(tool_name: &str, tool_input: Option<&Value>) -> Vec<String> {
    let Some(input) = tool_input else {
        return Vec::new();
    };
    match tool_name {
        "Read" => {
            if input.get("offset").is_some() || input.get("limit").is_some() {
                return Vec::new();
            }
            input
                .get("file_path")
                .and_then(|v| v.as_str())
                .filter(|p| !p.trim().is_empty())
                .map(|p| vec![p.to_string()])
                .unwrap_or_default()
        }
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(bash_read_targets)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// File operands of a single simple bulk-read shell command.
///
/// Why: see [`divert_targets`]. Kept intentionally dumb — anything it is not
/// certain about it declines, because a false positive here blocks a call the
/// agent legitimately needs.
/// What: returns empty when the command contains a shell composition character
/// (`| & ; < > $ ( ` \n`), when the first word is not in
/// [`BULK_READ_COMMANDS`], or when a bounding flag is present. Otherwise
/// returns every non-flag operand after the command word.
/// Test: `divert_targets_matches_bulk_bash_readers`,
/// `divert_targets_skips_bounded_reads`.
fn bash_read_targets(command: &str) -> Vec<String> {
    if command
        .chars()
        .any(|c| matches!(c, '|' | '&' | ';' | '<' | '>' | '$' | '(' | '`' | '\n'))
    {
        return Vec::new();
    }
    let mut words = command.split_whitespace();
    let Some(head) = words.next() else {
        return Vec::new();
    };
    let bare = Path::new(head)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(head);
    if !BULK_READ_COMMANDS.contains(&bare) {
        return Vec::new();
    }

    let rest: Vec<&str> = words.collect();
    // A bounding flag (`-n 40`, `-c 200`, `-40`) already caps the read.
    if rest
        .iter()
        .any(|w| w.starts_with('-') && w.len() > 1 && !w.starts_with("--"))
    {
        return Vec::new();
    }
    rest.iter()
        .filter(|w| !w.starts_with('-'))
        .map(|w| w.trim_matches(['"', '\'']).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether a worker credential is plausibly present for `provider`.
///
/// Why (#6887, BLOCKING design precondition): the hook must never block into a
/// dead end. If `tm divert bulk-read` cannot reach any provider, the block is
/// pure loss — the agent is denied the read AND denied the replacement. This is
/// the predicate that turns that case back into an allow.
/// What: a pure, non-network read of an already-constructed
/// [`ProviderRegistry`]. `auto` accepts any credential; an explicit provider
/// accepts only its own. AWS credentials count only when the `bedrock` feature
/// is compiled in ([`BEDROCK_BUILT_IN`]) — otherwise `construct_bedrock`
/// returns a config error and the worker could not run. An unrecognised
/// provider string is treated as `auto` rather than as "no worker": the
/// registry, not this hook, is the authority on provider names, and guessing
/// "unusable" here would silently disable the feature on a typo'd config.
/// Test: `worker_credentials_present_requires_a_matching_credential`,
/// `worker_credentials_present_ignores_aws_without_the_bedrock_feature`.
pub(crate) fn worker_credentials_present(registry: &ProviderRegistry, provider: &str) -> bool {
    let anthropic = registry.anthropic_api_key.is_some();
    let openrouter = registry.openrouter_api_key.is_some();
    let bedrock = registry.aws_credentials_available && BEDROCK_BUILT_IN;
    match provider.trim().to_ascii_lowercase().as_str() {
        "anthropic" => anthropic,
        "openrouter" => openrouter,
        "bedrock" => bedrock,
        _ => anthropic || openrouter || bedrock,
    }
}

/// Resolve a possibly-relative hook path against the session's `cwd`.
///
/// Why: Claude Code's payload carries whatever path the agent typed, which for
/// a `Bash` `cat` is usually relative to the session's working directory.
/// What: joins onto `cwd` unless `path` is already absolute.
/// Test: covered via `divert_check_blocks_when_worker_available_and_over_threshold`.
fn resolve_against(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Count the lines of a file, or `None` when it cannot be read.
///
/// Why: an unreadable path must ALLOW, not block — a missing file is the tool
/// call's problem to report, not the hook's to pre-empt.
/// What: reads the file and counts `\n`-separated lines, capped at `u32`. A
/// non-UTF-8 or unreadable file yields `None`.
/// Test: `count_lines_reads_a_real_file`.
fn count_lines(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(u32::try_from(text.lines().count()).unwrap_or(u32::MAX))
}

#[cfg(test)]
#[path = "divert_check_tests.rs"]
mod tests;
