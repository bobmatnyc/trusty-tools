//! `tm hook --pm-guard` — no tool call reads a kuzu-memory store, the kuzu
//! import venv, or the import bridge's temp export (#8625 PR-2).
//!
//! Why: issue #8625 closure condition 2 — "pm-guard rule denies model reads of
//! kuzu stores, the bridge temp export, and the import venv". Architect ruling
//! 2026-10-12 15:42Z removed the `trusty-memory import kuzu` exception; #9794
//! restored a narrowed one (`pm_guard_kuzu_exception`). The reviewer reads
//! only the masked `~/.trusty-tools/kuzu-review/<run>/masked.jsonl`, which
//! this rule leaves alone. Ruling B10: the deny "holds under `TRUSTY_MPM_UNRESTRICTED` and
//! `DISABLE_HOOKS`, and for the Architect" — so it runs in step 1 of
//! `pm_guard_architect_envfile::evaluate_secret_file_read_gated`, the one
//! decision both the guarded path and the bypass floor call, ahead of the
//! Architect's env-file exemption. Ruling Q7: its audit label is
//! [`KUZU_STORE_RULE`].
//! What: a Bash command naming both `trusty-memory` and `kuzu` outside a
//! quoted text slot denies first (owner ruling D5b,
//! `pm_guard_kuzu_import::trusty_memory_kuzu_text`), unless it is one of the
//! #9794 dry-run or review forms; a `--python` there denies on its own
//! (label `kuzu-import-python`). Then, like the #7266
//! rule, the NAME decides, not the verb: a Bash segment denies when any word
//! names a deny item (`pm_guard_kuzu_names`) or when it runs `kuzu-memory`
//! (`pm_guard_kuzu_import`). The only exemptions are a metadata verb
//! ([`KUZU_SAFE_VERBS`]) and the words the #7266 rule also reads as text (a
//! search pattern, a `--body` payload, prose a lone `echo` writes, a
//! `git commit -m` message). A `Read`/`Edit`/`Write`/`MultiEdit`/`NotebookEdit`
//! path, a `Grep` path or glob, a `Glob` path or pattern, and the hook's cwd
//! are judged as written AND as resolved through symlinks.
//! FAIL-CLOSED: an unlexable segment is scanned raw; a substitution nested
//! past `MAX_SUBSTITUTION_DEPTH` naming `kuzu` denies; a path or cwd whose
//! resolution errors (a symlink loop, a permission error) denies.
//! Residuals, named rather than silently allowed: a store path that reaches a
//! command only through a variable (`cat "$F"`) or a computed name; a Bash
//! word that is a symlink into a store (only tool paths and the cwd are
//! resolved); a script file whose body opens a store; a Python program that
//! builds the path from pieces. The D5b rule's own residuals are listed on
//! #8625 PR-2.
//! Test: `pm_guard_kuzu_store_tests.rs`; end to end under each bypass and for
//! the Architect in `tests/tm_hook_pm_guard_kuzu_store_8625.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::commands::pm_guard_bash::{
    MAX_SUBSTITUTION_DEPTH, command_substitutions, git_argv_at_subcommand, prose_write_indices,
    split_heredoc_bodies, split_shell_segments, tokenize,
};
use crate::commands::pm_guard_kuzu_exception::{ImportException, kuzu_import_exception};
use crate::commands::pm_guard_kuzu_import::{runs_kuzu_memory, trusty_memory_kuzu_text};
use crate::commands::pm_guard_kuzu_names::{kuzu_words_in, program_text_words, word_names_kuzu};
use crate::commands::pm_guard_secret_handling::body_written_to_a_file;
use crate::commands::pm_guard_secret_positions::text_payload_indices;
use crate::commands::pm_guard_secret_read::{
    NESTED_COMMAND_MARKERS, command_basename, inline_program_indices, pattern_argument_index,
};
use crate::commands::program_word::resolve_program_word;

/// The audit label of a kuzu-store deny (ruling Q7).
pub(crate) const KUZU_STORE_RULE: &str = "kuzu-store-read";

/// Programs that may name a deny item: each reports metadata, none prints
/// bytes. Narrower than the #7266 `SAFE_HANDLING_VERBS`: `cd`/`pushd` would
/// put a later `cat *` inside a store, `rm` destroys one, and `file -f` prints
/// every line of the path it is given (#8625).
const KUZU_SAFE_VERBS: &[&str] = &["ls", "stat", "test", "["];

