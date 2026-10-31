//! The program names a Bash command rebinds before it runs them (#10566), for
//! the #8875 secret-DELETE rule.
//!
//! Why: the DELETE rule judged a program by the word as written. After
//! `hash -p /usr/bin/gh ls`, `alias ls=gh`, `ls() { gh "$@"; }` or
//! `BASH_CMDS[ls]=/usr/bin/gh`, the word `ls` runs gh, so
//! `ls api -X DELETE …/secrets/K` deleted a secret unrefused.
//! What: [`Rebinds::scan`] lists the names a command rebinds — `hash` and
//! `enable` operands, `alias NAME=VALUE`, `function NAME` and `NAME()`
//! definitions, and the `BASH_CMDS`, `BASH_ALIASES`, `BASH_FUNC_`, zsh
//! `commands`, `aliases` and `functions` table entries — reading `eval`
//! operands too. The DELETE rule then judges a call to a rebound name as a
//! call to `gh`, `gh api` and `curl`, and an alias by its expansion, in the
//! command and in each `$( … )` or backtick body it carries.
//!
//! Fail closed: when the guard cannot tell which names are rebound — a
//! `source`/`.` file, a `PATH` assignment, an `eval` or table entry built
//! from an expansion, an unlexable segment that names a rebinding word, or
//! `eval` nested past [`MAX_SCAN_DEPTH`] — every word counts as rebound.
//! Test: `a_call_through_a_rebound_name_is_refused_10566`,
//! `an_unknown_rebinding_fails_closed_10566`,
//! `a_rebound_name_with_no_delete_still_allows_10566`.

use super::{argv_deletes_a_secret, judge};
use crate::commands::pm_guard_bash::{split_shell_segments, tokenize};

/// `eval` nesting the rebinding scan reads before it fails closed.
const MAX_SCAN_DEPTH: usize = 8;

/// Words that rebind a name, or load text that may (#10566).
const REBINDING_WORDS: &[&str] = &[
    "hash",
    "enable",
    "alias",
    "function",
    "eval",
    "source",
    "BASH_CMDS",
    "BASH_ALIASES",
    "BASH_FUNC",
    "commands",
    "aliases",
    "functions",
    "PATH",
    "path",
];

/// Assignment tables that bind a name: `TABLE[NAME]=VALUE`. The `bool` says
/// whether VALUE is an alias expansion.
const TABLES: &[(&str, bool)] = &[
    ("BASH_CMDS[", false),
    ("commands[", false),
    ("functions[", false),
    ("BASH_ALIASES[", true),
    ("aliases[", true),
];

/// The names one command rebinds.
#[derive(Clone, Debug, Default)]
pub(super) struct Rebinds {
    /// Every word may be rebound: the guard could not read the rebinding.
    any: bool,
    /// Each rebound name, with its alias expansion when it is an alias.
    names: Vec<(String, Option<String>)>,
}

impl Rebinds {
    /// The rebindings `command` makes, added to `self`'s.
    pub(super) fn with_scan(&self, command: &str) -> Self {
        let mut out = self.clone();
        out.scan_into(command, 0);
        out
    }

    /// Whether the command rebinds nothing.
    pub(super) fn is_empty(&self) -> bool {
        !self.any && self.names.is_empty()
    }

    /// Whether `word`, in a program position, may run a rebound name.
    pub(super) fn rebinds(&self, word: &str) -> bool {
        self.any
            || self
                .names
                .iter()
                .any(|(name, _)| *name == command_word(word))
    }

    /// The alias expansion `word` runs, when it is a known alias.
    pub(super) fn alias_of(&self, word: &str) -> Option<&str> {
        let head = command_word(word);
        self.names
            .iter()
            .find(|(name, _)| *name == head)
            .and_then(|(_, expansion)| expansion.as_deref())
    }

    /// `self` with the alias `word` names no longer expanding: bash does not
    /// re-expand an alias inside its own expansion, so `alias ls='ls -l'`
    /// ends there.
    pub(super) fn without_alias(&self, word: &str) -> Self {
        let head = command_word(word);
        let mut out = self.clone();
        for (name, expansion) in &mut out.names {
            if name == head {
                *expansion = None;
            }
        }
        out
    }

    /// Add the rebindings of every segment of `command`, `depth` evals deep.
    fn scan_into(&mut self, command: &str, depth: usize) {
        if depth > MAX_SCAN_DEPTH {
            // #10566: the guard cannot see what an eval this deep rebinds.
            self.any = true;
            return;
        }
        for segment in split_shell_segments(command) {
            match tokenize(&segment) {
                Ok(argv) => self.scan_argv(&argv, depth),
                // #10566: an unlexable segment that names a rebinding word.
                Err(_) => self.any |= names_a_rebinding_word(&segment),
            }
        }
    }

