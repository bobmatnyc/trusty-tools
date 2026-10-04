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
//! cannot parse with confidence: unbalanced quotes outside every body (#8111
//! lets an apostrophe INSIDE a body through), or a `<<` whose delimiter
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
    /// #8756: a quoted delimiter (`<<'EOF'`) leaves the body unexpanded.
    quoted: bool,
}

/// One here-document body its operator line hands to something other than a
/// shell (#8756).
///
/// What: `span` is the body's half-open byte range, `operator_line` the range
/// of the line that opened it, and `expands` is `true` when the delimiter was
/// unquoted, so the shell runs every `$( … )` and backtick in the body.
pub(crate) struct DataBody {
    pub(crate) span: (usize, usize),
    pub(crate) operator_line: (usize, usize),
    pub(crate) expands: bool,
}

/// Byte ranges of a command's here-document bodies.
///
/// Why: the guard's byte scanners need one shared answer to "is this byte
/// here-document content rather than shell syntax", computed once per command.
/// What: `spans` holds half-open `[start, end)` ranges covering the text
/// between a here-document's operator line and its terminator line, terminator
/// excluded. `frames` holds the wider separator-suppression ranges #6946 needs
/// — see [`HeredocBodies::suppresses_separator`]. `data` holds the subset
/// of `spans` whose operator line hands the body to something other than a
/// shell, which is the subset [`split_heredoc_bodies`] may lift out of the
/// command text (#7266). All three are empty whenever [`HeredocBodies::scan`]
/// could not parse the command with confidence.
/// Test: `heredoc_bodies_*`.
pub(super) struct HeredocBodies {
    spans: Vec<(usize, usize)>,
    frames: Vec<(usize, usize)>,
    data: Vec<DataBody>,
    /// #9150: a delimiter word this scan cannot split as the shell does.
    unscannable: bool,
}

/// Why [`HeredocBodies::collect`] gave up on a command.
enum Abandon {
    /// No terminator line, or an operator line's own quotes do not close:
    /// claim nothing and leave every byte live (an arithmetic `<<` lands here).
    NoConfidence,
    /// #9150: a delimiter word the shell reads differently from this scan.
    Delimiter,
}

