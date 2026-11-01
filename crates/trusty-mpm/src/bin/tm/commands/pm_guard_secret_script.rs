//! The credential rules applied to the BODY of a script a Bash command runs
//! (#8879, owner rulings 263 and 268).
//!
//! Why: every secret rule judges the text of the Bash command it is invoked
//! for. An agent refused an inline Keychain read wrote the same read into a
//! script file and ran it as `bash <file>` about 51 times; apex #2782 signed
//! with a private key the same way. The executing command never named the
//! credential, so no rule saw it. Ruling 268: a script an agent runs gets the
//! same credential-read refusal as the inline command.
//! What: [`evaluate_script_body_secret_read`] finds every script a segment
//! runs — an interpreter (`bash`, `sh`, `zsh`, `dash`, `ksh`, `source`, `.`,
//! `python*`, `node`, `ruby`, `perl`) given a script operand, a file it loads
//! as code (`node -r`, `ruby -r`, `bash --rcfile`), or its stdin redirected
//! from a file, and a first word that is a path to a file — in the segment
//! and in each `$( … )` or backtick body it carries (#10566). It reads the file
//! (see [`read_script`]), blanks its full-line comments, joins its line
//! continuations and rewrites it as the inline equivalent — a shell body is
//! itself the command text; any other body is handed to its interpreter as a
//! quoted here-document — then runs the inline judges over that text: the
//! credential-print rule, the #7266 file rule (confirmed by a literal secret
//! name, see [`judge_text`]), the nested dump rules and the launchd plist rule. Any of them denying
//! refuses the command. A shell script's own script runs are followed to
//! [`MAX_SCRIPT_DEPTH`]. Relative paths resolve against the hook cwd and every
//! directory a `cd` before the run resolves to; a file found in more than one
//! of them is judged in each. The refusal names the script path as written in
//! the command and the rule class, never a word of the body.
//!
//! Scope: this judges a script's body whoever wrote it. The hook cannot tell
//! which files an agent wrote this session, so a script it did not write is
//! judged the same way.
//!
//! Fail closed: a script whose body the guard cannot judge in full refuses
//! ([`Unread`]) — an open or read error (a symlink loop among them), not a
//! regular file, over the 256 KiB `MAX_SCRIPT_BYTES`, or not UTF-8 text; an
//! interpreter's script or loaded file computed at run time (`bash "$S"`,
//! `bash <(…)`, a glob); a literal script that does not exist and that
//! nothing else in the command names; a script run past
//! [`MAX_SCRIPT_DEPTH`]; and substitutions nested past
//! `MAX_SUBSTITUTION_DEPTH` (#10566). Nested runs are resolved as strictly as the top
//! level (#9037). A symlinked script is judged by its target. `$HOME` and
//! `$PWD` prefixes resolve. A compiled executable (ELF or Mach-O magic) is
//! not a script and allows.
//!
//! Documented residuals (ruling 268's accepted trade, pinned in
//! `pm_guard_secret_read`'s `DOCUMENTED_RESIDUALS` and
//! `the_documented_residuals_allow`). Each is allowed:
//! 1. A script an earlier stage of the same command writes (`printf … > s.sh;
//!    bash s.sh`): it does not exist when the hook runs, and the writing
//!    stage's own text is judged by the inline rules instead.
//! 2. A path run directly whose word is computed (`"$DIR"/tool`) — how every
//!    built binary runs; a script reached by `PATH` lookup (`deploy.sh` with
//!    no `/`); and a relative script after a `cd` the hook cannot resolve.
//! 3. Scripts a non-shell body runs (a Python `subprocess` call): only a
//!    shell body's own script runs are followed.
//! 4. A script run by a program not listed above (`xargs`, `make`, `find
//!    -exec`), by an interpreter option this module does not read, or behind
//!    a wrapper that takes options (`sudo -u x bash s.sh`, judged only when
//!    the file exists). #10672: the word after a `node` or `ruby` long option
//!    this module does not list may be its value, so the first such word that
//!    exists is judged as the script, even when it is data.
//! 5. A body changed between this check and the run.
//! 6. A compiled executable's own behaviour.
//! 7. A `#`-leading line inside a multi-line double-quoted string that runs a
//!    backtick substitution: full-line comments are blanked before judging.
//! 8. A secret DELETE (#8875) inside a script: the body is judged by the
//!    credential-READ rules only (ruling 268's scope).
//! 9. A secret file a body names only through a bracket or brace shape
//!    (`cat .en[v]`, `cat {.env,}`): the file rule fails closed on every such
//!    shape, and shell programs carry them as regexes and awk code, so a body
//!    refuses only on a secret name written without one ([`judge_text`]).
//! 10. Inside a script body, a substitution that runs a script whose literal
//!     word names no file (#10566): prose and here-documents quote code in
//!     backticks. A computed, `/dev/fd/` or resolved-variable word refuses.
//!
//! Fail-open check: this rule only ADDS refusals and runs after the inline
//! rules, which still judge the command text. Every [`Unread`] arm refuses.
//! Test: `pm_guard_secret_script_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_secret_batch.rs`
//! (`pm_guard_refuses_a_script_whose_body_reads_a_credential_8879`).

