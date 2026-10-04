//! The command bodies a Bash command's substitutions run (#2745, #8756).
//!
//! Why: a rule that reads a segment's argv sees `echo "$(pm2 jlist)"` as
//! `echo` plus one quoted word, so the `pm2 jlist` the shell runs first never
//! reaches it. The forbidden-verb guard already decomposed substitutions
//! (`classify_command_substitutions`); the secret rules did not (#8756 round
//! 2). One scanner now serves both, so the two cannot disagree on which bytes
//! are a substitution body.
//! What: [`segment_substitutions`] lists the top-level `$( … )`, `<( … )`,
//! `>( … )` and backtick bodies of one segment that are live shell syntax.
//! [`command_substitutions`] does the same for a whole command: every segment
//! [`split_shell_segments`] yields (so an `sh -c` string is read too), plus
//! the `$( … )` and backticks of every unquoted-delimiter here-document body,
//! which the shell expands whoever runs it (#9155). Callers recurse into each
//! body themselves, which is what reaches a nested substitution.
//! Test: `substitutions_list_each_live_body_once`,
//! `substitutions_read_an_expanding_heredoc_body_only`,
//! `inert_heredoc_bodies_leave_the_argv_text`; the forbidden-verb callers in
//! `evaluate_bash_command_*`.

use std::ops::Range;

use super::heredoc::{HeredocBodies, blank_spans};
use super::shell_lex::QuoteScan;
use super::{is_evaluator, split_shell_segments};

/// Maximum substitution recursion depth before a caller stops decomposing.
///
/// Why: adversarial deep nesting (`$($($(…`) would otherwise recurse without
/// bound and could exhaust the stack of the short-lived `tm hook --pm-guard`
/// process. The cap is generous — real commands nest a handful of levels.
/// What: the forbidden-verb guard denies past it; the #8756 secret rules read
/// the remaining text flattened instead.
pub(crate) const MAX_SUBSTITUTION_DEPTH: usize = 32;

/// One substitution body, as the shell would run it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Substitution {
    /// A balanced body: the text between the opener and its close.
    Closed(String),
    /// An opener with no close: every byte after it. Nothing later in the
    /// text is scanned, because the opener's extent is unknown.
    Unclosed(String),
}

impl Substitution {
    /// The body text, closed or not.
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Closed(text) | Self::Unclosed(text) => text,
        }
    }
}

/// Whether a paren-delimited substitution opens at byte `i`, and whether it is
/// live there given the segment's quote map.
///
/// Why (#2745): `$(…)`, `<(…)` and `>(…)` are one CLASS — bash executes the
/// body of each. They differ in one respect: double quotes suppress process
/// substitution (`echo "<(x)"` is literal text) but NOT command substitution.
/// What: `Some(true)` when a substitution opens at `i` and is live, `Some(false)`
/// when one opens but is quoted into literal text, `None` when none opens. On
/// unbalanced quotes every opener reads as live — the conservative direction.
/// Test: `evaluate_bash_command_denies_process_substitution_edit`,
/// `evaluate_bash_command_allows_quoted_process_substitution_prose`,
/// `evaluate_bash_command_allows_quoted_substitution_prose`.
pub(super) fn paren_substitution_live_at(scan: &QuoteScan, bytes: &[u8], i: usize) -> Option<bool> {
    if bytes.get(i + 1).copied() != Some(b'(') {
        return None;
    }
    match bytes[i] {
        b'$' => Some(!scan.balanced || scan.allows_substitution(i)),
        b'<' | b'>' => Some(!scan.balanced || scan.is_unquoted(i)),
        _ => None,
    }
}

/// The top-level substitution bodies of one segment, in order.
///
/// What: paren openers match their `)` by depth counting; a backtick matches
/// the next backtick. A quoted opener is skipped. An opener with no close ends
/// the scan with a [`Substitution::Unclosed`].
/// Test: `substitutions_list_each_live_body_once`,
/// `evaluate_bash_command_denies_unbalanced_substitution`.
pub(super) fn segment_substitutions(segment: &str) -> Vec<Substitution> {
    segment_substitution_spans(segment)
        .into_iter()
        .map(|(_, body)| body)
        .collect()
}

