//! The chained forms the compression rewrite accepts: `a && b`, `a; b`, `… | head`.
//!
//! Why (#10973, #11037): `hook_rewrite::has_unsafe_pipe_composition` refused
//! every `&`, `;` and `|` by substring, so `cargo build && cargo test`,
//! `git diff; git log -3` and `cargo test | tail -40` — 26-39% of Bash calls in
//! the 11-13 tm-dogfood data — were never compressed, and the 💸 footer read
//! `—` because `tm compress` wrote no `savings.jsonl` row.
//! What: [`chain_tool_decision`] judges each segment, never the raw string.
//! Segments come from the PM guard's own splitter
//! (`pm_guard_bash::split_shell_segments_raw`). It accepts only `&&` and `;`
//! separators plus one optional trailing `|` into a reader in [`READERS`].
//! Every other segment must pass `compressible_tool_decision` on its own (so an
//! orchestrator, source read or unfiltered program refuses the chain), or be
//! `true` or `export NAME=value`. Each such segment must also name an allowlisted program and verb
//! (`is_safe_program`); a segment the destructive-delete classifier flags
//! refuses the chain too. Anything ambiguous fails
//! closed as `Chained`: `||`, a background `&`, `$( )`, backticks, `( )`,
//! braces, any `<` or `>` (heredocs included), a line break, unlexable
//! quoting, and a `cd` past the one leading `cd <dir> && ` that
//! `hook_rewrite_cd` keeps outside the group (a `cd` inside the piped group
//! would run in a subshell and leave the caller's cwd unmoved).
//! An isolation worktree still refuses every chain: its no-shell
//! `tm compress -- <cmd>` shape (#7477) cannot carry one.
//! Test: `a_safe_chain_is_compressed`, `a_destructive_segment_refuses_the_chain`,
//! `an_ambiguous_shape_is_not_compressed`,
//! `an_isolation_worktree_refuses_every_chain`,
//! `a_chain_rewrite_runs_like_the_original_in_a_real_shell`.

use std::path::Path;

use super::hook_rewrite::{compressible_tool_decision, is_env_assignment, shlex_words};
use super::pm_guard_bash::{evaluate_destructive_delete_command, split_shell_segments_raw};
use trusty_agents_common::compress::tool_output::classify_tool;
use trusty_mpm::core::savings_opportunity::SkipReason;

/// The programs a trailing `|` may feed; each only reads stdin and prints.
const READERS: &[&str] = &["head", "tail", "wc", "grep", "sort", "uniq", "cut", "tr"];

/// `find` primaries that act on what they find instead of printing it.
const FIND_ACTIONS: &[&str] = &[
    "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
];

/// The `--tool` value for a chained `inner`, or why it is not compressed.
///
/// Why: see the module doc.
/// What: `Err(Chained)` for any shape the module doc lists as ambiguous, for a
/// non-leading `cd`, a pipe that is not the last separator, more than one
/// pipe, or a pipe into a non-reader. `;` is refused when `allow_semicolon` is
/// false — the caller kept a `cd <dir> && ` prefix outside the group, and
/// `cd x && a; b` parses as `(cd x && a); b`, which the group cannot mirror. A body segment refused by
/// `compressible_tool_decision` refuses the chain with that segment's reason.
/// Every filter-covered segment must share one `classify_tool` class (a mixed
/// chain such as `git diff; git log` is `Chained`: one filter would drop the
/// other's output). `Ok` carries the last covered segment's tool name; a chain
/// made only of `true`/`export` segments is `NoFilter`.
/// Test: `a_safe_chain_is_compressed`, `a_destructive_segment_refuses_the_chain`,
/// `an_ambiguous_shape_is_not_compressed`.
pub(crate) fn chain_tool_decision(
    inner: &str,
    allow_semicolon: bool,
) -> Result<String, SkipReason> {
    if has_ambiguous_shape(inner) {
        return Err(SkipReason::Chained);
    }
    let segments = split_shell_segments_raw(inner);
    let mut tool = None;
    for (i, segment) in segments.iter().enumerate() {
        let text = segment.trim();
        let separator = segments
            .get(i + 1)
            .map(|next| separator_after(inner, segment, next));
        match separator {
            Some("&&") | None => {}
            // `cd x && a; b` runs `b` even when the `cd` fails; the group would not.
            Some(";") if allow_semicolon => {}
            Some("|") if i + 2 == segments.len() => {
                if !is_reader(segments[i + 1].trim()) {
                    return Err(SkipReason::Chained);
                }
            }
            _ => return Err(SkipReason::Chained),
        }
        if i > 0 && separator_after(inner, segments[i - 1], segment) == "|" {
            continue; // the reader, checked above
        }
        if let Some(found) = body_segment(text)? {
            // #10973 critic: one filter reads the whole group's stdout, so every
            // covered segment must share its class or output is lost.
            let class = classify_tool(&found);
            if tool.as_ref().is_some_and(|(_, c)| *c != class) {
                return Err(SkipReason::Chained);
            }
            tool = Some((found, class));
        }
    }
    tool.map(|(t, _)| t).ok_or(SkipReason::NoFilter)
}

