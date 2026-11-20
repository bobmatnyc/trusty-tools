//! The chained forms the compression rewrite accepts: `a && b`, `a; b`,
//! `cd X; a`, `… | head`.
//!
//! Why (#10973, #11037): `hook_rewrite::has_unsafe_pipe_composition` refused
//! every `&`, `;` and `|` by substring, so `cargo build && cargo test`,
//! `git diff; git log -3` and `cargo test | tail -40` — 26-39% of Bash calls in
//! the 11-13 tm-dogfood data — were never compressed. #11507: one filter per
//! chain still refused mixed-class chains and `cd X;`, and the status reached
//! the caller as 0.
//! What: [`chain_rewrite`] judges each segment, never the raw string, from the
//! PM guard's own splitter (`pm_guard_bash::split_shell_segments_raw`). It
//! accepts only `&&` and `;` separators plus one optional trailing `|` into a
//! reader in [`READERS`]. Each step is covered (`compressible_tool_decision`
//! plus an allowlisted program and verb, `is_safe_program`) or inert (`true`,
//! `export NAME=value`, `echo`, `date`); anything
//! else refuses. The original text is kept byte for byte, and a `printf`
//! marker step is inserted after each separator (`a && printf M && b`,
//! `a; printf M; b`), so `&&` still short-circuits, `;` still continues and the
//! status is the last step's; `tm compress --chain` (`compress_chain`) cuts
//! the stream at the markers and filters each step with its own tool.
//! Anything ambiguous fails closed as `Chained`: `||`, a background `&`,
//! `$( )`, backticks, `( )`, braces, any `<` or `>` (heredocs included), a
//! line break, unlexable quoting, a destructive segment, and any `cd` but the
//! one leading `cd <dir>; ` or `cd <dir> && ` (`hook_rewrite_cd`), both kept
//! in front of the group so the caller's cwd moves as in the original. An isolation worktree still refuses every
//! chain: its no-shell `tm compress -- <cmd>` shape (#7477) cannot carry one.
//! Test: `a_safe_chain_is_compressed`, `a_destructive_segment_refuses_the_chain`,
//! `an_ambiguous_shape_is_not_compressed`,
//! `an_isolation_worktree_refuses_every_chain`,
//! `a_chain_rewrite_runs_like_the_original_in_a_real_shell`.

use std::path::Path;

use super::compress_chain::chain_marker;
use super::hook_rewrite::{compressible_tool_decision, is_env_assignment, shlex_words};
use super::pm_guard_bash::{evaluate_destructive_delete_command, split_shell_segments_raw};
use trusty_mpm::core::savings_opportunity::SkipReason;

/// The programs a trailing `|` may feed; each only reads stdin and prints.
pub(crate) const READERS: &[&str] = &["head", "tail", "wc", "grep", "sort", "uniq", "cut", "tr"];

/// `find` primaries that act on what they find instead of printing it.
const FIND_ACTIONS: &[&str] = &[
    "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
];

/// The chain rewrite for `inner`, behind `cd_prefix`, or why there is none.
///
/// Why: see the module doc.
/// What: [`chain_rewrite_with_nonce`] with a fresh per-call nonce.
/// Test: `a_safe_chain_is_compressed`.
pub(crate) fn chain_rewrite(cd_prefix: &str, inner: &str) -> Result<String, SkipReason> {
    use std::hash::{BuildHasher, Hasher};
    // #11507: a random per-call nonce, so no real output carries a marker.
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    let nonce = format!("{:08x}", hasher.finish() as u32);
    chain_rewrite_with_nonce(cd_prefix, inner, &nonce)
}

