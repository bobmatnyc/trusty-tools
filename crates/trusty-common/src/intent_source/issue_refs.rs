//! Issue references in free text: every `#N` or `owner/repo#N` a text links.
//!
//! Why: [`extract_pr_ticket`](super::extract_pr_ticket) returns one id, and it
//! accepts a PR body only through [`is_ticketed`](super::is_ticketed), so
//! `Refs #9192` links nothing and `ADR-0043` links as a JIRA id. trusty-review
//! needs every issue a PR body links, in order (#9197). This module is new so
//! that `linkage.rs` stays unchanged.
//! What: [`extract_issue_refs`] masks fenced code blocks, inline code spans and
//! HTML comments, then reads each line for a link keyword followed by a list
//! of refs. It is a std-only scanner; it uses no regex.
//! Test: `issue_refs_*`, one test per example row of #9197.

use std::collections::HashSet;

/// An issue reference found in free text.
///
/// Why: trusty-review fetches each linked issue, and a cross-repo ref must
/// name its repository.
/// What: `number` is the issue number, never 0. `owner_repo` is `None` for a
/// bare `#N` and `Some((owner, repo))` for `owner/repo#N`. It is not
/// `#[non_exhaustive]`, because trusty-review builds it in tests.
/// Test: `issue_refs_owner_repo_form`, `issue_refs_closes_single`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IssueRef {
    /// The issue number, 1 or greater.
    pub number: u64,
    /// `None` for a bare `#N` (same repository as the text). `Some((owner, repo))` for `owner/repo#N`.
    pub owner_repo: Option<(String, String)>,
}

/// Single-word link keywords, compared case-insensitively as whole words.
/// `part of` is two words and is matched in [`keyword_end`].
const KEYWORDS: &[&str] = &[
    "fix",
    "fixes",
    "fixed",
    "close",
    "closes",
    "closed",
    "resolve",
    "resolves",
    "resolved",
    "ref",
    "refs",
    "reference",
    "references",
    "see",
];

/// The most digits an issue number may carry; 18 digits always fit a `u64`.
const MAX_DIGITS: usize = 18;

/// Every issue ref `text` links, in order of first occurrence, without duplicates.
///
/// Why: trusty-review fetches the body of every issue a PR body links (#9197,
/// epic #9191). [`extract_pr_ticket`](super::extract_pr_ticket) returns one id
/// and reads `ADR-0043` as a ticket, so it cannot serve.
/// What: masks fenced code blocks (three or more backticks or tildes), inline
/// code spans and HTML comments. Then, per line, it finds each whole-word,
/// case-insensitive link keyword (`fix`, `fixes`, `fixed`, `close`, `closes`,
/// `closed`, `resolve`, `resolves`, `resolved`, `ref`, `refs`, `reference`,
/// `references`, `part of`, `see`) and reads the refs that directly follow it:
/// an optional `:`, spaces or tabs, then `#N` or `owner/repo#N` items
/// separated by `,`, `, and`, `and` or `&`. The list stops at the first token
/// that is not a ref. `N` is 1-18 digits, not 0, and not followed by a letter,
/// digit or `_`. JIRA ids, `AB#N` and URLs never match. A duplicate on
/// `(number, owner_repo)` keeps its first position.
/// Test: `issue_refs_dedupe_in_order`, `issue_refs_code_and_comments_ignored`,
/// `issue_refs_malformed_numbers`, `issue_refs_*` (one per example row, #9197).
#[must_use]
pub fn extract_issue_refs(text: &str) -> Vec<IssueRef> {
    let masked = mask_ignored(text);
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for line in masked.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        for found in scan_line(&chars) {
            if seen.insert(found.clone()) {
                refs.push(found);
            }
        }
    }
    refs
}

