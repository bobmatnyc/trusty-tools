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

use super::credential_print::{OperatorCtx, heredoc_operators};
use super::shell_lex::QuoteScan;

/// A here-document delimiter word plus its `<<-` tab-stripping mode.
struct Delimiter {
    word: String,
    /// `<<-` lets the terminator line be indented with tabs.
    strip_tabs: bool,
    /// #8756: a quoted delimiter (`<<'EOF'`) leaves the body unexpanded.
    quoted: bool,
    /// #9150: an [`OperatorCtx::Ambiguous`] `<<`, read loosely.
    ambiguous: bool,
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
    /// #9155: every body whose delimiter was unquoted, data or shell source.
    expanding: Vec<(usize, usize)>,
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
    /// #9155: a code `<<` with no terminator line. The shell runs that body
    /// to the end of input, so every byte stays live as under
    /// [`Abandon::NoConfidence`], but the unquoted-delimiter bodies found up
    /// to there — this one included, to the end of input — still expand.
    Unterminated(Vec<(usize, usize)>),
}

/// Deny reason for a here-document delimiter the guard cannot read (#9150).
pub(crate) const HEREDOC_DELIMITER_REASON: &str = "this command opens a here-document the \
     guard cannot delimit: its delimiter word is not letters, digits, `_`, `.` and `-` (bare, \
     in one pair of quotes, or `\\`-escaped), or its `<<` sits in arithmetic or in a `$(…)` or \
     backtick that closes on the same line while a later line matches the word. The shell may \
     end that body at a different line than the guard finds, so the guard cannot tell which \
     lines run (#9150). Use a plain delimiter such as `<<'EOF'` in plain command position.";

