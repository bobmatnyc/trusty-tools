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
//! from a file, and a first word that is a path to a file. It reads the file
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
//! nothing else in the command names; and a script run past
//! [`MAX_SCRIPT_DEPTH`]. Nested runs are resolved as strictly as the top
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
//!    the file exists).
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
//!
//! Fail-open check: this rule only ADDS refusals and runs after the inline
//! rules, which still judge the command text. Every [`Unread`] arm refuses.
//! Test: `pm_guard_secret_script_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_secret_batch.rs`
//! (`pm_guard_refuses_a_script_whose_body_reads_a_credential_8879`).

use std::path::{Path, PathBuf};

use crate::commands::hook_rewrite::strip_wrapper_prefix;
use crate::commands::pm_guard_bash::build_lease_cwd::{BuildDir, build_dir};
use crate::commands::pm_guard_bash::{
    RedirectRole, evaluate_credential_print_command, input_redirect_operand, redirect_role,
    split_heredoc_bodies, split_shell_segments, tokenize,
};
use crate::commands::pm_guard_secret_env_files::evaluate_env_plist_read;
use crate::commands::pm_guard_secret_nested::evaluate_nested_secret_read_rules;
use crate::commands::pm_guard_secret_read::{
    Scan, command_basename, evaluate_secret_file_read_command, secret_files_named_in,
};
// #8879: the bounded, fail-closed read lives in its own file (500-SLOC cap).
pub(crate) use crate::commands::pm_guard_secret_script_read::{Body, Unread, read_script};
use crate::commands::pm_guard_secret_script_self::resolve_self_paths;

/// Script-runs-script nesting the guard follows.
pub(crate) const MAX_SCRIPT_DEPTH: usize = 4;

/// An interpreter family: which options run inline code, and which take an
/// attached value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Shell,
    Python,
    Node,
    Ruby,
    Perl,
}

impl Family {
    /// Short option letters that make the interpreter run inline code or stdin.
    fn inline_letters(self) -> &'static [char] {
        match self {
            Family::Shell => &['c', 's'],
            Family::Python => &['c', 'm'],
            Family::Node => &['e', 'p'],
            Family::Ruby => &['e'],
            Family::Perl => &['e', 'E'],
        }
    }

    /// Short option letters whose value is the rest of the cluster.
    fn valued_letters(self) -> &'static [char] {
        match self {
            Family::Shell => &['o', 'O'],
            Family::Python => &['W', 'X'],
            Family::Node => &['r'],
            Family::Ruby => &['I', 'r', 'C', 'E', 'F', 'K', 'x', '0'],
            Family::Perl => &['I', 'M', 'm', 'i', 'l', '0', 'C', 'F', 'd', 'D', 'x'],
        }
    }

    /// Options whose separate value is a file run as code.
    fn code_loading_options(self) -> &'static [&'static str] {
        match self {
            Family::Shell => &["--rcfile", "--init-file"],
            Family::Node => &[
                "-r",
                "--require",
                "--import",
                "--loader",
                "--experimental-loader",
            ],
            Family::Ruby => &["-r"],
            Family::Python | Family::Perl => &[],
        }
    }
}

/// The family a program basename belongs to, if it is a listed interpreter.
fn family_of(program: &str) -> Option<Family> {
    let versioned_python = program
        .strip_prefix("python")
        .is_some_and(|v| v.is_empty() || v.chars().all(|c| c.is_ascii_digit() || c == '.'));
    match program {
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "source" | "." => Some(Family::Shell),
        "node" | "nodejs" => Some(Family::Node),
        "ruby" => Some(Family::Ruby),
        "perl" => Some(Family::Perl),
        _ if versioned_python => Some(Family::Python),
        _ => None,
    }
}

/// How a candidate word reaches the interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// A positional operand: the first that exists is the script.
    Positional,
    /// A file an option loads as code, beside the script.
    Loaded,
    /// The interpreter's stdin: the script when no positional one exists.
    Stdin,
}

/// One file a segment may run: the word as written, how it is reached, and
/// the interpreter (`None` for a path run directly, decided by its shebang).
#[derive(Debug)]
struct Candidate {
    word: String,
    role: Role,
    interpreter: Option<String>,
}

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
    each_script(
        command,
        &bases,
        true,
        |word, found, interpreter, seg_bases| {
            let rule = match found {
                Ok(path) => judge_script(&path, interpreter, seg_bases, 0, &mut seen),
                Err(why) => Some(Rule::Unread(why)),
            };
            rule.map(|rule| deny_reason(word, rule))
        },
    )
}

