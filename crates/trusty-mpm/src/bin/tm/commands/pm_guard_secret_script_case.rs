//! Shapes in a script body that the credential rules read as a script run or
//! a secret file although neither exists (#10548). Split out of
//! `pm_guard_secret_script` for the 500-SLOC cap.
//!
//! Why: the nested scan cuts a body at every unquoted `|` and takes any word
//! with a `/` in a segment's program position as a script the body runs. A
//! `case` arm `a | b)` spells its alternatives with unquoted `|`, so each path
//! pattern became a "script"; a workflow `.yml` named there was judged as an
//! `sh` body and refused `scripts/select-test-crates.sh` and
//! `scripts/detect-docs-only.sh`. A lone `/` between two pipes, cut out of a
//! `sed` expression, refused as "not a regular file". A Python `token.strip()`
//! in an embedded program read as a file named `token.strip` and refused
//! `scripts/check_rustdoc_links.sh`.
//! What: [`blank_case_patterns`] blanks the pattern list of every `case` arm;
//! [`cannot_run_directly`] says a path run directly can never execute;
//! [`mask_method_calls`] rewrites a word-family method call so it is no file.
//! Fail-open check: both only remove a candidate the shell cannot run. A
//! pattern list holding a command substitution is left in place, and every
//! read or metadata error keeps the candidate, so `read_script` still refuses.
//! Test: `pm_guard_secret_fp_10548_tests.rs`.

use std::borrow::Cow;
use std::path::Path;

/// String and mapping methods a program calls on a value; never a file
/// extension (#10548).
const VALUE_METHODS: &[&str] = &[
    "strip",
    "lstrip",
    "rstrip",
    "split",
    "rsplit",
    "partition",
    "rpartition",
    "lower",
    "upper",
    "replace",
    "startswith",
    "endswith",
    "encode",
    "decode",
    "join",
    "format",
    "get",
    "items",
    "keys",
    "values",
    "append",
    "extend",
    "trim",
    "trimStart",
    "trimEnd",
    "toLowerCase",
    "toUpperCase",
    "includes",
    "indexOf",
];

/// `text` with the `.` of each `<stem>.<method>(` call turned into `_`, where
/// the stem is a plain identifier carrying `token`, `secret` or `credential`
/// and the method is one of [`VALUE_METHODS`].
///
/// Why: see the module doc. The #7266 scan cuts at `(`, so `token.strip()`
/// reached it as `token.strip`, a `token*` name with an extension.
/// What: `token_strip` keeps the family core but no extension and no `/`, so
/// the word-family rule needs a directory in front of it, as for a bare
/// `token`. A stem with a `/` or a `.` before it, or a method not listed, is
/// left alone: `cat vault/token.get(` and `open("token.json")` still deny.
/// Test: `a_method_call_on_a_token_value_is_no_file_10548`.
pub(crate) fn mask_method_calls(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut dots = Vec::new();
    for (at, _) in text.match_indices('.') {
        let stem_start = bytes[..at]
            .iter()
            .rposition(|&b| !ident(b))
            .map_or(0, |p| p + 1);
        let stem = text[stem_start..at].to_ascii_lowercase();
        let before = stem_start.checked_sub(1).map(|p| bytes[p]);
        let family = ["token", "secret", "credential"]
            .iter()
            .any(|f| stem.contains(f));
        let method = VALUE_METHODS.iter().any(|m| {
            text[at + 1..]
                .strip_prefix(m)
                .is_some_and(|rest| rest.starts_with('('))
        });
        if family && method && !matches!(before, Some(b'/' | b'.' | b'~')) {
            dots.push(at);
        }
    }
    if dots.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = bytes.to_vec();
    for at in dots {
        out[at] = b'_';
    }
    Cow::Owned(String::from_utf8(out).unwrap_or_else(|_| text.to_string()))
}

