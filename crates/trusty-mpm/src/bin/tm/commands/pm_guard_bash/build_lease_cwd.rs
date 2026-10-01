//! The directory a leased build is meant to run in (#8969).
//!
//! Why: Claude Code strips a leading `cd <its tracked cwd> &&` from a Bash
//! command and runs the rest in the directory it tracks. On 2026-09-30 an
//! isolated agent's `cd <its worktree> && cargo test` ran, after the lease
//! rewrite, as `tm build-lease -- cargo test` in ANOTHER agent's worktree and
//! exited 0 — green evidence for the wrong tree. The `cd` the rewrite kept never
//! reached the shell, so the lease itself must know the directory.
//! What: [`lease_prefix`] adds `--chdir <dir>` when the command's own literal
//! absolute `cd` chain names the directory, `--expect-cwd <dir>` when the
//! directory is resolved from the hook's cwd (the lease refuses to run anywhere
//! else), nothing when no directory change precedes the build, and refuses a
//! build after a directory change it cannot resolve. It never leaves a `cd`-ed
//! build unchecked.
//! Test: `a_pure_absolute_cd_chain_pins_the_directory`,
//! `a_resolvable_cd_is_expected_not_pinned`, `an_unresolvable_cd_refuses_the_build`,
//! `a_cd_prefixed_build_runs_in_its_directory_when_the_cd_is_lost_8969`.

use std::path::{Component, Path, PathBuf};

use super::build_lease_program::{LEADING_KEYWORDS, Word, words};
use super::heredoc::HeredocBodies;
use super::shell_lex::QuoteScan;
use super::split_shell_segments_raw;
use crate::commands::hook_rewrite::is_env_assignment;

/// Shell builtins that change the working directory.
const DIR_CHANGERS: &[&str] = &["cd", "chdir", "pushd", "popd"];

/// Where a leased build is meant to run.
///
/// Test: `a_resolvable_cd_is_expected_not_pinned`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BuildDir {
    /// No directory change precedes the build: it runs where the shell is.
    Unchanged,
    /// A pure `cd /abs && …` chain names it; the lease runs the build there.
    Pinned(PathBuf),
    /// Resolved against the hook's cwd; the lease refuses to run elsewhere.
    Expected(PathBuf),
    /// A directory change the hook cannot resolve, and why.
    Unresolvable(String),
}

/// The lease invocation for the heavy build at `at` in `command`.
///
/// What: `head` is `'<tm>' build-lease [--wait-secs N]`; the result ends in
/// ` --`. `base` is the hook's cwd, `None` when unknown. `Err` refuses.
/// Test: `a_cd_prefixed_build_runs_in_its_directory_when_the_cd_is_lost_8969`,
/// `an_unresolvable_cd_refuses_the_build`.
pub(crate) fn lease_prefix(
    head: &str,
    command: &str,
    at: usize,
    base: Option<&Path>,
) -> Result<String, String> {
    let (flag, dir) = match build_dir(command, at, base, dirs::home_dir().as_deref()) {
        BuildDir::Unchanged => return Ok(format!("{head} --")),
        BuildDir::Pinned(dir) => ("--chdir", dir),
        BuildDir::Expected(dir) => ("--expect-cwd", dir),
        BuildDir::Unresolvable(why) => return Err(refusal(&why)),
    };
    let quoted = dir
        .to_str()
        .and_then(|d| shlex::try_quote(d).ok())
        .ok_or_else(|| refusal("its directory is not printable as one shell word"))?;
    Ok(format!("{head} {flag} {quoted} --"))
}

