//! Slack-specific rendering of [`CommandResult`].
//!
//! Why: the executor returns a structured, UI-agnostic [`CommandResult`]
//! (DOC-20 §2); the Slack adapter must turn that into Slack `mrkdwn` message
//! text. Keeping the rendering pure (no network, no Slack runtime) makes it
//! unit-testable and keeps the adapter thin. Slack's `mrkdwn` differs from
//! Telegram's HTML: bold is `*text*`, inline code is `` `text` ``, and code
//! blocks are triple-backtick fences — so this is a sibling of the Telegram
//! formatter, not a reuse of it.
//! What: [`SlackFormatter::format`] produces the `mrkdwn` body for every
//! [`CommandResult`] variant; the free functions render the list/detail shapes.
//! Test: `crate::slack::formatter::tests` covers each variant's rendering.

use crate::client::{
    CommandResult, DiscoveredProjectSummary, HealthReport, ManagedSessionView, ProjectFleetView,
    fleet_state_glyph,
};

#[cfg(test)]
mod tests;

/// How many characters of a session id to show in chat output.
///
/// Why: full UUIDs are unreadable in a Slack channel; the first 8 chars
/// disambiguate in practice while keeping messages compact (mirrors the Telegram
/// adapter's `SHORT_ID_LEN`).
const SHORT_ID_LEN: usize = 8;

/// How many lines of a captured pane / command output to keep in a Slack reply.
///
/// Why: Slack messages have a length budget and a wall of terminal output is
/// noise; the most-recent lines are what the operator cares about.
const MAX_OUTPUT_LINES: usize = 50;

/// Renders [`CommandResult`]s into Slack `mrkdwn` message bodies.
///
/// Why: the Slack message handler stays thin — it executes a command and hands
/// the result here for presentation.
/// What: a stateless formatter; the one method is an associated function.
/// Test: the `format_*` tests in `tests.rs`.
pub struct SlackFormatter;