/// `Some(tool)` for a filter-covered segment, `None` for `true`/`export`.
///
/// Test: `a_safe_chain_is_compressed`, `a_destructive_segment_refuses_the_chain`.
fn body_segment(text: &str) -> Result<Option<String>, SkipReason> {
    let words = shlex_words(text);
    match words.first().map(String::as_str) {
        None => Err(SkipReason::Chained),
        Some("true") if words.len() == 1 => Ok(None),
        Some("export") if words.len() > 1 && words[1..].iter().all(|w| is_env_assignment(w)) => {
            Ok(None)
        }
        Some("cd" | "export" | "true") => Err(SkipReason::Chained),
        Some(_) => {
            // #10973: a destructive segment refuses the whole chain. Judged
            // against `/` so a relative target resolves to the strictest case.
            if !is_safe_program(&words)
                || evaluate_destructive_delete_command(text, Path::new("/")).is_some()
            {
                return Err(SkipReason::Chained);
            }
            compressible_tool_decision(text).map(Some)
        }
    }
}

/// Whether `words` runs a read or a build/test step this module trusts.
///
/// Why (#10973): `compressible_tool_decision` matches filters by substring, so
/// `git checkout` reaches the `check` filter and `catalog-rm` the `cat` one. A
/// chain segment must instead name its program and verb outright.
/// What: `git` with a read verb in [`GIT_READ_VERBS`]; `cargo` with a
/// build/test verb in [`CARGO_VERBS`] (no `--fix`, `fmt` only with
/// `--check`); `go test|build|vet`; `npm|pnpm|yarn test`; `pytest`; and the
/// readers `ls`, `grep`, `rg`, `cat`, `diff`, `find` (no action primary).
/// No argument may start with `--output`; no `-O`/`--pre` program option.
/// Test: `a_destructive_segment_refuses_the_chain`.
fn is_safe_program(words: &[String]) -> bool {
    let verb = words.get(1).map(String::as_str).unwrap_or("");
    let has = |flag: &str| words.iter().any(|w| w == flag);
    if words.iter().any(|w| w.starts_with("--output")) {
        return false;
    }
    // #10973 critic: options that run another program or a pager.
    let runs_other = |w: &String| {
        w.starts_with("-O") || w.starts_with("--open-files-in-pager") || w.starts_with("--pre")
    };
    match words[0].as_str() {
        "git" => GIT_READ_VERBS.contains(&verb) && !words.iter().any(runs_other),
        "rg" => !words.iter().any(runs_other),
        "cargo" => {
            CARGO_VERBS.contains(&verb) && !has("--fix") && (verb != "fmt" || has("--check"))
        }
        "go" => matches!(verb, "test" | "build" | "vet"),
        "npm" | "pnpm" | "yarn" => verb == "test",
        "pytest" | "ls" | "grep" | "cat" | "diff" => true,
        "find" => !words.iter().any(|w| FIND_ACTIONS.contains(&w.as_str())),
        _ => false,
    }
}

/// `git` verbs that only read the repository.
const GIT_READ_VERBS: &[&str] = &[
    "diff",
    "log",
    "show",
    "status",
    "blame",
    "grep",
    "ls-files",
    "rev-parse",
    "shortlog",
    "describe",
];

/// `cargo` verbs that build, lint or test without installing or publishing.
const CARGO_VERBS: &[&str] = &[
    "build", "check", "test", "clippy", "nextest", "doc", "bench", "tree", "metadata", "fmt",
];

/// Whether `inner` holds a shape this module never judges (module doc).
fn has_ambiguous_shape(inner: &str) -> bool {
    let banned = ['\n', '\r', '`', '(', ')', '{', '}', '<', '>'];
    inner.contains(banned)
        || inner.contains("||")
        || inner.contains("|&")
        || inner.replace("&&", "").contains('&')
        || shlex::split(inner).is_none()
}

/// The operator text between two segments `split_shell_segments_raw` cut.
fn separator_after<'a>(inner: &'a str, segment: &str, next: &str) -> &'a str {
    let start = segment.as_ptr() as usize - inner.as_ptr() as usize + segment.len();
    let end = next.as_ptr() as usize - inner.as_ptr() as usize;
    inner.get(start..end).unwrap_or("").trim()
}

/// A trailing pipe stage that only reads stdin (no `sort -o`, no wrapper).
///
/// Test: `a_destructive_segment_refuses_the_chain`, `a_safe_chain_is_compressed`.
fn is_reader(text: &str) -> bool {
    let words = shlex_words(text);
    let Some(program) = words.first() else {
        return false;
    };
    let writes = words.iter().any(|w| {
        w.starts_with("-o")
            || w.starts_with("--output")
            || w.starts_with("--files0")
            || w.starts_with("--compress-program")
    });
    // #10973 critic: `uniq IN OUT` writes OUT.
    let uniq_output =
        program == "uniq" && words[1..].iter().filter(|w| !w.starts_with('-')).count() > 1;
    READERS.contains(&program.as_str()) && !writes && !uniq_output
}

#[cfg(test)]
#[path = "hook_rewrite_chain_tests.rs"]
mod tests;
