//! Output routing of one stage for the credential-print rule (#8596).
//!
//! Why: `security … -w 3>&1 1>&3` moved the value through fd 3, and a copy of
//! an fd the rule did not track was read as discarded, which failed open.
//! Round 3 found the input-side spellings: `1<&2` copies like `1>&2`, and
//! `1<>/dev/tty` opens the terminal read-write. Round 5 found a plain input
//! redirect (`wc -c < "$T"`): the shell's own "No such file" error names a
//! carrying target on stderr when it fails to open, and nothing had ever
//! parsed a bare `<`. Round 6 found the same missing-file leak through a
//! command substitution or backtick target (`wc -c < "$(echo "$T")"`) —
//! round 5's gate matched only a bare `$NAME` expansion, not the placeholder
//! a lifted `$(…)`/backtick leaves behind.
//! What: [`apply_redirections`] keeps a sink per descriptor 0-9. A copy of an
//! untracked descriptor (fd 0, an fd never assigned, fd 10+) is
//! [`Sink::Terminal`]; a copy from a descriptor chosen at run time
//! (`>&$fd`) is refused as unreadable. [`terminal_name_sink`] maps a path that
//! names a descriptor or the terminal to its sink. A plain input redirect
//! whose target carries the value — a bare `$NAME` expansion, or a
//! [`SubKind::Command`] substitution that yields — sets
//! [`Routed::read_target_carries`]; the caller routes it to the stage's
//! stderr. A [`SubKind::Input`] (`<(…)`) target stays excluded: bash always
//! opens that descriptor, so it never produces a missing-file error.
//! Test: `credential_print_tests::denies_the_round_two_bypasses`,
//! `credential_print_tests::denies_the_round_three_bypasses`,
//! `credential_print_tests::denies_the_round_five_bypasses`,
//! `credential_print_tests::denies_the_round_six_bypasses`,
//! `credential_print_tests::allows_the_round_six_neighbours`.

use super::super::bash_tokens::{RedirectRole, redirect_role};
use super::credential_print_heredoc::HEREDOC_MARK;
use super::credential_print_taint::expands_tainted;
use super::{
    Lifted, MARK, Refusal, Sink, SubKind, carries, carries_kind, input_is_program_text, marks_in,
};

/// A stage's argv and routing after its redirections.
pub(super) struct Routed {
    pub(super) argv: Vec<String>,
    pub(super) out: Sink,
    pub(super) err: Sink,
    /// Every descriptor 0-9 after the redirections; an unassigned one is the
    /// terminal.
    pub(super) fds: [Sink; 10],
    /// A here-string or unquoted here-document carries a credential value.
    pub(super) here_carries: bool,
    /// A here-string or here-document holds text an evaluator would run.
    pub(super) here_program_text: bool,
    /// #8676: stdin is any here-string or here-document.
    pub(super) here_any: bool,
    /// #8676 round 5/6: a plain input redirect (`<`, `N<`) names a target
    /// that carries — via a bare `$NAME` expansion or a yielding `$(…)`/
    /// backtick substitution — and a missing file's error would echo it on
    /// stderr.
    pub(super) read_target_carries: bool,
}

