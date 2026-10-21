//! The name rules for a write target the trust-anchor guard cannot place
//! (#8878, #9815).
//!
//! Why: a target behind an expansion, a glob or a `cd` has no directory the
//! guard can resolve, so only its file name can be judged. The #8878 rule —
//! PM, agents and the taint seal — denies every expansion or glob and any name
//! that is an anchor path component. Applied to the Architect's main thread for
//! the question table, it denied routine writes (`sed -i … *.rs`, `> "$OUT"`,
//! `cp a .` after a `cd`). Architect ruling (b), 02:11Z 2026-10-13: for the
//! question table, compare only the table's own names.
//! What: [`Anchors::could_be`] runs the #8878 rule, unchanged, unless
//! `table_names` is set; then [`could_name_table`] decides. The narrow rule
//! reads the target's final component as a pattern: a glob, or an expansion
//! that is not one plain variable, matches any text there. It denies when that
//! pattern could spell one of the table's names, and for `.`, `..`, an empty
//! name and `~user`, which could be the table's directory or one above it.
//! Residual: one plain variable as the whole final component (`> "$OUT"`,
//! `cp a "$DEST"`) is allowed to the Architect; the guard does not evaluate
//! the assignment that sets it.
//! Test: `the_architect_may_make_routine_writes_that_cannot_name_the_table`,
//! `the_architect_is_denied_targets_that_could_name_the_table`,
//! `a_linked_table_directory_is_matched_by_its_resolved_name`,
//! `the_pm_keeps_the_broad_name_rule`.

use std::path::{Component, Path, PathBuf};

use trusty_mpm::core::architect_conversation::CONVERSATION_EXT;
use trusty_mpm::core::architect_launch::ARCHITECT_EXT;
use trusty_mpm::core::architect_session::SESSION_EXT;

use crate::commands::env_file_grant::GRANT_EXT;
use crate::commands::pm_guard_trust_anchor_paths::{eq_fold, fold_case};

use super::{Anchors, has_shell_pattern, is_json};

/// How many directories above a table file the narrow rule matches by name.
const TABLE_DIR_DEPTH: usize = 3;

impl Anchors {
    /// Whether a target whose directory is unknown could still name an
    /// anchor: its file name is an expansion or glob, empty, `.`/`..`, an
    /// anchor path component (`envfile-grants` included), a `*.architect`
    /// launch record, its `*.architect-session` name sidecar, the
    /// `*.architect-conversation` record or a `*.grant` `tm env` grant (any
    /// case, folded as APFS compares names, #10107), or a `*.json` while
    /// records could be live. #9815: with `table_names` set,
    /// [`could_name_table`] decides instead.
    /// Test: `the_launch_record_dir_is_an_anchor`,
    /// `the_architect_is_denied_targets_that_could_name_the_table`,
    /// `egress_unicode_spellings_of_protected_names_are_denied`.
    pub(super) fn could_be(&self, spelling: &str) -> bool {
        // #9815 (ruling (b)): the question table matches its own names only.
        if let Some(names) = &self.table_names {
            return could_name_table(names, spelling);
        }
        let name = spelling.trim_end_matches('/');
        let name = name.rsplit('/').next().unwrap_or(name);
        // #10107: names fold as APFS compares them (`ſ`, `ﬁ`), not ASCII only.
        let folded = fold_case(name);
        name.is_empty()
            || name == "."
            || name == ".."
            || name.starts_with('~')
            || has_shell_pattern(name)
            || self.names.iter().any(|anchor| eq_fold(anchor, name))
            // #8878 ruling A: a planted launch record would be an identity,
            // and (R1 critic HIGH) a planted name sidecar would rename it.
            // #8939: a planted `tm env` grant would be a main-thread proof.
            // #8981: a planted conversation record would pick what it resumes.
            || Path::new(&folded)
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    [ARCHITECT_EXT, SESSION_EXT, GRANT_EXT, CONVERSATION_EXT]
                        .iter()
                        .any(|want| eq_fold(want, ext))
                })
            || (self.records_live && is_json(folded.as_ref()))
    }

    /// [`Self::could_be`] for a copy destination the guard cannot place.
    ///
    /// What: under the table rule, a `.` or `..` destination that is not
    /// itself written (`dest_too` false) is an existing directory; only the
    /// entries it receives are written, and those are judged by name.
    /// Test: `the_architect_may_make_routine_writes_that_cannot_name_the_table`,
    /// `the_architect_is_denied_targets_that_could_name_the_table`.
    pub(super) fn could_be_dest(&self, dest: &str, dest_too: bool) -> bool {
        let name = dest.trim_end_matches('/');
        let name = name.rsplit('/').next().unwrap_or(name);
        // #9815: `cp a .` after a `cd` writes `./a`, judged by its own name.
        if self.table_names.is_some() && !dest_too && matches!(name, "." | "..") {
            return false;
        }
        self.could_be(dest)
    }
}