use std::path::{Path, PathBuf};

use crate::commands::pm_guard_bash::build_lease_cwd::{BuildDir, build_dir};
use crate::commands::pm_guard_bash::evaluate_credential_print_command;
use crate::commands::pm_guard_secret_env_files::evaluate_env_plist_read;
use crate::commands::pm_guard_secret_nested::evaluate_nested_secret_read_rules;
use crate::commands::pm_guard_secret_read::{
    Scan, command_basename, evaluate_secret_file_read_command, secret_files_named_in,
};
// #8879: the bounded, fail-closed read lives in its own file (500-SLOC cap).
// #10548: so do the rule classes and the refusal text.
pub(crate) use crate::commands::pm_guard_secret_script_read::{
    Body, Rule, Unread, deny_reason, read_script,
};
#[cfg(test)]
use crate::commands::pm_guard_secret_script_self::resolve_self_paths;
use crate::commands::pm_guard_secret_script_self::{
    ToplevelRisk, passes_on_git_changes, resolve_self_paths_with,
};
use operands::{Candidate, Family, Role, candidates, family_of};
use walk::{Walk, each_script, each_script_in};

// #10672: which words name the program a segment runs (500-SLOC cap).
#[path = "pm_guard_secret_script_operands.rs"]
mod operands;

// #10566: the walk that locates each script, split out for the 500-SLOC cap.
#[path = "pm_guard_secret_script_walk.rs"]
mod walk;

/// Script-runs-script nesting the guard follows.
pub(crate) const MAX_SCRIPT_DEPTH: usize = 4;

/// Refuse a Bash command that runs a script whose body reads a credential:
/// `Some(reason)` denies, `None` allows.
///
/// Why: see the module doc — ruling 268 makes a script's credential read
/// refuse exactly as the inline read does.
/// What: every candidate script of every segment is located against the
/// hook cwd and each resolvable `cd`, read, rewritten as its inline
/// equivalent and judged ([`judge_script`]).
/// Test: `refuses_a_keychain_read_in_a_bash_script`,
/// `refuses_a_secret_file_read_in_a_python_script`,
/// `refuses_a_script_run_by_its_path`,
/// `allows_a_script_that_reads_no_credential`,
/// `every_unread_class_refuses_8879`,
/// `an_unresolvable_or_missing_script_refuses_8879`,
/// `the_documented_residuals_allow`.
pub(crate) fn evaluate_script_body_secret_read(command: &str, cwd: &Path) -> Option<String> {
    let mut seen = Vec::new();
    let bases = [cwd.to_path_buf()];
    // #10622: an env prefix or export on the invoking command reaches every
    // script it runs; the Bash tool's shell may be zsh, so `path` counts.
    let git_env = passes_on_git_changes(command, true);
    each_script(command, &bases, |word, found, interpreter, seg_bases| {
        let rule = match found {
            Ok(path) => judge_script(&path, interpreter, seg_bases, (0, git_env), &mut seen),
            Err(why) => Some((Rule::Unread(why), None)),
        };
        rule.map(|(rule, inner)| deny_reason(word, inner.as_deref(), rule))
    })
}