/// Split a stage's words into argv and its output routing.
///
/// What: applies each redirection in order — `>f`, `2>f`, `&>f`, `N<>f` send
/// a descriptor to [`Sink::Discarded`] (or to the sink [`terminal_name_sink`]
/// gives the path); `N>&M`, `N<&M`, `N>& M` and `N>&M-` copy `M`'s current
/// sink; `N>&-` closes. A `<<<` word and a stripped here-document feed stdin
/// and are recorded, not kept in argv; an unquoted here-document's body words
/// stay in argv and mark stdin as program text when they could run a call.
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
        fds,
        here_carries: false,
        here_program_text: false,
        here_any: false,
        read_target_carries: false,
    };
    let mut i = 0;
    while let Some(tok) = tokens.get(i) {
        i += 1;
        if let Some(index) = heredoc_index(tok) {
            let text = lifted.heredocs.get(index).is_none_or(|h| h.program_text);
            routed.here_program_text |= text;
            routed.here_any = true;
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
            routed.here_any = true;
            continue;
        }
        if tok.starts_with("<<") {
            // #8596 round 3: an unquoted here-document left in place; its body
            // words follow the operator.
            let rest = tokens.get(i..).unwrap_or_default();
            routed.here_program_text |= rest.iter().any(|w| input_is_program_text(w));
            routed.here_any = true;
            // #8676: a body word expanding a credential feeds it on stdin.
            routed.here_carries |= rest.iter().any(|w| carries(w, lifted));
        }
        if let Some((fd, src)) = dup_operands(tok, tokens.get(i).map(String::as_str))? {
            if src.consumed_next {
                i += 1;
            }
            let sink = match src.source {
                Source::Close => Sink::Discarded,
                Source::Fd(n) if n < 10 && assigned[n] => fds[n],
                Source::Fd(_) => Sink::Terminal,
                Source::Path(p) => {
                    terminal_name_sink(p, &fds, lifted, out).unwrap_or(Sink::Discarded)
                }
            };
            assign(&mut fds, &mut assigned, &[fd], sink);
            continue;
        }
        if let Some((fd, target)) = read_write_operands(tok) {
            let target = match target {
                "" => {
                    i += 1;
                    tokens
                        .get(i - 1)
                        .ok_or(Refusal::Unreadable("a redirection with no target"))?
                        .as_str()
                }
                t => t,
            };
            let sink = terminal_name_sink(target, &fds, lifted, out).unwrap_or(Sink::Discarded);
            assign(&mut fds, &mut assigned, &[fd], sink);
            continue;
        }
        if let Some((_fd, rest)) = input_redirect_operand(tok) {
            let target = match rest {
                "" => {
                    i += 1;
                    tokens
                        .get(i - 1)
                        .ok_or(Refusal::Unreadable("a redirection with no target"))?
                        .as_str()
                }
                t => t,
            };
            // #8676 round 5/6: a missing file's "No such file" error names
            // `target` on stderr — a bare `$T` (round 5) and a command
            // substitution's own filename (`$(…)`/backtick, round 6: the
            // substitution's result IS the missing path bash reports, not a
            // real descriptor). A `<(…)` target stays excluded — it is a real
            // descriptor bash always opens, never a missing-file error.
            routed.read_target_carries |= expands_tainted(target, &lifted.names)
                || carries_kind(target, lifted, Some(SubKind::Command));
            // A `<(…)`/`$(…)` target's file content becomes stdin, exactly
            // like a here-string (round 2's `sort < <(cred)`, `bash <
            // <(echo …)`) — this branch used to leave the target in `argv`,
            // which is what those rows actually relied on.
            routed.here_carries |= carries(target, lifted);
            routed.here_program_text |= input_is_program_text(target);
            routed.here_any = true;
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
        let sink = terminal_name_sink(target, &fds, lifted, out).unwrap_or(Sink::Discarded);
        let prefix = tok.split('>').next().unwrap_or_default();
        let targets: Vec<usize> = match prefix {
            "" => vec![1],
            "&" => vec![1, 2],
            digits => digits.parse::<usize>().into_iter().collect(),
        };
        assign(&mut fds, &mut assigned, &targets, sink);
    }
    routed.out = fds[1];
    routed.err = fds[2];
    routed.fds = fds;
    Ok(routed)
}

/// Point each descriptor in `targets` (0-9 only) at `sink`.
fn assign(fds: &mut [Sink; 10], assigned: &mut [bool; 10], targets: &[usize], sink: Sink) {
    for &n in targets {
        if let Some(slot) = fds.get_mut(n) {
            *slot = sink;
        }
        if let Some(flag) = assigned.get_mut(n) {
            *flag = true;
        }
    }
}

/// The sink a path writes to when it names a descriptor or the terminal;
/// `None` for an ordinary file.
///
/// What: `/dev/stdout` and `/dev/stderr` are the current fd 1 and fd 2,
/// `/dev/fd/N` (N 0-9) is fd N, any other `/dev/fd/…` and `/dev/tty` are the
/// terminal, and a `>(…)` target writes where the stage's own stdout
/// (`stage_out`, before its redirections) goes.
/// Test: `credential_print_tests::denies_the_round_three_bypasses`.
pub(super) fn terminal_name_sink(
    path: &str,
    fds: &[Sink; 10],
    lifted: &Lifted,
    stage_out: Sink,
) -> Option<Sink> {
    let fd_path = path
        .strip_prefix("/dev/fd/")
        .map(|n| n.parse::<usize>().ok().filter(|&n| n < 10));
    match path {
        "/dev/stdout" => Some(fds[1]),
        "/dev/stderr" => Some(fds[2]),
        "/dev/tty" => Some(Sink::Terminal),
        _ => match fd_path {
            Some(Some(n)) => Some(fds[n]),
            Some(None) => Some(Sink::Terminal),
            None => marks_in(path)
                .any(|m| {
                    lifted
                        .subs
                        .get(m)
                        .is_some_and(|s| s.kind == SubKind::Output)
                })
                .then_some(stage_out),
        },
    }
}

