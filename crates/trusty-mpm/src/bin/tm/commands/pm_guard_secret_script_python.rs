//! The Python reading of a script body, for `pm_guard_secret_script` (#10598).
//!
//! Why: the #8879 judge wraps a Python body as a shell here-document and runs
//! the shell scanners over it. Python's own words then read as shell: `ps = …`
//! as a process dump, `refs.key` and `token.lower()` as files, `if op == …`
//! beside `.read()` as `op read`. Seeded Architect scripts refused for it
//! (#10570). Matching the shell scanners to Python is what #10570's held head
//! tried, and three review bypasses came of it, so those scanners stay as they
//! are.
//! What: [`python_reading`] reads the body as Python: comments and docstrings
//! are prose, code is identifiers, and the string literals are the only text a
//! call or a path can come from. A body with nothing in that reading skips the
//! shell judges. A CLI call, a dump or a secret file in its literals refuses
//! outright, which also covers an argv list the shell words never join
//! (`["gh", "auth", "token"]`). A fixed trigger (`find-generic-password`) goes
//! to the shell judges, which tell a status check from a printed value.
//!
//! Fail closed: every shape the reader is unsure of goes to the shell judges
//! with its comments kept ([`Reading::Unread`]): an unterminated string, a
//! nested f-string (PEP 701, which can hide a `#`), a source-encoding cookie
//! other than UTF-8, `__doc__` used other than as an argparse description, and
//! `exec`/`eval` beside `__file__`, which can run a comment.
//!
//! Residuals, each also open under the shell judges unless noted: a name built
//! at run time (`".e" + "nv"`); a bare secret glob held in a list and matched
//! in-process (`fnmatch`), which is how a deny list reads, and which the shell
//! judges did refuse; a call built from a docstring through `ap.description`.
//! Test: `pm_guard_secret_script_python_tests.rs`;
//! `fleet::tests::every_seeded_architect_script_passes_10570`.

use std::collections::HashSet;

use super::Rule;
use crate::commands::pm_guard_bash::python_reader_words::{
    RUN_TIME_PREFIXES as CLI_RUN_TIME_PREFIXES, TRIGGERS as CREDENTIAL_TRIGGERS,
    WORD_TRIGGERS as CLI_WORD_TRIGGERS, security_is_interactive as security_reads_stdin_commands,
};
use crate::commands::pm_guard_secret_nested::evaluate_nested_secret_read_rules;
use crate::commands::pm_guard_secret_read::{Scan, names_a_secret_file, secret_files_named_in};

/// Words on a line that expand a glob or hand text to a shell.
const GATE_CODE_WORDS: &[&str] = &["glob", "iglob", "rglob", "system", "popen", "shell"];

/// Literals on a line that hand text to a shell.
const GATE_LITERALS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "-c"];

/// One logical Python line: its code with literals removed, and the contents
/// of the string literals on it, docstrings excluded.
#[derive(Debug, Default)]
pub(super) struct Line {
    code: String,
    literals: Vec<String>,
}

/// A Python body as read by [`read_python`].
#[derive(Debug, Default)]
pub(super) struct PyBody {
    lines: Vec<Line>,
    docstrings: Vec<String>,
}

/// What the Python reading of a body decides.
#[derive(Debug)]
pub(super) enum Reading {
    /// Nothing in the reading: the shell judges are skipped.
    Clean,
    /// A credential call, a dump or a secret file in its literals.
    Refuse(Rule),
    /// A trigger only the shell judges can weigh.
    Weigh,
    /// The reader cannot vouch for the body: the shell judges read it raw.
    Unread,
}

/// Read `body` as Python and decide; see the module doc.
/// Test: `a_python_body_is_read_as_python_10598`,
/// `a_real_credential_read_in_python_still_refuses_10598`,
/// `the_10570_hold_probes_still_refuse_10598`,
/// `a_body_the_reader_cannot_read_is_not_clean_10598`.
pub(super) fn python_reading(body: &str) -> Reading {
    let Some(read) = read_python(body) else {
        return Reading::Unread;
    };
    if names_cli_call(&read) {
        return Reading::Refuse(Rule::CredentialCommand);
    }
    if names_secret_file(&read) {
        return Reading::Refuse(Rule::SecretFile);
    }
    if names_a_dump(&read) {
        return Reading::Refuse(Rule::NestedOrDump);
    }
    if names_a_fixed_trigger(&read) || names_a_plist(&read) {
        return Reading::Weigh;
    }
    Reading::Clean
}