/// [`segment_substitutions`] with the byte range each substitution occupies
/// in `segment`, opener and close included (#8931: a caller that must know
/// which argv word a substitution lands in).
/// Test: `substitution_spans_cover_opener_and_close_8931`.
pub(crate) fn segment_substitution_spans(segment: &str) -> Vec<(Range<usize>, Substitution)> {
    let scan = QuoteScan::new(segment);
    scan_bodies(
        segment,
        scan.balanced,
        |bytes, i| paren_substitution_live_at(&scan, bytes, i),
        |i| !scan.balanced || scan.allows_substitution(i),
    )
}

/// The substitution walk shared by argv text and here-document bodies.
///
/// What: with `honour_escapes`, an opener behind an odd run of backslashes
/// is literal text (`\$(x)`), as the shell reads it (#8756).
fn scan_bodies(
    text: &str,
    honour_escapes: bool,
    paren_live: impl Fn(&[u8], usize) -> Option<bool>,
    backtick_live: impl Fn(usize) -> bool,
) -> Vec<(Range<usize>, Substitution)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Only an opener byte is checked, so each backslash run is counted once.
        if honour_escapes && matches!(bytes[i], b'$' | b'<' | b'>' | b'`') && escaped(bytes, i) {
            i += 1;
            continue;
        }
        if let Some(live) = paren_live(bytes, i) {
            if !live {
                i += 1;
                continue;
            }
            let (mut paren, mut j) = (1usize, i + 2);
            while j < bytes.len() && paren > 0 {
                match bytes[j] {
                    b'(' => paren += 1,
                    b')' => paren -= 1,
                    _ => {}
                }
                j += 1;
            }
            if paren != 0 {
                found.push((
                    i..text.len(),
                    Substitution::Unclosed(text[i + 2..].to_string()),
                ));
                return found;
            }
            found.push((i..j, Substitution::Closed(text[i + 2..j - 1].to_string())));
            i = j;
            continue;
        }
        if bytes[i] == b'`' && backtick_live(i) {
            let Some(len) = text[i + 1..].find('`') else {
                found.push((
                    i..text.len(),
                    Substitution::Unclosed(text[i + 1..].to_string()),
                ));
                return found;
            };
            let body = Substitution::Closed(text[i + 1..i + 1 + len].to_string());
            found.push((i..i + len + 2, body));
            i += len + 2;
            continue;
        }
        i += 1;
    }
    found
}

/// Whether byte `i` follows an odd run of backslashes, so it is escaped.
fn escaped(bytes: &[u8], i: usize) -> bool {
    bytes[..i].iter().rev().take_while(|b| **b == b'\\').count() % 2 == 1
}

/// Every top-level substitution body the shell runs while running `command`
/// (#8756).
///
/// What: data here-document bodies leave the argv text first, so a
/// quoted-delimiter body — literal text — contributes nothing; each remaining
/// segment contributes its [`segment_substitutions`]; every unquoted-delimiter
/// body, a shell-run one included (#9155), contributes every `$(` and
/// backtick in it, quotes included, since the body's quotes are literal
/// characters. A body found twice is listed once.
/// Test: `substitutions_read_an_expanding_heredoc_body_only`,
/// `substitutions_read_a_shell_run_expanding_heredoc_body_9155`.
pub(crate) fn command_substitutions(command: &str) -> Vec<Substitution> {
    let heredocs = HeredocBodies::scan(command);
    let spans: Vec<(usize, usize)> = heredocs.data().iter().map(|b| b.span).collect();
    let argv_text = blank_spans(command, &spans);
    let found: Vec<Substitution> = split_shell_segments(&argv_text)
        .iter()
        .flat_map(|segment| segment_substitutions(segment.trim()))
        .collect();
    with_expanded(found, command, heredocs.expanding())
}

