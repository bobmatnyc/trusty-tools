//! Native Telegram slash commands.
//!
//! Why: the bot used to hand-roll a `parse()` function over raw message text.
//! teloxide's `#[derive(BotCommands)]` gives the same dispatch for free, plus a
//! generated `/`-command picker and per-command help — so the manual parser is
//! gone.
//! What: [`TelegramCommand`] is the teloxide-native command enum; the `From`
//! impl converts it into the shared, UI-agnostic
//! [`crate::client::TrustyCommand`] that the executor consumes. An empty
//! argument to `/pair` or `/status` etc. is preserved as an empty string and
//! handled downstream (e.g. `/pair` with no code queries pairing status).
//! Test: `cargo test -p trusty-mpm-telegram` covers the conversion.

use crate::client::TrustyCommand;
use teloxide::utils::command::BotCommands;

/// The bot's native Telegram slash commands.
///
/// Why: deriving `BotCommands` makes teloxide parse `/command args` for us and
/// lets `set_my_commands` register the picker from `bot_commands()`.
/// What: one variant per operator action; single-`String` variants take the
/// remainder of the message as their argument (empty when omitted).
/// Test: `telegram_command_converts_to_trusty_command`.
#[derive(BotCommands, Clone, Debug, PartialEq, Eq)]
#[command(rename_rule = "lowercase", description = "trusty-mpm commands:")]
pub enum TelegramCommand {
    /// List managed sessions.
    #[command(description = "List managed sessions")]
    Sessions,
    /// Session status — `/status <id>`.
    #[command(description = "Session status")]
    Status(String),
    /// Approve a permission request — `/approve <id>`.
    #[command(description = "Approve a permission request")]
    Approve(String),
    /// Deny a permission request — `/deny <id>`.
    #[command(description = "Deny a permission request")]
    Deny(String),
    /// Show overseer status.
    #[command(description = "Show overseer status")]
    Overseer,
    /// List all tmux sessions.
    #[command(description = "List all tmux sessions")]
    Tmux,
    /// Discover projects from Claude Code config.
    #[command(description = "Discover projects from Claude Code config")]
    Projects,
    /// Auto-discover tmux sessions running Claude Code.
    #[command(description = "Auto-discover tmux sessions running Claude Code")]
    Discover,
    /// Adopt an external tmux session — `/adopt <session>`.
    #[command(description = "Adopt an external tmux session")]
    Adopt(String),
    /// Analyze Claude Code config — `/config <path>`.
    #[command(description = "Analyze Claude Code config")]
    Config(String),
    /// Capture tmux pane output — `/snapshot <session>`.
    #[command(description = "Capture tmux pane output")]
    Snapshot(String),
    /// Kill a session — `/kill <id>`.
    #[command(description = "Kill a session")]
    Kill(String),
    /// Send a prompt to a Claude Code session — `/send <session> <prompt>`.
    // The whole argument tail is captured as one string; the first whitespace-
    // separated token is the session, the remainder is the prompt (so a
    // multi-word prompt is preserved). A plain `//` comment keeps this note out
    // of the teloxide-generated command description, which Telegram caps at 256
    // characters.
    #[command(description = "Send a prompt to a session")]
    Send(String),
    /// Show alert subscriptions.
    #[command(description = "Show alert subscriptions")]
    Alerts,
    /// Pair with the daemon — `/pair <code>`.
    #[command(description = "Pair with daemon")]
    Pair(String),
    /// Start and pair — `/start [code]`.
    #[command(description = "Start and pair")]
    Start(String),
    /// Connect to or start a session without deployment — `/connect <path>`.
    #[command(description = "Connect to or start a session (no deployment)")]
    Connect(String),
    /// Run a full system diagnostic.
    #[command(description = "Run full system diagnostic")]
    Doctor,
    /// Report daemon health.
    #[command(description = "Report daemon health (reachability, catalog, fleet)")]
    Health,
    /// Launch a managed session — `/launch <workdir> | <task...>`.
    // The argument tail is split on the first `|`: the left side is the
    // workdir/repo (mapped to `repo_url` with an empty `git_ref`), the right side
    // is the task. A plain `//` comment keeps the syntax note out of the
    // teloxide-generated description (Telegram caps it at 256 chars).
    #[command(description = "Launch a managed session: /launch <workdir> | <task>")]
    Launch(String),
    /// List managed (fleet) sessions.
    #[command(description = "List managed fleet sessions")]
    Fleet,
    /// Get one managed session — `/get <id|name>`.
    #[command(description = "Get a managed session by id or name")]
    Get(String),
    /// Send text to a managed session — `/msend <id|name> <text>`.
    #[command(description = "Send text to a managed session")]
    Msend(String),
    /// Answer a managed session's pending decision — `/answer <id|name> <text>`.
    #[command(description = "Answer a managed session's pending decision")]
    Answer(String),
    /// Inspect a managed session's activity — `/activity <id|name>`.
    #[command(description = "Inspect a managed session's activity")]
    Activity(String),
    /// Resume a stopped managed session — `/resume <id|name>`.
    #[command(description = "Resume a stopped managed session")]
    Resume(String),
    /// Decommission a managed session — `/decommission <id|name>`.
    #[command(description = "Decommission a managed session (terminal)")]
    Decommission(String),
    /// Focus a managed session so plain messages route to it — `/focus <id|name>`.
    #[command(description = "Focus a session so plain messages route to it")]
    Focus(String),
    /// Clear the focused session, returning to coordinator chat — `/unfocus`.
    #[command(description = "Clear the focused session")]
    Unfocus,
    /// Digest the focused session's recent activity back to the chat — `/summary`.
    #[command(description = "Summarize the focused session's activity")]
    Summary,
    /// Show all commands.
    #[command(description = "Show all commands")]
    Help,
}

