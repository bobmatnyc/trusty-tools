//! Pure routing from a submitted input line to a chat-core [`TrustyCommand`].
//!
//! Why: Phase 1C (#1272/#1276) wires the coordinator TUI's submit path through
//! the shared chat-core executor instead of the old echo-only stub. The decision
//! of *what* to run — which slash verb maps to which [`TrustyCommand`], how a
//! target-less verb borrows the focused session, and when a non-slash line routes
//! as a `send` versus falling back to a hint/echo — is pure logic with no IO, so
//! it lives here as a unit-testable function the async run loop calls before it
//! ever touches the daemon (DOC-13 §9). The terminal/IO half (await the executor,
//! append the result) stays in [`super::mod`].
//! What: [`route`] takes the submitted line and the focused session's routing
//! target and returns a [`Dispatch`] — either a [`TrustyCommand`] to execute, a
//! one-line hint to show, or an `Echo` (plain chat with no focused session, which
//! the loop records without a daemon call). Slash verbs map through
//! [`super::events::parse_slash`]; arguments are split off the raw line.
//! Test: `super::tests` covers the slash → command mapping (incl. aliases),
//! target-less-verb focus borrowing, the missing-arg/no-focus hint branches, and
//! the free-text send-vs-echo decision.

use crate::client::TrustyCommand;

use super::events::{SlashCommand, parse_slash};

/// The routing decision for one submitted input line.
///
/// Why: the run loop must know whether to (a) execute a command against the
/// daemon, (b) just show the operator a hint (bad/under-specified command, or no
/// focused session to route at), or (c) treat the line as plain chat it records
/// without a daemon call. Modelling those three outcomes keeps the loop's branch
/// trivial and the decision testable.
/// What: `Command` carries the [`TrustyCommand`] to run; `Hint` carries a
/// single-line message to append to the output log; `Echo` means "plain chat, no
/// focus" — the loop records the line and does nothing else.
/// Test: every `route_*` test asserts on this enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    /// Execute this command via the chat-core executor.
    Command(TrustyCommand),
    /// Show this one-line hint in the output log; do not call the daemon.
    Hint(String),
    /// Plain chat with no focused session — record the line, do nothing else.
    Echo,
}

/// Route a submitted input line to a [`Dispatch`] decision.
///
/// Why: the single seam between "operator pressed Enter" and "run a command";
/// keeping it pure (no terminal, no daemon) makes every branch — slash mapping,
/// focus borrowing, missing-arg hints, free-text send — unit-testable.
/// What: when `line` is a slash command, maps it via [`parse_slash`] and the
/// remaining argument text to a [`TrustyCommand`], borrowing `focused` as the
/// target when a target-taking verb omits one (a [`Dispatch::Hint`] when neither
/// an explicit target nor a focus is available, or required text is missing).
/// When `line` is NOT a slash command, routes it as a `send` at `focused` if a
/// session is focused, else returns [`Dispatch::Echo`]. An empty/whitespace line
/// is an `Echo` no-op.
/// Test: `route_slash_maps_to_command`, `route_targetless_verb_borrows_focus`,
/// `route_free_text_sends_to_focus`, `route_free_text_no_focus_echoes`,
/// `route_unknown_command_hints`, `route_missing_target_hints`.
pub fn route(line: &str, focused: Option<&str>) -> Dispatch {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Dispatch::Echo;
    }
    match parse_slash(trimmed) {
        Some(cmd) => route_slash(cmd, trimmed, focused),
        // A non-slash line drives the focused session as a `send`; with no focus
        // it falls through to plain chat (the historical echo behaviour).
        None => match focused {
            Some(target) => Dispatch::Command(TrustyCommand::ManagedSend {
                target: target.to_string(),
                text: trimmed.to_string(),
            }),
            None => Dispatch::Echo,
        },
    }
}

/// Map a parsed [`SlashCommand`] + raw line to a [`Dispatch`].
///
/// Why: factored out of [`route`] so the slash-specific argument handling (split
/// the verb off, resolve a target, require text where needed) stays readable and
/// each verb's contract is in one place.
/// What: builds the matching [`TrustyCommand`] for each verb, drawing the target
/// from the first argument token or, when absent, the `focused` session; verbs
/// that need free-text (`send`/`answer`) take the remaining tokens as the body.
/// Returns a [`Dispatch::Hint`] for an unknown verb, a missing required target,
/// or missing required text. `/new` (spawn) and `/help` need no target.
/// Test: the `route_*` tests in `super::tests`.
fn route_slash(cmd: SlashCommand, line: &str, focused: Option<&str>) -> Dispatch {
    // `args` is everything after the leading `/verb` token.
    let args = line
        .split_whitespace()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");
    let args = args.trim();
    match cmd {
        SlashCommand::Help => Dispatch::Command(TrustyCommand::Help),
        SlashCommand::Sessions => Dispatch::Command(TrustyCommand::ManagedList),
        SlashCommand::New => route_new(args),
        SlashCommand::Attach => target_only(args, focused, |t| TrustyCommand::ManagedAttachCmd {
            target: t,
        }),
        SlashCommand::Stop => target_only(args, focused, |t| TrustyCommand::ManagedRuntimeStop {
            target: t,
        }),
        SlashCommand::Resume => target_only(args, focused, |t| TrustyCommand::ManagedResume {
            target: t,
        }),
        SlashCommand::Kill => target_only(args, focused, |t| TrustyCommand::ManagedDecommission {
            target: t,
        }),
        SlashCommand::Activity => target_only(args, focused, |t| TrustyCommand::ManagedActivity {
            target: t,
        }),
        SlashCommand::Get => {
            target_only(args, focused, |t| TrustyCommand::ManagedGet { target: t })
        }
        SlashCommand::Send => route_target_and_text(args, focused, "send", |target, text| {
            TrustyCommand::ManagedSend { target, text }
        }),
        SlashCommand::Answer => route_target_and_text(args, focused, "answer", |target, answer| {
            TrustyCommand::ManagedAnswer { target, answer }
        }),
        SlashCommand::Unknown(word) => {
            Dispatch::Hint(format!("unknown command: /{word} — try /help"))
        }
    }
}