/// `found` extended with the `$( … )` and backtick bodies each `expanding`
/// here-document body runs, read with the body's quotes as literal text, and
/// any body already listed dropped (#9155).
///
/// Why: a body that stays in the argv text (one a program runs as code) is
/// also read by the argv scan, so without the drop each of its substitutions
/// would be judged twice per level, which nested bodies multiply.
fn with_expanded(
    mut found: Vec<Substitution>,
    command: &str,
    expanding: &[(usize, usize)],
) -> Vec<Substitution> {
    for &(start, end) in expanding {
        let bodies = scan_bodies(
            &command[start..end],
            true,
            |bytes, i| (bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'(')).then_some(true),
            |_| true,
        );
        for (_, body) in bodies {
            if !found.contains(&body) {
                found.push(body);
            }
        }
    }
    found
}

/// `command` with each here-document body that is stdin text blanked, and the
/// substitutions those blanked bodies still run (#8735 round 2).
///
/// Why: the delete floor read a backtick in a commit message
/// (`git commit -F - <<'EOF'`) as a live substitution and denied it. A body is
/// kept when its operator line runs it as code ([`runs_as_code`]) or names one
/// of `runners` — a program that runs its stdin as shell text.
/// What: blanks the other data bodies ([`blank_spans`]). Every
/// unquoted-delimiter body's `$( … )` and backticks are returned, read with
/// the body's quotes as literal text, whether the body was blanked or kept:
/// the shell expands them before any program reads the body (#9155). A kept
/// body stays in the argv text too, so the verb scan still sees what runs.
/// Test: `allows_a_backtick_in_a_quoted_heredoc_body`,
/// `blank_inert_heredocs_returns_every_expanding_body_9155`.
pub(crate) fn blank_inert_heredocs(command: &str, runners: &[&str]) -> (String, Vec<Substitution>) {
    let heredocs = HeredocBodies::scan(command);
    let spans: Vec<(usize, usize)> = heredocs
        .data()
        .iter()
        .filter(|b| {
            let line = &command[b.operator_line.0..b.operator_line.1];
            !runs_as_code(line)
                && !line
                    .split_whitespace()
                    .any(|w| runners.contains(&w.rsplit('/').next().unwrap_or(w)))
        })
        .map(|b| b.span)
        .collect();
    // #9155: every expanding body, not only the blanked ones, reaches the floor.
    (
        blank_spans(command, &spans),
        with_expanded(Vec::new(), command, heredocs.expanding()),
    )
}

/// `command` with each here-document body that no program runs as code
/// blanked out (#8756).
///
/// Why: a commit message or PR body naming `pm2 jlist` is stdin text to `git`
/// or `cat`; read as argv, it made the dump rules refuse the command. A body
/// handed to an evaluator (`ssh host <<'EOF'`, `python3 <<'EOF'`) or a shell is
/// code, and stays.
/// What: blanks each data body whose operator line names no evaluator,
/// keeping every byte offset and newline.
/// Test: `inert_heredoc_bodies_leave_the_argv_text`.
pub(crate) fn without_inert_heredoc_bodies(command: &str) -> String {
    let inert: Vec<(usize, usize)> = HeredocBodies::scan(command)
        .data()
        .iter()
        .filter(|b| !runs_as_code(&command[b.operator_line.0..b.operator_line.1]))
        .map(|b| b.span)
        .collect();
    blank_spans(command, &inert)
}