    /// Add the rebindings one segment's argv makes.
    fn scan_argv(&mut self, argv: &[String], depth: usize) {
        for (at, raw) in argv.iter().enumerate() {
            let rest = &argv[at + 1..];
            if let Some(name) = raw.strip_suffix("()").filter(|n| !n.is_empty()) {
                self.add(command_word(name), None);
            } else if rest.first().is_some_and(|w| w == "()") {
                self.add(command_word(raw), None);
            } else if rest.first().is_some_and(|w| w == "{") && is_plain_name(raw) {
                // `ls() {` read with its parens blanked (`pm_guard_secret_nested`).
                self.add(raw, None);
            }
            // #10566: a new PATH may resolve any name to any program.
            self.any |= ["PATH=", "PATH+=", "path=", "path+=", "path["]
                .iter()
                .any(|p| raw.starts_with(p));
            self.scan_table(raw);
            match command_word(raw) {
                "hash" | "enable" => operands(rest).for_each(|w| self.add(w, None)),
                "function" => {
                    if let Some(name) = rest.first() {
                        self.add(name.trim_end_matches("()"), None);
                    }
                }
                "alias" => {
                    for (name, value) in rest.iter().filter_map(|w| w.split_once('=')) {
                        self.add(name, Some(value.to_string()));
                    }
                }
                "eval" => {
                    self.any |= rest.iter().any(|w| w.contains(['$', '`']));
                    self.scan_into(&rest.join(" "), depth + 1);
                }
                "source" => self.any = true,
                "." if at == 0 => self.any = true,
                _ => {}
            }
        }
    }

    /// Add a `TABLE[NAME]=VALUE` or `BASH_FUNC_NAME%%=…` binding in `raw`.
    fn scan_table(&mut self, raw: &str) {
        if let Some(name) = raw
            .strip_prefix("BASH_FUNC_")
            .and_then(|r| r.split_once("%%"))
            .map(|(name, _)| name)
        {
            self.add(name, None);
        }
        for (table, is_alias) in TABLES {
            let Some((name, value)) = raw
                .strip_prefix(table)
                .and_then(|r| r.split_once(']'))
                .map(|(name, tail)| (name, tail.trim_start_matches('+')))
            else {
                continue;
            };
            let expansion = value.strip_prefix('=').filter(|_| *is_alias);
            self.add(name, expansion.map(str::to_string));
        }
    }

    /// Record `name` as rebound; a computed name could be any word.
    fn add(&mut self, name: &str, expansion: Option<String>) {
        if name.is_empty() || name.contains(['$', '`', '*', '?', '[', '{']) {
            self.any = true;
        } else {
            self.names.push((name.to_string(), expansion));
        }
    }
}

/// The non-option words of `rest` (a `hash`/`enable` operand list).
fn operands(rest: &[String]) -> impl Iterator<Item = &str> {
    rest.iter()
        .map(String::as_str)
        .filter(|w| !w.starts_with('-'))
}

/// The command name inside a word: the text after its last substitution,
/// subshell or group opener (`X=$(ls` → `ls`), then its basename.
fn command_word(word: &str) -> &str {
    let tail = word.rsplit(['(', '`', '{']).next().unwrap_or(word);
    tail.rsplit('/').next().unwrap_or(tail)
}

/// Whether `word` is a plain command name (letters, digits, `_`, `-`, `.`).
fn is_plain_name(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// Whether an unlexable segment, read with its quotes removed, carries a
/// rebinding word.
fn names_a_rebinding_word(segment: &str) -> bool {
    segment
        .replace(['\'', '"', '\\'], "")
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|w| REBINDING_WORDS.contains(&w) || w.starts_with("BASH_FUNC_"))
        || segment.contains("()")
}

/// Whether a call through a rebound name deletes a secret (#10566).
///
/// What: each word [`Rebinds::rebinds`] is judged as `gh`, `gh api` and
/// `curl` in front of the words after it ([`rebound_argvs`]); the first known
/// alias is also re-judged by its expansion, one level deeper (bash expands
/// an alias in command position, recursively).
/// Test: `a_call_through_a_rebound_name_is_refused_10566`,
/// `an_unknown_rebinding_fails_closed_10566`.
pub(super) fn rebound_call_deletes(
    argv: &[String],
    secrets_named: bool,
    depth: usize,
    rebinds: &Rebinds,
) -> bool {
    let through_alias = argv.iter().enumerate().find_map(|(at, word)| {
        let expansion = rebinds.alias_of(word)?;
        let text = format!("{expansion} {}", argv[at + 1..].join(" "));
        // bash never re-expands the alias it is expanding.
        Some((text, rebinds.without_alias(word)))
    });
    through_alias.is_some_and(|(text, inner)| judge(&text, depth + 1, &inner))
        || argv.iter().enumerate().any(|(at, word)| {
            rebinds.rebinds(word)
                && rebound_argvs(&argv[at + 1..])
                    .iter()
                    .any(|real| argv_deletes_a_secret(real, secrets_named, false, depth, true))
        })
}

/// The argv a call through a rebound name might really run: the rest of the
/// words behind `gh`, `gh api` and `curl`.
fn rebound_argvs(rest: &[String]) -> [Vec<String>; 3] {
    let behind = |prefix: &[&str]| {
        prefix
            .iter()
            .map(|w| (*w).to_string())
            .chain(rest.iter().cloned())
            .collect()
    };
    [behind(&["gh"]), behind(&["gh", "api"]), behind(&["curl"])]
}
