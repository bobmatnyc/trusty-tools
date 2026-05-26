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
            TelegramCommand::Help => TrustyCommand::Help,
        }
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
        // teloxide's generated descriptor must enumerate all nineteen commands.
        let descriptions = TelegramCommand::bot_commands();
        assert_eq!(descriptions.len(), 19);
        assert!(descriptions.iter().any(|c| c.command == "/sessions"));
        assert!(descriptions.iter().any(|c| c.command == "/connect"));
        assert!(descriptions.iter().any(|c| c.command == "/pair"));
        assert!(descriptions.iter().any(|c| c.command == "/start"));
        assert!(descriptions.iter().any(|c| c.command == "/projects"));
        assert!(descriptions.iter().any(|c| c.command == "/adopt"));
        assert!(descriptions.iter().any(|c| c.command == "/send"));
        assert!(descriptions.iter().any(|c| c.command == "/discover"));
        assert!(descriptions.iter().any(|c| c.command == "/doctor"));
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
