//! What a Claude Code pane's input box holds: nothing, a suggestion, or a draft (#8407).
//!
//! Why: Claude Code draws an auto-generated next-prompt suggestion in the input
//! box as dim text (SGR 2). A plain `capture-pane -p` shows it exactly like text
//! a human typed, so a supervisor read two suggestions as user drafts and
//! deferred work about 20 minutes each time. Telling them apart took a
//! `capture-pane -e` and a search for `ESC[2m`.
//! What: [`classify_input_box`] reads a capture taken WITH escape codes and
//! returns an [`InputBox`], so a caller never parses escapes itself. The input
//! line is the last line carrying Claude Code's `❯` prompt glyph — the same
//! rule as the Architect's `scripts/input-state.py`. A box this cannot read is
//! `None`, never a guess.
//! Test: inline `#[cfg(test)]` module.

use serde::{Deserialize, Serialize};

/// The glyph Claude Code draws at the start of its input line.
const PROMPT: char = '❯';

/// What the input box holds.
///
/// Why: "empty", "a suggestion the harness wrote" and "a draft a human typed"
/// call for different actions; only the last one must never be overwritten.
/// What: serialized lowercase (`empty`, `suggestion`, `typed`).
/// Test: `input_box_serializes_lowercase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InputBox {
    /// No text after the prompt.
    Empty,
    /// Only dim (SGR 2) text: Claude Code's own next-prompt suggestion.
    Suggestion,
    /// Text drawn at normal intensity: a draft someone typed.
    Typed,
}

impl InputBox {
    /// The wire spelling, as `tm sessions output` prints it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Suggestion => "suggestion",
            Self::Typed => "typed",
        }
    }
}

/// Classify the input box in a pane capture taken with `capture-pane -e`.
///
/// Why: see the module docs.
/// What: finds the last line holding `❯` and reads what follows it, plus any
/// continuation lines up to the box's closing `─` rule (only when that rule is
/// there). It walks the text's SGR state: no visible character is
/// [`InputBox::Empty`]; only dim characters is [`InputBox::Suggestion`]; any
/// character at normal intensity is [`InputBox::Typed`]. A reverse-video
/// character is Claude Code's cursor and is skipped, unless it is the only
/// visible one — then the box reads as typed, because the safe mistake is to
/// leave a draft alone. No `❯` line is `None`.
/// Test: `an_empty_box_is_empty`, `dim_text_is_a_suggestion`,
/// `plain_text_is_typed`, `a_cursor_over_a_suggestion_is_still_a_suggestion`,
/// `mixed_dim_and_plain_text_is_typed`, `a_multiline_draft_reads_its_continuation`,
/// `no_prompt_line_is_none`, `a_lone_cursor_character_reads_as_typed`,
/// `colour_operands_are_never_read_as_attributes`.
pub fn classify_input_box(capture: &str) -> Option<InputBox> {
    let lines: Vec<&str> = capture.lines().collect();
    let at = lines.iter().rposition(|l| l.contains(PROMPT))?;
    let (_, first) = lines[at].split_once(PROMPT)?;
    let mut region = vec![first];
    // Continuation lines belong to the box only when its closing rule follows.
    let rest = &lines[at + 1..];
    if let Some(end) = rest.iter().position(|l| is_rule(l)) {
        region.extend_from_slice(&rest[..end]);
    }
    let mut seen = Visible::default();
    for line in region {
        seen.scan(line);
    }
    Some(seen.verdict())
}

/// Whether `line`, escapes removed, is a horizontal rule (`────`).
fn is_rule(line: &str) -> bool {
    let text = strip_escapes(line);
    let text = text.trim();
    !text.is_empty()
        && text
            .chars()
            .all(|c| matches!(c, '─' | '╰' | '╯' | '╭' | '╮'))
}

/// `line` with every `ESC [ … <final>` sequence removed.
fn strip_escapes(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape(&mut chars);
        } else {
            out.push(c);
        }
    }
    out
}

/// Consume one escape sequence after its `ESC`, returning a CSI's parameters
/// when its final byte is `m` (an SGR).
fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    if chars.peek() != Some(&'[') {
        chars.next();
        return None;
    }
    chars.next();
    let mut params = String::new();
    for c in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            return (c == 'm').then_some(params);
        }
        params.push(c);
    }
    None
}

/// What one pass over the box's text saw.
#[derive(Default)]
struct Visible {
    dim: bool,
    reverse: bool,
    normal_chars: usize,
    dim_chars: usize,
    cursor_chars: usize,
}