/// `text` with the pattern list of every `case` arm (`a | b)`, `(a)`) blanked
/// to spaces, newlines kept.
///
/// Why: see the module doc.
/// What: walks `text` word by word, quote-aware. `case WORD in` opens an arm
/// list; after `in` and after each `;;`, `;&` or `;;&` the shell reads a
/// pattern list or `esac`, so that is the only place a list is blanked. A list
/// is words joined by unquoted `|` and closed by an unquoted `)`; one holding
/// a newline, `;`, `&`, `<`, `>`, `(`, a backtick or `$(` is not blanked and
/// stays in the scan.
/// Test: `a_multi_pattern_case_arm_runs_no_script_10548`,
/// `a_case_pattern_with_a_substitution_is_still_scanned_10548`.
pub(crate) fn blank_case_patterns(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    // One entry per open `case`: whether a pattern list comes next.
    let mut arms: Vec<bool> = Vec::new();
    let mut awaiting_in = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // A comment is skipped whole: its words open no `case` and close no list.
        if bytes[i] == b'#' {
            i = bytes[i..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |n| i + n);
            continue;
        }
        if awaiting_in == 0 && arms.last() == Some(&true) {
            let (word, _) = read_word(bytes, i);
            if word.trim_end_matches([';', ')']) != "esac" {
                if let Some(end) = pattern_list_end(bytes, i) {
                    for b in &mut out[i..=end] {
                        if *b != b'\n' {
                            *b = b' ';
                        }
                    }
                    i = end + 1;
                }
                if let Some(top) = arms.last_mut() {
                    *top = false;
                }
                continue;
            }
        }
        let (word, end) = read_word(bytes, i);
        match word.trim_end_matches([';', ')']) {
            "case" => awaiting_in += 1,
            "in" if awaiting_in > 0 => {
                awaiting_in -= 1;
                arms.push(true);
            }
            "esac" => {
                arms.pop();
            }
            _ => {}
        }
        if [";;", ";&", ";;&"].iter().any(|t| word.ends_with(t))
            && let Some(top) = arms.last_mut()
        {
            *top = true;
        }
        i = end;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// One whitespace-delimited word starting at `start`, quote-aware, and the
/// index just past it.
fn read_word(bytes: &[u8], start: usize) -> (String, usize) {
    let (mut i, mut quote) = (start, None);
    while let Some(&b) = bytes.get(i) {
        match quote {
            Some(q) if b == q => quote = None,
            Some(b'"') if b == b'\\' => i += 1,
            Some(_) => {}
            None if b == b'\\' => i += 1,
            None if b == b'\'' || b == b'"' => quote = Some(b),
            None if b.is_ascii_whitespace() => break,
            None => {}
        }
        i += 1;
    }
    let end = i.min(bytes.len());
    (
        String::from_utf8_lossy(&bytes[start..end]).into_owned(),
        end,
    )
}

/// The index of the `)` closing a pattern list that starts at `start`, or
/// `None` when the text there is not a plain pattern list.
fn pattern_list_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    // POSIX allows one leading `(` before the first pattern.
    if bytes.get(i) == Some(&b'(') {
        i += 1;
    }
    let (mut quote, mut in_word, mut words_since_bar) = (None, false, 0usize);
    while let Some(&b) = bytes.get(i) {
        if let Some(q) = quote {
            if b == q {
                quote = None;
            } else if q == b'"' && (b == b'`' || (b == b'$' && bytes.get(i + 1) == Some(&b'('))) {
                return None;
            } else if q == b'"' && b == b'\\' {
                i += 1;
            }
            i += 1;
            continue;
        }
        match b {
            b')' => return (words_since_bar + usize::from(in_word) == 1).then_some(i),
            b'|' => {
                if words_since_bar + usize::from(in_word) != 1 {
                    return None;
                }
                (in_word, words_since_bar) = (false, 0);
            }
            b' ' | b'\t' => {
                words_since_bar += usize::from(in_word);
                in_word = false;
            }
            b'\\' => {
                in_word = true;
                i += 1;
            }
            b'\'' | b'"' => {
                in_word = true;
                quote = Some(b);
            }
            b'\n' | b';' | b'&' | b'<' | b'>' | b'(' | b'`' => return None,
            b'$' if bytes.get(i + 1) == Some(&b'(') => return None,
            _ => in_word = true,
        }
        if words_since_bar > 1 {
            return None;
        }
        i += 1;
    }
    None
}

/// Whether a file a body runs by its path can never execute: a directory, or
/// a regular file with no execute bit while `text` changes no mode (`chmod`).
///
/// Why: see the module doc. `./ci.yml` with mode 0644 fails with EACCES and
/// `/` fails with EISDIR, so neither runs a byte.
/// What: follows symlinks; any metadata error answers `false`, which keeps
/// the candidate so `read_script` refuses it.
/// Test: `a_pipe_inside_a_quoted_expression_runs_no_script_10548`,
/// `a_non_executable_file_made_executable_is_still_judged_10548`.
pub(crate) fn cannot_run_directly(path: &Path, text: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    meta.is_dir()
        || (meta.is_file() && meta.permissions().mode() & 0o111 == 0 && !text.contains("chmod"))
}