/// Deny reason for a here-document delimiter the guard cannot read (#9150).
pub(crate) const HEREDOC_DELIMITER_REASON: &str = "this command opens a here-document whose \
     delimiter word is quoted or escaped across a space, tab, `<`, `>`, `|`, `;`, `&`, `(` or \
     `)`, or whose quote or escape never closes on the `<<` line. The shell ends that body at a \
     different line than the guard can find, so the guard cannot tell which lines run (#9150). \
     Use a plain delimiter such as `<<'EOF'`.";

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
        if quotes.balanced {
            return Self::settle(Self::collect(command, Some(&quotes)));
        }
        // #8111: an apostrophe in a BODY (`it's`) unbalances the whole-command
        // map, and claiming nothing made that body prose live shell. Retry with
        // each operator line's own quotes; keep the answer only when blanking
        // the bodies it found leaves the rest of the command balanced.
        match Self::collect(command, None) {
            Ok(found)
                if !found.spans.is_empty()
                    && QuoteScan::new(&blank_spans(command, &found.spans)).balanced =>
            {
                found
            }
            Ok(_) | Err(Abandon::NoConfidence) => Self::empty(),
            Err(Abandon::Delimiter) => Self::settle(Err(Abandon::Delimiter)),
        }
    }

    /// Whether `command` opens a here-document whose delimiter this scan
    /// cannot read as the shell does (#9150).
    ///
    /// Why: such a scan claims no bodies, and every rule then reads every byte
    /// as live — but the guard still cannot say where the shell's body ends, so
    /// [`super::unclassifiable_command`] refuses the command outright.
    /// Test: `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`.
    pub(super) fn is_unscannable(&self) -> bool {
        self.unscannable
    }

    /// A [`HeredocBodies::collect`] result as a scan: an abandoned scan claims
    /// nothing, and #9150 records a delimiter it could not read.
    fn settle(collected: Result<Self, Abandon>) -> Self {
        match collected {
            Ok(found) => found,
            Err(why) => Self {
                unscannable: matches!(why, Abandon::Delimiter),
                ..Self::empty()
            },
        }
    }

    /// The line walk behind [`HeredocBodies::scan`]: `quotes` is the
    /// whole-command map, or `None` to read each operator line's quotes alone.
    /// [`Abandon::NoConfidence`] back when a delimiter has no terminator line,
    /// or an operator line's own quotes do not close; [`Abandon::Delimiter`]
    /// when [`delimiters_on`] cannot read a delimiter word (#9150).
    fn collect(command: &str, quotes: Option<&QuoteScan>) -> Result<Self, Abandon> {
        let lines = line_spans(command);
        let mut spans = Vec::new();
        let mut frames = Vec::new();
        let mut data = Vec::new();
        let mut line = 0;
        while line < lines.len() {
            let (start, end) = lines[line];
            let operator_line = &command[start..end];
            let delimiters = match quotes {
                Some(quotes) => delimiters_on(operator_line, start, quotes),
                None => {
                    let own = QuoteScan::new(operator_line);
                    // #9150: an unclosed quote in a delimiter word is a
                    // delimiter the shell reads past this line.
                    let read = delimiters_on(operator_line, 0, &own);
                    if !own.balanced && read.is_some() {
                        return Err(Abandon::NoConfidence);
                    }
                    read
                }
            };
            let delimiters = delimiters.ok_or(Abandon::Delimiter)?;
            let framing = !delimiters.is_empty() && !line_runs_a_shell(operator_line);
            line += 1;
            for delimiter in delimiters {
                let body =
                    body_span(command, &lines, line, &delimiter).ok_or(Abandon::NoConfidence)?;
                if body.span.0 < body.span.1 {
                    spans.push(body.span);
                    if framing {
                        // #7266: a body its operator line does not hand to a
                        // shell is data, so no word in it is a path the
                        // command opens.
                        data.push(DataBody {
                            span: body.span,
                            operator_line: (start, end),
                            expands: !delimiter.quoted,
                        });
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
        Ok(Self {
            spans,
            frames,
            data,
            unscannable: false,
        })
    }

    /// The no-confidence result: every byte stays live shell syntax.
    fn empty() -> Self {
        Self {
            spans: Vec::new(),
            frames: Vec::new(),
            data: Vec::new(),
            unscannable: false,
        }
    }

    /// The end of the body that starts at `idx`, when its operator line hands
    /// it to a shell (`bash <<'EOF'`), so the body is shell source (#8730).
    ///
    /// Test: `write_targets_read_a_shell_heredoc_body`.
    pub(super) fn shell_body_starting_at(&self, idx: usize) -> Option<usize> {
        self.spans
            .iter()
            .find(|span| span.0 == idx && !self.data.iter().any(|d| d.span == **span))
            .map(|span| span.1)
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
    let spans: Vec<(usize, usize)> = data_bodies(command).iter().map(|b| b.span).collect();
    let texts = spans
        .iter()
        .map(|&(s, e)| command[s..e].to_string())
        .collect();
    (blank_spans(command, &spans), texts)
}

/// Every here-document body in `command` that is stdin data rather than shell
/// source, in order (#8756).
///
/// Test: `data_bodies_record_whether_the_delimiter_was_quoted`.
pub(crate) fn data_bodies(command: &str) -> Vec<DataBody> {
    HeredocBodies::scan(command).data
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

/// `command` with every byte inside `spans` replaced by a space, newlines kept,
/// so every byte offset survives (#7266, #8756).
pub(crate) fn blank_spans(command: &str, spans: &[(usize, usize)]) -> String {
    let mut bytes = command.as_bytes().to_vec();
    for &(start, end) in spans {
        for byte in &mut bytes[start..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
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
///
/// #9150: `None` — no confidence — when a delimiter word quotes or escapes a
/// word-break byte (`<<'A B'`, `<<A\ B`), or a quote or `\` in it does not
/// close on this line. The shell keeps that whole word and ends the body at a
/// different line than a cut at the break byte would.
/// Test: `heredoc_bodies_ignore_a_here_string`,
/// `heredoc_bodies_ignore_quoted_operator`,
/// `heredoc_bodies_handle_tab_stripped_delimiter`,
/// `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`.
fn delimiters_on(line: &str, offset: usize, quotes: &QuoteScan) -> Option<Vec<Delimiter>> {
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
        // #9150: a cut at a quoted break byte ended the body too late.
        j = word_end(bytes, j)?;
        let raw = &line[word_start..j];
        let word: String = raw
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '\\'))
            .collect();
        if !word.is_empty() {
            // #8756: any quoting on the word keeps the body unexpanded.
            let quoted = word.len() != raw.len();
            found.push(Delimiter {
                word,
                strip_tabs,
                quoted,
            });
        }
        i = j.max(i + 2);
    }
    Some(found)
}

/// The end of the delimiter word starting at `start`: the first unquoted,
/// unescaped word-break byte, or the end of the line.
///
/// What: `None` when a quoted or escaped byte inside the word is a word-break
/// byte, or a quote or trailing `\` is still open at the end of the line —
/// both shapes the shell reads past where a plain cut would stop (#9150).
fn word_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    let mut j = start;
    while j < bytes.len() {
        let byte = bytes[j];
        match quote {
            Some(open) if byte == open => quote = None,
            Some(b'"') | None if byte == b'\\' => {
                // An escape of nothing (line continuation) or of a break byte.
                j += 1;
                if bytes.get(j).is_none_or(|next| is_word_break(*next)) {
                    return None;
                }
            }
            Some(_) if is_word_break(byte) => return None,
            Some(_) => {}
            None if byte == b'\'' || byte == b'"' => quote = Some(byte),
            None if is_word_break(byte) => return Some(j),
            None => {}
        }
        j += 1;
    }
    quote.is_none().then_some(j)
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

    /// #8756: only an unquoted delimiter expands its body, and each data body
    /// knows the line that opened it.
    #[test]
    fn data_bodies_record_whether_the_delimiter_was_quoted() {
        for (command, expands) in [
            ("cat <<EOF\n$(date)\nEOF", true),
            ("cat <<'EOF'\n$(date)\nEOF", false),
            ("cat <<\"EOF\"\n$(date)\nEOF", false),
            ("cat <<\\EOF\n$(date)\nEOF", false),
            ("cat <<-E'O'F\n$(date)\n\tEOF", false),
        ] {
            let bodies = data_bodies(command);
            assert_eq!(bodies.len(), 1, "{command:?}");
            assert_eq!(bodies[0].expands, expands, "{command:?}");
            let (start, end) = bodies[0].operator_line;
            assert!(command[start..end].starts_with("cat <<"), "{command:?}");
        }
    }

    /// #9150: bash and zsh keep a quoted or escaped break byte in the
    /// delimiter word, so a cut there ends the body at the wrong line. Each
    /// such word, and one whose quote or `\` never closes on its line, makes
    /// the scan unscannable and claim nothing; a plain delimiter does not.
    #[test]
    fn heredoc_bodies_refuse_a_delimiter_split_by_a_word_break() {
        for command in [
            "cat <<'A B'\nx\nA B\necho \"$(rm -rf /)\"\nA",
            "cat <<'A>B'\nx\nA>B\nA",
            "cat <<'A<B'\nx\nA<B\nA",
            "cat <<-'A B'\n\tx\n\tA B\n\tA",
            "cat <<'A\tB'\nx\nA\tB\nA",
            "cat <<\"A;B\"\nx\nA;B\nA",
            "cat <<A\\ B\nx\nA B\nA",
            "cat <<A\\\nB\n$(rm -rf /)\nA\nAB",
            "cat <<'A\nB'\nx\nA",
            // #8111 retry path: an apostrophe in the body unbalances the map.
            "cat <<'A B'\nit's\nA B\nA",
        ] {
            let bodies = HeredocBodies::scan(command);
            assert!(bodies.is_unscannable(), "{command:?}");
            assert!(bodies.spans.is_empty(), "{command:?} claims nothing");
            assert_eq!(
                super::super::unclassifiable_command(command),
                Some(HEREDOC_DELIMITER_REASON),
                "{command:?}"
            );
        }
        for command in [
            "cat <<'EOF'\n$(rm -rf /)\nEOF\necho benign",
            "cat <<\"EOF\" > out\nx\nEOF",
            "cat <<\\EOF\nx\nEOF",
            "cat <<-E'O'F\nx\n\tEOF",
            "cat <<'EOF';echo hi\nx\nEOF",
            "cat <<'EOF'\nit's\nEOF",
            "echo $((1 << 3))",
            "python3 <<'PY'\nprint(1)\n",
        ] {
            assert!(
                !HeredocBodies::scan(command).is_unscannable(),
                "{command:?}"
            );
            assert_eq!(
                super::super::unclassifiable_command(command),
                None,
                "{command:?}"
            );
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