impl SlackFormatter {
    /// Render a [`CommandResult`] into a Slack `mrkdwn` message body.
    ///
    /// Why: every command's reply text is produced here so presentation is
    /// consistent and testable, and the adapter never embeds formatting logic.
    /// What: matches each variant and returns a `mrkdwn`-formatted string.
    /// Test: `format_sessions_*`, `format_health_up_and_down`, etc.
    pub fn format(result: &CommandResult) -> String {
        match result {
            CommandResult::Sessions(sessions) => format_sessions(sessions),
            CommandResult::SessionDetail { id, events, .. } => format_session_detail(id, events),
            CommandResult::OverseerStatus {
                enabled,
                handler,
                decisions,
            } => format!(
                "*Overseer Status*\nHandler: `{handler}`\nEnabled: {}\n\
                 Recent decisions: allow ({}), block ({}), flag ({})",
                if *enabled { "✅" } else { "❌" },
                decisions.allow,
                decisions.block,
                decisions.flag,
            ),
            CommandResult::TmuxSessions(sessions) => format_tmux(sessions),
            CommandResult::DiscoveredProjects(projects) => format_discovered_projects(projects),
            CommandResult::Adopted { session } => {
                format!("✅ Adopted tmux session `{session}` for oversight")
            }
            CommandResult::Discovered { count } => {
                if *count == 0 {
                    "🔍 No new Claude Code tmux sessions found".to_string()
                } else {
                    format!("🔍 Discovered and adopted {count} Claude Code tmux session(s)")
                }
            }
            CommandResult::ProjectRegistered { path } => {
                format!("✅ Registered project `{path}`")
            }
            CommandResult::ConfigAnalysis {
                project,
                recommendations,
            } => {
                if recommendations.is_empty() {
                    format!(
                        "*Claude config* for `{project}`\nNo recommendations — config looks healthy."
                    )
                } else {
                    let lines = recommendations
                        .iter()
                        .map(|r| format!("• {}", r.message))
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("*Claude config* for `{project}`\n{lines}")
                }
            }
            CommandResult::Snapshot { session, output } => {
                let lines = tail_lines(output);
                if lines.is_empty() {
                    format!("Session `{session}`: empty pane")
                } else {
                    format!("*Snapshot: {session}*\n{}", code_block(&lines))
                }
            }
            CommandResult::Killed { session_id } => {
                format!("🗑️ Session {} killed", short_id(session_id))
            }
            CommandResult::CommandSent { session, output } => {
                if output.trim().is_empty() {
                    format!("📨 Sent to `{session}` — no output captured")
                } else {
                    format!("*📨 {session}*\n{}", code_block(output))
                }
            }
            CommandResult::ChatReply { reply } => reply.clone(),
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
                "*Pairing code:* `{code}`\nExpires in {} minutes",
                expires_in_seconds / 60,
            ),
            CommandResult::PairSuccess { chat_info } => {
                format!("✅ Successfully paired! This channel ({chat_info}) is now registered.")
            }
            CommandResult::PairState { paired } => {
                if *paired {
                    "✅ Bot is paired with this daemon. Type `/help` to see commands.".to_string()
                } else {
                    "👋 trusty-mpm Slack bot — not yet paired. Run `tm slack pair` on your server."
                        .to_string()
                }
            }
            CommandResult::AlertSubscriptions(lines) => {
                format!("*Alert subscription*\n{}", lines.join("\n"))
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
                format!("✅ Session *{session}* {mode}\n`{workdir}`")
            }
            CommandResult::ManagedSpawned {
                id,
                name,
                state,
                runtime,
                attach_cmd,
            } => format!(
                "✅ Spawned *{name}* (`{}`) [{state}] runtime={runtime}\nattach: `{attach_cmd}`",
                short_id(id),
            ),
            CommandResult::ManagedAdopted {
                id,
                name,
                state,
                cwd,
                runtime,
                attach_cmd,
            } => format!(
                "✅ Adopted *{name}* (`{}`) [{state}] runtime={runtime}\ncwd: `{cwd}`\nattach: `{attach_cmd}`",
                short_id(id),
            ),
            CommandResult::ManagedSessions(sessions) => format_managed_sessions(sessions),
            CommandResult::ManagedSession(view) => format_managed_session(view),
            CommandResult::ManagedSent { id, tmux_name } => {
                format!("📨 Sent to managed `{tmux_name}` ({})", short_id(id))
            }
            CommandResult::ManagedAnswered {
                id,
                answer,
                tmux_name,
            } => format!(
                "✅ Answered decision on `{tmux_name}` ({}) → `{answer}`",
                short_id(id),
            ),
            CommandResult::ManagedActivity {
                id,
                state,
                summary,
                pending_decision,
                ..
            } => {
                let decision = pending_decision
                    .as_deref()
                    .map(|d| format!("\n⚠️ pending: {d}"))
                    .unwrap_or_default();
                format!("*Activity {} [{state}]*\n{summary}{decision}", short_id(id))
            }
            CommandResult::ManagedAttachCmd { id, attach_cmd } => {
                format!("*Attach {}*\n`{attach_cmd}`", short_id(id))
            }
            CommandResult::ManagedLifecycle {
                id,
                name,
                state,
                action,
            } => format!("✅ {name} ({}) {action} → {state}", short_id(id)),
            CommandResult::Help(text) => text.clone(),
            CommandResult::Health(report) => format_health(report),
            CommandResult::Error(msg) => format!("❌ {msg}"),
            // #1586: fleet-by-project view — render the same grouped layout as
            // the Telegram formatter but in Slack mrkdwn (no HTML tags).
            CommandResult::ManagedFleet(fleet) => format_fleet_by_project_slack(fleet),
        }
    }
}

/// Render the managed/project session list as a Slack `mrkdwn` body.
///
/// Why: `/sessions` reports the session list; keeping rendering a free function
/// makes it unit-testable.
/// What: a placeholder when empty, else one line per session with a status dot,
/// short id, status, and workdir.
/// Test: `format_sessions_lists_each`, `format_sessions_empty`.
fn format_sessions(sessions: &[crate::client::SessionSummary]) -> String {
    if sessions.is_empty() {
        return "No active sessions.".to_string();
    }
    let mut text = String::from("*trusty-mpm sessions*\n");
    for s in sessions {
        let dot = if s.status.eq_ignore_ascii_case("active") {
            "🟢"
        } else {
            "🔴"
        };
        text.push_str(&format!(
            "\n{dot} `{}` — {}\n  📁 `{}`",
            short_id(&s.id),
            s.status,
            s.workdir,
        ));
    }
    text
}