/// The kuzu-store decision for one tool call: `Some(reason)` denies.
///
/// Why: see the module doc; one entry for every tool shape.
/// What: Bash is judged by its cwd, then by the #9794 exception or the D5b
/// text rule, then by [`evaluate_command`]; the file tools by their path
/// fields.
/// Test: `denies_every_tool_shape_naming_a_store`,
/// `denies_bash_reads_of_a_store`, `denies_the_formerly_sanctioned_import`,
/// `denies_a_bash_call_from_inside_a_store_directory`,
/// `the_allow_keeps_the_by_name_store_deny`.
pub(crate) fn evaluate_kuzu_store_access(
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
) -> Option<String> {
    let field = |key| tool_input.and_then(|v| v.get(key)).and_then(Value::as_str);
    let how = format!("the `{tool_name}` tool");
    match tool_name {
        "Bash" => {
            let command = field("command").unwrap_or_default();
            // #9794: the narrowed exception lifts the D5b text rule only.
            let text_rule = || match kuzu_import_exception(command, dirs::home_dir().as_deref()) {
                ImportException::Allow => None,
                ImportException::DenyPython(reason) => Some(reason),
                ImportException::NoMatch => trusty_memory_kuzu_text(command),
            };
            cwd_verdict(cwd)
                .or_else(text_rule)
                .or_else(|| evaluate_command(command, 0))
        }
        "Read" | "Edit" | "MultiEdit" | "Write" => path_verdict(field("file_path")?, cwd, &how),
        "NotebookEdit" => path_verdict(field("notebook_path")?, cwd, &how),
        "Grep" | "Glob" => {
            let glob_key = if tool_name == "Grep" {
                "glob"
            } else {
                "pattern"
            };
            if let Some(glob) = field(glob_key)
                && word_names_kuzu(glob, false)
            {
                return Some(deny_reason(glob, &how));
            }
            match field("path") {
                Some(path) => path_verdict(path, cwd, &how),
                None => cwd_verdict(cwd),
            }
        }
        _ => None,
    }
}

/// The deny a call earns for running from inside a deny item.
fn cwd_verdict(cwd: &Path) -> Option<String> {
    path_verdict(&cwd.to_string_lossy(), cwd, "the working directory")
}

/// The deny for one tool path, judged as written and as resolved.
///
/// What: the path, then the path placed against `cwd`, then [`resolve`]'s
/// answer; a resolution error denies (#8625 Fail-Open Check).
/// Test: `denies_a_read_through_a_symlink_into_a_store`,
/// `denies_when_a_tool_path_cannot_be_resolved`.
fn path_verdict(target: &str, cwd: &Path, how: &str) -> Option<String> {
    let joined = cwd.join(target);
    if word_names_kuzu(target, false) || word_names_kuzu(&joined.to_string_lossy(), false) {
        return Some(deny_reason(target, how));
    }
    match resolve(&joined) {
        Ok(real) if word_names_kuzu(&real.to_string_lossy(), false) => {
            let named = format!("{target} -> {}", real.display());
            Some(deny_reason(&named, how))
        }
        Ok(_) => None,
        Err(err) => Some(format!(
            "{KUZU_STORE_RULE}: {how} names `{target}`, which cannot be resolved ({err}), so \
             it cannot be shown not to reach a kuzu-memory store (#8625)."
        )),
    }
}

/// `path` with every symlink resolved; a missing tail is appended as written.
///
/// What: walks up to the deepest entry that exists (a dangling symlink
/// counts), canonicalises it, and re-appends the missing components. Any
/// error but "not found" is returned, and so is a canonicalisation error.
/// Test: `denies_a_read_through_a_symlink_into_a_store`,
/// `denies_when_a_tool_path_cannot_be_resolved`.
fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        if existing.as_os_str().is_empty() {
            existing = PathBuf::from(".");
            break;
        }
        match std::fs::symlink_metadata(&existing) {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name().map(|n| n.to_os_string()) else {
                    return Err(err);
                };
                missing.push(name);
                existing.pop();
            }
            Err(err) => return Err(err),
        }
    }
    let mut out = std::fs::canonicalize(&existing)?;
    out.extend(missing.iter().rev());
    Ok(out)
}

/// The kuzu-store decision for a Bash command text.
///
/// What: each segment of the argv text ([`segment_verdict`]), then each
/// here-document body as program text unless a lone `cat` only writes it to
/// a file, then each command-substitution body, recursively.
/// Test: `denies_bash_reads_of_a_store`, `denies_running_kuzu_memory`,
/// `denies_a_store_read_beside_an_import_call`.
fn evaluate_command(command: &str, depth: usize) -> Option<String> {
    if depth > MAX_SUBSTITUTION_DEPTH {
        let lower = command.to_ascii_lowercase();
        return (lower.contains("kuzu") || lower.contains("memories.db"))
            .then(|| deny_reason("kuzu", "a substitution nested past the guard's depth bound"));
    }
    let (argv_text, bodies) = split_heredoc_bodies(command);
    let segments = split_shell_segments(&argv_text);
    let live: Vec<&str> = segments
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let lone = live.len() == 1;
    for segment in &live {
        if let Some(reason) = segment_verdict(segment, lone) {
            return Some(reason);
        }
    }
    if !body_written_to_a_file(command) {
        for body in &bodies {
            if let Some(word) = program_text_words(body).first() {
                return Some(deny_reason(word, "a here-document body"));
            }
        }
    }
    command_substitutions(command)
        .iter()
        .find_map(|sub| evaluate_command(sub.text(), depth + 1))
}