/// Resolve the directory the build at `at` in `command` is meant to run in.
///
/// What: collects every `cd`/`chdir`/`pushd`/`popd` before `at` that is a
/// segment's program or opens a `(`/`$(`, keeps those whose subshell is still
/// open at `at`, and folds them over `base` lexically (`..` pops, as a logical
/// `cd` does). [`BuildDir::Pinned`] only for a pure `&&` chain of literal
/// absolute `cd`s; any other resolvable shape is [`BuildDir::Expected`].
/// Test: `a_pure_absolute_cd_chain_pins_the_directory`,
/// `a_resolvable_cd_is_expected_not_pinned`,
/// `a_scoped_or_quoted_cd_leaves_the_directory_unchanged`,
/// `an_unresolvable_cd_refuses_the_build`.
pub(crate) fn build_dir(
    command: &str,
    at: usize,
    base: Option<&Path>,
    home: Option<&Path>,
) -> BuildDir {
    let changes = dir_changes(command, at);
    if changes.is_empty() {
        return BuildDir::Unchanged;
    }
    let scan = QuoteScan::new(command);
    if !scan.balanced {
        return BuildDir::Unresolvable("its quotes do not balance".into());
    }
    let bodies = HeredocBodies::scan(command);
    let mut cur = base.filter(|b| b.is_absolute()).map(Path::to_path_buf);
    let mut stack: Vec<Option<PathBuf>> = Vec::new();
    let mut all_absolute = true;
    let mut applied = 0;
    for (offset, verb, operands) in changes {
        match subshell_still_open(command, &scan, &bodies, offset, at) {
            Some(true) => {}
            Some(false) => continue,
            None => return BuildDir::Unresolvable("its parentheses do not balance".into()),
        }
        applied += 1;
        let target = match verb.as_str() {
            "popd" if operands.is_empty() => match stack.pop() {
                Some(prev) => {
                    all_absolute = false;
                    cur = prev;
                    continue;
                }
                None => return BuildDir::Unresolvable("`popd` pops nothing it pushed".into()),
            },
            "popd" | "pushd" if operands.len() != 1 => {
                return BuildDir::Unresolvable(format!("`{verb}` names no literal directory"));
            }
            _ => match target_of(&operands, home) {
                Ok(target) => target,
                Err(why) => return BuildDir::Unresolvable(why),
            },
        };
        if verb == "pushd" {
            stack.push(cur.clone());
            all_absolute = false;
        }
        all_absolute &= target.is_absolute();
        cur = match (target.is_absolute(), &cur) {
            (true, _) => Some(normalize(&target)),
            (false, Some(dir)) => Some(normalize(&dir.join(&target))),
            (false, None) => {
                return BuildDir::Unresolvable(
                    "a relative `cd` with no known starting directory".into(),
                );
            }
        };
    }
    match cur {
        _ if applied == 0 => BuildDir::Unchanged,
        Some(dir) if all_absolute && is_pure_cd_chain(command, at) => BuildDir::Pinned(dir),
        Some(dir) => BuildDir::Expected(dir),
        None => BuildDir::Unresolvable("its directory cannot be resolved".into()),
    }
}

/// Every directory change before `at`: (offset of the verb, verb, operands).
fn dir_changes(command: &str, at: usize) -> Vec<(usize, String, Vec<String>)> {
    let origin = command.as_ptr() as usize;
    let bodies = HeredocBodies::scan(command);
    let mut out = Vec::new();
    for seg in split_shell_segments_raw(command) {
        let seg_start = seg.as_ptr() as usize - origin;
        let ws = words(seg);
        let program = program_index(&ws);
        for (i, word) in ws.iter().enumerate() {
            let bare = word.text.trim_start_matches(['(', '$', '`']);
            // A verb opening a subshell, or following a `case` label `a)`.
            let opens = bare.len() < word.text.len()
                || i.checked_sub(1).is_some_and(|p| ws[p].text.ends_with(')'));
            let offset = seg_start + word.start + (word.text.len() - bare.len());
            if offset >= at
                || bodies.contains(offset)
                || !DIR_CHANGERS.contains(&bare)
                || !(Some(i) == program || opens)
            {
                continue;
            }
            // The operands end where the subshell around the verb closes.
            let close = ws[i + 1..]
                .iter()
                .position(|w| w.text.ends_with(')'))
                .map_or(ws.len(), |p| i + 2 + p);
            let operands = ws[i + 1..close]
                .iter()
                .map(|w| w.text.trim_end_matches(')').to_string())
                .filter(|w| !w.is_empty() && !w.contains(['<', '>']))
                .collect();
            out.push((offset, bare.to_string(), operands));
        }
    }
    out
}