/// What a descriptor copy reads from.
enum Source<'a> {
    Close,
    Fd(usize),
    /// `N<&path`: opens the path, which bash names a file.
    Path(&'a str),
}

/// A parsed descriptor copy.
struct DupSource<'a> {
    source: Source<'a>,
    consumed_next: bool,
}

/// Parse `N>&M`, `N<&M`, `N>&M-`, `N>&-`, or `N>&`/`N<&` followed by a word.
///
/// What: `N` defaults to 1 for `>&` and 0 for `<&`. A `>&word` whose word is
/// a literal path is a file (bash's `>&file`), left to [`redirect_role`]; a
/// `<&path` opens that path. A source holding `$`, a backtick or a lifted
/// substitution is chosen at run time and refused (#8596 round 3).
fn dup_operands<'a>(
    tok: &'a str,
    next: Option<&'a str>,
) -> Result<Option<(usize, DupSource<'a>)>, Refusal> {
    let (left, right, default_fd, input) = match (tok.split_once(">&"), tok.split_once("<&")) {
        (Some((l, r)), _) => (l, r, 1, false),
        (None, Some((l, r))) => (l, r, 0, true),
        (None, None) => return Ok(None),
    };
    let fd = if left.is_empty() {
        default_fd
    } else if left.chars().all(|c| c.is_ascii_digit()) {
        left.parse().unwrap_or(usize::MAX)
    } else {
        return Ok(None);
    };
    let (word, consumed_next) = match (right, next) {
        ("", Some(w)) => (w, true),
        ("", None) => return Ok(None),
        (r, _) => (r, false),
    };
    if word.contains('$') || word.contains('`') || word.contains(MARK) {
        return Err(Refusal::Unreadable("a descriptor chosen at run time"));
    }
    let digits = word.strip_suffix('-').unwrap_or(word);
    let source = if word == "-" {
        Source::Close
    } else if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        Source::Fd(digits.parse().unwrap_or(usize::MAX))
    } else if input {
        Source::Path(word)
    } else {
        // `>&file` names a file: [`redirect_role`] reads it.
        return Ok(None);
    };
    Ok(Some((
        fd,
        DupSource {
            source,
            consumed_next,
        },
    )))
}

/// Parse `N<>path` or `N<>` (path follows): a read-write open of fd `N`,
/// default 0. Returns the descriptor and the attached path, empty if it
/// follows.
fn read_write_operands(tok: &str) -> Option<(usize, &str)> {
    let (left, right) = tok.split_once("<>")?;
    let fd = if left.is_empty() {
        0
    } else if left.chars().all(|c| c.is_ascii_digit()) {
        left.parse().unwrap_or(usize::MAX)
    } else {
        return None;
    };
    Some((fd, right))
}

/// Parse a plain input redirect: `<`, `N<`, `<path`, or `N<path` (#8676
/// round 5) — never `<<`, `<<<`, `<>`, or `<&`, each already consumed by an
/// earlier check before this one runs. Returns the descriptor (default 0) and
/// the attached path, empty if it follows as the next token.
fn input_redirect_operand(tok: &str) -> Option<(usize, &str)> {
    let (left, right) = tok.split_once('<')?;
    if right.starts_with(['<', '>', '&']) {
        return None;
    }
    let fd = if left.is_empty() {
        0
    } else if left.chars().all(|c| c.is_ascii_digit()) {
        left.parse().unwrap_or(usize::MAX)
    } else {
        return None;
    };
    Some((fd, right))
}

/// The index a [`HEREDOC_MARK`] token names.
fn heredoc_index(tok: &str) -> Option<usize> {
    tok.strip_prefix(HEREDOC_MARK)?
        .strip_suffix("__")?
        .parse()
        .ok()
}