impl Visible {
    /// Walk `line`, tracking SGR dim (2 / 22) and reverse (7 / 27); `0` or an
    /// empty parameter list resets both.
    fn scan(&mut self, line: &str) {
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                if let Some(params) = skip_escape(&mut chars) {
                    self.apply_sgr(&params);
                }
                continue;
            }
            if c.is_whitespace() || c == '│' {
                continue;
            }
            if self.reverse {
                self.cursor_chars += 1;
            } else if self.dim {
                self.dim_chars += 1;
            } else {
                self.normal_chars += 1;
            }
        }
    }

    /// Apply one SGR parameter list.
    ///
    /// #8407: the list is walked as a sequence, not as a bag of numbers. An
    /// extended colour (38 foreground, 48 background, 58 underline) consumes
    /// its own operands — `5;n` or `2;r;g;b` — so a colour component of `2` or
    /// `7` is never read as dim or reverse. A colon group (`38:2::r:g:b`,
    /// `4:3`) is one self-contained attribute and is skipped whole. An empty
    /// parameter is `0`, as ECMA-48 defines it.
    fn apply_sgr(&mut self, params: &str) {
        let mut it = params.split(';');
        while let Some(p) = it.next() {
            if p.contains(':') {
                continue;
            }
            let code = if p.is_empty() {
                Some(0)
            } else {
                p.parse::<u32>().ok()
            };
            match code {
                Some(0) => (self.dim, self.reverse) = (false, false),
                Some(2) => self.dim = true,
                Some(22) => self.dim = false,
                Some(7) => self.reverse = true,
                Some(27) => self.reverse = false,
                Some(38 | 48 | 58) => match it.next() {
                    Some("5") => {
                        it.next();
                    }
                    Some("2") => {
                        it.nth(2);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    fn verdict(&self) -> InputBox {
        if self.normal_chars > 0 {
            InputBox::Typed
        } else if self.dim_chars > 0 {
            InputBox::Suggestion
        } else if self.cursor_chars > 0 {
            InputBox::Typed
        } else {
            InputBox::Empty
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "────────────────────────────────────────";

    fn pane(input: &str) -> String {
        format!("● Done.\n\n{RULE}\n❯ {input}\n{RULE}\n  ? for shortcuts\n")
    }

    #[test]
    fn an_empty_box_is_empty() {
        assert_eq!(classify_input_box(&pane("")), Some(InputBox::Empty));
        // Claude Code's cursor on an empty line is a reverse-video space.
        assert_eq!(
            classify_input_box(&pane("\u{1b}[7m \u{1b}[27m")),
            Some(InputBox::Empty)
        );
    }

    /// Why (#8407): the two misread incidents — a dim suggestion read as a
    /// user draft.
    /// Test: itself.
    #[test]
    fn dim_text_is_a_suggestion() {
        let capture = pane("\u{1b}[2myes to both, go ahead\u{1b}[22m");
        assert_eq!(classify_input_box(&capture), Some(InputBox::Suggestion));
    }

    #[test]
    fn plain_text_is_typed() {
        let capture = pane("#8372: non_exhaustive, no major bump\u{1b}[7m \u{1b}[0m");
        assert_eq!(classify_input_box(&capture), Some(InputBox::Typed));
    }

    #[test]
    fn a_cursor_over_a_suggestion_is_still_a_suggestion() {
        let capture = pane("\u{1b}[7my\u{1b}[27m\u{1b}[2mes to both\u{1b}[0m");
        assert_eq!(classify_input_box(&capture), Some(InputBox::Suggestion));
    }

    #[test]
    fn mixed_dim_and_plain_text_is_typed() {
        let capture = pane("go \u{1b}[2mahead\u{1b}[22m");
        assert_eq!(classify_input_box(&capture), Some(InputBox::Typed));
    }

    #[test]
    fn a_multiline_draft_reads_its_continuation() {
        let capture =
            format!("{RULE}\n❯ \n  second line typed\n{RULE}\n  \u{1b}[2m? for shortcuts\n");
        assert_eq!(classify_input_box(&capture), Some(InputBox::Typed));
        // With no closing rule, only the prompt line is read: the footer below
        // an unterminated box is not input.
        let open = "❯ \n  ⏵⏵ accept edits on\n";
        assert_eq!(classify_input_box(open), Some(InputBox::Empty));
    }

    /// Why: a capture with no input line (tmux absent, a shell pane, a
    /// harness not drawn yet) must say "unknown", never "empty".
    /// Test: itself.
    #[test]
    fn no_prompt_line_is_none() {
        assert_eq!(classify_input_box(""), None);
        assert_eq!(classify_input_box("$ ls\nfoo bar\n"), None);
    }

    #[test]
    fn a_lone_cursor_character_reads_as_typed() {
        assert_eq!(
            classify_input_box(&pane("\u{1b}[7mx\u{1b}[27m")),
            Some(InputBox::Typed)
        );
    }

    /// Why (#8407): the SGR parser read every number as an attribute, so the
    /// `2` in `38;2;r;g;b` (truecolour) or a colour index of `2` set dim, and a
    /// typed draft in a coloured span read as a suggestion.
    /// What: extended-colour operands are consumed as colour, in the `5;n`,
    /// `2;r;g;b` and colon forms, while an attribute after them still applies.
    /// Test: itself.
    #[test]
    fn colour_operands_are_never_read_as_attributes() {
        for sgr in [
            "38;2;255;255;255",
            "38;5;2",
            "38;5;7",
            "48;2;0;2;0",
            "48;2;0;0;2",
            "58;5;2",
            "38:2::255:2:255",
            "38:5:2",
            "1;38;5;2;4",
        ] {
            let capture = pane(&format!("\u{1b}[{sgr}mdraft\u{1b}[0m"));
            assert_eq!(
                classify_input_box(&capture),
                Some(InputBox::Typed),
                "SGR {sgr:?}"
            );
        }
        // An attribute after a colour still applies: dim grey is a suggestion.
        let capture = pane("\u{1b}[38;5;7;2mhint\u{1b}[0m");
        assert_eq!(classify_input_box(&capture), Some(InputBox::Suggestion));
    }

    #[test]
    fn input_box_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(InputBox::Suggestion).unwrap(),
            "suggestion"
        );
        assert_eq!(InputBox::Typed.as_str(), "typed");
        assert_eq!(InputBox::Empty.as_str(), "empty");
    }
}