/// The script word a segment runs but the guard cannot resolve to a body:
/// an interpreter's script or loaded file computed at run time
/// ([`Unread::Unresolvable`]), or a literal script that does not exist and
/// that nothing else in `command` names, so no earlier stage writes it
/// ([`Unread::Missing`]). A path run directly whose word is computed stays a
/// documented residual: that is how every built binary is run.
fn unresolved(
    candidates: &[Candidate],
    bases: &[PathBuf],
    command: &str,
) -> Option<(String, Unread)> {
    let computed = |c: &Candidate| is_computed(&expand(&c.word, bases));
    if let Some(c) = candidates
        .iter()
        .find(|c| c.role == Role::Loaded && computed(c))
    {
        return Some((c.word.clone(), Unread::Unresolvable));
    }
    let script = candidates
        .iter()
        .find(|c| c.role == Role::Positional)
        .or_else(|| candidates.iter().find(|c| c.role == Role::Stdin))?;
    if computed(script) {
        let interpreted = script.interpreter.is_some();
        return interpreted.then(|| (script.word.clone(), Unread::Unresolvable));
    }
    let any_exists = candidates
        .iter()
        .filter(|c| c.role != Role::Loaded)
        .any(|c| !existing_paths(&c.word, bases).is_empty());
    let named_once = command.matches(script.word.as_str()).count() <= 1;
    (!any_exists && named_once).then(|| (script.word.clone(), Unread::Missing))
}

/// Whether the shell computes `word` at run time: a variable, a substitution
/// or a glob.
fn is_computed(word: &str) -> bool {
    word.contains(['$', '`', '*', '?', '[', '{', '('])
}

/// `word` with a leading `$HOME`/`${HOME}` or `$PWD`/`${PWD}` replaced by the
/// home directory or the first base; any other word unchanged.
fn expand(word: &str, bases: &[PathBuf]) -> String {
    let swap = |dir: Option<&Path>, names: [&str; 2]| {
        let dir = dir?;
        names.iter().find_map(|name| {
            let rest = word.strip_prefix(name)?;
            (rest.is_empty() || rest.starts_with('/')).then(|| format!("{}{rest}", dir.display()))
        })
    };
    let home = dirs::home_dir();
    swap(home.as_deref(), ["$HOME", "${HOME}"])
        .or_else(|| swap(bases.first().map(PathBuf::as_path), ["$PWD", "${PWD}"]))
        .unwrap_or_else(|| word.to_string())
}

/// Judge one script: its inline equivalent, then (for a shell body) the
/// scripts it runs in turn, to [`MAX_SCRIPT_DEPTH`].
///
/// #9037: a nested run is resolved strictly — an unresolvable or missing
/// nested script refuses — after its self-location idioms resolve
/// ([`resolve_self_paths_with`]); a script past the bound refuses
/// ([`Unread::TooDeep`]) unless it is a compiled executable. `(depth,
/// git_env)`: `git_env` says a parent may have changed git's environment
/// (#10622), which every nested script inherits.
/// Test: `refuses_a_read_one_script_deeper`,
/// `a_nested_unresolvable_script_refuses_9037`,
/// `a_chain_past_the_depth_bound_refuses_9037`,
/// `a_toplevel_inherits_the_invoking_git_environment_10622`.
fn judge_script(
    path: &Path,
    interpreter: Option<&str>,
    bases: &[PathBuf],
    (depth, git_env): (usize, bool),
    seen: &mut Vec<PathBuf>,
) -> Option<(Rule, Option<String>)> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&key) {
        return None;
    }
    seen.push(key);
    // #8879: a body that cannot be read in full refuses; a compiled program is no script.
    let body = match read_script(path) {
        Ok(Body::Script(body)) if depth < MAX_SCRIPT_DEPTH => body,
        // #9037: a script run past the nesting bound is not followed; it refuses.
        Ok(Body::Script(_)) => return Some((Rule::Unread(Unread::TooDeep), None)),
        Ok(Body::Executable) => return None,
        Err(why) => return Some((Rule::Unread(why), None)),
    };
    let interpreter = interpreter
        .map(str::to_string)
        .unwrap_or_else(|| shebang_interpreter(&body));
    let cwd = bases.first().map_or(Path::new("/"), PathBuf::as_path);
    let plain = without_comment_lines(&body);
    if let Some(rule) = judge_text(&inline_equivalent(&interpreter, &plain), &plain, cwd) {
        return Some((rule, None));
    }
    if family_of(&interpreter) != Some(Family::Shell) {
        return None;
    }
    // #10622: read once per body; a nested script inherits what this one sets.
    let risk = ToplevelRisk::scan(&body, git_env, interpreter.contains("zsh"));
    let git_env = risk.passes_on();
    // #9037: nested runs resolve as strictly as the top level.
    let nested = resolve_self_paths_with(&body, path, risk);
    // #10548: a nested refusal names the innermost word it could not judge.
    let mut refuse = |word: &str,
                      inner: Result<PathBuf, Unread>,
                      inner_interp: Option<&str>,
                      inner_bases: &[PathBuf]| {
        let refused = match inner {
            Ok(inner) => judge_script(
                &inner,
                inner_interp,
                inner_bases,
                (depth + 1, git_env),
                seen,
            ),
            Err(why) => Some((Rule::Unread(why), None)),
        };
        refused.map(|(rule, named)| (rule, named.or_else(|| Some(word.to_string()))))
    };
    let own = Walk {
        root: &nested,
        written: &body,
        strict: true,
        own: true,
        subs: false,
    };
    // #10566: a body's substitutions are read with comment lines blanked and
    // leniently — a literal script word that is missing allows — so a
    // backtick pair quoted in prose or in a here-document the guard cannot
    // split is not a run (residual 10).
    let substituted = resolve_self_paths_with(&blank_comment_lines(&body, true), path, risk);
    let subs = Walk {
        strict: false,
        own: false,
        subs: true,
        ..own
    };
    // #10566 round 2: substitutions first, so a body's `$(bash "$B")` is
    // named as the computed word it was written as.
    each_script_in(&substituted, bases, subs, 0, &mut refuse)
        .or_else(|| each_script_in(&nested, bases, own, 0, &mut refuse))
}