/// The names the narrow rule compares: each path's file name and the last
/// [`TABLE_DIR_DEPTH`] directories above it. The caller passes the lexical
/// and resolved spellings, so a linked directory counts by both names.
pub(super) fn table_names(paths: &[PathBuf]) -> Vec<String> {
    let mut names = Vec::new();
    for path in paths {
        names.extend(path.file_name().map(|n| n.to_string_lossy().into_owned()));
        let dirs: Vec<String> = path
            .parent()
            .into_iter()
            .flat_map(Path::components)
            .filter_map(|c| match c {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        names.extend(dirs.into_iter().rev().take(TABLE_DIR_DEPTH));
    }
    names
}

/// Whether `spelling`'s final component could spell one of `names`.
///
/// What: `.`, `..`, an empty name and a `~user` home could be the table's
/// directory or one above it, so they deny. One plain variable (`$OUT`,
/// `${OUT}`) has no text to compare and passes. Anything else is matched as a
/// pattern, both sides folded as APFS compares names (#10107): a glob, or any
/// other expansion, matches any text.
/// Test: `the_architect_is_denied_unicode_spellings_of_the_table`,
/// `the_architect_is_denied_targets_that_could_name_the_table`,
/// `the_architect_may_make_routine_writes_that_cannot_name_the_table`.
pub(super) fn could_name_table(names: &[String], spelling: &str) -> bool {
    // #10107: each literal folds as APFS compares names, `ﬁ` to `fi`.
    let mut tokens: Vec<Tok> = tokens(spelling)
        .into_iter()
        .flat_map(|tok| match tok {
            Tok::Lit(c) => fold_case(c.encode_utf8(&mut [0; 4]))
                .chars()
                .map(Tok::Lit)
                .collect(),
            other => vec![other],
        })
        .collect();
    while tokens.last() == Some(&Tok::Slash) {
        tokens.pop();
    }
    let start = tokens
        .iter()
        .rposition(|t| *t == Tok::Slash)
        .map_or(0, |i| i + 1);
    match &tokens[start..] {
        // #9815: these could be the table directory or a directory above it.
        [] | [Tok::Lit('.')] | [Tok::Lit('.'), Tok::Lit('.')] | [Tok::Lit('~'), ..] => true,
        // #9815 (deviation 1): one plain variable names nothing to compare.
        [Tok::Var] => false,
        last => names.iter().any(|name| {
            let name: Vec<char> = fold_case(name).chars().collect();
            matches(last, &name)
        }),
    }
}

/// One element of a shell word, as the narrow rule reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    /// A literal character.
    Lit(char),
    /// A `/` outside an expansion.
    Slash,
    /// `*`, or an expansion other than one plain variable: any text.
    Many,
    /// `?` or a `[...]` class: any one character.
    One,
    /// `$NAME` or `${NAME}`: any text, or no name at all when alone.
    Var,
}

/// Split `word` into [`Tok`]s. A `/` inside `${...}`, `$(...)`, backticks or
/// a `{...}` brace is part of that expansion. An unterminated one runs to the
/// end of the word.
fn tokens(word: &str) -> Vec<Tok> {
    let chars: Vec<char> = word.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let tok = match c {
            '/' => Tok::Slash,
            '*' => Tok::Many,
            '?' => Tok::One,
            '[' => match chars[i..].iter().position(|&c| c == ']') {
                Some(n) => {
                    i += n + 1;
                    Tok::One
                }
                None => Tok::Lit('['),
            },
            '`' => {
                i = chars[i..]
                    .iter()
                    .position(|&c| c == '`')
                    .map_or(chars.len(), |n| i + n + 1);
                Tok::Many
            }
            '{' => {
                i = close(&chars, i, '{', '}');
                Tok::Many
            }
            '$' => match chars.get(i) {
                Some('{') => {
                    let end = close(&chars, i + 1, '{', '}');
                    // A closing `}` sits after the `{` at `i`, so the slice holds.
                    let plain = chars[end - 1] == '}' && is_ident(&chars[i + 1..end - 1]);
                    i = end;
                    if plain { Tok::Var } else { Tok::Many }
                }
                Some('(') => {
                    i = close(&chars, i + 1, '(', ')');
                    Tok::Many
                }
                Some(&c) if c == '_' || c.is_ascii_alphabetic() => {
                    while chars
                        .get(i)
                        .is_some_and(|&c| c == '_' || c.is_ascii_alphanumeric())
                    {
                        i += 1;
                    }
                    Tok::Var
                }
                Some(_) => {
                    i += 1;
                    Tok::Many
                }
                None => Tok::Lit('$'),
            },
            c => Tok::Lit(c),
        };
        out.push(tok);
    }
    out
}

/// The index just past the `shut` that closes an `open` at `i - 1`, nesting
/// counted; the word's length when it is never closed.
fn close(chars: &[char], mut i: usize, open: char, shut: char) -> usize {
    let mut depth = 1;
    while let Some(&c) = chars.get(i) {
        i += 1;
        if c == shut {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        } else if c == open {
            depth += 1;
        }
    }
    i
}

/// Whether `chars` is a shell variable name.
fn is_ident(chars: &[char]) -> bool {
    chars
        .first()
        .is_some_and(|&c| c == '_' || c.is_ascii_alphabetic())
        && chars.iter().all(|&c| c == '_' || c.is_ascii_alphanumeric())
}

/// Whether `pattern` matches all of `name`, letters compared ASCII
/// case-insensitively. A table over name prefixes, so a pattern of many `*`
/// costs pattern length times name length, not an exponential search.
fn matches(pattern: &[Tok], name: &[char]) -> bool {
    // reach[j]: the pattern so far can consume exactly name[..j].
    let mut reach = vec![false; name.len() + 1];
    reach[0] = true;
    for tok in pattern {
        let mut next = vec![false; name.len() + 1];
        let mut seen = false;
        for j in 0..=name.len() {
            seen |= reach[j];
            next[j] = match *tok {
                Tok::Many | Tok::Var => seen,
                Tok::One => j > 0 && reach[j - 1],
                Tok::Lit(c) => j > 0 && reach[j - 1] && name[j - 1].eq_ignore_ascii_case(&c),
                Tok::Slash => false,
            };
        }
        reach = next;
    }
    reach[name.len()]
}