impl HeredocBodies {
    /// Locate every here-document body in `command`.
    ///
    /// What: walks the command line by line. Each line contributes the
    /// delimiters of the here-document operators it opens ([`delimiters_on`]),
    /// and the lines after it are consumed as those bodies in order, each
    /// running to its own terminator line. A delimiter with no terminator line
    /// abandons the whole scan and yields no spans, so an unterminated
    /// here-document and an arithmetic left-shift both leave every byte live;
    /// #9155: an unterminated code `<<` still records the unquoted bodies, its
    /// own to the end of input, in [`HeredocBodies::expanding`].
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
        // #9150: only a `<<` the shell reads as an operator opens a body.
        let ops = heredoc_operators(command);
        if quotes.balanced {
            return Self::settle(Self::collect(command, Some(&quotes), &ops));
        }
        // #8111: an apostrophe in a BODY (`it's`) unbalances the whole-command
        // map, and claiming nothing made that body prose live shell. Retry with
        // each operator line's own quotes; keep the answer only when blanking
        // the bodies it found leaves the rest of the command balanced.
        match Self::collect(command, None, &ops) {
            Ok(found)
                if !found.spans.is_empty()
                    && QuoteScan::new(&blank_spans(command, &found.spans)).balanced =>
            {
                found
            }
            Ok(_) | Err(Abandon::NoConfidence) => Self::empty(),
            Err(why) => Self::settle(Err(why)),
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
            // #9155: claim nothing, but keep what the shell still expands.
            Err(Abandon::Unterminated(expanding)) => Self {
                expanding,
                ..Self::empty()
            },
            Err(why) => Self {
                unscannable: matches!(why, Abandon::Delimiter),
                ..Self::empty()
            },
        }
    }

    /// The line walk behind [`HeredocBodies::scan`]: `quotes` is the
    /// whole-command map, or `None` to read each operator line's quotes alone.
    /// [`Abandon::Unterminated`] back when a delimiter has no terminator line
    /// (#9155), [`Abandon::NoConfidence`] when an operator line's own quotes
    /// do not close; [`Abandon::Delimiter`]
    /// when [`delimiters_on`] cannot read a delimiter word, or an ambiguous
    /// `<<` would claim a body (#9150).
    fn collect(
        command: &str,
        quotes: Option<&QuoteScan>,
        ops: &[(usize, OperatorCtx)],
    ) -> Result<Self, Abandon> {
        let lines = line_spans(command);
        let mut spans = Vec::new();
        let mut frames = Vec::new();
        let mut data = Vec::new();
        let mut expanding = Vec::new();
        let mut line = 0;
        while line < lines.len() {
            let (start, end) = lines[line];
            let operator_line = &command[start..end];
            let delimiters = match quotes {
                Some(quotes) => delimiters_on(operator_line, (start, start), quotes, ops),
                None => {
                    let own = QuoteScan::new(operator_line);
                    // #9150: an unclosed quote in a delimiter word is a
                    // delimiter the shell reads past this line.
                    let read = delimiters_on(operator_line, (start, 0), &own, ops);
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
                if delimiter.ambiguous {
                    // #9150: a shell that reads this `<<` as an operator
                    // ends its body at a line the guard cannot place.
                    if body_span(command, &lines, line, &delimiter).is_some() {
                        return Err(Abandon::Delimiter);
                    }
                    continue;
                }
                let Some(body) = body_span(command, &lines, line, &delimiter) else {
                    // #9155: bash runs an unterminated body to end of input, so
                    // an unquoted one expands there; `X ` never ends `<<X`.
                    let rest = lines.get(line).map_or(command.len(), |l| l.0);
                    if !delimiter.quoted && rest < command.len() {
                        expanding.push((rest, command.len()));
                    }
                    return Err(Abandon::Unterminated(expanding));
                };
                if body.span.0 < body.span.1 {
                    spans.push(body.span);
                    if !delimiter.quoted {
                        // #9155: the shell expands this body whoever runs it.
                        expanding.push(body.span);
                    }
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
            expanding,
            unscannable: false,
        })
    }

    /// The no-confidence result: every byte stays live shell syntax.
    fn empty() -> Self {
        Self {
            spans: Vec::new(),
            frames: Vec::new(),
            data: Vec::new(),
            expanding: Vec::new(),
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

    /// Every claimed body's half-open span, in order (#9155).
    pub(super) fn spans(&self) -> &[(usize, usize)] {
        &self.spans
    }

    /// The data bodies: those whose operator line hands them to something
    /// other than a shell (#8756).
    pub(super) fn data(&self) -> &[DataBody] {
        &self.data
    }

    /// Every body whose delimiter was unquoted, in order (#9155).
    ///
    /// Why: the shell expands `$( … )` and backticks in such a body before any
    /// program reads it, so a `'` there is a literal character, not quoting.
    /// [`Self::data`] omits a body a shell runs (`bash <<X`), which hid a
    /// single-quoted `$(rm -rf /)` from the delete floor.
    /// What: the span of every unquoted-delimiter body, data or shell source;
    /// an unterminated one runs to the end of input, though no span is claimed.
    /// Test: `heredoc_bodies_record_every_expanding_body_9155`,
    /// `heredoc_bodies_expand_an_unterminated_unquoted_body_9155`.
    pub(super) fn expanding(&self) -> &[(usize, usize)] {
        &self.expanding
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
/// terminator). `at` is the line's byte offset in the command and the offset
/// `quotes` was built over: the whole command's, or `0` for the line's own.
///
/// #9150: a `<<` opens a body only where [`heredoc_operators`] read it as
/// shell code; one in a comment, `${…}` text or after a `\` is skipped, and an
/// [`OperatorCtx::Ambiguous`] one is read loosely for [`HeredocBodies::collect`]
/// to refuse. `None` — no confidence — when a code `<<` has a word
/// [`delimiter_word`] does not accept.
/// Test: `heredoc_bodies_ignore_a_here_string`,
/// `heredoc_bodies_ignore_quoted_operator`,
/// `heredoc_bodies_handle_tab_stripped_delimiter`,
/// `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`,
/// `heredoc_bodies_skip_an_operator_outside_code`.
fn delimiters_on(
    line: &str,
    (at, offset): (usize, usize),
    quotes: &QuoteScan,
    ops: &[(usize, OperatorCtx)],
) -> Option<Vec<Delimiter>> {
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
        let Some(&(_, ctx)) = ops.iter().find(|(pos, _)| *pos == at + i) else {
            // #9150: a comment, `${…}` text or `\<<` opens no body.
            i += 2;
            continue;
        };
        let mut j = i + 2;
        let strip_tabs = bytes.get(j) == Some(&b'-');
        if strip_tabs {
            j += 1;
        }
        while matches!(bytes.get(j), Some(b' ' | b'\t')) {
            j += 1;
        }
        let ambiguous = ctx == OperatorCtx::Ambiguous;
        let (word, quoted, end) = if ambiguous {
            loose_word(bytes, j)
        } else {
            delimiter_word(bytes, j)?
        };
        if !word.is_empty() {
            found.push(Delimiter {
                word,
                strip_tabs,
                quoted,
                ambiguous,
            });
        }
        i = end.max(i + 2);
    }
    Some(found)
}

/// The delimiter word starting at `start`: the word, whether any of it was
/// quoted (#8756), and where it ends.
///
/// What: an allowlist (#9150). The word is a run of [`is_word_byte`] bytes,
/// `'…'` or `"…"` around such bytes, and `\` before one, in any
/// concatenation (`E'O'F`); it ends at a blank, `<`, `>`, `|`, `;`, `&`, `)`
/// or the line's end. Anything else — `$`, a backtick, `(`, `<(`, a `\` inside
/// quotes, an empty quoted word, a quote left open — is `None`, because the
/// shell's word there is not one this scan can compare a line with. An empty
/// unquoted word is `Some` and opens no body. `bytes` is the operator line,
/// newline excluded, so a CRLF line's `\r` is its last byte.
///
/// #9150: the credential-print walk reads its operators through this too,
/// so both scanners split a delimiter word one way.
/// Test: `heredoc_bodies_refuse_a_delimiter_split_by_a_word_break`,
/// `credential_print_tests::denies_a_body_behind_a_refused_delimiter`.
pub(super) fn delimiter_word(bytes: &[u8], start: usize) -> Option<(String, bool, usize)> {
    let mut word = String::new();
    let mut quoted = false;
    let mut j = start;
    while let Some(&byte) = bytes.get(j) {
        match byte {
            b'\'' | b'"' => {
                let len = bytes[j + 1..].iter().position(|b| *b == byte)?;
                let inner = &bytes[j + 1..j + 1 + len];
                if inner.is_empty() || !inner.iter().all(|b| is_word_byte(*b)) {
                    return None;
                }
                word.extend(inner.iter().map(|b| char::from(*b)));
                quoted = true;
                j += len + 2;
            }
            b'\\' => {
                let next = bytes.get(j + 1).filter(|b| is_word_byte(**b))?;
                word.push(char::from(*next));
                quoted = true;
                j += 2;
            }
            // A CRLF line keeps its `\r`, as the shell's word does.
            b'\r' if j + 1 == bytes.len() => {
                word.push('\r');
                j += 1;
            }
            // A `<(…)` or `>(…)` is process substitution inside the word.
            b'<' | b'>' if bytes.get(j + 1) == Some(&b'(') => return None,
            b' ' | b'\t' | b'<' | b'>' | b'|' | b';' | b'&' | b')' => break,
            _ if is_word_byte(byte) => {
                word.push(char::from(byte));
                j += 1;
            }
            _ => return None,
        }
    }
    Some((word, quoted, j))
}

/// Whether `byte` may appear in a delimiter word: `[A-Za-z0-9_.-]` (#9150).
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')
}

/// An ambiguous `<<`'s word read loosely — quotes and `\` removed, cut at an
/// unquoted break byte — only to ask whether a later line matches it (#9150).
fn loose_word(bytes: &[u8], start: usize) -> (String, bool, usize) {
    let mut word = Vec::new();
    let mut quote = None;
    let mut j = start;
    while let Some(&byte) = bytes.get(j) {
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => word.push(byte),
            None if byte == b'\'' || byte == b'"' => quote = Some(byte),
            None if byte == b'\\' => {
                j += 1;
                word.extend(bytes.get(j));
            }
            None if is_word_break(byte) => break,
            None => word.push(byte),
        }
        j += 1;
    }
    let word = String::from_utf8_lossy(&word).into_owned();
    (word, true, j.min(bytes.len()))
}

/// Whether `byte` ends a loosely read word.
fn is_word_break(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'<' | b'>' | b'|' | b';' | b'&' | b'(' | b')' | b']'
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
        let last = index + 1 == lines.len();
        if is_terminator(
            &command[*start..*end],
            &delimiter.word,
            delimiter.strip_tabs,
            last,
        ) {
            return Some(Body {
                span: (body_start, *start),
                frame_end: *end,
                next_line: index + 1,
            });
        }
    }
    None
}

/// Whether `line`, newline excluded, ends a body whose delimiter is `word`.
///
/// What: an exact match after `<<-` strips leading tabs. A `\r` is not
/// trimmed (#9150): bash 3.2 and zsh 5.9 keep a CRLF line's `\r` in the
/// delimiter word and in the terminator line alike, so `EOF\r` ends an
/// `EOF\r` body and never an `EOF` one. The one exception is the `last` line
/// of the text, which a caller's `trim()` may have cut from `EOF\r` to `EOF`:
/// no line follows it, so ending the body there leaves nothing hidden.
/// Test: `heredoc_bodies_read_a_crlf_heredoc_as_the_shell_does`,
/// `credential_print_tests::denies_a_body_behind_a_refused_delimiter`.
pub(super) fn is_terminator(line: &str, word: &str, strip_tabs: bool, last: bool) -> bool {
    let line = if strip_tabs {
        line.trim_start_matches('\t')
    } else {
        line
    };
    line == word || (last && line == word.trim_end_matches('\r'))
}

#[cfg(test)]
#[path = "heredoc_tests.rs"]
mod tests;