/// Run the inline credential judges over `command`, the inline spelling of
/// running `body`.
///
/// #8879: the #7266 file rule fails closed on a bracket or brace shape it
/// cannot resolve (`git grep -E '(fn|mod)[[:space:]]+…'`, awk's `{print`), and
/// a shell program is full of them; it refused this repository's own gate
/// scripts. A file-rule denial therefore stands only when `body`, read as
/// program text, names a secret-bearing file by a word with no bracket or
/// brace (`.env`, `keys/app.pem`, `.env*`). Residual 9 in the module doc.
/// Test: `refuses_a_secret_file_read_in_a_python_script`,
/// `the_repo_gate_scripts_allow_8879`.
fn judge_text(command: &str, body: &str, cwd: &Path) -> Option<Rule> {
    if evaluate_credential_print_command(command).is_some() {
        return Some(Rule::CredentialCommand);
    }
    if evaluate_secret_file_read_command(command).is_some()
        && secret_files_named_in(body, Scan::ProgramText)
            .iter()
            .any(|word| !word.contains(['[', ']', '{', '}']))
    {
        return Some(Rule::SecretFile);
    }
    if evaluate_nested_secret_read_rules(command).is_some() {
        return Some(Rule::NestedOrDump);
    }
    let input = serde_json::json!({ "command": command });
    evaluate_env_plist_read("Bash", Some(&input), cwd).map(|_| Rule::LaunchdPlist)
}

/// `body` with each full-line `#` comment blanked, so an apostrophe in prose
/// (`# the crate's gate`) cannot unbalance the quoting the inline rules read,
/// and each backslash-newline joined as the shell joins it, so a continued
/// line lexes as the one command it is.
/// A `#` line carrying `$(` stays: inside a multi-line double-quoted string
/// it runs. A backtick does not keep a line (prose quotes code in backticks);
/// that is residual 7 in the module doc.
/// Test: `without_comment_lines_keeps_a_substitution_8879`,
/// `the_repo_gate_scripts_allow_8879`.
fn without_comment_lines(body: &str) -> String {
    blank_comment_lines(body, false).replace("\\\n", " ")
}

/// `body` with each full-line `#` comment blanked (see
/// [`without_comment_lines`]) and every other byte kept, so a here-document's
/// delimiter line still closes it (#10566). With `keep_backticks`, a line
/// carrying a backtick stays as well: inside a multi-line double-quoted
/// string it runs, and the substitution walk blanks real comments itself.
fn blank_comment_lines(body: &str, keep_backticks: bool) -> String {
    body.split_inclusive('\n')
        .map(|line| {
            let runs = line.contains("$(") || (keep_backticks && line.contains('`'));
            let comment = line.trim_start().starts_with('#') && !runs;
            if comment {
                if line.ends_with('\n') { "\n" } else { "" }
            } else {
                line
            }
        })
        .collect()
}

