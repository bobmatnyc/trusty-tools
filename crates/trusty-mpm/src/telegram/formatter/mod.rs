//! Telegram-specific rendering of [`CommandResult`].
//!
//! Why: the executor returns a structured, UI-agnostic [`CommandResult`]; the
//! Telegram bot must turn that into HTML message text and, for the session
//! list, an inline keyboard. Keeping the rendering pure (no network, no
//! teloxide runtime) makes it unit-testable.
//! What: [`TelegramFormatter::format`] produces the HTML body and
//! [`TelegramFormatter::keyboard_for`] the optional inline keyboard.
//! Test: `cargo test -p trusty-mpm-telegram` covers each variant's rendering.

use crate::client::{CommandResult, DiscoveredProjectSummary};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

#[cfg(test)]
mod tests;

/// How many characters of a session id to show in chat output.
///
/// Why: full UUIDs are unreadable on a phone; the first 8 chars disambiguate in
/// practice while keeping messages compact.
const SHORT_ID_LEN: usize = 8;

/// Renders [`CommandResult`]s into Telegram HTML messages and keyboards.
///
/// Why: the bot's message handler stays thin — it executes a command and hands
/// the result here for presentation.
/// What: a stateless formatter; both methods are associated functions.
/// Test: the `format_*` tests in `tests.rs`.
pub struct TelegramFormatter;