/// Call `judge` on each located script of `command` until one answers. With
/// `strict`, a script the command runs but whose path cannot be resolved is
/// handed over as an [`Unread`] error ([`unresolved`]).
fn each_script<T>(
    command: &str,
    bases: &[PathBuf],
    strict: bool,
    mut judge: impl FnMut(&str, Result<PathBuf, Unread>, Option<&str>, &[PathBuf]) -> Option<T>,
) -> Option<T> {
    let (argv_text, _) = split_heredoc_bodies(command);
    for segment in split_shell_segments(&argv_text) {
        let trimmed = segment.trim();
        let Ok(argv) = tokenize(trimmed) else {
            continue;
        };
        let (candidates, exact) = candidates(&argv);
        if candidates.is_empty() {
            continue;
        }
        let seg_bases = segment_bases(&argv_text, trimmed, bases);
        for (word, path, interpreter) in locate(&candidates, &seg_bases) {
            if let Some(answer) = judge(&word, Ok(path), interpreter.as_deref(), &seg_bases) {
                return Some(answer);
            }
        }
        // #8879: an unresolvable or missing script fails closed at the top level.
        if strict
            && exact
            && let Some((word, why)) = unresolved(&candidates, &seg_bases, command)
            && let Some(answer) = judge(&word, Err(why), None, &seg_bases)
        {
            return Some(answer);
        }
    }
    None
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

/// The rule class that refused a script body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rule {
    /// The credential-print rule (#8596, #8248).
    CredentialCommand,
    /// The secret-bearing file rule (#7266).
    SecretFile,
    /// A nested command or a pod/process environment dump (#7648, #8756).
    NestedOrDump,
    /// A credential-bearing launchd plist (#8523).
    LaunchdPlist,
    /// A body the guard could not read in full; it fails closed (#8879).
    Unread(Unread),
}

/// Judge one script: its inline equivalent, then (for a shell body) the
/// scripts it runs in turn, to [`MAX_SCRIPT_DEPTH`].
///
/// #9037: a nested run is resolved strictly — an unresolvable or missing
/// nested script refuses — after its self-location idioms resolve
/// ([`resolve_self_paths`]); a script past the bound refuses
/// ([`Unread::TooDeep`]) unless it is a compiled executable.
/// Test: `refuses_a_read_one_script_deeper`,
/// `a_nested_unresolvable_script_refuses_9037`,
/// `a_chain_past_the_depth_bound_refuses_9037`.
fn judge_script(
    path: &Path,
    interpreter: Option<&str>,
    bases: &[PathBuf],
    depth: usize,
    seen: &mut Vec<PathBuf>,
) -> Option<Rule> {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.contains(&key) {
        return None;
    }
    seen.push(key);
    // #8879: a body that cannot be read in full refuses; a compiled program is no script.
    let body = match read_script(path) {
        Ok(Body::Script(body)) if depth < MAX_SCRIPT_DEPTH => body,
        // #9037: a script run past the nesting bound is not followed; it refuses.
        Ok(Body::Script(_)) => return Some(Rule::Unread(Unread::TooDeep)),
        Ok(Body::Executable) => return None,
        Err(why) => return Some(Rule::Unread(why)),
    };
    let interpreter = interpreter
        .map(str::to_string)
        .unwrap_or_else(|| shebang_interpreter(&body));
    let cwd = bases.first().map_or(Path::new("/"), PathBuf::as_path);
    let plain = without_comment_lines(&body);
    if let Some(rule) = judge_text(&inline_equivalent(&interpreter, &plain), &plain, cwd) {
        return Some(rule);
    }
    if family_of(&interpreter) != Some(Family::Shell) {
        return None;
    }
    // #9037: nested runs resolve as strictly as the top level.
    let nested = resolve_self_paths(&body, path);
    each_script(
        &nested,
        bases,
        true,
        |_, inner, inner_interp, inner_bases| match inner {
            Ok(inner) => judge_script(&inner, inner_interp, inner_bases, depth + 1, seen),
            Err(why) => Some(Rule::Unread(why)),
        },
    )
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
    body.split_inclusive('\n')
        .map(|line| {
            let comment = line.trim_start().starts_with('#') && !line.contains("$(");
            if comment {
                if line.ends_with('\n') { "\n" } else { "" }
            } else {
                line
            }
        })
        .collect::<String>()
        .replace("\\\n", " ")
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

/// The candidate scripts of one segment's argv, and whether the program word
/// was resolved exactly rather than found by a scan for an interpreter name.
fn candidates(argv: &[String]) -> (Vec<Candidate>, bool) {
    // #9037: a case arm's pattern (`pat)`) is not a command; the words after it are.
    if let Some(first) = argv.first()
        && first.ends_with(')')
        && !first.contains('(')
    {
        return candidates(&argv[1..]);
    }
    let exact = strip_wrapper_prefix(argv);
    let start = exact
        .or_else(|| {
            argv.iter()
                .position(|t| family_of(&command_basename(t)).is_some())
        })
        .unwrap_or(0);
    let Some(program) = argv.get(start) else {
        return (Vec::new(), false);
    };
    let basename = command_basename(program);
    let found = match family_of(&basename) {
        Some(family) => {
            let interpreter = if matches!(basename.as_str(), "source" | ".") {
                "sh".to_string()
            } else {
                basename
            };
            interpreter_candidates(family, &interpreter, &argv[start + 1..])
        }
        None if program.contains('/') => vec![Candidate {
            word: program.clone(),
            role: Role::Positional,
            interpreter: None,
        }],
        None => Vec::new(),
    };
    (found, exact.is_some())
}

/// The candidates an interpreter's arguments name; empty after an option that
/// runs inline code, because the inline rules already read that code.
fn interpreter_candidates(family: Family, interpreter: &str, args: &[String]) -> Vec<Candidate> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<Candidate>, word: &str, role| {
        out.push(Candidate {
            word: word.to_string(),
            role,
            interpreter: Some(interpreter.to_string()),
        });
    };
    let mut options_done = false;
    let mut i = 0;
    while let Some(token) = args.get(i) {
        i += 1;
        // A here-document or here-string is inline text the inline rules read.
        if token.starts_with("<<") {
            i += usize::from(matches!(token.as_str(), "<<" | "<<-" | "<<<"));
            continue;
        }
        if let Some((0, attached)) = input_redirect_operand(token) {
            let word = if attached.is_empty() {
                i += 1;
                args.get(i - 1).map_or("", String::as_str)
            } else {
                attached
            };
            push(&mut out, word, Role::Stdin);
            continue;
        }
        match redirect_role(token) {
            RedirectRole::None => {}
            RedirectRole::TargetFollows => {
                i += 1;
                continue;
            }
            _ => continue,
        }
        if options_done || !token.starts_with('-') {
            push(&mut out, token, Role::Positional);
            continue;
        }
        if token == "--" {
            options_done = true;
        } else if token == "-" || runs_inline(family, token) {
            out.retain(|c| c.role != Role::Positional);
            return out;
        } else if family.code_loading_options().contains(&token.as_str()) {
            if let Some(value) = args.get(i) {
                push(&mut out, value, Role::Loaded);
            }
            i += 1;
        } else if let Some((flag, value)) = token.split_once('=')
            && family.code_loading_options().contains(&flag)
        {
            push(&mut out, value, Role::Loaded);
        }
    }
    out
}

