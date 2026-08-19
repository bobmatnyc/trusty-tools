//! Here-document body detection for the PM Bash guard (issue #5356).
//!
//! Why: [`super::has_file_write_redirection`] treats any unquoted `>` as a
//! file-write redirection, and [`super::shell_lex::QuoteScan`] only knows `'`
//! and `"`. A here-document body is quoted data to the shell but plain text to
//! that scan, so a `>` that is script content — a Python `len(k) > 3`
//! comparison, an `->` arrow in prose — read as a redirect. The read-only
//! command was then classified [`super::SHELL_EDIT_REASON`], which is
//! budget-eligible: three such reads were ALLOWED silently while consuming the
//! PM's per-turn file-change budget, and the fourth was denied as "PM
//! file-change budget 3/3 used this turn" with zero files changed.
//! What: [`HeredocBodies::scan`] returns the byte ranges of a command's
//! here-document bodies, so a scanner can skip a metacharacter that is body
//! content. Only the BODY is claimed — the operator line keeps its live
//! syntax, so `python3 <<'PY' > out.rs` still reads as a redirect. The scan
//! claims nothing at all (preserving the pre-#5356 over-deny) whenever it
//! cannot parse with confidence: unbalanced quotes, or a `<<` whose delimiter
//! never appears on a line of its own, which is also what an arithmetic
//! `$((1 << 3))` looks like.
//! Test: `heredoc_bodies_*` in this module's `tests` submodule;
//! `has_file_write_redirection_ignores_heredoc_body` and
//! `evaluate_bash_command_allows_readonly_heredoc_script` in the parent's.

use super::shell_lex::QuoteScan;

/// A here-document delimiter word plus its `<<-` tab-stripping mode.
struct Delimiter {
    word: String,
    /// `<<-` lets the terminator line be indented with tabs.
    strip_tabs: bool,
}

/// Byte ranges of a command's here-document bodies.
///
/// Why: the guard's byte scanners need one shared answer to "is this byte
/// here-document content rather than shell syntax", computed once per command.
/// What: half-open `[start, end)` ranges covering the text between a
/// here-document's operator line and its terminator line, terminator excluded.
/// Empty whenever [`HeredocBodies::scan`] could not parse the command with
/// confidence.
/// Test: `heredoc_bodies_*`.
pub(super) struct HeredocBodies {
    spans: Vec<(usize, usize)>,
}

impl HeredocBodies {
    /// Locate every here-document body in `command`.
    ///
    /// What: walks the command line by line. Each line contributes the
    /// delimiters of the here-document operators it opens ([`delimiters_on`]),
    /// and the lines after it are consumed as those bodies in order, each
    /// running to its own terminator line. A delimiter with no terminator line
    /// abandons the whole scan and yields no spans, so an unterminated
    /// here-document and an arithmetic left-shift both leave every byte live.
    /// Test: `heredoc_bodies_cover_a_quoted_delimiter_body`,
    /// `heredoc_bodies_claim_nothing_when_unterminated`,
    /// `heredoc_bodies_span_two_heredocs_on_one_line`.
    pub(super) fn scan(command: &str) -> Self {
        let quotes = QuoteScan::new(command);
        if !quotes.balanced {
            return Self { spans: Vec::new() };
        }
        let lines = line_spans(command);
        let mut spans = Vec::new();
        let mut line = 0;
        while line < lines.len() {
            let (start, end) = lines[line];
            let delimiters = delimiters_on(&command[start..end], start, &quotes);
            line += 1;
            for delimiter in delimiters {
                let Some((body, next)) = body_span(command, &lines, line, &delimiter) else {
                    return Self { spans: Vec::new() };
                };
                if body.0 < body.1 {
                    spans.push(body);
                }
                line = next;
            }
        }
        Self { spans }
    }

    /// Whether byte `idx` is here-document body content.
    ///
    /// Test: `heredoc_bodies_exclude_the_operator_line`.
    pub(super) fn contains(&self, idx: usize) -> bool {
        self.spans
            .iter()
            .any(|(start, end)| idx >= *start && idx < *end)
    }
}

/// Half-open `[start, end)` byte ranges of each line, newline excluded.
fn line_spans(command: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (idx, byte) in command.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, idx));
            start = idx + 1;
        }
    }
    spans.push((start, command.len()));
    spans
}