/// Copy `text` with every fenced block, code span and HTML comment replaced
/// by a line break, so nothing inside one links and no list runs across one.
fn mask_ignored(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(char, usize)> = None;
    let mut in_comment = false;
    for line in text.split('\n') {
        match fence {
            Some((ch, len)) => {
                if fence_closes(line, ch, len) {
                    fence = None;
                }
            }
            None => {
                let opener = if in_comment { None } else { fence_opener(line) };
                match opener {
                    Some(open) => fence = Some(open),
                    None => in_comment = mask_inline(line, in_comment, &mut out),
                }
            }
        }
        out.push('\n');
    }
    out
}

/// The fence character and run length when `line` opens a fenced code block:
/// at most three spaces of indent, then three or more `` ` `` or `~`.
fn fence_opener(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let run = rest.chars().take_while(|c| *c == ch).count();
    if run < 3 {
        return None;
    }
    // A backtick fence's info string holds no backtick; ```a``` is a code span.
    if ch == '`' && rest[run..].contains('`') {
        return None;
    }
    Some((ch, run))
}

/// Whether `line` closes a fence opened with `len` copies of `ch`.
fn fence_closes(line: &str, ch: char, len: usize) -> bool {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let run = rest.chars().take_while(|c| *c == ch).count();
    run >= len && rest[run..].trim().is_empty()
}

/// Append `line` to `out` with its code spans and HTML comments replaced by a
/// line break. `in_comment` says whether a comment is open at the line start;
/// the return value says whether one is still open at its end.
fn mask_inline(line: &str, mut in_comment: bool, out: &mut String) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if in_comment {
            match find_seq(&chars, i, &['-', '-', '>']) {
                Some(end) => {
                    in_comment = false;
                    out.push('\n');
                    i = end + 3;
                }
                None => return true,
            }
            continue;
        }
        if chars[i..].starts_with(&['<', '!', '-', '-']) {
            in_comment = true;
            out.push('\n');
            i += 4;
            continue;
        }
        if chars[i] == '`' {
            let run = run_end(&chars, i, |c| c == '`') - i;
            match closing_backticks(&chars, i + run, run) {
                Some(close) => {
                    out.push('\n');
                    i = close + run;
                }
                None => {
                    out.extend(&chars[i..i + run]);
                    i += run;
                }
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    in_comment
}

/// Start of the first `seq` in `chars` at or after `from`.
fn find_seq(chars: &[char], from: usize, seq: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i..].starts_with(seq))
}

/// Start of the first backtick run of exactly `len` at or after `from`.
fn closing_backticks(chars: &[char], from: usize, len: usize) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] != '`' {
            i += 1;
            continue;
        }
        let end = run_end(chars, i, |c| c == '`');
        if end - i == len {
            return Some(i);
        }
        i = end;
    }
    None
}

/// Every ref in one masked line, in order, duplicates kept.
fn scan_line(chars: &[char]) -> Vec<IssueRef> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_word(chars[i]) || (i > 0 && is_word(chars[i - 1])) {
            i += 1;
            continue;
        }
        let word_end = run_end(chars, i, is_word);
        i = match keyword_end(chars, i, word_end) {
            Some(kw_end) => parse_ref_list(chars, kw_end, &mut found),
            None => word_end,
        };
    }
    found
}

/// End of the link keyword that starts at `start`, whose first word ends at
/// `word_end`; `None` when the word is not a keyword.
fn keyword_end(chars: &[char], start: usize, word_end: usize) -> Option<usize> {
    let word: String = chars[start..word_end].iter().collect();
    if KEYWORDS.iter().any(|k| word.eq_ignore_ascii_case(k)) {
        return Some(word_end);
    }
    if !word.eq_ignore_ascii_case("part") {
        return None;
    }
    let of_start = skip_blanks(chars, word_end);
    if of_start == word_end {
        return None;
    }
    let of_end = run_end(chars, of_start, is_word);
    let of: String = chars[of_start..of_end].iter().collect();
    of.eq_ignore_ascii_case("of").then_some(of_end)
}

