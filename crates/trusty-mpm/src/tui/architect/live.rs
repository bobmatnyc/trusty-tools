//! The Live tab: a vertical session tracker, sessions top to bottom.
//!
//! Why (#10966, goal ar-G01): Bob asked for a vertical tracker in place of
//! the dashboard's Visualize view: each session, its running agents, its
//! current step. Dc4 (Q4) rules it list-only; there is no chart.
//! What: [`fetch_live`] reads `mpm.sessions.list`, then `mpm.delegation.list`
//! once per distinct working directory, and `mpm.managed.activity` for the
//! selected session only — over the daemon socket and never HTTP (#10899).
//! [`group`] (pure) attaches each live delegation to its session by the
//! listing's caller-safe owner label. [`live_lines`] draws the tree.
//! Test: `group_attaches_agents_by_owner_label`,
//! `live_lines_draw_sessions_with_nested_agents`,
//! `fetch_live_reads_the_daemon_socket_only`.

use std::collections::BTreeSet;

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use serde::Deserialize;

use crate::client::{DaemonClient, SessionRow};
use crate::tui::theme::{Tone, palette};

/// One live delegation under a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAgent {
    /// Agent type (`engineer`, `qa`, …).
    pub agent: String,
    /// `running`, `queued`, `stale`.
    pub status: String,
    /// Seconds since it started.
    pub age_secs: i64,
}

/// One session row and the agents running under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    /// The daemon's session id; a managed session shares its UUID.
    pub id: String,
    /// The tmux name the fleet knows it by, else the id.
    pub name: String,
    /// The daemon's status word.
    pub status: String,
    /// Working directory.
    pub workdir: String,
    /// Live delegations, in the listing's order.
    pub agents: Vec<LiveAgent>,
    /// `mpm.managed.activity` summary; only the selected session is asked.
    pub step: Option<String>,
}

/// The tracker's whole content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveSnapshot {
    /// Sessions in the daemon's order.
    pub sessions: Vec<LiveSession>,
}

/// One `mpm.delegation.list` record, the fields the tracker reads.
#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    /// Agent type.
    pub agent: String,
    /// "session `<tmux name>`" (`delegation_records::owner_label`).
    #[serde(default)]
    pub owner: String,
    /// Lifecycle status, snake_case.
    pub status: String,
    /// Seconds since start.
    #[serde(default)]
    pub age_secs: i64,
}

#[derive(Debug, Deserialize)]
struct Listing {
    #[serde(default)]
    records: Vec<Record>,
}

/// The session a record belongs to: the name inside the owner label's backticks.
fn owner_name(owner: &str) -> Option<&str> {
    owner.strip_prefix("session `")?.strip_suffix('`')
}

/// Attach records to sessions; a record with no matching session is dropped.
///
/// Test: `group_attaches_agents_by_owner_label`.
pub fn group(sessions: &[SessionRow], records: &[Record]) -> LiveSnapshot {
    let sessions = sessions
        .iter()
        .map(|row| {
            let id = row.id.0.to_string();
            let name = if row.tmux_name.is_empty() {
                id.clone()
            } else {
                row.tmux_name.clone()
            };
            let agents = records
                .iter()
                .filter(|r| owner_name(&r.owner) == Some(name.as_str()))
                .map(|r| LiveAgent {
                    agent: r.agent.clone(),
                    status: r.status.clone(),
                    age_secs: r.age_secs,
                })
                .collect();
            LiveSession {
                id,
                name,
                status: row.status.as_str().to_string(),
                workdir: row.workdir.clone(),
                agents,
                step: None,
            }
        })
        .collect();
    LiveSnapshot { sessions }
}