impl TelegramFormatter {
    /// Render a [`CommandResult`] into an HTML message body.
    ///
    /// Why: every command's reply text is produced here so presentation is
    /// consistent and testable.
    /// What: matches each variant and returns an HTML-formatted string suitable
    /// for teloxide's `ParseMode::Html`.
    /// Test: `format_sessions_*`, `pair_code_command_formats_correctly`, etc.
    pub fn format(result: &CommandResult) -> String {
        match result {
            CommandResult::Sessions(sessions) => {
                if sessions.is_empty() {
                    return "No active sessions.".to_string();
                }
                let mut text = String::from("<b>trusty-mpm sessions</b>\n");
                for s in sessions {
                    let dot = if s.status.eq_ignore_ascii_case("active") {
                        "🟢"
                    } else {
                        "🔴"
                    };
                    text.push_str(&format!(
                        "\n{dot} <code>{}</code> — {}\n  📁 <code>{}</code>\n",
                        short_id(&s.id),
                        s.status,
                        s.workdir,
                    ));
                }
                text
            }
            CommandResult::SessionDetail { id, events, .. } => {
                if events.is_empty() {
                    format!("Session {id}: no recent events")
                } else {
                    let lines = events
                        .iter()
                        .map(|e| format!("• {e}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("<b>Session {}</b>\n{lines}", short_id(id))
                }
            }
            CommandResult::OverseerStatus {
                enabled,
                handler,
                decisions,
            } => format!(
                "<b>Overseer Status</b>\nHandler: <code>{handler}</code>\nEnabled: {}\n\
                 Recent decisions: allow ({}), block ({}), flag ({})",
                if *enabled { "✅" } else { "❌" },
                decisions.allow,
                decisions.block,
                decisions.flag,
            ),
            CommandResult::TmuxSessions(sessions) => {
                if sessions.is_empty() {
                    return "No tmux sessions found.".to_string();
                }
                let lines = sessions
                    .iter()
                    .map(|s| {
                        let tag = if s.managed {
                            "🟢 managed"
                        } else {
                            "⚪ external"
                        };
                        format!("• <code>{}</code> — {tag}", s.name)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("<b>tmux sessions</b>\n{lines}")
            }
            CommandResult::DiscoveredProjects(projects) => format_discovered_projects(projects),
            CommandResult::Adopted { session } => {
                format!("✅ Adopted tmux session <code>{session}</code> for oversight")
            }
            CommandResult::Discovered { count } => {
                if *count == 0 {
                    "🔍 No new Claude Code tmux sessions found".to_string()
                } else {
                    format!("🔍 Discovered and adopted {count} Claude Code tmux session(s)")
                }
            }
            CommandResult::ProjectRegistered { path } => {
                format!("✅ Registered project <code>{path}</code>")
            }
            CommandResult::ConfigAnalysis {
                project,
                recommendations,
            } => {
                if recommendations.is_empty() {
                    format!(
                        "<b>Claude config</b> for <code>{project}</code>\n\
                         No recommendations — config looks healthy."
                    )
                } else {
                    let lines = recommendations
                        .iter()
                        .map(|r| format!("• {}", r.message))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("<b>Claude config</b> for <code>{project}</code>\n{lines}")
                }
            }
            CommandResult::Snapshot { session, output } => {
                let tail: Vec<&str> = output.lines().rev().take(50).collect();
                let lines = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
                if lines.is_empty() {
                    format!("Session <code>{session}</code>: empty pane")
                } else {
                    format!(
                        "<b>Snapshot: {session}</b>\n<pre>{}</pre>",
                        html_escape(&lines)
                    )
                }
            }
            CommandResult::Killed { session_id } => {
                format!("🗑️ Session {} killed", short_id(session_id))
            }
            CommandResult::CommandSent { session, output } => {
                if output.trim().is_empty() {
                    format!("📨 Sent to <code>{session}</code> — no output captured")
                } else {
                    format!("<b>📨 {session}</b>\n<pre>{}</pre>", html_escape(output))
                }
            }
            CommandResult::ChatReply { reply } => html_escape(reply),
            CommandResult::Approved { session_id } => {
                format!("✅ Permission approved for session {session_id}")
            }
            CommandResult::Denied { session_id } => {
                format!("❌ Permission denied for session {session_id}")
            }
            CommandResult::PairCode {
                code,
                expires_in_seconds,
            } => format!(
                "<b>Pairing code:</b> <code>{code}</code>\n\
                 Expires in {} minutes\n\nSend to your bot: <code>/pair {code}</code>",
                expires_in_seconds / 60,
            ),
            CommandResult::PairSuccess { chat_info } => {
                format!(
                    "✅ Successfully paired! This chat ({chat_info}) is now registered for alerts."
                )
            }
            CommandResult::PairState { paired } => {
                if *paired {
                    "✅ Bot is paired with this daemon. Type /help to see commands.".to_string()
                } else {
                    "👋 Welcome to trusty-mpm bot! To pair this bot with your daemon, run \
                     `tm pair` on your server, then send the code with /pair <code>"
                        .to_string()
                }
            }
            CommandResult::AlertSubscriptions(lines) => {
                format!("<b>Alert subscription</b>\n{}", lines.join("\n"))
            }
            CommandResult::Doctor(report) => format_doctor_report(report),
            CommandResult::SessionStarted {
                session,
                workdir,
                deployed,
            } => {
                let mode = if *deployed {
                    "launched (framework deployed)"
                } else {
                    "connected (no deployment)"
                };
                format!("✅ Session <b>{session}</b> {mode}\n<code>{workdir}</code>")
            }
            CommandResult::Help(text) => text.clone(),
            CommandResult::Error(msg) => format!("❌ {msg}"),
        }
    }

    /// Build the inline keyboard for a [`CommandResult`], if it warrants one.
    ///
    /// Why: several lists decorate their rows with action buttons — `/sessions`
    /// gets `[Status] [Approve] [Deny]`, `/projects` gets a `[Set Active]`
    /// button per project, and `/tmux` gets an `[Adopt]` button for each
    /// unmanaged session.
    /// What: returns one button row per item for the list variants above,
    /// `None` for every other variant. Callback data is `verb:arg` so the
    /// callback handler can route the tap.
    /// Test: `keyboard_for_sessions_has_rows`, `keyboard_for_projects_has_rows`,
    /// `keyboard_for_tmux_adopts_external`, `keyboard_for_help_is_none`.
    pub fn keyboard_for(result: &CommandResult) -> Option<InlineKeyboardMarkup> {
        match result {
            CommandResult::Sessions(sessions) if !sessions.is_empty() => {
                let rows: Vec<Vec<InlineKeyboardButton>> = sessions
                    .iter()
                    .map(|s| {
                        vec![
                            InlineKeyboardButton::callback("📋 Status", format!("status:{}", s.id)),
                            InlineKeyboardButton::callback(
                                "✅ Approve",
                                format!("approve:{}", s.id),
                            ),
                            InlineKeyboardButton::callback("❌ Deny", format!("deny:{}", s.id)),
                        ]
                    })
                    .collect();
                Some(InlineKeyboardMarkup::new(rows))
            }
            CommandResult::DiscoveredProjects(projects) if !projects.is_empty() => {
                let rows: Vec<Vec<InlineKeyboardButton>> = projects
                    .iter()
                    .filter(|p| callback_fits(&p.path))
                    .map(|p| {
                        vec![InlineKeyboardButton::callback(
                            format!("📁 Set Active — {}", project_basename(&p.path)),
                            format!("setproj:{}", p.path),
                        )]
                    })
                    .collect();
                if rows.is_empty() {
                    None
                } else {
                    Some(InlineKeyboardMarkup::new(rows))
                }
            }
            CommandResult::TmuxSessions(sessions) => {
                let rows: Vec<Vec<InlineKeyboardButton>> = sessions
                    .iter()
                    .filter(|s| !s.managed && callback_fits(&s.name))
                    .map(|s| {
                        vec![InlineKeyboardButton::callback(
                            format!("➕ Adopt — {}", s.name),
                            format!("adopt:{}", s.name),
                        )]
                    })
                    .collect();
                if rows.is_empty() {
                    None
                } else {
                    Some(InlineKeyboardMarkup::new(rows))
                }
            }
            _ => None,
        }
    }
}

/// Render a discovered-project list as a Telegram HTML message body.
///
/// Why: the `/projects` command lists projects mined from `~/.claude/projects/`;
/// keeping the rendering as a free function lets it be unit-tested and reused.
/// What: returns a placeholder line when the list is empty, otherwise one line
/// per project showing the path, recorded session count, and last-used date.
/// Test: `format_discovered_projects_lists_each`, `format_discovered_projects_empty`.
pub fn format_discovered_projects(projects: &[DiscoveredProjectSummary]) -> String {
    if projects.is_empty() {
        return "No projects discovered in Claude Code config.".to_string();
    }
    let mut text = String::from("<b>Discovered projects</b>\n");
    for p in projects {
        let last = p
            .last_session
            .as_deref()
            .map(|s| s.split('T').next().unwrap_or(s).to_string())
            .unwrap_or_else(|| "never".to_string());
        text.push_str(&format!(
            "\n📁 <code>{}</code>\n  {} session(s) · last used {last}\n",
            p.path, p.session_count,
        ));
    }
    text
}

/// Render a [`DoctorReport`] as a Telegram HTML message body.
///
/// Why: the `/doctor` command's diagnostic must be readable on a phone — one
/// emoji-tagged line per check plus an overall verdict.
/// What: returns a heading, one `<icon> <name> — <message>` line per check, and
/// a final overall-status line.
/// Test: `format_doctor_report_lists_each_check`.
fn format_doctor_report(report: &crate::client::DoctorReport) -> String {
    let mut text = String::from("<b>trusty-mpm doctor</b>\n");
    for check in &report.checks {
        text.push_str(&format!(
            "\n{} <b>{}</b> — {}",
            status_icon(check.status),
            html_escape(&check.name),
            html_escape(&check.message),
        ));
    }
    text.push_str(&format!(
        "\n\n{} <b>overall: {}</b>",
        status_icon(report.overall),
        status_word(report.overall),
    ));
    text
}

/// The emoji icon for a doctor [`CheckStatus`](crate::core::doctor::CheckStatus).
///
/// Why: the `/doctor` message marks each check with a glanceable status symbol.
/// What: `Ok → ✅`, `Warn → ⚠️`, `Fail → ❌`.
/// Test: covered by `format_doctor_report_lists_each_check`.
fn status_icon(status: crate::core::doctor::CheckStatus) -> &'static str {
    use crate::core::doctor::CheckStatus;
    match status {
        CheckStatus::Ok => "✅",
        CheckStatus::Warn => "⚠️",
        CheckStatus::Fail => "❌",
    }
}

/// A one-word label for a doctor [`CheckStatus`](crate::core::doctor::CheckStatus).
///
/// Why: the overall verdict reads better with a word than a bare icon.
/// What: `Ok → "healthy"`, `Warn → "warnings"`, `Fail → "failed"`.
/// Test: covered by `format_doctor_report_lists_each_check`.
fn status_word(status: crate::core::doctor::CheckStatus) -> &'static str {
    use crate::core::doctor::CheckStatus;
    match status {
        CheckStatus::Ok => "healthy",
        CheckStatus::Warn => "warnings",
        CheckStatus::Fail => "failed",
    }
}

/// True when a string fits within Telegram's 64-byte callback-data budget.
///
/// Why: Telegram rejects inline-keyboard callback data over 64 bytes; a `verb:`
/// prefix (≤8 bytes) plus the argument must stay under the limit, so a button
/// is omitted rather than crashing the bot when the argument is too long.
/// What: returns true when `arg`'s byte length leaves room for the prefix.
/// Test: covered by `keyboard_for_projects_has_rows`.
fn callback_fits(arg: &str) -> bool {
    arg.len() <= 55
}

/// Extract the final path component for a compact button label.
///
/// Why: a full project path is too long for an inline-keyboard button label;
/// the directory name alone identifies the project at a glance.
/// What: returns the last `/`-separated component, or the whole string when it
/// has no separator.
/// Test: covered by `keyboard_for_projects_has_rows`.
fn project_basename(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(path)
}

/// Shorten a session id for compact chat display.
///
/// Why: full UUIDs do not fit comfortably on a phone screen.
/// What: returns the first [`SHORT_ID_LEN`] chars plus an ellipsis, or the id
/// unchanged when already short.
/// Test: `short_id_truncates_long_ids`.
fn short_id(id: &str) -> String {
    if id.len() > SHORT_ID_LEN {
        format!("{}…", &id[..SHORT_ID_LEN])
    } else {
        id.to_string()
    }
}

/// Escape the three HTML-significant characters for teloxide's HTML parse mode.
///
/// Why: snapshot output is arbitrary terminal text; un-escaped `<`/`>`/`&`
/// would break the message or be silently dropped by Telegram.
/// What: replaces `&`, `<`, `>` with their HTML entities.
/// Test: covered indirectly by the snapshot formatting test.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
