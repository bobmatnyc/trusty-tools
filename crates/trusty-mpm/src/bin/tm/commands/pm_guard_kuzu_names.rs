//! The fixed deny names of the #8625 kuzu-store rule, and how one path word is
//! matched against them.
//!
//! Why: issue #8625 — the model must never read a kuzu-memory store, the raw
//! export the import bridge writes, or the import venv, except through
//! `trusty-memory import kuzu`. Architect ruling Q2: "fixed deny constants. No
//! configuration." Q1: "deny the `kuzu-import-venv` component as the issue
//! says." Q3: "deny any glob that can match a deny name, except words that are
//! wildcards only."
//! What: [`word_names_kuzu`] splits a word into path COMPONENTS (at `/` and
//! `,`) after brace expansion, and denies when one component is a deny name
//! ([`KUZU_DENY_COMPONENTS`], or the [`KUZU_TEMP_PREFIX`] temp directory), or
//! is a glob that can match one. Matching is ASCII case-insensitive, because
//! the macOS default volume is. The secret-read word cutter
//! (`pm_guard_secret_words`) supplies the words, so a parameter expansion, a
//! quote join and a brace alternation reach this test the way they reach the
//! #7266 rule.
//! Test: `pm_guard_kuzu_store_tests.rs`, `denies_a_glob_that_can_match_a_deny_name`.

use crate::commands::pm_guard_bash::{
    expand_brace_alternatives, has_regex_quantifier, tokenize, without_glob_metacharacters,
};
use crate::commands::pm_guard_secret_words::{
    is_path_byte, normalize_bracket_classes, scan_spellings,
};

/// Path components that name a kuzu store, a store sidecar, a store directory
/// or the import venv (#8625 plan section 3, ruling Q1).
pub(crate) const KUZU_DENY_COMPONENTS: &[&str] = &[
    "memories.db",
    "memories.db.wal",
    "memories.db.shadow",
    "memories.db.lock",
    "memories.db.tmp",
    ".kuzu-memory",
    "kuzu-import-venv",
];

/// The prefix of the import bridge's temp directory, which holds the RAW
/// `export.json` (`trusty-memory` `kuzu_import::bridge::export_store`).
pub(crate) const KUZU_TEMP_PREFIX: &str = "trusty-kuzu-import-";

/// The Python module `kuzu-memory` ships; program text importing it denies.
pub(crate) const KUZU_MODULE: &str = "kuzu_memory";

/// One position of a deny name, for the glob overlap test.
#[derive(Clone, Copy)]
enum Atom {
    /// One literal byte.
    Lit(u8),
    /// One ASCII alphanumeric byte.
    Alnum,
    /// Any run of ASCII alphanumeric bytes; only ever the last atom.
    AlnumStar,
}

/// Whether one word — a path, a path fragment or a glob — names a kuzu deny
/// item.
///
/// Why: see the module doc. `program_text` releases a regex wildcard (`.*` in
/// a `sed` or `awk` program) whose literal core names nothing, the release
/// `pm_guard_secret_read::Scan::ProgramText` makes for the #7266 rule.
/// What: bracket classes become `?`, then every brace reading is split into
/// components; a brace shape the shared expander cannot read is WIDENED
/// ([`widen_braces`]) rather than skipped, so it fails closed.
/// Test: `denies_a_glob_that_can_match_a_deny_name`,
/// `denies_bash_reads_of_a_store`, `keeps_ordinary_commands_allowed`.
pub(crate) fn word_names_kuzu(word: &str, program_text: bool) -> bool {
    let word = normalize_bracket_classes(word);
    if program_text
        && has_regex_quantifier(&word)
        && !contains_deny_literal(&without_glob_metacharacters(&word))
    {
        return false;
    }
    let readings = expand_brace_alternatives(&word).unwrap_or_else(|| vec![widen_braces(&word)]);
    readings
        .iter()
        .any(|reading| reading.split(['/', ',']).any(component_denies))
}

/// Every distinct word of `text` that names a kuzu deny item, in order.
///
/// What: every [`scan_spellings`] reading, cut at each byte a path cannot
/// carry ([`is_path_byte`]), each word judged by [`word_names_kuzu`]. A text no
/// lexer can read still yields its words, so this is the fail-closed arm.
/// Test: `denies_bash_reads_of_a_store`, `denies_an_unlexable_segment_naming_a_store`.
pub(crate) fn kuzu_words_in(text: &str, program_text: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for spelling in scan_spellings(text) {
        for word in spelling.split(|c: char| !is_path_byte(c)) {
            if !word.is_empty()
                && word_names_kuzu(word, program_text)
                && !out.iter().any(|w| w == word)
            {
                out.push(word.to_string());
            }
        }
    }
    out
}