/// Whether any word of an operator line is a program that runs its input.
fn runs_as_code(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        let base = word.rsplit('/').next().unwrap_or(word);
        is_evaluator(&base.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn closed(texts: &[&str]) -> Vec<Substitution> {
        texts
            .iter()
            .map(|t| Substitution::Closed((*t).to_string()))
            .collect()
    }

    /// Each live form is one body; a nested one stays inside its parent; a
    /// single-quoted or double-quoted process substitution is text.
    #[test]
    fn substitutions_list_each_live_body_once() {
        let cases: &[(&str, Vec<Substitution>)] = &[
            ("echo \"$(pm2 jlist)\"", closed(&["pm2 jlist"])),
            ("echo `date` $(a $(b))", closed(&["date", "a $(b)"])),
            ("diff <(a) >(b)", closed(&["a", "b"])),
            ("echo '$(a)' \"<(b)\"", Vec::new()),
            ("echo $(a", vec![Substitution::Unclosed("a".to_string())]),
            // #8756: an escaped opener is literal; an escaped backslash is not.
            (r#"echo \$(a) "\`b\`" \\$(c)"#, closed(&["c"])),
        ];
        for (segment, want) in cases {
            assert_eq!(&segment_substitutions(segment), want, "{segment:?}");
        }
    }

    /// #8931: each span runs from the opener to its close, so the caller can
    /// map a substitution back onto the argv word it sits in.
    #[test]
    fn substitution_spans_cover_opener_and_close_8931() {
        let segment = "cat x$(a b)/y `c` \"$(d)\" $(e";
        let spans: Vec<&str> = segment_substitution_spans(segment)
            .into_iter()
            .map(|(range, _)| &segment[range])
            .collect();
        assert_eq!(spans, ["$(a b)", "`c`", "$(d)", "$(e"]);
    }

    /// An unquoted-delimiter body expands; a quoted one is literal.
    #[test]
    fn substitutions_read_an_expanding_heredoc_body_only() {
        assert_eq!(
            command_substitutions("cat <<EOF\nit's '$(a)' `b`\nEOF"),
            closed(&["a", "b"])
        );
        assert!(command_substitutions("cat <<'EOF'\n$(a)\nEOF").is_empty());
        assert_eq!(command_substitutions("bash -c 'echo $(a)'"), closed(&["a"]));
    }

    /// #9155: a body a shell or interpreter runs still expands, so its
    /// single-quoted `$(…)` and backtick are listed, each once; a quoted
    /// delimiter lists nothing.
    #[test]
    fn substitutions_read_a_shell_run_expanding_heredoc_body_9155() {
        assert_eq!(
            command_substitutions("bash <<X\necho '$(a)' `b` $(c)\nX"),
            closed(&["b", "c", "a"])
        );
        assert!(command_substitutions("bash <<'X'\necho '$(a)'\nX").is_empty());
    }

    /// #9155: the kept (code-run) body stays in the argv text, and its
    /// single-quoted substitution is returned beside it; a quoted delimiter
    /// returns nothing; an inert body is blanked and still returns its own.
    #[test]
    fn blank_inert_heredocs_returns_every_expanding_body_9155() {
        for kept in [
            "python3 - <<PY\nprint('$(a)')\nPY",
            "bash <<X\necho '$(a)'\nX",
        ] {
            let (argv, found) = blank_inert_heredocs(kept, &[]);
            assert_eq!(argv, kept);
            assert_eq!(found, closed(&["a"]), "{kept:?}");
        }
        let (_, found) = blank_inert_heredocs("python3 - <<'PY'\nprint('$(a)')\nPY", &[]);
        assert!(found.is_empty());
        let (argv, found) = blank_inert_heredocs("cat <<EOF\n'$(a)'\nEOF", &[]);
        assert!(!argv.contains("$(a)"));
        assert_eq!(found, closed(&["a"]));
    }

    /// A body `cat` or `git` reads is blanked; one `ssh` or a shell runs stays.
    #[test]
    fn inert_heredoc_bodies_leave_the_argv_text() {
        let inert = "git commit -F - <<'EOF'\npm2 jlist\nEOF";
        assert!(!without_inert_heredoc_bodies(inert).contains("pm2"));
        for kept in [
            "ssh host <<'EOF'\npm2 jlist\nEOF",
            "/usr/bin/python3 - <<'EOF'\npm2 jlist\nEOF",
            "bash <<'EOF'\npm2 jlist\nEOF",
        ] {
            assert_eq!(without_inert_heredoc_bodies(kept), kept);
        }
    }
}
