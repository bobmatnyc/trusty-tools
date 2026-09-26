//! Output routing of one stage for the credential-print rule (#8596).
//!
//! Why: `security … -w 3>&1 1>&3` moved the value through fd 3, and a copy of
//! an fd the rule did not track was read as discarded, which failed open.
//! What: [`apply_redirections`] keeps a sink per descriptor 0-9. A copy of an
//! untracked descriptor (fd 0, an fd never assigned, fd 10+) is
//! [`Sink::Terminal`].
//! Test: `credential_print_tests::denies_the_round_two_bypasses`.

use super::super::bash_tokens::{RedirectRole, redirect_role};
use super::credential_print_heredoc::HEREDOC_MARK;
use super::{Lifted, Refusal, Sink, SubKind, carries, input_is_program_text, marks_in};

/// A stage's argv and routing after its redirections.
pub(super) struct Routed {
    pub(super) argv: Vec<String>,
    pub(super) out: Sink,
    pub(super) err: Sink,
    /// A here-string carries a credential value.
    pub(super) here_carries: bool,
    /// A here-string or here-document holds text an evaluator would run.
    pub(super) here_program_text: bool,
}

/// Split a stage's words into argv and its output routing.
///
/// What: applies each redirection in order — `>f`, `2>f`, `&>f` send a
/// descriptor to [`Sink::Discarded`] (or to a live sink for `/dev/stdout`,
/// `/dev/stderr`, `/dev/tty`, or a `>(…)` target); `N>&M`, `N>& M` and
/// `N>&M-` copy `M`'s current sink; `N>&-` closes. A `<<<` word and a
/// stripped here-document feed stdin and are recorded, not kept in argv.
pub(super) fn apply_redirections(
    tokens: &[String],
    out: Sink,
    err: Sink,
    lifted: &Lifted,
) -> Result<Routed, Refusal> {
    let mut fds = [Sink::Terminal; 10];
    fds[1] = out;
    fds[2] = err;
    let mut assigned = [false; 10];
    assigned[1] = true;
    assigned[2] = true;
    let mut routed = Routed {
        argv: Vec::new(),
        out,
        err,
        here_carries: false,
        here_program_text: false,
    };
    let mut i = 0;
    while let Some(tok) = tokens.get(i) {
        i += 1;
        if let Some(index) = heredoc_index(tok) {
            let text = lifted.heredocs.get(index).is_none_or(|h| h.program_text);
            routed.here_program_text |= text;
            continue;
        }
        if let Some(word) = tok.strip_prefix("<<<") {
            let word = if word.is_empty() {
                i += 1;
                tokens.get(i - 1).map(String::as_str).unwrap_or_default()
            } else {
                word
            };
            routed.here_carries |= carries(word, lifted);
            routed.here_program_text |= input_is_program_text(word);
            continue;
        }
        if let Some((fd, src)) = dup_operands(tok, tokens.get(i).map(String::as_str)) {
            if src.consumed_next {
                i += 1;
            }
            let sink = match src.fd {
                None => Sink::Discarded,
                Some(n) if n < 10 && assigned[n] => fds[n],
                Some(_) => Sink::Terminal,
            };
            if fd < 10 {
                fds[fd] = sink;
                assigned[fd] = true;
            }
            continue;
        }
        let target = match redirect_role(tok) {
            RedirectRole::None | RedirectRole::FileDescriptor => {
                routed.argv.push(tok.clone());
                continue;
            }
            RedirectRole::TargetFollows => {
                i += 1;
                tokens
                    .get(i - 1)
                    .ok_or(Refusal::Unreadable("a redirection with no target"))?
                    .as_str()
            }
            RedirectRole::Target(t) => t,
        };
        let sink = match target {
            "/dev/stdout" | "/dev/fd/1" => fds[1],
            "/dev/stderr" | "/dev/fd/2" => fds[2],
            "/dev/tty" => Sink::Terminal,
            t if t.starts_with("/dev/fd/") => Sink::Terminal,
            t if marks_in(t).any(|m| {
                lifted
                    .subs
                    .get(m)
                    .is_some_and(|s| s.kind == SubKind::Output)
            }) =>
            {
                Sink::Terminal
            }
            _ => Sink::Discarded,
        };
        let prefix = tok.split('>').next().unwrap_or_default();
        let targets: Vec<usize> = match prefix {
            "" => vec![1],
            "&" => vec![1, 2],
            digits => digits.parse::<usize>().into_iter().collect(),
        };
        for n in targets {
            if let Some(slot) = fds.get_mut(n) {
                *slot = sink;
            }
            if let Some(flag) = assigned.get_mut(n) {
                *flag = true;
            }
        }
    }
    routed.out = fds[1];
    routed.err = fds[2];
    Ok(routed)
}

/// The source of a descriptor copy: `None` closes.
struct DupSource {
    fd: Option<usize>,
    consumed_next: bool,
}

/// Parse `N>&M`, `N>&M-`, `N>&-`, or `N>&` followed by a digits word.
///
/// What: `N` defaults to 1. A `>&word` whose word is not digits is a file
/// (bash's `>&file`), left to [`redirect_role`].
fn dup_operands(tok: &str, next: Option<&str>) -> Option<(usize, DupSource)> {
    let (left, right) = tok.split_once(">&")?;
    let fd = if left.is_empty() {
        1
    } else if left.chars().all(|c| c.is_ascii_digit()) {
        left.parse().unwrap_or(usize::MAX)
    } else {
        return None;
    };
    let (word, consumed_next) = if right.is_empty() {
        let word = next?;
        if !is_dup_word(word) {
            return None;
        }
        (word, true)
    } else {
        (right, false)
    };
    if !is_dup_word(word) {
        return None;
    }
    let digits = word.strip_suffix('-').unwrap_or(word);
    let src = if digits.is_empty() {
        None
    } else {
        Some(digits.parse().unwrap_or(usize::MAX))
    };
    Some((
        fd,
        DupSource {
            fd: src,
            consumed_next,
        },
    ))
}

/// `-`, digits, or digits followed by `-`.
fn is_dup_word(word: &str) -> bool {
    let digits = word.strip_suffix('-').unwrap_or(word);
    (word == "-" || !digits.is_empty()) && digits.chars().all(|c| c.is_ascii_digit())
}

/// The index a [`HEREDOC_MARK`] token names.
fn heredoc_index(tok: &str) -> Option<usize> {
    tok.strip_prefix(HEREDOC_MARK)?
        .strip_suffix("__")?
        .parse()
        .ok()
}