/// The index of a segment's program word, past keywords, `(`, `KEY=value`,
/// `builtin` and `command`.
fn program_index(ws: &[Word]) -> Option<usize> {
    ws.iter().position(|w| {
        let bare = w.text.trim_start_matches('(');
        !(LEADING_KEYWORDS.contains(&w.text.as_str())
            || bare.is_empty()
            || is_env_assignment(bare)
            || matches!(bare, "builtin" | "command"))
    })
}

/// The directory a `cd`'s operands name; `Err` when it is not a literal.
fn target_of(operands: &[String], home: Option<&Path>) -> Result<PathBuf, String> {
    let mut rest = operands.iter().map(String::as_str);
    let mut operand = None;
    for word in rest.by_ref() {
        match word {
            "-L" | "-P" | "-e" | "-@" => {}
            "--" => {
                operand = rest.next();
                break;
            }
            _ => {
                operand = Some(word);
                break;
            }
        }
    }
    if rest.next().is_some() {
        return Err("its `cd` has more than one operand".into());
    }
    let home_or = |tail: &str| {
        home.map(|h| h.join(tail.trim_start_matches('/')))
            .ok_or_else(|| "its `cd ~` has no home directory to resolve".to_string())
    };
    match operand {
        None | Some("~") => home_or(""),
        Some(op) if op.starts_with("~/") => home_or(&op[2..]),
        Some(op) if op == "-" || op.starts_with(['~', '+', '-']) => {
            Err(format!("`cd {op}` names a directory only the shell knows"))
        }
        Some(op) if op.contains(['$', '`', '*', '?', '[', '{']) => {
            Err(format!("`cd {op}` expands at run time"))
        }
        Some(op) => Ok(PathBuf::from(op)),
    }
}

/// Whether the subshell around `from` is still open at `to`; `None` when a
/// `)` closes more than was opened (a `case` label), so depth is unknowable.
fn subshell_still_open(
    command: &str,
    scan: &QuoteScan,
    bodies: &HeredocBodies,
    from: usize,
    to: usize,
) -> Option<bool> {
    let (mut depth, mut at_from, mut open) = (0i64, 0i64, true);
    for (i, b) in command.bytes().enumerate().take(to) {
        if i == from {
            at_from = depth;
        }
        if scan.is_unquoted(i) && !bodies.contains(i) {
            depth += i64::from(b == b'(') - i64::from(b == b')');
        }
        if depth < 0 {
            return None;
        }
        open &= i < from || depth >= at_from;
    }
    Some(open)
}

/// Whether everything before the build at `at` is `cd <word>` joined by `&&`.
fn is_pure_cd_chain(command: &str, at: usize) -> bool {
    let origin = command.as_ptr() as usize;
    let prefix = &command[..at];
    let unjoined = prefix.replace("&&", " ");
    if unjoined.contains([';', '|', '&', '\n', '(', ')', '`', '$', '<', '>', '{', '}']) {
        return false;
    }
    let segs = split_shell_segments_raw(prefix);
    let build_seg = segs.last().map_or(0, |s| s.as_ptr() as usize - origin);
    segs.iter()
        .filter(|s| (s.as_ptr() as usize - origin) < build_seg)
        .all(|s| {
            let ws = words(s);
            ws.len() == 2 && matches!(ws[0].text.as_str(), "cd" | "chdir")
        })
}

/// Resolve `.` and `..` lexically, as a logical `cd` does.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn refusal(why: &str) -> String {
    format!(
        "Build lease (#8969): this heavy build follows a directory change the hook cannot \
         resolve — {why}. Claude Code can drop a leading `cd`, and the lease must not run cargo \
         in a directory the command did not mean, so it needs the directory up front. Use a \
         literal path (`cd /abs/path && cargo …`), or wrap the build yourself: \
         `cd <dir> && tm build-lease -- <command>`."
    )
}

#[cfg(test)]
#[path = "build_lease_cwd_tests.rs"]
mod tests;
