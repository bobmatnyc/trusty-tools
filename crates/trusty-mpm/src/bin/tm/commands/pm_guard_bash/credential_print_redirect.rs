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
//! [`Routed::target_carries`]; the caller routes it to the stage's
//! stderr. A [`SubKind::Input`] (`<(…)`) target stays excluded: bash always
//! opens that descriptor, so it never produces a missing-file error.
//! #8677: an output target is read the same way, a device path is read with
//! its `//`, `.` and `..` segments resolved, and a target chosen at run time
//! (`> "$OUT"`) is [`Sink::Unknown`], which refuses once a value reaches it.
//! Test: `credential_print_tests::denies_a_credential_redirected_to_an_unread_target_8677`,
//! `credential_print_tests::denies_the_round_two_bypasses`,
//! `credential_print_tests::denies_the_round_three_bypasses`,
//! `credential_print_tests::denies_the_round_five_bypasses`,
//! `credential_print_tests::denies_the_round_six_bypasses`,
//! `credential_print_tests::allows_the_round_six_neighbours`.

use super::super::bash_tokens::{RedirectRole, redirect_role};
use super::credential_print_heredoc::HEREDOC_MARK;
use super::credential_print_programs::basename;
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
    /// stderr. #8677: an output target does too.
    pub(super) target_carries: bool,
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
        target_carries: false,
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
                // #8677: a device read with its extra segments resolved.
                Source::Path(p) => redirect_target_sink(p, &fds, lifted, out),
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
            // #8677: an open error names a carrying target, as for `<`.
            routed.target_carries |= target_names_a_value(target, lifted);
            let sink = redirect_target_sink(target, &fds, lifted, out);
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
            routed.target_carries |= target_names_a_value(target, lifted);
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
        // #8677: `echo x > "/nonexistent/$T"` names `$T` in its open error.
        routed.target_carries |= target_names_a_value(target, lifted);
        let sink = redirect_target_sink(target, &fds, lifted, out);
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
/// `/dev/fd/N` (N 0-9) is fd N, any other `/dev/fd/…` is the terminal, and a
/// `>(…)` target writes where the stage's own stdout (`stage_out`, before its
/// redirections) goes. #8677: the path is read by [`device_name`], so extra
/// `/`, `.` and `..` segments change nothing, and `/proc/self/fd/N` is fd N.
/// Any other device but `/dev/null`, `zero`, `random`, `urandom` and `stdin`
/// — `tty`, a `ttys…`/`pts/…` terminal — is the terminal.
/// Test: `credential_print_tests::denies_the_round_three_bypasses`,
/// `credential_print_tests::denies_a_credential_redirected_to_an_unread_target_8677`.
pub(super) fn terminal_name_sink(
    path: &str,
    fds: &[Sink; 10],
    lifted: &Lifted,
    stage_out: Sink,
) -> Option<Sink> {
    let output_sub = marks_in(path).any(|m| {
        lifted
            .subs
            .get(m)
            .is_some_and(|s| s.kind == SubKind::Output)
    });
    if output_sub {
        return Some(stage_out);
    }
    let device = device_name(path)?;
    let fd = |n: &str| n.parse::<usize>().ok().filter(|&n| n < 10);
    match device.as_str() {
        "null" | "zero" | "random" | "urandom" | "stdin" => None,
        "stdout" => Some(fds[1]),
        "stderr" => Some(fds[2]),
        d => Some(match d.strip_prefix("fd/").and_then(fd) {
            Some(n) => fds[n],
            None => Sink::Terminal,
        }),
    }
}