/// [`kuzu_words_in`] over PROGRAM TEXT — a here-document body or an
/// interpreter's inline program — plus an import of [`KUZU_MODULE`].
///
/// What: each line raw and lexed (a quote join `".kuzu""-memory"` reads whole),
/// and the `\<newline>`-joined spelling as well; a word whose first dotted
/// part is `kuzu_memory` denies too.
/// Test: `denies_bash_reads_of_a_store`.
pub(crate) fn program_text_words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let joined = text.replace("\\\n", "");
    for block in [text, joined.as_str()] {
        for line in block.lines() {
            let mut spellings = vec![line.to_string()];
            if let Ok(tokens) = tokenize(line) {
                spellings.extend(tokens);
            }
            for spelling in spellings {
                let mut words = kuzu_words_in(&spelling, true);
                words.extend(module_words(&spelling));
                for word in words {
                    if !out.contains(&word) {
                        out.push(word);
                    }
                }
            }
        }
    }
    out
}

/// The words of `text` that name the [`KUZU_MODULE`] Python package.
fn module_words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
        .filter(|w| names_kuzu_module(w))
        .map(str::to_string)
        .collect()
}

/// Whether a dotted module path starts with [`KUZU_MODULE`].
pub(crate) fn names_kuzu_module(module: &str) -> bool {
    module
        .split('.')
        .next()
        .is_some_and(|head| head.eq_ignore_ascii_case(KUZU_MODULE))
}

/// Whether `text` carries a deny name as a literal substring.
fn contains_deny_literal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("memories.db")
        || lower.contains(".kuzu-memory")
        || lower.contains("kuzu-import-venv")
        || lower.contains(KUZU_TEMP_PREFIX)
}

/// Whether one path component is, or as a glob can match, a deny name.
///
/// What: a wildcard-only component never denies (ruling Q3). A literal
/// component denies on an exact [`KUZU_DENY_COMPONENTS`] match or a
/// [`KUZU_TEMP_PREFIX`] prefix. A glob denies when it carries that prefix
/// literally or [`overlaps`] a deny name; the temp directory is modelled as
/// the prefix plus one or more alphanumerics, the `tempfile` name shape, so
/// `*.md` does not reach it and `*import-ab*` does.
/// Test: `denies_a_glob_that_can_match_a_deny_name`.
fn component_denies(component: &str) -> bool {
    let lower = component.to_ascii_lowercase();
    if lower.is_empty() || lower.bytes().all(|b| b == b'*' || b == b'?') {
        return false;
    }
    if lower.starts_with(KUZU_TEMP_PREFIX) || KUZU_DENY_COMPONENTS.contains(&lower.as_str()) {
        return true;
    }
    if !lower.contains(['*', '?']) {
        return false;
    }
    let candidate = lower.as_bytes();
    let mut temp: Vec<Atom> = KUZU_TEMP_PREFIX.bytes().map(Atom::Lit).collect();
    temp.extend([Atom::Alnum, Atom::AlnumStar]);
    KUZU_DENY_COMPONENTS.iter().any(|name| {
        let atoms: Vec<Atom> = name.bytes().map(Atom::Lit).collect();
        overlaps(candidate, &atoms)
    }) || overlaps(candidate, &temp)
}

/// Whether the glob `glob` (`*` and `?`) can match a string `atoms` matches.
///
/// What: `dp[i][j]` answers for `glob[i..]` against `atoms[j..]`, bottom-up.
/// [`Atom::AlnumStar`] is last, so it holds when the rest of the glob carries
/// no literal outside `[a-z0-9]`.
/// Test: `denies_a_glob_that_can_match_a_deny_name`.
fn overlaps(glob: &[u8], atoms: &[Atom]) -> bool {
    let (la, lb) = (glob.len(), atoms.len());
    let width = lb + 1;
    let mut dp = vec![false; (la + 1) * width];
    for i in (0..=la).rev() {
        for j in (0..=lb).rev() {
            let rest = &glob[i..];
            dp[i * width + j] = match atoms.get(j) {
                None => rest.iter().all(|b| *b == b'*'),
                Some(Atom::AlnumStar) => rest
                    .iter()
                    .all(|b| *b == b'*' || *b == b'?' || b.is_ascii_alphanumeric()),
                Some(atom) => match glob.get(i) {
                    None => false,
                    Some(b'*') => dp[(i + 1) * width + j] || dp[i * width + j + 1],
                    Some(b'?') => dp[(i + 1) * width + j + 1],
                    Some(byte) => atom_matches(*atom, *byte) && dp[(i + 1) * width + j + 1],
                },
            };
        }
    }
    dp[0]
}

/// Whether one single-byte atom matches `byte`.
fn atom_matches(atom: Atom, byte: u8) -> bool {
    match atom {
        Atom::Lit(lit) => lit == byte,
        Atom::Alnum | Atom::AlnumStar => byte.is_ascii_alphanumeric(),
    }
}

/// `word` with each brace group replaced by `*`, a widening of every reading.
///
/// Why: fail closed on a brace shape `expand_brace_alternatives` cannot read
/// (a nested comma group, a group past its cap): `*` reaches every string the
/// group could produce, so a deny name inside it still matches.
/// What: a balanced group (nesting counted) becomes one `*`; an unclosed `{`
/// turns the rest of the word into `*`; a stray `}` is dropped.
/// Test: `denies_a_glob_that_can_match_a_deny_name`.
fn widen_braces(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut depth = 0usize;
    for c in word.chars() {
        match c {
            '{' => {
                if depth == 0 {
                    out.push('*');
                }
                depth += 1;
            }
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}
