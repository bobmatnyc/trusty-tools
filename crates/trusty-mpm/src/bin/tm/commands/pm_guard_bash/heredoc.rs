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
//!
//! #7190 adds a second, narrower answer for the destructive-delete rule:
//! [`mask_allowlisted_heredoc_bodies`] blanks the body of a here-document
//! whose CONSUMER is on the [`STDIN_PROGRAM_INTERPRETERS`] ALLOWLIST, so
//! Python or JavaScript source is not tokenized as shell. The allowlist is the
//! design, not a detail: three earlier attempts on #7190 tried to enumerate
//! the consumers that RE-EXECUTE a body — `eval`, then `read`/`tee`/`>`
//! captures, then pipes — and each enumeration was bypassed by the next
//! capture idiom (`cp /dev/stdin f.sh` then `chmod +x f.sh; ./f.sh` needs no
//! shell token at all). Enumerating capture is unbounded; enumerating the
//! interpreters whose stdin IS their program text is bounded and checkable, so
//! every consumer NOT on the list keeps its body live and scanned.
//! Test: `heredoc_bodies_*` and `allowlisted_*` in this module's `tests`
//! submodule; `has_file_write_redirection_ignores_heredoc_body` and
//! `evaluate_bash_command_allows_readonly_heredoc_script` in the parent's;
//! `allows_a_delete_verb_inside_an_allowlisted_heredoc_body` and siblings in
//! `super::destructive_delete`.

use super::shell_lex::QuoteScan;

/// A here-document delimiter word plus its `<<-` tab-stripping mode.
struct Delimiter {
    word: String,
    /// `<<-` lets the terminator line be indented with tabs.
    strip_tabs: bool,
    /// Whether the delimiter word was quoted (`<<'PY'`, `<<"PY"`, `<<\PY`).
    /// #7190: an unquoted delimiter leaves the body open to parameter and
    /// command substitution, so the text written is not the text run.
    quoted: bool,
    /// Line-local `[start, end)` byte range of the operator, `<<` through the
    /// last byte of the delimiter word (#7190).
    operator: (usize, usize),
}

