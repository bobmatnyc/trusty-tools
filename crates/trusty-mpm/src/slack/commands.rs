//! Native Slack slash-command parsing.
//!
//! Why: the Slack adapter must be a *thin* peer of the Telegram adapter (DOC-20
//! §2): it parses its native input into the shared, UI-agnostic
//! [`crate::client::TrustyCommand`] and lets the one [`crate::client::CommandExecutor`]
//! do every bit of session logic and daemon I/O. Slack delivers slash commands
//! as a `command` field (e.g. `/fleet`) plus a free-form `text` argument tail;
//! this module turns that pair into a `TrustyCommand` using the SAME verb surface
//! and the SAME argument-splitting rules the Telegram adapter uses, so the two
//! surfaces can never drift.
//! What: [`SlackCommand`] is the native verb enum; [`parse_slash`] turns a
//! `(command, text)` pair into one; the `From` impl projects it onto
//! [`TrustyCommand`]. Argument tails (`/launch <workdir> | <task>`,
//! `/msend <id> <text>`, …) are split exactly as the Telegram adapter splits
//! them.
//! Test: the `#[cfg(test)]` module covers parsing every verb, the argument
//! splits, and the projection onto `TrustyCommand`.

use crate::client::TrustyCommand;

/// The Slack adapter's native slash commands.
///
/// Why: Slack hands a slash command as a bare verb name plus a single argument
/// string; modelling that as a typed enum (mirroring the Telegram
/// `TelegramCommand`) keeps the parse explicit and the projection onto
/// [`TrustyCommand`] exhaustive — a new verb is a compile error until it is
/// mapped.
/// What: one variant per operator action. Single-`String` variants carry the
/// whole argument tail (empty when omitted); multi-argument verbs split the tail
/// in the `From` impl. The verb set is the SAME managed-fleet + project-session
/// surface the Telegram adapter exposes.
/// Test: `parse_slash_maps_every_verb`, `slack_command_converts_to_trusty`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlackCommand {
    /// `/sessions` — list managed sessions.
    Sessions,
    /// `/status <id>` — session status.
    Status(String),
    /// `/approve <id>` — approve a permission request.
    Approve(String),
    /// `/deny <id>` — deny a permission request.
    Deny(String),
    /// `/overseer` — show overseer status.
    Overseer,
    /// `/tmux` — list all tmux sessions.
    Tmux,
    /// `/projects` — discover projects from Claude Code config.
    Projects,
    /// `/discover` — auto-discover tmux sessions running Claude Code.
    Discover,
    /// `/adopt <session>` — adopt an external tmux session.
    Adopt(String),
    /// `/config <path>` — analyze a Claude Code config.
    Config(String),
    /// `/snapshot <session>` — capture tmux pane output.
    Snapshot(String),
    /// `/kill <id>` — kill a session.
    Kill(String),
    /// `/send <session> <prompt>` — send a prompt to a Claude Code session.
    Send(String),
    /// `/alerts` — show alert subscriptions.
    Alerts,
    /// `/doctor` — run a full system diagnostic.
    Doctor,
    /// `/health` — report daemon health.
    Health,
    /// `/launch <workdir> | <task>` — launch a managed session.
    Launch(String),
    /// `/fleet` — list managed (fleet) sessions.
    Fleet,
    /// `/get <id|name>` — get one managed session.
    Get(String),
    /// `/msend <id|name> <text>` — send text to a managed session.
    Msend(String),
    /// `/answer <id|name> <text>` — answer a managed session's pending decision.
    Answer(String),
    /// `/activity <id|name>` — inspect a managed session's activity.
    Activity(String),
    /// `/resume <id|name>` — resume a stopped managed session.
    Resume(String),
    /// `/decommission <id|name>` — decommission a managed session.
    Decommission(String),
    /// `/help` — show all commands.
    Help,
}

impl SlackCommand {
    /// The canonical verb name (without the leading `/`) for each variant.
    ///
    /// Why: Slack's slash-command registration and the help surface both need the
    /// authoritative list of verb names; deriving them from one place keeps the
    /// registration, the parser, and the help body consistent.
    /// What: returns the lowercase verb name used both as the Slack `command`
    /// (Slack delivers it WITH the leading `/`) and in help text.
    /// Test: `all_names_parse_back` round-trips every name through [`parse_slash`].
    pub fn all_names() -> &'static [&'static str] {
        &[
            "sessions",
            "status",
            "approve",
            "deny",
            "overseer",
            "tmux",
            "projects",
            "discover",
            "adopt",
            "config",
            "snapshot",
            "kill",
            "send",
            "alerts",
            "doctor",
            "health",
            "launch",
            "fleet",
            "get",
            "msend",
            "answer",
            "activity",
            "resume",
            "decommission",
            "help",
        ]
    }
}

