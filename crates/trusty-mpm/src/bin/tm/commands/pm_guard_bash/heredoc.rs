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
/// What: `spans` holds half-open `[start, end)` ranges covering the text
/// between a here-document's operator line and its terminator line, terminator
/// excluded. `frames` holds the wider separator-suppression ranges #6946 needs
/// — see [`HeredocBodies::suppresses_separator`]. `data_spans` holds the subset
/// of `spans` whose operator line hands the body to something other than a
/// shell, which is the subset [`split_heredoc_bodies`] may lift out of the
/// command text (#7266). All three are empty whenever [`HeredocBodies::scan`]
/// could not parse the command with confidence.
/// Test: `heredoc_bodies_*`.
pub(super) struct HeredocBodies {
    spans: Vec<(usize, usize)>,
    frames: Vec<(usize, usize)>,
    data_spans: Vec<(usize, usize)>,
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
    ///
    /// #6946: each body also contributes a `frames` entry, unless its operator
    /// line names a shell ([`line_runs_a_shell`]) — see
    /// [`HeredocBodies::suppresses_separator`].
    /// Test: `heredoc_bodies_cover_a_quoted_delimiter_body`,
    /// `heredoc_bodies_claim_nothing_when_unterminated`,
    /// `heredoc_bodies_span_two_heredocs_on_one_line`,
    /// `heredoc_frames_cover_the_terminator_line`,
    /// `heredoc_frames_are_empty_for_a_shell_operator_line`.
    pub(super) fn scan(command: &str) -> Self {
        let quotes = QuoteScan::new(command);
        if !quotes.balanced {
            return Self::empty();
        }
        let lines = line_spans(command);
        let mut spans = Vec::new();
        let mut frames = Vec::new();
        let mut data_spans = Vec::new();
        let mut line = 0;
        while line < lines.len() {
            let (start, end) = lines[line];
            let operator_line = &command[start..end];
            let delimiters = delimiters_on(operator_line, start, &quotes);
            let framing = !delimiters.is_empty() && !line_runs_a_shell(operator_line);
            line += 1;
            for delimiter in delimiters {
                let Some(body) = body_span(command, &lines, line, &delimiter) else {
                    return Self::empty();
                };
                if body.span.0 < body.span.1 {
                    spans.push(body.span);
                    if framing {
                        // #7266: a body its operator line does not hand to a
                        // shell is data, so no word in it is a path the
                        // command opens.
                        data_spans.push(body.span);
                    }
                }
                if framing {
                    // The frame opens on the newline that ended the operator
                    // line so that separator too stops splitting.
                    frames.push((body.span.0.saturating_sub(1), body.frame_end));
                }
                line = body.next_line;
            }
        }
        Self {
            spans,
            frames,
            data_spans,
        }
    }

    /// The no-confidence result: every byte stays live shell syntax.
    fn empty() -> Self {
        Self {
            spans: Vec::new(),
            frames: Vec::new(),
            data_spans: Vec::new(),
        }
    }

    /// Whether byte `idx` is here-document body content.
    ///
    /// Test: `heredoc_bodies_exclude_the_operator_line`.
    pub(super) fn contains(&self, idx: usize) -> bool {
        self.spans
            .iter()
            .any(|(start, end)| idx >= *start && idx < *end)
    }

    /// Whether byte `idx` is here-document framing, so a separator there is
    /// data rather than a composition operator (#6946).
    ///
    /// Why: [`super::split_shell_segments_raw`] cuts on every bare newline, so
    /// `cat <<'EOF' > notes.md` / `git commit -m wip` / `EOF` reached the
    /// ADR-0049 composed-commit guard as three segments and the whole call was
    /// denied — with no git process anywhere in it. The body is stdin data to
    /// `cat`; nothing in it runs.
    /// What: `true` across a body, the newline that opened it, and its
    /// terminator line — the terminator is delimiter text, not a segment of its
    /// own. `false` for every byte of the operator line, which keeps its live
    /// syntax, and for every body whose operator line names a shell, where the
    /// body IS shell source and its separators must still split.
    /// Test: `heredoc_frames_cover_the_terminator_line`,
    /// `heredoc_frames_are_empty_for_a_shell_operator_line`,
    /// `split_shell_segments_keeps_a_heredoc_body_whole`.
    pub(super) fn suppresses_separator(&self, idx: usize) -> bool {
        self.frames
            .iter()
            .any(|(start, end)| idx >= *start && idx < *end)
    }
}