/// Read the tracker's data; `selected` is the session id whose step is asked.
///
/// Why: three read-only socket methods, the same transport rule as the
/// dashboard fetch (#10899: UDS only, no HTTP).
/// What: refuses a client with no socket; else lists sessions, then
/// delegations per distinct working directory, then the selected session's
/// activity summary. An unmanaged session has no activity record, so its
/// step stays `None` and draws as `-`.
/// Test: `fetch_live_reads_the_daemon_socket_only`; the socket calls run live
/// against a sandbox daemon, and the shaping is `group`'s.
pub async fn fetch_live(
    client: &DaemonClient,
    selected: Option<&str>,
) -> Result<LiveSnapshot, String> {
    if client.socket_path().is_none() {
        return Err("the Live tracker reads the daemon socket only (#10899)".into());
    }
    let rows = client
        .sessions()
        .await
        .map_err(|e| format!("mpm.sessions.list: {e}"))?;
    let dirs: BTreeSet<&str> = rows.iter().map(|r| r.workdir.as_str()).collect();
    let mut records = Vec::new();
    for dir in dirs {
        let listing: Listing = client
            .get("/api/v1/delegations")
            .query(&[("cwd", dir)])
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("mpm.delegation.list: {e}"))?
            .json()
            .await
            .map_err(|e| format!("mpm.delegation.list: {e}"))?;
        records.extend(listing.records);
    }
    let mut snap = group(&rows, &records);
    if let Some(id) = selected
        && let Some(session) = snap.sessions.iter_mut().find(|s| s.id == id)
    {
        session.step = client
            .managed_session_activity(id)
            .await
            .ok()
            .map(|a| a.summary)
            .filter(|s| !s.is_empty());
    }
    Ok(snap)
}

fn glyph(status: &str) -> (&'static str, Tone) {
    match status {
        "Active" => ("●", Tone::Ok),
        "Starting" | "AwaitingApproval" => ("◐", Tone::Warn),
        "Detached" | "Paused" => ("○", Tone::Info),
        _ => ("○", Tone::Muted),
    }
}

fn age(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// The tracker as styled lines: a session row, its step, then its agents.
///
/// Test: `live_lines_draw_sessions_with_nested_agents`.
pub fn live_lines(snap: &LiveSnapshot, selected: usize) -> Vec<Line<'static>> {
    let p = palette();
    let mut lines = Vec::new();
    for (i, s) in snap.sessions.iter().enumerate() {
        let (g, tone) = glyph(&s.status);
        let count = match s.agents.len() {
            0 => "no agents".to_string(),
            1 => "1 agent".to_string(),
            n => format!("{n} agents"),
        };
        let mut spans = vec![
            Span::styled(format!("{g} "), Style::default().fg(p.tone(tone))),
            Span::styled(
                format!("{:<24}", s.name),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<18}", s.status.to_lowercase()),
                Style::default().fg(p.tone(tone)),
            ),
            Span::styled(format!("{count:<11}"), Style::default().fg(p.muted)),
            Span::styled(s.workdir.clone(), Style::default().fg(p.muted)),
        ];
        if i == selected {
            spans = spans
                .into_iter()
                .map(|sp| sp.patch_style(p.selection()))
                .collect();
        }
        lines.push(Line::from(spans));
        let step = s.step.clone().unwrap_or_else(|| "-".to_string());
        lines.push(Line::from(vec![
            Span::styled("│  step  ", Style::default().fg(p.muted)),
            Span::styled(step, Style::default().fg(p.text)),
        ]));
        let last = s.agents.len().saturating_sub(1);
        for (j, a) in s.agents.iter().enumerate() {
            let branch = if j == last { "└" } else { "├" };
            let tone = if a.status == "running" {
                Tone::Ok
            } else {
                Tone::Warn
            };
            lines.push(Line::from(vec![
                Span::styled(format!("│  {branch} "), Style::default().fg(p.muted)),
                Span::styled(format!("{:<22}", a.agent), Style::default().fg(p.accent)),
                Span::styled(
                    format!("{:<10}", a.status),
                    Style::default().fg(p.tone(tone)),
                ),
                Span::styled(age(a.age_secs), Style::default().fg(p.muted)),
            ]));
        }
    }
    lines
}