/// Parse a Slack `(command, text)` pair into a [`SlackCommand`].
///
/// Why: Slack delivers a slash command as a `command` field (with the leading
/// `/`, e.g. `/fleet`) and a separate free-form `text` argument tail. This is the
/// one place that maps that wire shape onto the typed verb, so the adapter's
/// dispatch stays a single thin call.
/// What: strips an optional leading `/` from `command`, lowercases it, and
/// matches it to a variant, carrying the trimmed `text` as the argument tail.
/// Returns `None` for an unknown verb so the caller can fall back to free-text
/// routing (the action-capable coordinator) rather than erroring.
/// Test: `parse_slash_maps_every_verb`, `parse_slash_unknown_is_none`,
/// `parse_slash_strips_leading_slash`.
pub fn parse_slash(command: &str, text: &str) -> Option<SlackCommand> {
    let verb = command.trim().trim_start_matches('/').to_ascii_lowercase();
    let arg = text.trim().to_string();
    let cmd = match verb.as_str() {
        "sessions" => SlackCommand::Sessions,
        "status" => SlackCommand::Status(arg),
        "approve" => SlackCommand::Approve(arg),
        "deny" => SlackCommand::Deny(arg),
        "overseer" => SlackCommand::Overseer,
        "tmux" => SlackCommand::Tmux,
        "projects" => SlackCommand::Projects,
        "discover" => SlackCommand::Discover,
        "adopt" => SlackCommand::Adopt(arg),
        "config" => SlackCommand::Config(arg),
        "snapshot" => SlackCommand::Snapshot(arg),
        "kill" => SlackCommand::Kill(arg),
        "send" => SlackCommand::Send(arg),
        "alerts" => SlackCommand::Alerts,
        "doctor" => SlackCommand::Doctor,
        "health" => SlackCommand::Health,
        "launch" => SlackCommand::Launch(arg),
        "fleet" => SlackCommand::Fleet,
        "get" => SlackCommand::Get(arg),
        "msend" => SlackCommand::Msend(arg),
        "answer" => SlackCommand::Answer(arg),
        "activity" => SlackCommand::Activity(arg),
        "resume" => SlackCommand::Resume(arg),
        "decommission" => SlackCommand::Decommission(arg),
        "help" => SlackCommand::Help,
        _ => return None,
    };
    Some(cmd)
}

impl From<SlackCommand> for TrustyCommand {
    /// Project a Slack verb onto the shared command model.
    ///
    /// Why: the executor only understands [`TrustyCommand`]; the adapter's native
    /// enum must be projected onto it before dispatch (DOC-20 §2). Reusing the
    /// SAME multi-argument splits as the Telegram adapter guarantees identical
    /// behaviour across surfaces.
    /// What: maps each variant; multi-argument tails are split by
    /// [`split_launch_args`] / [`split_target_args`] / [`split_send_args`]; an
    /// empty `/pair`-style argument is normalized where relevant.
    /// Test: `slack_command_converts_to_trusty`.
    fn from(cmd: SlackCommand) -> Self {
        match cmd {
            SlackCommand::Sessions => TrustyCommand::Sessions,
            SlackCommand::Status(session_id) => TrustyCommand::Status { session_id },
            SlackCommand::Approve(session_id) => TrustyCommand::Approve { session_id },
            SlackCommand::Deny(session_id) => TrustyCommand::Deny { session_id },
            SlackCommand::Overseer => TrustyCommand::Overseer,
            SlackCommand::Tmux => TrustyCommand::Tmux,
            SlackCommand::Projects => TrustyCommand::Projects,
            SlackCommand::Discover => TrustyCommand::Discover,
            SlackCommand::Adopt(session) => TrustyCommand::Adopt { session },
            SlackCommand::Config(project) => TrustyCommand::Config { project },
            SlackCommand::Snapshot(session) => TrustyCommand::Snapshot { session },
            SlackCommand::Kill(session_id) => TrustyCommand::Kill { session_id },
            SlackCommand::Send(args) => {
                let (session, prompt) = split_send_args(&args);
                TrustyCommand::Send { session, prompt }
            }
            SlackCommand::Alerts => TrustyCommand::Alerts,
            SlackCommand::Doctor => TrustyCommand::Doctor,
            SlackCommand::Health => TrustyCommand::Health,
            SlackCommand::Launch(args) => {
                let (repo_url, task) = split_launch_args(&args);
                TrustyCommand::ManagedNew {
                    repo_url,
                    git_ref: String::new(),
                    task,
                    name_hint: None,
                    runtime: None,
                }
            }
            SlackCommand::Fleet => TrustyCommand::ManagedList,
            SlackCommand::Get(target) => TrustyCommand::ManagedGet {
                target: target.trim().to_string(),
            },
            SlackCommand::Msend(args) => {
                let (target, text) = split_target_args(&args);
                TrustyCommand::ManagedSend { target, text }
            }
            SlackCommand::Answer(args) => {
                let (target, answer) = split_target_args(&args);
                TrustyCommand::ManagedAnswer { target, answer }
            }
            SlackCommand::Activity(target) => TrustyCommand::ManagedActivity {
                target: target.trim().to_string(),
            },
            SlackCommand::Resume(target) => TrustyCommand::ManagedResume {
                target: target.trim().to_string(),
            },
            SlackCommand::Decommission(target) => TrustyCommand::ManagedDecommission {
                target: target.trim().to_string(),
            },
            SlackCommand::Help => TrustyCommand::Help,
        }
    }
}

