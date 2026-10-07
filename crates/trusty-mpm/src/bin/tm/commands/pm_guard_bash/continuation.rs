//! Line continuations the shell removes before it reads a command (#9180).
//!
//! Why: bash drops each `\`-newline pair before it splits words, but the
//! guard's segment splitter cuts at every newline. `r\`, newline, `m -rf /`
//! reached each rule as the harmless segments `r\` and `m -rf /`; `echo $\`,
//! newline, `(rm -rf /)` hid a substitution the shell runs; `cu\`, newline,
//! `rl …` hid a network fetch.
//! What: [`joined`] returns the command with every live continuation removed.
//! A rule judges that spelling ALONGSIDE the original, so the join can only
//! add denials — the shape `pm_guard_secret_read`'s program-text scan already
//! uses for the same join (#7266). A continuation stays when it sits inside
//! single quotes, where it is literal, or inside a here-document body a
//! program reads as data, whose text [`super::substitutions`] reads on its own.
//! Test: `joined_removes_each_live_continuation_9180`, and the deny rows in
//! `continuation_tests`.

use super::heredoc::HeredocBodies;
use super::shell_lex::QuoteScan;

/// `command` with each live `\`-newline continuation removed, or `None` when
/// it has none (#9180).
///
/// What: a newline is a continuation when an odd run of `\` precedes it and
/// that `\` is outside single quotes — anywhere at all when the quotes do not
/// balance, the conservative reading — and outside every here-document body
/// [`HeredocBodies::data`] lists. A shell-run body is joined: the shell that
/// runs it removes the pair. A continuation in a comment is joined too, which
/// bash does not do; the original spelling is still judged, so that can only
/// over-deny. `None` is returned when nothing was removed, which also bounds
/// a caller that judges the joined spelling recursively.
/// Test: `joined_removes_each_live_continuation_9180`.
pub(super) fn joined(command: &str) -> Option<String> {
    if !command.contains("\\\n") {
        return None;
    }
    let quotes = QuoteScan::new(command);
    let heredocs = HeredocBodies::scan(command);
    let data: Vec<(usize, usize)> = heredocs.data().iter().map(|b| b.span).collect();
    let bytes = command.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut run = 0usize;
    let mut removed = false;
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        let live = |at: usize| {
            (!quotes.balanced || quotes.allows_substitution(at))
                && !data.iter().any(|&(start, end)| at >= start && at < end)
        };
        if byte == b'\\' && run.is_multiple_of(2) && bytes.get(i + 1) == Some(&b'\n') && live(i) {
            removed = true;
            run = 0;
            i += 2;
            continue;
        }
        run = if byte == b'\\' { run + 1 } else { 0 };
        out.push(byte);
        i += 1;
    }
    // Only ASCII `\` and `\n` bytes were removed, so the text stays UTF-8.
    removed.then(|| String::from_utf8_lossy(&out).into_owned())
}

/// Whether the newline at byte `i` is a continuation the shell joins, so it
/// separates no commands (#9180).
///
/// Why: the segment splitter cut `git \`, newline, `apply x.patch` into `git \`
/// and `apply x.patch`, and no rule saw a `git apply`.
/// What: `true` when an odd run of `\` precedes the newline and no `#` comment
/// is open on its line; bash ends a comment at the newline whatever precedes
/// it, so there the newline still separates. `scan` is the command's quote
/// map; a quoted newline never reaches this check.
/// Test: `split_shell_segments_keep_a_continued_command_whole_9180`.
pub(super) fn joins_at(command: &str, scan: &QuoteScan, i: usize) -> bool {
    let bytes = command.as_bytes();
    let run = bytes[..i].iter().rev().take_while(|b| **b == b'\\').count();
    if run.is_multiple_of(2) {
        return false;
    }
    let start = bytes[..i]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |p| p + 1);
    let comment = (start..i).any(|j| {
        let opens = j == start
            || matches!(
                bytes[j - 1],
                b' ' | b'\t' | b';' | b'&' | b'|' | b'(' | b')'
            );
        bytes[j] == b'#' && opens && (!scan.balanced || scan.is_unquoted(j))
    });
    !comment
}

#[cfg(test)]
#[path = "continuation_tests.rs"]
mod tests;