/// The kuzu-store decision for one segment.
///
/// What: an unlexable segment is scanned raw. Otherwise the `kuzu-memory`
/// program rule, then every token's deny words minus the exempt positions
/// (module doc); a segment whose deny words are all arguments of a
/// [`KUZU_SAFE_VERBS`] program allows.
/// Test: `denies_an_unlexable_segment_naming_a_store`,
/// `keeps_ordinary_commands_allowed`, `denies_a_store_read_beside_an_import_call`.
fn segment_verdict(segment: &str, lone: bool) -> Option<String> {
    let Ok(argv) = tokenize(segment) else {
        let word = kuzu_words_in(segment, false)
            .into_iter()
            .next()
            .or_else(|| {
                let lower = segment.to_ascii_lowercase();
                lower
                    .contains("kuzu-memory")
                    .then(|| "kuzu-memory".to_string())
            })?;
        return Some(deny_reason(&word, "a command the guard cannot lex"));
    };
    if runs_kuzu_memory(&argv) {
        return Some(format!(
            "{KUZU_STORE_RULE}: running `kuzu-memory` reads its store directly (#8625). The \
             operator runs `trusty-memory import kuzu` outside the guard."
        ));
    }
    let mut skip: Vec<usize> = pattern_argument_index(segment, &argv).into_iter().collect();
    skip.extend(text_payload_indices(segment, &argv));
    skip.extend(commit_message_indices(segment, &argv));
    if lone {
        skip.extend(prose_write_indices(segment, &argv));
    }
    let program_at = inline_program_indices(&argv);
    let mut named: Vec<(usize, String)> = Vec::new();
    for (index, token) in argv.iter().enumerate() {
        if skip.contains(&index) {
            continue;
        }
        let words = if program_at.contains(&index) {
            program_text_words(token)
        } else {
            kuzu_words_in(token, false)
        };
        named.extend(words.into_iter().map(|w| (index, w)));
    }
    let (_, first) = named.first()?;
    if only_handles(segment, &argv, &named) {
        return None;
    }
    let program = argv
        .first()
        .map(|p| command_basename(p))
        .unwrap_or_default();
    Some(deny_reason(first, &format!("the `{program}` command")))
}

/// The `git commit -m <message>` token indices: prose, never a path.
fn commit_message_indices(segment: &str, argv: &[String]) -> Vec<usize> {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return Vec::new();
    }
    let Some((git_argv, at)) = git_argv_at_subcommand(segment) else {
        return Vec::new();
    };
    if git_argv != argv || argv[at] != "commit" {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, token) in argv.iter().enumerate().skip(at + 1) {
        if token == "-m" || token == "--message" {
            out.push(i + 1);
        } else if token.starts_with("-m") || token.starts_with("--message=") {
            out.push(i);
        }
    }
    out
}

/// Whether every deny word is an argument of a [`KUZU_SAFE_VERBS`] program in
/// a segment that runs nothing else.
fn only_handles(segment: &str, argv: &[String], named: &[(usize, String)]) -> bool {
    if NESTED_COMMAND_MARKERS.iter().any(|m| segment.contains(m)) {
        return false;
    }
    let Ok(word) = resolve_program_word(argv) else {
        return false;
    };
    let safe = !word.lookup
        && word.xargs_at.is_none()
        && argv
            .get(word.index)
            .is_some_and(|p| KUZU_SAFE_VERBS.contains(&command_basename(p).as_str()));
    safe && named.iter().all(|(index, _)| *index > word.index)
}

/// The deny text for `target`, named by `how`.
fn deny_reason(target: &str, how: &str) -> String {
    format!(
        "{KUZU_STORE_RULE}: {how} names `{target}`, a kuzu-memory store, the kuzu import venv \
         or the import bridge's temp export (#8625). The operator runs `trusty-memory import \
         kuzu` outside the guard; the reviewer reads only the masked \
         ~/.trusty-tools/kuzu-review/<run>/masked.jsonl."
    )
}

#[cfg(test)]
#[path = "pm_guard_kuzu_store_tests.rs"]
mod tests;