/// The here-document delimiters opened by one line, in the order the shell
/// reads their bodies.
///
/// What: scans for an unquoted `<<` that is not the `<<<` here-string
/// operator, honours the `<<-` tab-stripping form, and reads the delimiter
/// word with its quoting removed (`<<'PY'`, `<<"PY"`, and `<<PY` name the same
/// terminator). `offset` maps a line-local index onto `quotes`, which was
/// built over the whole command.
/// Test: `heredoc_bodies_ignore_a_here_string`,
/// `heredoc_bodies_ignore_quoted_operator`,
/// `heredoc_bodies_handle_tab_stripped_delimiter`.
fn delimiters_on(line: &str, offset: usize, quotes: &QuoteScan) -> Vec<Delimiter> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if !quotes.is_unquoted(offset + i) || bytes[i] != b'<' || bytes[i + 1] != b'<' {
            i += 1;
            continue;
        }
        if bytes.get(i + 2) == Some(&b'<') {
            // `<<<` is a here-string: its word is on this line, no body follows.
            i += 3;
            continue;
        }
        let mut j = i + 2;
        let strip_tabs = bytes.get(j) == Some(&b'-');
        if strip_tabs {
            j += 1;
        }
        while matches!(bytes.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        let word_start = j;
        while j < bytes.len() && !is_word_break(bytes[j]) {
            j += 1;
        }
        let word: String = line[word_start..j]
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '\\'))
            .collect();
        if !word.is_empty() {
            found.push(Delimiter { word, strip_tabs });
        }
        i = j.max(i + 2);
    }
    found
}

/// Whether `byte` ends a here-document delimiter word.
fn is_word_break(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'<' | b'>' | b'|' | b';' | b'&' | b'(' | b')'
    )
}

/// The body span for one delimiter, starting at line `from`, plus the line
/// index the next body starts from.
///
/// What: `None` when no later line consists solely of the delimiter word (with
/// leading tabs allowed under `<<-`) — the caller then claims nothing.
/// Test: `heredoc_bodies_claim_nothing_when_unterminated`.
fn body_span(
    command: &str,
    lines: &[(usize, usize)],
    from: usize,
    delimiter: &Delimiter,
) -> Option<((usize, usize), usize)> {
    let body_start = lines.get(from)?.0;
    for (index, (start, end)) in lines.iter().enumerate().skip(from) {
        let text = command[*start..*end].trim_end_matches('\r');
        let candidate = if delimiter.strip_tabs {
            text.trim_start_matches('\t')
        } else {
            text
        };
        if candidate == delimiter.word {
            return Some(((body_start, *start), index + 1));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body of a `<<'PY'` script — the #5356 reproduction — is claimed.
    #[test]
    fn heredoc_bodies_cover_a_quoted_delimiter_body() {
        let command = "python3 <<'PY'\nprint(len(k) > 3)\nPY";
        let bodies = HeredocBodies::scan(command);
        let gt = command.find('>').expect("comparison operator");
        assert!(bodies.contains(gt), "the script's `>` must be body content");
    }

    /// A redirect on the operator line stays live syntax.
    #[test]
    fn heredoc_bodies_exclude_the_operator_line() {
        let command = "python3 <<'PY' > out.rs\nprint(1)\nPY";
        let bodies = HeredocBodies::scan(command);
        let gt = command.find('>').expect("redirect");
        assert!(!bodies.contains(gt), "the operator line must stay live");
    }

    /// `<<<` is a here-string; nothing follows it as a body.
    #[test]
    fn heredoc_bodies_ignore_a_here_string() {
        let command = "grep x <<< 'a > b'\necho done > f.rs";
        let bodies = HeredocBodies::scan(command);
        let redirect = command.rfind('>').expect("redirect");
        assert!(!bodies.contains(redirect));
    }

    /// An unterminated delimiter — and an arithmetic `<<`, which looks the
    /// same — claims nothing, so the pre-#5356 over-deny is preserved.
    #[test]
    fn heredoc_bodies_claim_nothing_when_unterminated() {
        let unterminated = "python3 <<'PY'\nprint(1)\n";
        assert!(!HeredocBodies::scan(unterminated).contains(20));
        let shift = "echo $((1 << 3))\necho x > f.rs";
        let redirect = shift.rfind('>').expect("redirect");
        assert!(!HeredocBodies::scan(shift).contains(redirect));
    }

    /// `<<-` allows a tab-indented terminator.
    #[test]
    fn heredoc_bodies_handle_tab_stripped_delimiter() {
        let command = "cat <<-EOF\n\ta > b\n\tEOF";
        let bodies = HeredocBodies::scan(command);
        let gt = command.find('>').expect("arrow");
        assert!(bodies.contains(gt));
    }

    /// A `<<` inside quotes is argument text, not an operator.
    #[test]
    fn heredoc_bodies_ignore_quoted_operator() {
        let command = "grep -n '<<EOF' f\necho x > g.rs";
        let bodies = HeredocBodies::scan(command);
        let redirect = command.rfind('>').expect("redirect");
        assert!(!bodies.contains(redirect));
    }

    /// Two here-documents opened on one line consume their bodies in order.
    #[test]
    fn heredoc_bodies_span_two_heredocs_on_one_line() {
        let command = "diff <<A <<B\na > b\nA\nc > d\nB";
        let bodies = HeredocBodies::scan(command);
        let first = command.find('>').expect("first arrow");
        let second = command.rfind('>').expect("second arrow");
        assert!(bodies.contains(first), "first body claimed");
        assert!(bodies.contains(second), "second body claimed");
    }
}