/// Split a `/launch` argument tail into `(workdir, task)` on the first `|`.
///
/// Why: launching a managed session needs a workspace and a task; Slack hands the
/// whole tail as one string, so `/launch <workdir> | <task...>` is parsed here —
/// the SAME pipe convention the Telegram adapter uses, so the two surfaces behave
/// identically.
/// What: returns the trimmed text before the first `|` as the workdir and the
/// trimmed remainder as the task. With no `|`, the whole tail is the workdir and
/// the task is empty (the executor then reports the missing task — never a panic).
/// Test: `split_launch_args_splits_on_pipe`.
fn split_launch_args(args: &str) -> (String, String) {
    match args.split_once('|') {
        Some((workdir, task)) => (workdir.trim().to_string(), task.trim().to_string()),
        None => (args.trim().to_string(), String::new()),
    }
}

/// Split a managed `target <text...>` argument tail into `(target, text)`.
///
/// Why: `/msend` and `/answer` carry the session id/name as the first token and
/// the (possibly multi-word) payload as the remainder; Slack hands the whole tail
/// as one string, so the split happens here (mirroring the Telegram adapter).
/// What: returns the first whitespace-separated token as `target` and the trimmed
/// remainder as the payload; both are empty strings when absent (the executor
/// reports the missing argument rather than panicking).
/// Test: `split_target_args_separates_target_and_text`.
fn split_target_args(args: &str) -> (String, String) {
    let trimmed = args.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((target, text)) => (target.to_string(), text.trim().to_string()),
        None => (trimmed.to_string(), String::new()),
    }
}

