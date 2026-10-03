//! Cutting a command into the pieces the credential-print rule judges
//! (#8596, #8676).
//!
//! Why: the flow scan in the parent module judges one stage at a time, with
//! every substitution already scanned and replaced by a placeholder. Keeping
//! the cutting apart keeps the parent under the file cap.
//! What: [`lift_substitutions`] replaces each live substitution with a
//! [`MARK`] placeholder, [`split_stages`] cuts at the pipeline and list
//! operators, and [`ungroup`] blanks subshell grouping parens.
//! Test: `credential_print_tests` (sibling module).

use super::super::bash_tokens::is_clobber_bar;
use super::super::heredoc::HeredocBodies;
use super::super::shell_lex::QuoteScan;
use super::{Lifted, MARK, Refusal, Sink, Sub, SubKind, scan};

/// Blank unquoted grouping parens: `(gcloud …)` runs `gcloud` in a subshell.
/// #8676: parens inside `${…}` are zsh flags (`${(P)T}`) and stay.
pub(super) fn ungroup(stage: &str) -> String {
    let quotes = QuoteScan::new(stage);
    if !quotes.balanced {
        return stage.to_string();
    }
    let bytes = stage.as_bytes();
    let mut braces = 0usize;
    stage
        .char_indices()
        .map(|(i, c)| {
            if c == '{' && i > 0 && bytes[i - 1] == b'$' {
                braces += 1;
            } else if c == '}' && braces > 0 {
                braces -= 1;
            }
            if matches!(c, '(' | ')') && braces == 0 && quotes.is_unquoted(i) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Replace every live substitution in `text` with a [`MARK`] placeholder,
/// scanning each body first and appending it to `lifted.subs`.
///
/// What: the same opener/closer scan as `super::classify_command_substitutions`
/// — `$(…)`/`<(…)`/`>(…)` with paren counting, backtick pairs — on a
/// [`QuoteScan`] of `text`. `$(…)`, backtick and `<(…)` bodies run with a
/// captured stdout; a `>(…)` body writes to the outer stdout. An unclosed
/// opener is refused. `$((` arithmetic ([`opens_arithmetic`]) stays in place
/// (#8676), while a substitution inside it is still lifted. Every slice starts
/// and ends at an ASCII byte.
pub(super) fn lift_substitutions(
    text: &str,
    stdout: Sink,
    stderr: Sink,
    depth: usize,
    lifted: &mut Lifted,
) -> Result<String, Refusal> {
    let scan_quotes = QuoteScan::new(text);
    let live = |i: usize| !scan_quotes.balanced || scan_quotes.allows_substitution(i);
    let unquoted = |i: usize| !scan_quotes.balanced || scan_quotes.is_unquoted(i);
    let bytes = text.as_bytes();
    let mut flat = String::with_capacity(text.len());
    let (mut i, mut copied) = (0, 0);
    while i < bytes.len() {
        let arithmetic = opens_arithmetic(bytes, i);
        let paren = bytes.get(i + 1) == Some(&b'(')
            && !arithmetic
            && match bytes[i] {
                b'$' => live(i),
                b'<' | b'>' => unquoted(i),
                _ => false,
            };
        let (body, end) = if paren {
            let mut level = 1usize;
            let mut j = i + 2;
            while j < bytes.len() && level > 0 {
                match bytes[j] {
                    b'(' => level += 1,
                    b')' => level -= 1,
                    _ => {}
                }
                j += 1;
            }
            if level != 0 {
                return Err(Refusal::Unreadable("an unclosed substitution"));
            }
            (&text[i + 2..j - 1], j)
        } else if bytes[i] == b'`' && live(i) {
            let close = text[i + 1..]
                .find('`')
                .ok_or(Refusal::Unreadable("an unclosed backtick"))?;
            (&text[i + 1..i + 1 + close], i + close + 2)
        } else {
            i += 1;
            continue;
        };
        let kind = match (paren, bytes[i]) {
            (true, b'<') => SubKind::Input,
            (true, b'>') => SubKind::Output,
            _ => SubKind::Command,
        };
        let body_out = if kind == SubKind::Output {
            stdout
        } else {
            Sink::Captured
        };
        let yields = scan(body, body_out, stderr, depth + 1, lifted)?;
        flat.push_str(&text[copied..i]);
        flat.push_str(&format!("{MARK}{}__", lifted.subs.len()));
        lifted.subs.push(Sub { kind, yields });
        i = end;
        copied = end;
    }
    flat.push_str(&text[copied..]);
    Ok(flat)
}

/// Whether `$((` at byte `i` opens arithmetic rather than a command
/// substitution whose body starts with a subshell (#8931 critic round).
///
/// What: the shell reads `$((` as arithmetic only when the `(` at `i + 2`
/// closes immediately before the outer `)`; `$((cmd) )` and `$((a); (b))` run
/// commands. An unclosed opener is not arithmetic, so the caller refuses it.
/// Test: `denies_a_subshell_substitution_shaped_like_arithmetic_8931`.
fn opens_arithmetic(bytes: &[u8], i: usize) -> bool {
    if bytes[i] != b'$' || bytes.get(i + 1) != Some(&b'(') || bytes.get(i + 2) != Some(&b'(') {
        return false;
    }
    let mut level = 0usize;
    for (j, &b) in bytes.iter().enumerate().skip(i + 2) {
        match b {
            b'(' => level += 1,
            b')' => {
                level -= 1;
                if level == 0 {
                    return bytes.get(j + 1) == Some(&b')');
                }
            }
            _ => {}
        }
    }
    false
}

/// Split `text` into stages: `(stage, stdout piped on, stderr piped on)`.
///
/// What: cuts at unquoted `|`, `|&`, `||`, `&&`, `;`, newline and a bare `&`
/// (never at a `>&`/`<&`/`&>` redirection), leaving unquoted here-document bodies
/// whole the way `super::split_shell_segments_raw` does.
pub(super) fn split_stages(text: &str) -> Vec<(String, bool, bool)> {
    let quotes = QuoteScan::new(text);
    let bodies = HeredocBodies::scan(text);
    let bytes = text.as_bytes();
    let mut stages = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        // #8730: the `|` of `>|`/`>&|` is the redirect's clobber mark, not a pipe.
        if (quotes.balanced && !quotes.is_unquoted(i))
            || bodies.suppresses_separator(i)
            || is_clobber_bar(bytes, i)
        {
            i += 1;
            continue;
        }
        let next = bytes.get(i + 1).copied();
        let (width, piped, pipe_stderr) = match (bytes[i], next) {
            (b'|', Some(b'|')) | (b'&', Some(b'&')) => (2, false, false),
            (b'|', Some(b'&')) => (2, true, true),
            (b'|', _) => (1, true, false),
            (b';' | b'\n', _) => (1, false, false),
            (b'&', _) if i > 0 && matches!(bytes[i - 1], b'>' | b'<') => (0, false, false),
            (b'&', Some(b'>')) => (0, false, false),
            (b'&', _) => (1, false, false),
            _ => (0, false, false),
        };
        if width == 0 {
            i += 1;
            continue;
        }
        stages.push((text[start..i].to_string(), piped, pipe_stderr));
        i += width;
        start = i;
    }
    stages.push((text[start..].to_string(), false, false));
    stages
}