/// Build a target-only managed command, borrowing the focused session when no
/// explicit target was given.
///
/// Why: every lifecycle/inspection verb (`/stop`, `/resume`, `/kill`, `/attach`,
/// `/activity`, `/get`) shares the same target-resolution rule — use the first
/// argument token if present, else the focused session, else hint. Centralising
/// it keeps the six verbs identical and the hint message single-sourced.
/// What: resolves the target (explicit arg → focus) and applies `build` to it, or
/// returns a [`Dispatch::Hint`] when neither a target nor a focus exists.
/// Test: `route_targetless_verb_borrows_focus`, `route_missing_target_hints`.
fn target_only(
    args: &str,
    focused: Option<&str>,
    build: impl FnOnce(String) -> TrustyCommand,
) -> Dispatch {
    match resolve_target(args, focused) {
        Some(target) => Dispatch::Command(build(target)),
        None => {
            Dispatch::Hint("no session selected — focus a session or pass an id/name".to_string())
        }
    }
}

/// Build a managed command that needs both a target and a free-text body.
///
/// Why: `/send` and `/answer` both take `<target?> <text>` where the target may
/// be implicit (the focused session). The parsing is subtle — with a focus, the
/// WHOLE argument string is the text (no token is stolen as a target); without a
/// focus, the first token is the target and the rest is the text. One helper
/// keeps that rule identical for both verbs.
/// What: when `focused` is set, the target is the focus and `args` is the text;
/// otherwise the first token is the target and the remainder is the text. Returns
/// a [`Dispatch::Hint`] when there is no target to resolve or the text is empty.
/// Test: `route_send_uses_focus_for_target`, `route_send_explicit_target`,
/// `route_send_missing_text_hints`.
fn route_target_and_text(
    args: &str,
    focused: Option<&str>,
    verb: &str,
    build: impl FnOnce(String, String) -> TrustyCommand,
) -> Dispatch {
    let (target, text) = match focused {
        // A focused session owns the whole line as text — the operator is talking
        // to *that* session, so no leading token is consumed as a target.
        Some(t) => (t.to_string(), args.to_string()),
        // No focus: the first token is the target, the rest is the body.
        None => {
            let mut parts = args.splitn(2, char::is_whitespace);
            let target = parts.next().unwrap_or("").trim().to_string();
            let text = parts.next().unwrap_or("").trim().to_string();
            (target, text)
        }
    };
    if target.is_empty() {
        return Dispatch::Hint(format!(
            "{verb}: no session selected — focus a session or pass an id/name"
        ));
    }
    if text.trim().is_empty() {
        return Dispatch::Hint(format!("{verb}: text is required"));
    }
    Dispatch::Command(build(target, text.trim().to_string()))
}

/// Route `/new` (spawn) which takes `<repo> <ref> <task>` (no target).
///
/// Why: spawning a managed session is the one verb that does NOT borrow the
/// focused session — it provisions a brand-new workspace — so its argument
/// handling differs from the target-taking family and lives in its own helper.
/// What: splits `args` into the repo URL, git ref, and the remaining task text;
/// returns a [`Dispatch::Hint`] when fewer than three pieces are supplied.
/// Test: `route_new_requires_repo_ref_task`, `route_new_builds_managed_new`.
fn route_new(args: &str) -> Dispatch {
    let mut parts = args.splitn(3, char::is_whitespace);
    let repo = parts.next().unwrap_or("").trim();
    let git_ref = parts.next().unwrap_or("").trim();
    let task = parts.next().unwrap_or("").trim();
    if repo.is_empty() || git_ref.is_empty() || task.is_empty() {
        return Dispatch::Hint("new: usage /new <repo> <ref> <task>".to_string());
    }
    Dispatch::Command(TrustyCommand::ManagedNew {
        repo_url: repo.to_string(),
        git_ref: git_ref.to_string(),
        task: task.to_string(),
        name_hint: None,
        runtime: None,
    })
}

/// Resolve a target from an explicit argument or the focused session.
///
/// Why: the target-only verbs all prefer an explicit id/name argument and fall
/// back to the focused session; sharing one resolver keeps the precedence rule
/// (explicit beats focus) in a single place.
/// What: returns the first whitespace-delimited token of `args` when non-empty,
/// else `focused`, else `None`.
/// Test: covered via `route_targetless_verb_borrows_focus` and
/// `route_slash_explicit_target_overrides_focus`.
fn resolve_target(args: &str, focused: Option<&str>) -> Option<String> {
    args.split_whitespace()
        .next()
        .map(str::to_string)
        .or_else(|| focused.map(str::to_string))
}