impl From<TelegramCommand> for TrustyCommand {
    /// Convert a teloxide command into the shared command model.
    ///
    /// Why: the executor only understands [`TrustyCommand`]; the bot's native
    /// enum must be projected onto it before dispatch.
    /// What: maps each variant; an empty `/pair`/`/start` argument becomes
    /// `Pair { code: None }` / `Start` so "no code" queries pairing status.
    /// Test: `telegram_command_converts_to_trusty_command`.
    fn from(cmd: TelegramCommand) -> Self {
        match cmd {
            TelegramCommand::Sessions => TrustyCommand::Sessions,
            TelegramCommand::Status(session_id) => TrustyCommand::Status { session_id },
            TelegramCommand::Approve(session_id) => TrustyCommand::Approve { session_id },
            TelegramCommand::Deny(session_id) => TrustyCommand::Deny { session_id },
            TelegramCommand::Overseer => TrustyCommand::Overseer,
            TelegramCommand::Tmux => TrustyCommand::Tmux,
            TelegramCommand::Projects => TrustyCommand::Projects,
            TelegramCommand::Discover => TrustyCommand::Discover,
            TelegramCommand::Adopt(session) => TrustyCommand::Adopt { session },
            TelegramCommand::Config(project) => TrustyCommand::Config { project },
            TelegramCommand::Snapshot(session) => TrustyCommand::Snapshot { session },
            TelegramCommand::Kill(session_id) => TrustyCommand::Kill { session_id },
            TelegramCommand::Send(args) => {
                let (session, prompt) = split_send_args(&args);
                TrustyCommand::Send { session, prompt }
            }
            TelegramCommand::Alerts => TrustyCommand::Alerts,
            TelegramCommand::Pair(code) => TrustyCommand::Pair {
                code: non_empty(code),
            },
            TelegramCommand::Connect(project) => TrustyCommand::Connect {
                project: project.trim().into(),
                session_name: None,
            },
            TelegramCommand::Start(_) => TrustyCommand::Start,
            TelegramCommand::Doctor => TrustyCommand::Doctor,
            TelegramCommand::Health => TrustyCommand::Health,
            TelegramCommand::Launch(args) => {
                let (repo_url, task) = split_launch_args(&args);
                TrustyCommand::ManagedNew {
                    repo_url,
                    git_ref: String::new(),
                    task,
                    name_hint: None,
                    runtime: None,
                    inject_task: None,
                }
            }
            TelegramCommand::Fleet => TrustyCommand::ManagedFleet,
            TelegramCommand::Get(target) => TrustyCommand::ManagedGet {
                target: target.trim().to_string(),
            },
            TelegramCommand::Msend(args) => {
                let (target, text) = split_target_args(&args);
                TrustyCommand::ManagedSend { target, text }
            }
            TelegramCommand::Answer(args) => {
                let (target, answer) = split_target_args(&args);
                TrustyCommand::ManagedAnswer { target, answer }
            }
            TelegramCommand::Activity(target) => TrustyCommand::ManagedActivity {
                target: target.trim().to_string(),
            },
            TelegramCommand::Resume(target) => TrustyCommand::ManagedResume {
                target: target.trim().to_string(),
            },
            TelegramCommand::Decommission(target) => TrustyCommand::ManagedDecommission {
                target: target.trim().to_string(),
            },
            // `/focus`, `/unfocus`, and `/summary` drive the session-manager PROXY
            // (per-chat focus + INJECT/SUMMARIZE), not the daemon command surface —
            // they are intercepted in `telegram::on_message` before this conversion
            // and never reach the executor. These arms only keep the exhaustive
            // match total; mapping to the harmless `Help` command is a safe,
            // non-destructive fallback should the interception ever be bypassed.
            TelegramCommand::Focus(_) | TelegramCommand::Unfocus | TelegramCommand::Summary => {
                TrustyCommand::Help
            }
            TelegramCommand::Help => TrustyCommand::Help,
        }
    }
}