/// [`chain_rewrite`] with the marker nonce supplied.
///
/// What: `Err(Chained)` for any ambiguous shape (module doc), a non-leading
/// `cd`, a pipe that is not the last separator, more than one pipe, or a pipe
/// into a non-reader. `;` is refused behind a `cd <dir> && ` prefix
/// (`cd x && a; b` parses as `(cd x && a); b`, which the group cannot mirror).
/// A refused step refuses the chain with its reason; a chain with no covered
/// step is `NoFilter`. `Ok` is `<cd_prefix>{ <chain with markers>; <exit
/// sentinel>; } | tm compress --tool "<spec>" --chain <nonce>`, where the spec
/// (`compress_chain::parse_chain_spec`) lists each step's tool, or `=<program>`
/// for an inert step, joined by the chain's own separators.
/// Test: `a_safe_chain_is_compressed`, `a_destructive_segment_refuses_the_chain`,
/// `an_ambiguous_shape_is_not_compressed`.
pub(crate) fn chain_rewrite_with_nonce(
    cd_prefix: &str,
    inner: &str,
    nonce: &str,
) -> Result<String, SkipReason> {
    if has_ambiguous_shape(inner) {
        return Err(SkipReason::Chained);
    }
    // #11507 (Architect, 07:45Z): a leading `cd <dir>;` runs in front of the
    // group, so the caller's cwd moves as in the original and a failed `cd`
    // still lets the rest run, as `;` does.
    let (cd_prefix, inner, after_semicolon_cd) = match split_leading_cd_semicolon(inner) {
        Some((prefix, rest)) if cd_prefix.is_empty() => (prefix, rest, true),
        _ => (cd_prefix, inner, false),
    };
    let allow_semicolon = cd_prefix.is_empty() || after_semicolon_cd;
    let segments = split_shell_segments_raw(inner);
    let (mut body, mut spec, mut pos, mut step, mut covered) =
        (String::new(), String::new(), 0, 0, false);
    for (i, segment) in segments.iter().enumerate() {
        let text = segment.trim();
        let separator = segments
            .get(i + 1)
            .map(|next| separator_after(inner, segment, next));
        match separator {
            Some("&&") | None => {}
            Some(";") if allow_semicolon => {}
            Some("|") if i + 2 == segments.len() => {
                if !is_reader(segments[i + 1].trim()) {
                    return Err(SkipReason::Chained);
                }
            }
            _ => return Err(SkipReason::Chained),
        }
        if i > 0 {
            let before = separator_after(inner, segments[i - 1], segment);
            if before == "|" {
                // The reader, checked above; it belongs to the step before it.
                let reader = shlex_words(text).into_iter().next().unwrap_or_default();
                spec.push_str(&format!(" | {reader}"));
                continue;
            }
            // #11507: a marker step after the separator, the original kept.
            step += 1;
            let at = text.as_ptr() as usize - inner.as_ptr() as usize;
            let sep = if before == "&&" { " &&" } else { ";" };
            body.push_str(&inner[pos..at]);
            body.push_str(&format!(
                "printf '\\n{}\\n'{sep} ",
                chain_marker(nonce, step)
            ));
            pos = at;
            spec.push_str(if before == "&&" { " && " } else { " ; " });
        }
        match step_tool(text)? {
            Step::Covered(tool) => {
                covered = true;
                spec.push_str(&tool);
            }
            Step::Inert(program) => spec.push_str(&format!("={program}")),
        }
    }
    if !covered {
        return Err(SkipReason::NoFilter);
    }
    body.push_str(&inner[pos..]);
    Ok(format!(
        "{cd_prefix}{} | tm compress --tool \"{spec}\" --chain {nonce}",
        crate::commands::compress::wrap_command_reporting_exit(&body)
    ))
}

/// One step's class; any other step refuses the chain.
enum Step {
    /// Filter-covered, with its `tm compress` tool name.
    Covered(String),
    /// Prints nothing to compress, or only a line; passed through as-is.
    Inert(String),
}

/// `inner` split into its leading `cd <dir>; ` and the rest, when it has one.
///
/// What: the first segment is exactly `cd <dir>` in `hook_rewrite_cd`'s DIR
/// grammar (no `-`, `$`, glob or operator) and the separator after it is `;`.
/// Test: `a_cd_semicolon_chain_leaves_the_same_cwd_in_a_real_shell`.
fn split_leading_cd_semicolon(inner: &str) -> Option<(&str, &str)> {
    let segments = split_shell_segments_raw(inner);
    let (first, next) = (segments.first()?, segments.get(1)?);
    if separator_after(inner, first, next) != ";" || !super::hook_rewrite_cd::is_plain_cd(first) {
        return None;
    }
    let at = next.trim_start().as_ptr() as usize - inner.as_ptr() as usize;
    Some((&inner[..at], &inner[at..]))
}

/// Classify one step; any `cd` here refuses the chain.
///
/// Test: `a_safe_chain_is_compressed`, `a_destructive_segment_refuses_the_chain`.
fn step_tool(text: &str) -> Result<Step, SkipReason> {
    let words = shlex_words(text);
    let inert = |w: &[String]| Ok(Step::Inert(w[0].clone()));
    match words.first().map(String::as_str) {
        None => Err(SkipReason::Chained),
        Some("true") if words.len() == 1 => inert(&words),
        Some("export") if words.len() > 1 && words[1..].iter().all(|w| is_env_assignment(w)) => {
            inert(&words)
        }
        Some("echo") => inert(&words),
        // `date -s`/`--set` changes the clock; a format or `-u` only prints.
        Some("date") if words[1..].iter().all(|w| w.starts_with('+') || w == "-u") => inert(&words),
        Some("cd" | "export" | "true" | "date") => Err(SkipReason::Chained),
        Some(_) => {
            // #10973: a destructive segment refuses the whole chain. Judged
            // against `/` so a relative target resolves to the strictest case.
            if !is_safe_program(&words)
                || evaluate_destructive_delete_command(text, Path::new("/")).is_some()
            {
                return Err(SkipReason::Chained);
            }
            compressible_tool_decision(text).map(Step::Covered)
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
pub(crate) fn separator_after<'a>(inner: &'a str, segment: &str, next: &str) -> &'a str {
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