/// The sink an output target writes to: a redirect's word, or a path a CLI
/// flag names (#8677).
///
/// What: [`terminal_name_sink`] first; `/dev/stdin` (read lexically) opens
/// fd 0 for writing; a target the shell expands at run time — a `$`, a
/// backtick, a lifted substitution, a glob character — is [`Sink::Unknown`],
/// as is a relative target [`relative_target_is_unread`] flags; anything else
/// is a file, [`Sink::Discarded`].
/// Test: `credential_print_tests::denies_a_relative_target_in_an_unknown_directory_8677`,
/// `credential_print_tests::denies_a_tee_operand_chosen_at_run_time_8677`.
pub(super) fn redirect_target_sink(
    target: &str,
    fds: &[Sink; 10],
    lifted: &Lifted,
    stage_out: Sink,
) -> Sink {
    if let Some(sink) = terminal_name_sink(target, fds, lifted, stage_out) {
        return sink;
    }
    if device_name(target).as_deref() == Some("stdin") {
        return fds[0];
    }
    if target.contains(['$', '`', '*', '?', '[']) || target.contains(MARK) {
        return Sink::Unknown;
    }
    if relative_target_is_unread(target, lifted.changes_dir) {
        return Sink::Unknown;
    }
    Sink::Discarded
}

/// Whether `text`, quotes removed, names `cd`, `pushd`, `popd` or `autocd`
/// as a word: after one, no relative target's directory is known (#8677).
pub(super) fn changes_directory(text: &str) -> bool {
    unquoted(text)
        .split(|c: char| c.is_whitespace() || ";&|(){}<>`$=".contains(c))
        .any(|w| matches!(basename(w).as_str(), "cd" | "pushd" | "popd" | "autocd"))
}

/// Whether a relative target may name a device, since its working directory
/// is unknown (#8677 review round 2).
///
/// What: a `~+`/`~-` prefix (the current or previous directory), any relative
/// path when the command changes directory (`changes_dir`), and a relative
/// path whose tail names a device — `stdout`, `stderr`, `stdin`, `console`,
/// `tty…`, `fd/N`, `pts/N` — read in lowercase.
fn relative_target_is_unread(target: &str, changes_dir: bool) -> bool {
    if target.starts_with("~+") || target.starts_with("~-") {
        return true;
    }
    if target.starts_with(['/', '~']) {
        return false;
    }
    if changes_dir {
        return true;
    }
    let lower = target.to_ascii_lowercase();
    let mut tail = lower.rsplit('/').filter(|s| !s.is_empty() && *s != ".");
    let last = tail.next().unwrap_or_default();
    let parent = tail.next().unwrap_or_default();
    matches!(last, "stdout" | "stderr" | "stdin" | "console")
        || last.starts_with("tty")
        || (matches!(parent, "fd" | "pts") && last.chars().all(|c| c.is_ascii_digit()))
}

/// `text` with every quote and backslash removed.
fn unquoted(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\'))
        .collect()
}

/// The device a path names under `/dev`, as `tty` or `fd/1` (#8677).
///
/// What: resolves empty, `.` and `..` segments lexically, then reads the rest
/// after a leading `dev`; `/proc/self/fd/N` reads as `fd/N`. A relative path
/// is read as rooted, since its working directory is unknown: `../../dev/tty`
/// is `tty`. #8677 round 2: read in lowercase, as APFS matches `/DEV/stdout`.
/// `None` for any other path.
fn device_name(path: &str) -> Option<String> {
    let path = path.to_ascii_lowercase();
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    match parts.as_slice() {
        ["dev", rest @ ..] if !rest.is_empty() => Some(rest.join("/")),
        ["proc", "self", "fd", n] => Some(format!("fd/{n}")),
        _ => None,
    }
}

/// Whether a redirect target carries a value an open error would print: a
/// bare `$NAME` expansion or a yielding `$(…)`/backtick substitution
/// (#8676 round 5/6, #8677 for output targets).
fn target_names_a_value(target: &str, lifted: &Lifted) -> bool {
    expands_tainted(target, &lifted.names) || carries_kind(target, lifted, Some(SubKind::Command))
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
// #8869: shared with the secret-read key-consumer rule, which reads a
// `gh secret set NAME < key.pem` key through it; each excluded form answers
// `None` here as well, so that caller denies it.
pub(crate) fn input_redirect_operand(tok: &str) -> Option<(usize, &str)> {
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