/// Split a `/launch` argument tail into `(workdir, task)` on the first `|`.
///
/// Why: launching a managed session needs a workspace and a task; Telegram hands
/// the whole tail as one string, so `/launch <workdir> | <task...>` is parsed
/// here. The pipe keeps multi-word workdirs and tasks unambiguous without a
/// fixed token count.
/// What: returns the trimmed text before the first `|` as the workdir and the
/// trimmed remainder as the task. With no `|`, the whole tail is treated as the
/// workdir and the task is empty — the executor then reports the missing task
/// (never a panic), and the help text documents the usage.
/// Test: `split_launch_args_splits_on_pipe`, `launch_command_converts`.
fn split_launch_args(args: &str) -> (String, String) {
    match args.split_once('|') {
        Some((workdir, task)) => (workdir.trim().to_string(), task.trim().to_string()),
        None => (args.trim().to_string(), String::new()),
    }
}

/// Split a managed `target <text...>` argument tail into `(target, text)`.
///
/// Why: `/msend` and `/answer` carry the session id/name as the first token and
/// the (possibly multi-word) payload as the remainder; teloxide hands the whole
/// tail as one string, so the split happens here.
/// What: returns the first whitespace-separated token as `target` and the
/// trimmed remainder as the payload; both are empty strings when absent (the
/// executor then reports the missing argument rather than panicking).
/// Test: `split_target_args_separates_target_and_text`, `msend_command_converts`.
fn split_target_args(args: &str) -> (String, String) {
    let trimmed = args.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((target, text)) => (target.to_string(), text.trim().to_string()),
        None => (trimmed.to_string(), String::new()),
    }
}

/// Split a `/send` argument tail into `(session, prompt)`.
///
/// Why: `/send <session> <prompt>` carries the session as the first token and
/// the (possibly multi-word) prompt as the remainder; teloxide hands the whole
/// tail as one string, so the split happens here.
/// What: returns the first whitespace-separated token as `session` and the
/// trimmed remainder as `prompt`; both are empty strings when absent (the
/// executor then reports the missing argument).
/// Test: `split_send_args_separates_session_and_prompt`.
fn split_send_args(args: &str) -> (String, String) {
    let trimmed = args.trim();
    match trimmed.split_once(char::is_whitespace) {
        Some((session, prompt)) => (session.to_string(), prompt.trim().to_string()),
        None => (trimmed.to_string(), String::new()),
    }
}