/// Separate `command` into the text a word scan may read as argv and the
/// here-document BODIES it must read as program text instead (#7266).
///
/// Why: `crate::commands::pm_guard_secret_read` scans every word of a command
/// for a secret-bearing filename, and it scanned here-document bodies too. A
/// body is stdin data — `cat >> verb.rs <<'RSEOF'` carrying `struct VerbStub {`
/// opens no file the body names — but the scan read `{` as a path word, the
/// shared brace expander cannot resolve a lone `{`, and the guard fails CLOSED
/// on that, so writing ordinary Rust denied with "naming `{` in a `cat`
/// command is refused". [`super::has_file_write_redirection`] already frames
/// bodies through [`HeredocBodies`]; this is the same framing, one
/// implementation, for the second scanner.
/// What: replaces the bytes of every `data_spans` body with spaces, keeping
/// `\n` so the operator line, the terminator line, every byte offset and the
/// whole segment structure survive unchanged, and returns those bodies' text
/// beside it. A body whose operator line names a shell ([`line_runs_a_shell`])
/// stays in place, because it IS shell source and its own segments must still
/// be classified. A scan that claimed nothing returns `command` unchanged and
/// no bodies, so an unparsable here-document keeps the whole pre-#7266 scan.
/// Test: `splits_a_heredoc_body_out_of_the_argv_text`,
/// `leaves_a_shell_heredoc_body_in_the_argv_text`,
/// `splits_nothing_without_a_heredoc`.
pub(crate) fn split_heredoc_bodies(command: &str) -> (String, Vec<String>) {
    let bodies = HeredocBodies::scan(command);
    if bodies.data_spans.is_empty() {
        return (command.to_string(), Vec::new());
    }
    let mut argv_text = String::with_capacity(command.len());
    let mut texts = Vec::with_capacity(bodies.data_spans.len());
    let mut cursor = 0;
    for &(start, end) in &bodies.data_spans {
        argv_text.push_str(&command[cursor..start]);
        let body = &command[start..end];
        for c in body.chars() {
            if c == '\n' {
                argv_text.push('\n');
            } else {
                // Space per BYTE keeps offsets identical to `command`.
                argv_text.extend(std::iter::repeat_n(' ', c.len_utf8()));
            }
        }
        texts.push(body.to_string());
        cursor = end;
    }
    argv_text.push_str(&command[cursor..]);
    (argv_text, texts)
}