/// The inline spelling of running the script: a shell body is itself the
/// command text; any other body is handed to `interpreter` as a quoted
/// here-document.
// #8879: a shell body read as here-document text is split on a `|` inside a
// quoted regex (`git grep -E '(fn|mod)…'`), which refused this repo's gates.
pub(crate) fn inline_equivalent(interpreter: &str, body: &str) -> String {
    if family_of(interpreter) == Some(Family::Shell) {
        return body.to_string();
    }
    let mut delimiter = String::from("TM_PM_GUARD_SCRIPT_BODY");
    while body.lines().any(|line| line.trim() == delimiter) {
        delimiter.push('_');
    }
    format!("{interpreter} <<'{delimiter}'\n{body}\n{delimiter}\n")
}

/// The interpreter a body's `#!` line names, or `sh` (what the kernel's
/// no-shebang fallback runs).
fn shebang_interpreter(body: &str) -> String {
    let Some(line) = body.lines().next().and_then(|l| l.strip_prefix("#!")) else {
        return "sh".to_string();
    };
    let mut words = line.split_whitespace().map(command_basename);
    let mut program = words.next().unwrap_or_default();
    if program == "env" {
        program = words
            .find(|w| !w.starts_with('-') && !w.contains('='))
            .unwrap_or_default();
    }
    let plain = |c: char| c.is_ascii_alphanumeric() || "._-".contains(c);
    if program.is_empty() || !program.chars().all(plain) {
        "sh".to_string()
    } else {
        program
    }
}

/// The existing files the candidates name: every loaded file, the first
/// positional operand that exists, and the stdin file when no script is named.
fn locate(candidates: &[Candidate], bases: &[PathBuf]) -> Vec<(String, PathBuf, Option<String>)> {
    let mut out = Vec::new();
    for role in [Role::Loaded, Role::Positional, Role::Stdin] {
        for c in candidates.iter().filter(|c| c.role == role) {
            let paths = existing_paths(&c.word, bases);
            if paths.is_empty() {
                continue;
            }
            let interpreter = c.interpreter.clone();
            out.extend(
                paths
                    .into_iter()
                    .map(|p| (c.word.clone(), p, interpreter.clone())),
            );
            if role == Role::Positional {
                break;
            }
        }
    }
    out
}

/// Every existing file `word` names under `bases`; nothing for a word the
/// shell computes at run time.
fn existing_paths(word: &str, bases: &[PathBuf]) -> Vec<PathBuf> {
    let word = expand(word, bases);
    let word = word.as_str();
    if word.is_empty() || is_computed(word) {
        return Vec::new();
    }
    let mut paths = Vec::new();
    if let Some(rest) = word.strip_prefix("~/") {
        paths.extend(dirs::home_dir().map(|h| h.join(rest)));
    } else if Path::new(word).is_absolute() {
        paths.push(PathBuf::from(word));
    } else {
        paths.extend(bases.iter().map(|b| b.join(word)));
    }
    // #8879: any existing entry is a candidate; `read_script` refuses what it cannot judge.
    paths.retain(|p| std::fs::symlink_metadata(p).is_ok());
    paths.dedup();
    paths
}

/// The hook's base directories plus every directory a `cd` before this
/// segment resolves to (`build_dir`); an unresolvable `cd` adds none.
fn segment_bases(command: &str, segment: &str, bases: &[PathBuf]) -> Vec<PathBuf> {
    let home = dirs::home_dir();
    let mut out = bases.to_vec();
    let mut offsets: Vec<usize> = command.match_indices(segment).map(|(i, _)| i).collect();
    offsets.push(command.len());
    for base in bases {
        for &at in &offsets {
            if let BuildDir::Pinned(dir) | BuildDir::Expected(dir) =
                build_dir(command, at, Some(base), home.as_deref())
                && !out.contains(&dir)
            {
                out.push(dir);
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "pm_guard_secret_script_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pm_guard_secret_fp_10548_tests.rs"]
mod fp_10548_tests;

#[cfg(test)]
#[path = "pm_guard_secret_script_10566_tests.rs"]
mod substitution_10566_tests;