/// Map an empty argument string to `None`, a non-empty one to `Some`.
///
/// Why: teloxide hands a missing `String` argument as `""`; the command model
/// distinguishes "no argument" via `Option`, so the empty string is normalized.
/// What: trims and returns `Some(trimmed)` when non-empty, else `None`.
/// Test: covered by `telegram_command_converts_to_trusty_command`.
fn non_empty(s: String) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_command_converts_to_trusty_command() {
        // The no-argument commands map one-to-one.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Sessions),
            TrustyCommand::Sessions
        );
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Help),
            TrustyCommand::Help
        );
        // An argument-carrying command threads its argument through.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Status("abc-123".into())),
            TrustyCommand::Status {
                session_id: "abc-123".into()
            }
        );
        // `/pair` with a code carries it; `/pair` with no code is a status query.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Pair("A4X9KZ".into())),
            TrustyCommand::Pair {
                code: Some("A4X9KZ".into())
            }
        );
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Pair(String::new())),
            TrustyCommand::Pair { code: None }
        );
        // `/start` always becomes the Start command regardless of any argument.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Start("A4X9KZ".into())),
            TrustyCommand::Start
        );
        // `/doctor` maps one-to-one onto the diagnostic command.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Doctor),
            TrustyCommand::Doctor
        );
        // `/health` maps one-to-one onto the health verb.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Health),
            TrustyCommand::Health
        );
        // `/connect <path>` threads the project path through, no session name.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Connect("/work/p".into())),
            TrustyCommand::Connect {
                project: "/work/p".into(),
                session_name: None,
            }
        );
    }

    #[test]
    fn bot_commands_lists_every_command() {
        // teloxide's generated descriptor must enumerate all commands: the 20
        // legacy ones, the 8 managed-fleet verbs added for the drive surface, and
        // the 3 proxy verbs (/focus, /unfocus, /summary) added for TELUI-6 (#1440).
        let descriptions = TelegramCommand::bot_commands();
        assert_eq!(descriptions.len(), 31);
        assert!(descriptions.iter().any(|c| c.command == "/health"));
        assert!(descriptions.iter().any(|c| c.command == "/sessions"));
        assert!(descriptions.iter().any(|c| c.command == "/connect"));
        assert!(descriptions.iter().any(|c| c.command == "/pair"));
        assert!(descriptions.iter().any(|c| c.command == "/start"));
        assert!(descriptions.iter().any(|c| c.command == "/projects"));
        assert!(descriptions.iter().any(|c| c.command == "/adopt"));
        assert!(descriptions.iter().any(|c| c.command == "/send"));
        assert!(descriptions.iter().any(|c| c.command == "/discover"));
        assert!(descriptions.iter().any(|c| c.command == "/doctor"));
        // The managed-fleet drive verbs.
        assert!(descriptions.iter().any(|c| c.command == "/launch"));
        assert!(descriptions.iter().any(|c| c.command == "/fleet"));
        assert!(descriptions.iter().any(|c| c.command == "/get"));
        assert!(descriptions.iter().any(|c| c.command == "/msend"));
        assert!(descriptions.iter().any(|c| c.command == "/answer"));
        assert!(descriptions.iter().any(|c| c.command == "/activity"));
        assert!(descriptions.iter().any(|c| c.command == "/resume"));
        assert!(descriptions.iter().any(|c| c.command == "/decommission"));
        // The TELUI-6 proxy verbs.
        assert!(descriptions.iter().any(|c| c.command == "/focus"));
        assert!(descriptions.iter().any(|c| c.command == "/unfocus"));
        assert!(descriptions.iter().any(|c| c.command == "/summary"));
    }

    #[test]
    fn proxy_verbs_are_adapter_local() {
        // `/focus`, `/unfocus`, and `/summary` drive the session-manager proxy in
        // the adapter, not the daemon command surface. The From conversion is
        // never invoked for them in practice (they are intercepted before
        // dispatch); this documents the safe Help fallback the exhaustive match
        // provides.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Focus("api".into())),
            TrustyCommand::Help
        );
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Unfocus),
            TrustyCommand::Help
        );
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Summary),
            TrustyCommand::Help
        );
    }

    #[test]
    fn split_launch_args_splits_on_pipe() {
        // `/launch <workdir> | <task...>` splits on the first pipe; both sides
        // are trimmed and the task may be multi-word.
        let (workdir, task) = split_launch_args("/work/repo | run the full test suite");
        assert_eq!(workdir, "/work/repo");
        assert_eq!(task, "run the full test suite");
        // With no pipe, the whole tail is the workdir and the task is empty.
        let (workdir, task) = split_launch_args("/work/repo");
        assert_eq!(workdir, "/work/repo");
        assert!(task.is_empty());
    }

    #[test]
    fn split_target_args_separates_target_and_text() {
        // The first token is the target; the remainder (multi-word) is the text.
        let (target, text) = split_target_args("sess-1 please continue now");
        assert_eq!(target, "sess-1");
        assert_eq!(text, "please continue now");
        // A target with no text yields an empty payload.
        let (target, text) = split_target_args("sess-1");
        assert_eq!(target, "sess-1");
        assert!(text.is_empty());
    }

    #[test]
    fn launch_command_converts() {
        // `/launch <workdir> | <task>` maps to ManagedNew with an empty git_ref.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Launch("/work/repo | build it".into())),
            TrustyCommand::ManagedNew {
                repo_url: "/work/repo".into(),
                git_ref: String::new(),
                task: "build it".into(),
                name_hint: None,
                runtime: None,
                inject_task: None,
            }
        );
    }

    #[test]
    fn fleet_command_converts() {
        // `/fleet` maps to ManagedFleet (project-grouped view) since WI-B (#1586).
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Fleet),
            TrustyCommand::ManagedFleet
        );
    }

    #[test]
    fn get_command_converts() {
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Get(" sess-1 ".into())),
            TrustyCommand::ManagedGet {
                target: "sess-1".into()
            }
        );
    }

    #[test]
    fn msend_command_converts() {
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Msend("sess-1 build now".into())),
            TrustyCommand::ManagedSend {
                target: "sess-1".into(),
                text: "build now".into(),
            }
        );
    }

    #[test]
    fn answer_command_converts() {
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Answer("sess-1 yes proceed".into())),
            TrustyCommand::ManagedAnswer {
                target: "sess-1".into(),
                answer: "yes proceed".into(),
            }
        );
    }

    #[test]
    fn activity_resume_decommission_convert() {
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Activity("a".into())),
            TrustyCommand::ManagedActivity { target: "a".into() }
        );
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Resume("b".into())),
            TrustyCommand::ManagedResume { target: "b".into() }
        );
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Decommission("c".into())),
            TrustyCommand::ManagedDecommission { target: "c".into() }
        );
    }

    #[test]
    fn bot_command_descriptions_fit_telegram_limits() {
        // Telegram rejects `set_my_commands` if any command name exceeds 32
        // characters or any description exceeds 256. teloxide concatenates the
        // doc comment with the `description` attribute, so this guards against
        // a long doc comment silently re-introducing the startup crash.
        for cmd in TelegramCommand::bot_commands() {
            let name = cmd.command.trim_start_matches('/');
            assert!(
                (1..=32).contains(&name.chars().count()),
                "command `{name}` name length out of Telegram's 1..=32 range",
            );
            assert!(
                (3..=256).contains(&cmd.description.chars().count()),
                "command `{}` description length {} out of Telegram's 3..=256 range",
                cmd.command,
                cmd.description.chars().count(),
            );
        }
    }

    #[test]
    fn split_send_args_separates_session_and_prompt() {
        // The first token is the session; the remainder (multi-word) is the prompt.
        let (session, prompt) = split_send_args("frontend run the tests please");
        assert_eq!(session, "frontend");
        assert_eq!(prompt, "run the tests please");
        // A session with no prompt yields an empty prompt.
        let (session, prompt) = split_send_args("frontend");
        assert_eq!(session, "frontend");
        assert!(prompt.is_empty());
    }

    #[test]
    fn send_command_converts_to_trusty_command() {
        // `/send` threads the session and prompt through to the shared command.
        assert_eq!(
            TrustyCommand::from(TelegramCommand::Send("frontend build now".into())),
            TrustyCommand::Send {
                session: "frontend".into(),
                prompt: "build now".into(),
            }
        );
    }

    #[test]
    fn parse_round_trips_a_command() {
        // teloxide's parser turns raw text into the typed command.
        let cmd = TelegramCommand::parse("/status abc-123", "trusty_mpm_bot").unwrap();
        assert_eq!(cmd, TelegramCommand::Status("abc-123".into()));
    }
}