/// Read `body` as Python, or `None` for a shape the reader cannot vouch for.
pub(super) fn read_python(body: &str) -> Option<PyBody> {
    if has_foreign_coding_cookie(body) {
        return None;
    }
    let chars: Vec<char> = body.chars().collect();
    let mut read = PyBody::default();
    let mut line = Line::default();
    let (mut i, mut depth, mut at_start) = (0, 0usize, true);
    while let Some(&c) = chars.get(i) {
        match c {
            // Python's tokenizer ends a line at `\r` as well as `\n`.
            '\n' | '\r' => {
                if depth == 0 {
                    read.lines.push(std::mem::take(&mut line));
                    at_start = true;
                }
                i += 1;
            }
            '\\' if matches!(chars.get(i + 1), Some('\n' | '\r')) => {
                at_start = false;
                i += 2;
            }
            '#' => {
                while chars.get(i).is_some_and(|c| !matches!(c, '\n' | '\r')) {
                    i += 1;
                }
            }
            '\'' | '"' => {
                let (text, prefix, end) = read_literal(&chars, i)?;
                i = end;
                let prose = at_start && depth == 0 && ends_statement(&chars, i);
                if prose && !prefix.contains(['f', 't', 'b']) {
                    read.docstrings.push(text);
                } else {
                    line.literals.push(text);
                }
                line.code.push(' ');
                at_start = false;
            }
            _ => {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                at_start = at_start && c.is_whitespace();
                line.code.push(c);
                i += 1;
            }
        }
    }
    read.lines.push(line);
    if code_can_run_prose(&read) {
        return None;
    }
    Some(read)
}

/// One string literal opening at `open`: its text, its lowercased prefix, and
/// the index after its closing quote. `None` when it does not close, or when
/// an f-string closes with a replacement field still open (PEP 701 nesting).
fn read_literal(chars: &[char], open: usize) -> Option<(String, String, usize)> {
    let quote = chars[open];
    let prefix = literal_prefix(chars, open);
    let formatted = prefix.contains(['f', 't']);
    let triple = chars.get(open + 1) == Some(&quote) && chars.get(open + 2) == Some(&quote);
    let width = if triple { 3 } else { 1 };
    let start = open + width;
    let (mut k, mut fields) = (start, 0usize);
    loop {
        let c = *chars.get(k)?;
        if c == '\\' {
            k += 2;
            continue;
        }
        if !triple && matches!(c, '\n' | '\r') {
            return None;
        }
        if c == quote
            && (!triple || (chars.get(k + 1) == Some(&quote) && chars.get(k + 2) == Some(&quote)))
        {
            break;
        }
        if formatted {
            let doubled = chars.get(k + 1) == Some(&c);
            match c {
                '{' | '}' if doubled => k += 1,
                '{' => fields += 1,
                '}' => fields = fields.saturating_sub(1),
                _ => {}
            }
        }
        k += 1;
    }
    if fields > 0 {
        return None;
    }
    let text: String = chars[start..k].iter().collect();
    Some((text, prefix, k + width))
}

/// The lowercased string prefix (`rb`, `f`, …) before `open`, or empty.
fn literal_prefix(chars: &[char], open: usize) -> String {
    let mut from = open;
    while from > 0 && chars[from - 1].is_alphanumeric() {
        from -= 1;
    }
    let word: String = chars[from..open].iter().collect::<String>().to_lowercase();
    if word.chars().all(|c| "rbfut".contains(c)) {
        word
    } else {
        String::new()
    }
}

/// Whether nothing but a comment or `;` follows a literal on its line, so the
/// literal is a statement of its own: `"x".format()` and `"a" "b"` are not.
fn ends_statement(chars: &[char], mut at: usize) -> bool {
    while chars
        .get(at)
        .is_some_and(|c| matches!(c, ' ' | '\t' | '\x0c'))
    {
        at += 1;
    }
    chars
        .get(at)
        .is_none_or(|c| matches!(c, '\n' | '\r' | '#' | ';'))
}