/// Whether option `token` makes `family` run inline code or read stdin.
fn runs_inline(family: Family, token: &str) -> bool {
    if family == Family::Node {
        return matches!(token, "-e" | "-p" | "--eval" | "--print")
            || token.starts_with("--eval=")
            || token.starts_with("--print=");
    }
    let Some(cluster) = token.strip_prefix('-').filter(|c| !c.starts_with('-')) else {
        return false;
    };
    for c in cluster.chars() {
        if family.valued_letters().contains(&c) {
            return false;
        }
        if family.inline_letters().contains(&c) {
            return true;
        }
    }
    false
}

/// The existing files the candidates name: every loaded file, the first
/// positional operand that exists, else the stdin file.
fn locate(candidates: &[Candidate], bases: &[PathBuf]) -> Vec<(String, PathBuf, Option<String>)> {
    let mut out = Vec::new();
    let mut found_positional = false;
    for role in [Role::Loaded, Role::Positional, Role::Stdin] {
        if role == Role::Stdin && found_positional {
            break;
        }
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
                found_positional = true;
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

/// The refusal: the script word from the command, the rule class, and the
/// ruling-263 remedy. No byte of the body is quoted.
fn deny_reason(word: &str, rule: Rule) -> String {
    let what = match rule {
        Rule::CredentialCommand => {
            "its body, or a script it runs, runs a credential-reading command (a Keychain, \
             token or secret CLI; #8596, #8248)"
        }
        Rule::SecretFile => "its body, or a script it runs, names a secret-bearing file (#7266)",
        Rule::NestedOrDump => {
            "its body, or a script it runs, reads a credential through a nested command or an \
             environment dump (#8756)"
        }
        Rule::LaunchdPlist => {
            "its body, or a script it runs, reads a credential-bearing launchd plist (#8523)"
        }
        // #8879: every unread class fails closed and says which it was.
        Rule::Unread(why) => match why {
            Unread::Unreadable => "the guard could not read its body, so it fails closed",
            Unread::NotRegular => "it is not a regular file, so the guard fails closed",
            Unread::TooDeep => {
                "it runs scripts nested past the guard's depth bound, so the guard fails closed"
            }
            Unread::TooLarge => {
                "its body is over the guard's 256 KiB read bound, so the guard fails closed"
            }
            Unread::NotText => "its body is not UTF-8 text, so the guard fails closed",
            Unread::Missing => {
                "it does not exist and nothing else in the command names it, so the guard \
                 cannot judge its body and fails closed"
            }
            Unread::Unresolvable => {
                "its path is computed at run time (a variable, a substitution or a glob), so \
                 the guard cannot judge its body and fails closed"
            }
        },
    };
    format!(
        "`tm hook --pm-guard` refused running the script `{word}` (#8879): {what}. A script \
         gets the same credential-read refusal as the command written inline (owner rulings \
         263 and 268), so moving a refused read into a script does not change the answer. \
         Stop and report this refusal to the Architect; do not retry the read in another form."
    )
}

#[cfg(test)]
#[path = "pm_guard_secret_script_tests.rs"]
mod tests;