/// Split a `/send` argument tail into `(session, prompt)`.
///
/// Why: `/send <session> <prompt>` carries the session as the first token and the
/// (possibly multi-word) prompt as the remainder; Slack hands the whole tail as
/// one string, so the split happens here (mirroring the Telegram adapter).
/// What: returns the first whitespace-separated token as `session` and the
/// trimmed remainder as `prompt`; both are empty strings when absent.
/// Test: `split_send_args_separates_session_and_prompt`.
fn split_send_args(args: &str) -> (String, String) {
    let trimmed = args.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((session, prompt)) => (session.to_string(), prompt.trim().to_string()),
        None => (trimmed.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_slash_strips_leading_slash() {
        // Slack delivers the command WITH a leading slash; the parser strips it.
        assert_eq!(parse_slash("/fleet", ""), Some(SlackCommand::Fleet));
        // It also tolerates a verb with no slash (defensive).
        assert_eq!(parse_slash("fleet", ""), Some(SlackCommand::Fleet));
        // Case-insensitive.
        assert_eq!(parse_slash("/FLEET", ""), Some(SlackCommand::Fleet));
    }

    #[test]
    fn parse_slash_unknown_is_none() {
        // An unknown verb yields None so the caller can fall back to free text.
        assert_eq!(parse_slash("/notacommand", "blah"), None);
        assert_eq!(parse_slash("", ""), None);
    }

    #[test]
    fn parse_slash_carries_argument_tail() {
        // The text argument is trimmed and carried through verbatim.
        assert_eq!(
            parse_slash("/status", "  abc-123  "),
            Some(SlackCommand::Status("abc-123".into()))
        );
        assert_eq!(
            parse_slash("/msend", "sess-1 build it now"),
            Some(SlackCommand::Msend("sess-1 build it now".into()))
        );
    }

    #[test]
    fn parse_slash_maps_every_verb() {
        // Every advertised verb name round-trips through the parser: the help/
        // registration list and the parser can never drift (a missing arm makes
        // this fail, not a brittle hardcoded count). The list must be non-empty.
        let names = SlackCommand::all_names();
        assert!(!names.is_empty(), "all_names() must advertise verbs");
        for name in names {
            let with_slash = format!("/{name}");
            assert!(
                parse_slash(&with_slash, "").is_some(),
                "verb `{name}` failed to parse back",
            );
        }
    }

    #[test]
    fn slack_command_converts_to_trusty() {
        // No-argument verbs map one-to-one.
        assert_eq!(
            TrustyCommand::from(SlackCommand::Sessions),
            TrustyCommand::Sessions
        );
        assert_eq!(TrustyCommand::from(SlackCommand::Help), TrustyCommand::Help);
        assert_eq!(
            TrustyCommand::from(SlackCommand::Fleet),
            TrustyCommand::ManagedList
        );
        // Argument-carrying verbs thread the argument through.
        assert_eq!(
            TrustyCommand::from(SlackCommand::Status("abc-123".into())),
            TrustyCommand::Status {
                session_id: "abc-123".into()
            }
        );
        assert_eq!(
            TrustyCommand::from(SlackCommand::Get(" sess-1 ".into())),
            TrustyCommand::ManagedGet {
                target: "sess-1".into()
            }
        );
        // Health maps to the health verb.
        assert_eq!(
            TrustyCommand::from(SlackCommand::Health),
            TrustyCommand::Health
        );
    }

    #[test]
    fn launch_command_converts() {
        // `/launch <workdir> | <task>` maps to ManagedNew with an empty git_ref.
        assert_eq!(
            TrustyCommand::from(SlackCommand::Launch("/work/repo | build it".into())),
            TrustyCommand::ManagedNew {
                repo_url: "/work/repo".into(),
                git_ref: String::new(),
                task: "build it".into(),
                name_hint: None,
                runtime: None,
            }
        );
    }

    #[test]
    fn msend_and_answer_commands_convert() {
        assert_eq!(
            TrustyCommand::from(SlackCommand::Msend("sess-1 build now".into())),
            TrustyCommand::ManagedSend {
                target: "sess-1".into(),
                text: "build now".into(),
            }
        );
        assert_eq!(
            TrustyCommand::from(SlackCommand::Answer("sess-1 yes proceed".into())),
            TrustyCommand::ManagedAnswer {
                target: "sess-1".into(),
                answer: "yes proceed".into(),
            }
        );
    }

    #[test]
    fn activity_resume_decommission_convert() {
        assert_eq!(
            TrustyCommand::from(SlackCommand::Activity("a".into())),
            TrustyCommand::ManagedActivity { target: "a".into() }
        );
        assert_eq!(
            TrustyCommand::from(SlackCommand::Resume("b".into())),
            TrustyCommand::ManagedResume { target: "b".into() }
        );
        assert_eq!(
            TrustyCommand::from(SlackCommand::Decommission("c".into())),
            TrustyCommand::ManagedDecommission { target: "c".into() }
        );
    }

    #[test]
    fn split_launch_args_splits_on_pipe() {
        let (workdir, task) = split_launch_args("/work/repo | run the full test suite");
        assert_eq!(workdir, "/work/repo");
        assert_eq!(task, "run the full test suite");
        let (workdir, task) = split_launch_args("/work/repo");
        assert_eq!(workdir, "/work/repo");
        assert!(task.is_empty());
    }

    #[test]
    fn split_target_args_separates_target_and_text() {
        let (target, text) = split_target_args("sess-1 please continue now");
        assert_eq!(target, "sess-1");
        assert_eq!(text, "please continue now");
        let (target, text) = split_target_args("sess-1");
        assert_eq!(target, "sess-1");
        assert!(text.is_empty());
    }

    #[test]
    fn split_send_args_separates_session_and_prompt() {
        let (session, prompt) = split_send_args("frontend run the tests please");
        assert_eq!(session, "frontend");
        assert_eq!(prompt, "run the tests please");
        let (session, prompt) = split_send_args("frontend");
        assert_eq!(session, "frontend");
        assert!(prompt.is_empty());
    }
}