/// Read the ref list that follows a keyword ending at `kw_end` into `found`.
/// Returns where scanning resumes: after the last ref, or `kw_end` for none.
fn parse_ref_list(chars: &[char], kw_end: usize, found: &mut Vec<IssueRef>) -> usize {
    let mut pos = kw_end;
    if chars.get(pos) == Some(&':') {
        pos += 1;
    }
    let start = skip_blanks(chars, pos);
    if start == pos {
        return kw_end;
    }
    let Some((first, mut end)) = parse_ref(chars, start) else {
        return kw_end;
    };
    found.push(first);
    while let Some(next) = separator_end(chars, end) {
        match parse_ref(chars, next) {
            Some((issue, after)) => {
                found.push(issue);
                end = after;
            }
            None => break,
        }
    }
    end
}

/// Where the next ref starts when a list separator (`,`, `, and`, `and`, `&`)
/// follows position `end`; `None` when no separator does.
fn separator_end(chars: &[char], end: usize) -> Option<usize> {
    let p = skip_blanks(chars, end);
    match chars.get(p) {
        Some(',') => {
            let q = skip_blanks(chars, p + 1);
            Some(after_and(chars, q).unwrap_or(q))
        }
        Some('&') => Some(skip_blanks(chars, p + 1)),
        _ => after_and(chars, p),
    }
}

/// Position after a whole-word, case-insensitive `and` at `p` and its blanks.
fn after_and(chars: &[char], p: usize) -> Option<usize> {
    if p > 0 && is_word(chars[p - 1]) {
        return None;
    }
    let end = run_end(chars, p, is_word);
    let word: String = chars[p..end].iter().collect();
    word.eq_ignore_ascii_case("and")
        .then(|| skip_blanks(chars, end))
}

/// The ref `#N` or `owner/repo#N` starting at `p`, and the position after it.
fn parse_ref(chars: &[char], p: usize) -> Option<(IssueRef, usize)> {
    if p > 0 && is_word(chars[p - 1]) {
        return None;
    }
    if chars.get(p) == Some(&'#') {
        let (number, end) = parse_number(chars, p + 1)?;
        let issue = IssueRef {
            number,
            owner_repo: None,
        };
        return Some((issue, end));
    }
    let owner_end = run_end(chars, p, |c| c.is_ascii_alphanumeric() || c == '-');
    if owner_end == p || chars.get(owner_end) != Some(&'/') {
        return None;
    }
    let repo_start = owner_end + 1;
    let repo_end = run_end(chars, repo_start, |c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
    });
    if repo_end == repo_start || chars.get(repo_end) != Some(&'#') {
        return None;
    }
    let (number, end) = parse_number(chars, repo_end + 1)?;
    let owner: String = chars[p..owner_end].iter().collect();
    let repo: String = chars[repo_start..repo_end].iter().collect();
    let issue = IssueRef {
        number,
        owner_repo: Some((owner, repo)),
    };
    Some((issue, end))
}

/// The issue number at `p` and the position after it: 1-18 ASCII digits, not
/// 0, and not followed by a letter, digit or `_`.
fn parse_number(chars: &[char], p: usize) -> Option<(u64, usize)> {
    let end = run_end(chars, p, |c| c.is_ascii_digit());
    let digits = end - p;
    if digits == 0 || digits > MAX_DIGITS {
        return None;
    }
    if chars.get(end).is_some_and(|&c| is_word(c)) {
        return None;
    }
    let text: String = chars[p..end].iter().collect();
    let number: u64 = text.parse().ok()?;
    (number != 0).then_some((number, end))
}

/// A letter, digit or `_`: the characters a word boundary separates.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// First position at or after `from` whose char fails `pred`.
fn run_end(chars: &[char], from: usize, pred: impl Fn(char) -> bool) -> usize {
    (from..chars.len())
        .find(|&i| !pred(chars[i]))
        .unwrap_or(chars.len())
}

/// First position at or after `from` that is not a space or tab.
fn skip_blanks(chars: &[char], from: usize) -> usize {
    run_end(chars, from, |c| c == ' ' || c == '\t')
}