/// Whether a here-document operator line hands its body to a shell.
///
/// Why (#6946 fail-open check): `bash <<'EOF'` runs the body as shell source,
/// so suppressing the body's separators there would hide `rm -rf …` from the
/// destructive-delete rule that catches it today. `cat`, `python3`, `jq` and
/// the rest consume the body as data instead.
/// What: `true` when any whitespace-delimited token of the line, basename
/// stripped, is one of [`QuoteScan`]'s sibling [`super::shell_lex::DASH_C_SHELLS`].
/// Scanning every token rather than the leading one keeps `sudo bash`,
/// `env - bash` and `foo && bash <<EOF` on the conservative side; a false
/// positive only restores the pre-#6946 splitting.
/// Test: `heredoc_frames_are_empty_for_a_shell_operator_line`.
fn line_runs_a_shell(line: &str) -> bool {
    line.split_whitespace().any(|token| {
        let base = token.rsplit('/').next().unwrap_or(token);
        super::shell_lex::DASH_C_SHELLS.contains(&base)
    })
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

/// One located here-document body.
struct Body {
    /// Half-open body range, terminator line excluded.
    span: (usize, usize),
    /// End of the terminator line, its own newline excluded (#6946).
    frame_end: usize,
    /// Line index the next body on the same operator line starts from.
    next_line: usize,
}

/// The body for one delimiter, starting at line `from`.
///
/// What: `None` when no later line consists solely of the delimiter word (with
/// leading tabs allowed under `<<-`) — the caller then claims nothing.
/// Test: `heredoc_bodies_claim_nothing_when_unterminated`.
fn body_span(
    command: &str,
    lines: &[(usize, usize)],
    from: usize,
    delimiter: &Delimiter,
) -> Option<Body> {
    let body_start = lines.get(from)?.0;
    for (index, (start, end)) in lines.iter().enumerate().skip(from) {
        let text = command[*start..*end].trim_end_matches('\r');
        let candidate = if delimiter.strip_tabs {
            text.trim_start_matches('\t')
        } else {
            text
        };
        if candidate == delimiter.word {
            return Some(Body {
                span: (body_start, *start),
                frame_end: *end,
                next_line: index + 1,
            });
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

    /// #6946: the frame runs from the newline that opened the body through the
    /// end of the terminator line, and stops there.
    #[test]
    fn heredoc_frames_cover_the_terminator_line() {
        let command = "cat <<'EOF' > f\nbody\nEOF\nnext";
        let bodies = HeredocBodies::scan(command);
        let opening_newline = command.find('\n').expect("operator line ends");
        assert!(bodies.suppresses_separator(opening_newline));
        assert!(!bodies.suppresses_separator(opening_newline - 1));
        let terminator = command.find("EOF\nnext").expect("terminator line");
        for idx in terminator..terminator + 3 {
            assert!(bodies.suppresses_separator(idx));
        }
        // The newline after the terminator is a live separator again.
        assert!(!bodies.suppresses_separator(terminator + 3));
    }

    /// #6946 fail-open check: a body handed to a shell is shell source, so it
    /// gets no frame and its separators keep splitting.
    #[test]
    fn heredoc_frames_are_empty_for_a_shell_operator_line() {
        for command in [
            "bash <<'EOF'\nbody\nEOF",
            "sudo /bin/sh <<EOF\nbody\nEOF",
            "true && zsh <<EOF\nbody\nEOF",
        ] {
            let bodies = HeredocBodies::scan(command);
            let newline = command.find('\n').expect("operator line ends");
            assert!(
                !bodies.suppresses_separator(newline),
                "{command} should not be framed"
            );
            // The #5356 body span is still claimed — only framing differs.
            assert!(bodies.contains(newline + 1), "{command} body span");
        }
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

    /// #7266: the body leaves the argv text as spaces and comes back as text.
    #[test]
    fn splits_a_heredoc_body_out_of_the_argv_text() {
        let command = "cat >> verb.rs <<'RSEOF'\nstruct VerbStub {\n}\nRSEOF";
        let (argv_text, bodies) = split_heredoc_bodies(command);
        assert_eq!(argv_text.len(), command.len(), "byte offsets are preserved");
        assert!(argv_text.starts_with("cat >> verb.rs <<'RSEOF'"));
        assert!(argv_text.trim_end().ends_with("RSEOF"), "{argv_text:?}");
        assert!(!argv_text.contains('{'), "{argv_text:?}");
        assert_eq!(bodies, vec!["struct VerbStub {\n}\n".to_string()]);
    }

    /// #7266 fail-open check: a body the operator line hands to a shell IS
    /// shell source, so it stays in place for the segment classifiers.
    #[test]
    fn leaves_a_shell_heredoc_body_in_the_argv_text() {
        let command = "bash <<'EOF'\nls .env\nEOF";
        let (argv_text, bodies) = split_heredoc_bodies(command);
        assert_eq!(argv_text, command);
        assert!(bodies.is_empty());
    }

    /// #7266: no here-document, and an unterminated one, both leave the
    /// command byte-identical — the pre-fix scan runs unchanged.
    #[test]
    fn splits_nothing_without_a_heredoc() {
        for command in ["awk '{print}' f", "cat <<EOF\nno terminator"] {
            let (argv_text, bodies) = split_heredoc_bodies(command);
            assert_eq!(argv_text, command);
            assert!(bodies.is_empty(), "{command:?}");
        }
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