/// Render one session's recent-event detail as a Slack `mrkdwn` body.
///
/// Why: `/status <id>` shows a session's recent hook-event names.
/// What: a no-events placeholder, else a bulleted event list under a heading.
/// Test: `format_session_detail_lists_events`.
fn format_session_detail(id: &str, events: &[String]) -> String {
    if events.is_empty() {
        format!("Session {id}: no recent events")
    } else {
        let lines = events
            .iter()
            .map(|e| format!("• {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("*Session {}*\n{lines}", short_id(id))
    }
}

/// Render the tmux-session list as a Slack `mrkdwn` body.
///
/// Why: `/tmux` lists every tmux session on the daemon host with a managed tag.
/// What: a placeholder when empty, else one line per session tagged managed or
/// external.
/// Test: `format_tmux_lists_each`.
fn format_tmux(sessions: &[crate::client::TmuxSessionSummary]) -> String {
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
            format!("• `{}` — {tag}", s.name)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("*tmux sessions*\n{lines}")
}

/// Render a discovered-project list as a Slack `mrkdwn` body.
///
/// Why: `/projects` lists projects mined from `~/.claude/projects/`.
/// What: a placeholder when empty, else one block per project (path, session
/// count, last-used date).
/// Test: `format_discovered_projects_lists_each`.
pub fn format_discovered_projects(projects: &[DiscoveredProjectSummary]) -> String {
    if projects.is_empty() {
        return "No projects discovered in Claude Code config.".to_string();
    }
    let mut text = String::from("*Discovered projects*\n");
    for p in projects {
        let last = p
            .last_session
            .as_deref()
            .map(|s| s.split('T').next().unwrap_or(s).to_string())
            .unwrap_or_else(|| "never".to_string());
        text.push_str(&format!(
            "\n📁 `{}`\n  {} session(s) · last used {last}",
            p.path, p.session_count,
        ));
    }
    text
}

/// Render the managed session list as a Slack `mrkdwn` body.
///
/// Why: `/fleet` reports the managed fleet.
/// What: a placeholder when empty, else one line per session: state, short id,
/// name, and a pending-decision flag when blocked.
/// Test: `format_managed_sessions_lists_each`.
pub fn format_managed_sessions(sessions: &[ManagedSessionView]) -> String {
    if sessions.is_empty() {
        return "No managed sessions.".to_string();
    }
    let mut text = String::from("*managed sessions*\n");
    for s in sessions {
        let flag = if s.pending_decision.is_some() {
            " ⚠️"
        } else {
            ""
        };
        text.push_str(&format!(
            "\n• `{}` {} [{}]{flag}",
            short_id(&s.id),
            s.name,
            s.state,
        ));
    }
    text
}

/// Render one managed session's detail as a Slack `mrkdwn` body.
///
/// Why: `/get <id|name>` shows a single managed session's full record.
/// What: name, state, workspace/repo/branch when present, and any pending
/// decision with its proposed default.
/// Test: `format_managed_session_renders_fields`.
pub fn format_managed_session(view: &ManagedSessionView) -> String {
    let mut text = format!(
        "*{}* (`{}`) [{}]",
        view.name,
        short_id(&view.id),
        view.state
    );
    if let Some(ws) = &view.workspace_path {
        text.push_str(&format!("\n📁 `{ws}`"));
    }
    if let Some(repo) = &view.repo_url {
        let branch = view.branch.as_deref().unwrap_or("");
        text.push_str(&format!("\n🔗 {repo} {branch}"));
    }
    if let Some(d) = &view.pending_decision {
        let def = view.proposed_default.as_deref().unwrap_or("");
        text.push_str(&format!("\n⚠️ pending: {d} (default: `{def}`)"));
    }
    text
}

/// Render a [`crate::core::doctor::DoctorReport`] as a Slack `mrkdwn` body.
///
/// Why: `/doctor` must be readable at a glance — one emoji-tagged line per check
/// plus an overall verdict.
/// What: a heading, one `<icon> <name> — <message>` line per check, and a final
/// overall-status line.
/// Test: `format_doctor_report_lists_each_check`.
fn format_doctor_report(report: &crate::core::doctor::DoctorReport) -> String {
    let mut text = String::from("*trusty-mpm doctor*\n");
    for check in &report.checks {
        text.push_str(&format!(
            "\n{} *{}* — {}",
            status_icon(check.status),
            check.name,
            check.message,
        ));
    }
    text.push_str(&format!(
        "\n\n{} *overall: {}*",
        status_icon(report.overall),
        status_word(report.overall),
    ));
    text
}

/// Render a [`HealthReport`] as a glanceable Slack health summary.
///
/// Why: the `/health` reply must read at a glance — daemon up/down first, then
/// catalog freshness and a one-line fleet summary.
/// What: a ✅/❌ liveness line; when up, the catalog-freshness line and the fleet
/// counts. A down daemon renders just the liveness line.
/// Test: `format_health_up_and_down`.
fn format_health(report: &HealthReport) -> String {
    if !report.reachable {
        return format!("❌ *daemon unreachable*\n`{}`", report.url);
    }
    let catalog = if report.catalog_unknown {
        "catalog: never synced"
    } else if report.catalog_stale {
        "catalog: ⚠️ updates available"
    } else {
        "catalog: up to date"
    };
    format!(
        "✅ *daemon {}*\n`{}`\n{}\nfleet: {} session(s), {} awaiting a decision",
        report.status, report.url, catalog, report.managed_total, report.managed_pending_decisions,
    )
}

/// The emoji icon for a doctor check status.
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

/// A one-word label for a doctor check status.
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

/// Keep the most-recent [`MAX_OUTPUT_LINES`] lines of a text block.
///
/// Why: terminal output can be huge; only the tail is interesting and Slack
/// caps message length.
/// What: returns the last `MAX_OUTPUT_LINES` lines joined with newlines.
/// Test: covered by `format_snapshot_tails_output`.
fn tail_lines(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    lines[lines.len().saturating_sub(MAX_OUTPUT_LINES)..].join("\n")
}

/// Wrap text in a Slack triple-backtick code fence.
///
/// Why: pane output and captured command output should render monospaced in
/// Slack; `mrkdwn` uses triple backticks for that.
/// What: returns ```` ```\n<text>\n``` ````.
/// Test: covered by `format_command_sent_uses_code_block`.
fn code_block(text: &str) -> String {
    format!("```\n{text}\n```")
}

/// Render managed sessions grouped by project as a Slack `mrkdwn` body.
///
/// Why: `/fleet` (WI-B, #1586) shows a per-project breakdown of live sessions
/// so the operator can see which work belongs to which repo at a glance.
/// What: emits a bold heading, then one block per project — bold project name +
/// repo URL followed by session rows or a `—` placeholder when empty.
/// State glyphs are delegated to [`crate::client::fleet_state_glyph`] to stay
/// consistent with the Telegram adapter (🟢 active  🟡 provisioning  🔴 other).
/// Test: `format_fleet_by_project_slack_renders_projects` in `tests.rs`.
fn format_fleet_by_project_slack(fleet: &[ProjectFleetView]) -> String {
    if fleet.is_empty() {
        return "No registered projects.".to_string();
    }
    let mut text = String::from("*fleet by project*");
    for pf in fleet {
        text.push_str(&format!("\n\n*{}* — `{}`", pf.project_name, pf.repo_url,));
        if pf.sessions.is_empty() {
            text.push_str("\n  —");
        } else {
            for s in &pf.sessions {
                let dot = fleet_state_glyph(&s.state);
                let flag = if s.pending_decision.is_some() {
                    " ⚠️"
                } else {
                    ""
                };
                text.push_str(&format!(
                    "\n  {dot} `{}` {} [{}]{flag}",
                    short_id(&s.id),
                    s.name,
                    s.state,
                ));
            }
        }
    }
    text
}

/// Shorten a session id for compact chat display.
///
/// Why: full UUIDs do not read well in a channel.
/// What: returns the first [`SHORT_ID_LEN`] *chars* plus an ellipsis, or the id
/// unchanged when already short. Char-based (not byte-slice) truncation so a
/// multi-byte UTF-8 id can never panic on a non-char boundary.
/// Test: `short_id_truncates_long_ids`, `short_id_handles_multibyte`.
fn short_id(id: &str) -> String {
    let mut chars = id.chars();
    let head: String = chars.by_ref().take(SHORT_ID_LEN).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