/// Whether line 1 or 2 declares a source encoding other than UTF-8; `utf-7`
/// and its kin can spell a quote the reader never sees.
fn has_foreign_coding_cookie(body: &str) -> bool {
    body.lines().take(2).any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        line.starts_with('#')
            && (line.contains("coding:") || line.contains("coding="))
            && !(line.contains("utf-8") || line.contains("utf8"))
    })
}

/// Whether code can reach the prose the reader set aside: `__doc__` other than
/// as an argparse description, a source-reading helper, or `exec`/`eval`
/// beside `__file__`.
fn code_can_run_prose(read: &PyBody) -> bool {
    let code: String = read
        .lines
        .iter()
        .map(|l| l.code.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let words = code_words(&code);
    let described = code.matches("description=__doc__").count();
    code.matches("__doc__").count() != described
        || words.contains("getdoc")
        || words.contains("getsource")
        || (words.contains("__file__") && (words.contains("exec") || words.contains("eval")))
}

/// The lowercased identifiers of `code`.
fn code_words(code: &str) -> HashSet<String> {
    code.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// `literal` cut as a command line: lowercase, whitespace and `;|&` separate,
/// and each word reduced to its basename, outer brackets and quotes removed.
fn literal_words(literal: &str) -> Vec<String> {
    literal
        .to_lowercase()
        .split(|c: char| c.is_whitespace() || ";|&".contains(c))
        .map(|w| w.trim_matches(|c: char| "()[]<>,'\"".contains(c)))
        .filter(|w| !w.is_empty())
        .map(|w| w.rsplit('/').next().unwrap_or(w).to_string())
        .collect()
}

/// A word the shell or a format call fills in at run time.
fn is_placeholder(word: &str) -> bool {
    word.contains(['{', '}', '%', '$', '`'])
}

/// Whether word `at` of a literal can be a subcommand after `prev`.
///
/// Why: the #10570 hold probes spell the subcommand mid-literal (`" read
/// op://…"`, `" --account work read "`, `"%(b)s read %(r)s"`, `"$b read $r"`),
/// while prose has it after an ordinary word (`f"cannot read {arg}"`).
/// What: true when it opens the literal, follows `prev` or a placeholder, or
/// only an option and its value lie between.
/// Test: `the_10570_hold_probes_still_refuse_10598`,
/// `a_python_body_is_read_as_python_10598`.
fn in_command_slot(words: &[String], at: usize, prev: &str) -> bool {
    let mut j = at;
    while j > 0 {
        let before = &words[j - 1];
        if before == prev || is_placeholder(before) || before.starts_with('-') {
            return true;
        }
        if j >= 2 && words[j - 2].starts_with('-') {
            j -= 2;
            continue;
        }
        return false;
    }
    true
}

/// Whether a fixed trigger appears anywhere but a comment.
fn names_a_fixed_trigger(read: &PyBody) -> bool {
    let fixed = |text: &str| {
        let lower = text.to_lowercase();
        CREDENTIAL_TRIGGERS.iter().any(|t| lower.contains(t))
    };
    read.docstrings.iter().any(|d| fixed(d.as_str()))
        || read
            .lines
            .iter()
            .any(|l| fixed(&l.code) || l.literals.iter().any(|t| fixed(t.as_str())))
}

/// Whether the literals name an interactive `security` or a sibling CLI call,
/// or a docstring spells one.
fn names_cli_call(read: &PyBody) -> bool {
    let live: Vec<Vec<String>> = read
        .lines
        .iter()
        .flat_map(|l| &l.literals)
        .map(|l| literal_words(l))
        .collect();
    let mut words = HashSet::new();
    for line in &read.lines {
        words.extend(code_words(&line.code));
    }
    let named: HashSet<&str> = live.iter().flatten().map(String::as_str).collect();
    read.docstrings.iter().any(|d| spells_adjacent_call(d))
        || read
            .lines
            .iter()
            .any(|l| reads_interactive_security(&l.literals))
        || CLI_WORD_TRIGGERS.iter().any(|seq| {
            (words.contains(seq[0]) || named.contains(seq[0]))
                && seq.windows(2).all(|pair| {
                    live.iter().any(|l| {
                        l.iter()
                            .enumerate()
                            .any(|(at, w)| w == pair[1] && in_command_slot(l, at, pair[0]))
                    })
                })
        })
        || live.iter().any(|l| picks_subcommand_at_run_time(l))
}

/// Whether a docstring spells a CLI call with its words adjacent; run-time use
/// of a docstring is otherwise refused by [`code_can_run_prose`].
fn spells_adjacent_call(doc: &str) -> bool {
    let words = literal_words(doc);
    CLI_WORD_TRIGGERS.iter().any(|seq| {
        words
            .windows(seq.len())
            .any(|w| w.iter().zip(*seq).all(|(a, b)| a == b))
    })
}

/// Whether the literals of one line, read as an argv, start `security` in
/// interactive mode.
fn reads_interactive_security(literals: &[String]) -> bool {
    let words: Vec<String> = literals.iter().flat_map(|l| literal_words(l)).collect();
    words
        .iter()
        .enumerate()
        .any(|(at, w)| w == "security" && security_reads_stdin_commands(&words[at + 1..]))
}

/// Whether a literal runs a [`CLI_RUN_TIME_PREFIXES`] prefix with its next
/// word filled in at run time (`"op {verb}"`, `"gh auth $X"`, #8677).
fn picks_subcommand_at_run_time(words: &[String]) -> bool {
    CLI_RUN_TIME_PREFIXES.iter().any(|prefix| {
        words.windows(prefix.len() + 1).any(|w| {
            w[..prefix.len()].iter().zip(*prefix).all(|(a, b)| a == b)
                && is_placeholder(&w[prefix.len()])
        })
    })
}

/// Whether a live literal names a secret-bearing file. A literal that is only
/// a glob counts when something can expand it ([`glob_can_expand`]).
fn names_secret_file(read: &PyBody) -> bool {
    let gated = gated_identifiers(read);
    read.lines.iter().any(|line| {
        let split_path = line.literals.windows(2).any(|pair| {
            pair[0].starts_with('.')
                && !pair[0].contains(char::is_whitespace)
                && names_a_secret_file(&format!("{}/{}", pair[0], pair[1]), Scan::ProgramText)
        });
        split_path
            || line.literals.iter().any(|text| {
                // #10598: a replacement field is a word break, as `{HOME}/.env` is a path.
                let spaced = text.replace(['{', '}'], " ");
                let named = secret_files_named_in(&spaced, Scan::ProgramText)
                    .iter()
                    .any(|word| !word.contains(['[', ']', '{', '}']));
                named
                    && (!text.trim().contains('*')
                        || !is_bare_word(text)
                        || glob_can_expand(line, &gated))
            })
    })
}

/// Whether a literal is one word, as a filename glob in a deny list is.
fn is_bare_word(text: &str) -> bool {
    !text.trim().contains(char::is_whitespace)
}

/// Whether line `code` can expand or shell out a text.
fn line_is_a_gate(line: &Line) -> bool {
    let words = code_words(&line.code);
    GATE_CODE_WORDS.iter().any(|w| words.contains(*w))
        || line
            .literals
            .iter()
            .any(|l| GATE_LITERALS.contains(&l.trim()))
}

/// The identifiers on every line that can expand or shell out a text.
fn gated_identifiers(read: &PyBody) -> HashSet<String> {
    let mut out = HashSet::new();
    for line in read.lines.iter().filter(|l| line_is_a_gate(l)) {
        out.extend(code_words(&line.code));
    }
    out
}

/// Whether a glob literal on `line` reaches a gate: the line is one, or the
/// name it assigns the glob to appears on one (`P = "*.env*"`, `glob(P)`).
fn glob_can_expand(line: &Line, gated: &HashSet<String>) -> bool {
    line_is_a_gate(line)
        || line
            .code
            .split_once('=')
            .map(|(name, _)| name.trim().to_ascii_lowercase())
            .is_some_and(|name| gated.contains(&name))
}

/// Whether a literal names a launchd plist, which the shell judges resolve.
fn names_a_plist(read: &PyBody) -> bool {
    read.lines.iter().flat_map(|l| &l.literals).any(|t| {
        let t = t.to_lowercase();
        t.contains(".plist") || t.contains("launchagents") || t.contains("launchdaemons")
    })
}

/// Whether the literals of any line, read as a command line, are a process or
/// pod environment dump.
fn names_a_dump(read: &PyBody) -> bool {
    let text: String = read
        .lines
        .iter()
        .filter(|l| !l.literals.is_empty())
        .map(|l| {
            l.literals
                .join(" ")
                .replace(['\'', '"', '`', '$', '\\'], " ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    evaluate_nested_secret_read_rules(&text).is_some()
}