/// Byte ranges of a command's here-document bodies.
///
/// Why: the guard's byte scanners need one shared answer to "is this byte
/// here-document content rather than shell syntax", computed once per command.
/// What: `spans` holds half-open `[start, end)` ranges covering the text
/// between a here-document's operator line and its terminator line, terminator
/// excluded. `frames` holds the wider separator-suppression ranges #6946 needs
/// — see [`HeredocBodies::suppresses_separator`]. Both are empty whenever
/// [`HeredocBodies::scan`] could not parse the command with confidence.
/// Test: `heredoc_bodies_*`.
pub(super) struct HeredocBodies {
    spans: Vec<(usize, usize)>,
    frames: Vec<(usize, usize)>,
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
                }
                if framing {
                    // The frame opens on the newline that ended the operator
                    // line so that separator too stops splitting.
                    frames.push((body.span.0.saturating_sub(1), body.frame_end));
                }
                line = body.next_line;
            }
        }
        Self { spans, frames }
    }

    /// The no-confidence result: every byte stays live shell syntax.
    fn empty() -> Self {
        Self {
            spans: Vec::new(),
            frames: Vec::new(),
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
            // #7190: a leading quote or backslash is what makes the body inert
            // to expansion.
            let quoted = matches!(bytes.get(word_start), Some(b'\'' | b'"' | b'\\'));
            found.push(Delimiter {
                word,
                strip_tabs,
                quoted,
                operator: (i, j),
            });
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

/// Consumer programs whose here-document body is their own program text
/// (#7190).
///
/// Why: these five read stdin as source in their own language and execute it
/// there; the shell never tokenizes those bytes, so a `rm` written in them is
/// not a shell deletion. Everything else — a shell, a script path, an unknown
/// binary, `cat`, `tee`, `cp /dev/stdin` — either runs the body as shell or
/// can persist it somewhere that later will, so it stays off the list and its
/// body stays scanned.
/// What: BARE program names only. A path-qualified spelling
/// (`/usr/bin/python3`, `./python3`) is deliberately absent: this classifier
/// reads text and cannot tell a real interpreter from a wrapper of the same
/// basename, and denying is the safe direction.
/// Test: `allowlisted_mask_blanks_only_the_body`,
/// `allowlisted_mask_declines_a_consumer_off_the_list`.
const STDIN_PROGRAM_INTERPRETERS: &[&str] = &["node", "perl", "python", "python3", "ruby"];

/// Blank every provably-inert here-document body in `command`, or `None` when
/// there is none to blank (#7190).
///
/// Why: [`super::destructive_delete`] scans each segment's TOKENS for a delete
/// verb, and since #6946 a here-document body stays inside the segment its
/// operator line opened — so a Python script piped through `python3 <<'PY'`
/// was tokenized as shell and its prose denied. Blanking rather than dropping
/// keeps every byte offset, line, and segment boundary identical, so the
/// caller's existing scan needs no offset arithmetic.
/// What: replaces each allowlisted body's bytes with spaces, newlines
/// preserved. The operator line and the terminator line are untouched, so a
/// redirect or a second command there is still live syntax the caller scans.
/// Test: `allowlisted_mask_blanks_only_the_body`,
/// `allowlisted_mask_survives_unbalanced_quotes_in_the_body`.
pub(super) fn mask_allowlisted_heredoc_bodies(command: &str) -> Option<String> {
    let spans = allowlisted_body_spans(command);
    if spans.is_empty() {
        return None;
    }
    let mut bytes = command.as_bytes().to_vec();
    for (start, end) in spans {
        for byte in bytes.get_mut(start..end)? {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8(bytes).ok()
}

/// Byte ranges of the bodies [`mask_allowlisted_heredoc_bodies`] blanks.
///
/// Why: [`HeredocBodies::scan`] abandons the whole command when
/// [`QuoteScan`] reports unbalanced quotes, and an apostrophe inside a body
/// (`print('It\'s …')`) is exactly what does that — the #7190 report. A
/// here-document body is not part of the shell's quoting stream, so this scan
/// tracks quotes over the LIVE lines only and skips each body and its
/// terminator, which leaves an apostrophe in Python source unable to
/// destabilize the parse.
/// What: walks lines in order, accumulating the live ones into a buffer whose
/// [`QuoteScan`] answers "is this `<<` an operator". A line opening exactly
/// one quoted here-document whose consumer passes
/// [`reads_stdin_as_program`] contributes its body span; every other
/// here-document contributes nothing and stays live. Unbalanced quotes on a
/// live line, or a delimiter with no terminator, abandon the scan entirely and
/// return no spans — the fail-closed direction, where the caller scans more
/// text rather than less. A line whose predecessor ended in a backslash is a
/// CONTINUATION, so the words before its `<<` are not the whole command
/// (`sh -s \` then `python3 <<'PY'` runs the body as shell); such a line is
/// never allowlisted.
/// Test: `allowlisted_mask_declines_an_unterminated_body`,
/// `allowlisted_mask_declines_a_consumer_off_the_list`,
/// `allowlisted_mask_declines_an_unquoted_delimiter`,
/// `allowlisted_mask_declines_a_continued_operator_line`.
fn allowlisted_body_spans(command: &str) -> Vec<(usize, usize)> {
    let lines = line_spans(command);
    let mut live = String::new();
    let mut spans = Vec::new();
    let mut line = 0;
    let mut continued = false;
    while line < lines.len() {
        let (start, end) = lines[line];
        let text = &command[start..end];
        let was_continued = continued;
        continued = ends_with_line_continuation(text);
        let offset = live.len();
        live.push_str(text);
        live.push('\n');
        let quotes = QuoteScan::new(&live);
        if !quotes.balanced {
            return Vec::new();
        }
        let delimiters = delimiters_on(text, offset, &quotes);
        line += 1;
        if delimiters.is_empty() {
            continue;
        }
        // One here-document per line, or the operator line carries text after
        // the first delimiter word and `reads_stdin_as_program` rejects it
        // anyway. Stating the count keeps that implicit case explicit.
        let inert = !was_continued
            && delimiters.len() == 1
            && delimiters[0].quoted
            && reads_stdin_as_program(text, &delimiters[0]);
        for delimiter in &delimiters {
            let Some(body) = body_span(command, &lines, line, delimiter) else {
                return Vec::new();
            };
            if inert && body.span.0 < body.span.1 {
                spans.push(body.span);
            }
            line = body.next_line;
        }
    }
    spans
}

/// Whether `line` ends in a backslash that joins it to the next line (#7190).
///
/// What: counts the trailing backslashes; an odd count continues the line, an
/// even count is an escaped backslash that ends it. A trailing `\r` is ignored
/// so a CRLF command reads the same as an LF one.
/// Test: `allowlisted_mask_declines_a_continued_operator_line`.
fn ends_with_line_continuation(line: &str) -> bool {
    line.trim_end_matches('\r')
        .chars()
        .rev()
        .take_while(|c| *c == '\\')
        .count()
        % 2
        == 1
}

/// Whether an operator line is a single allowlisted interpreter reading its
/// program from this here-document (#7190).
///
/// What: everything after the delimiter word must be whitespace — that one
/// rule rejects an output redirect (`> f.sh`), a pipe (`| sh`), a second
/// command (`; ./f.sh`), and a second here-document. Everything BEFORE the
/// `<<` must shlex-split to an [`STDIN_PROGRAM_INTERPRETERS`] program whose
/// only argument, if any, is `-`; a `-c` string, a script path, a leading
/// `VAR=value`, and any wrapper or pipeline all fail that test, so the body
/// stays live.
/// Test: `allowlisted_mask_declines_a_redirected_or_piped_operator_line`,
/// `allowlisted_mask_declines_a_consumer_off_the_list`.
fn reads_stdin_as_program(line: &str, delimiter: &Delimiter) -> bool {
    let (operator_start, operator_end) = delimiter.operator;
    if !line[operator_end..].trim().is_empty() {
        return false;
    }
    let Some(argv) = shlex::split(&line[..operator_start]) else {
        return false;
    };
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    if !STDIN_PROGRAM_INTERPRETERS.contains(&program.as_str()) {
        return false;
    }
    args.is_empty() || (args.len() == 1 && args[0] == "-")
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

    /// #7190: the body is blanked, the operator and terminator lines are not,
    /// and the masked text is the same length so byte offsets still line up.
    #[test]
    fn allowlisted_mask_blanks_only_the_body() {
        let command = "python3 <<'PY'\nrm -rf /\nPY";
        let masked = mask_allowlisted_heredoc_bodies(command).expect("body masked");
        assert_eq!(masked.len(), command.len(), "offsets must be preserved");
        assert_eq!(masked, "python3 <<'PY'\n        \nPY");
    }

    /// #7190: every consumer off [`STDIN_PROGRAM_INTERPRETERS`] keeps its body
    /// live — a shell, a capture target, a path-qualified interpreter, and an
    /// interpreter whose stdin is not its program.
    #[test]
    fn allowlisted_mask_declines_a_consumer_off_the_list() {
        for command in [
            "bash <<'EOF'\nrm -rf /\nEOF",
            "cat <<'EOF'\nrm -rf /\nEOF",
            "tee /tmp/x.sh <<'EOF'\nrm -rf /\nEOF",
            "cp /dev/stdin /tmp/x.sh <<'EOF'\nrm -rf /\nEOF",
            "/usr/bin/python3 <<'PY'\nrm -rf /\nPY",
            "python3 -c 'pass' <<'PY'\nrm -rf /\nPY",
            "python3 script.py <<'PY'\nrm -rf /\nPY",
            "PYTHONPATH=/x python3 <<'PY'\nrm -rf /\nPY",
        ] {
            assert_eq!(
                mask_allowlisted_heredoc_bodies(command),
                None,
                "expected no mask for: {command}"
            );
        }
    }

    /// #7190: an unquoted delimiter leaves the body open to expansion, so the
    /// text written is not the text run.
    #[test]
    fn allowlisted_mask_declines_an_unquoted_delimiter() {
        for command in ["python3 <<PY\nrm -rf /\nPY", "python3 <<-PY\nrm -rf /\nPY"] {
            assert_eq!(mask_allowlisted_heredoc_bodies(command), None);
        }
    }

    /// #7190: anything after the delimiter word — a redirect, a pipe, a second
    /// command — can persist or re-execute the body, so it is not inert.
    #[test]
    fn allowlisted_mask_declines_a_redirected_or_piped_operator_line() {
        for command in [
            "python3 <<'PY' > /tmp/x.sh\nrm -rf /\nPY",
            "python3 <<'PY' | sh\nrm -rf /\nPY",
            "python3 <<'PY' ; ./x.sh\nrm -rf /\nPY",
            "python3 <<'A' <<'B'\nrm -rf /\nA\nrm -rf /\nB",
        ] {
            assert_eq!(
                mask_allowlisted_heredoc_bodies(command),
                None,
                "expected no mask for: {command}"
            );
        }
    }

    /// #7190's reported shape: an apostrophe in the body unbalances the
    /// command's quotes, which is what defeated [`HeredocBodies::scan`]. The
    /// live-line scan is unaffected because a body is not part of the shell's
    /// quoting stream.
    #[test]
    fn allowlisted_mask_survives_unbalanced_quotes_in_the_body() {
        let command = "python3 <<'PY'\nprint('It\\'s time to find it')\nPY";
        let masked = mask_allowlisted_heredoc_bodies(command).expect("body masked");
        assert!(!masked.contains("find"), "body text must be blanked");
        assert!(masked.starts_with("python3 <<'PY'"));
        assert!(masked.ends_with("\nPY"));
    }

    /// #7190: a backslash continuation puts the real command on the previous
    /// line, so the words before `<<` are not the whole command — `sh -s \`
    /// then `python3 <<'PY'` runs `sh -s`, which reads the body as shell. An
    /// even number of trailing backslashes is an escaped backslash rather than
    /// a continuation, so that line is still allowlisted.
    #[test]
    fn allowlisted_mask_declines_a_continued_operator_line() {
        for command in [
            "sh -s \\\npython3 <<'PY'\nrm -rf /\nPY",
            "true \\\npython3 <<'PY'\nrm -rf /\nPY",
        ] {
            assert_eq!(
                mask_allowlisted_heredoc_bodies(command),
                None,
                "expected no mask for: {command}"
            );
        }
        assert!(
            mask_allowlisted_heredoc_bodies("echo a\\\\\npython3 <<'PY'\nrm -rf /\nPY").is_some(),
            "an escaped backslash does not continue the line"
        );
    }

    /// #7190 fail-closed arm: with no terminator the body's extent is unknown,
    /// so nothing is masked and the whole remainder stays live.
    #[test]
    fn allowlisted_mask_declines_an_unterminated_body() {
        assert_eq!(
            mask_allowlisted_heredoc_bodies("python3 <<'PY'\nrm -rf /\n"),
            None
        );
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
