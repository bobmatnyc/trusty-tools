//! The Live tab: a vertical session tracker, sessions top to bottom.
//!
//! Why (#10966, goal ar-G01): Bob asked for a vertical tracker in place of
//! the dashboard's Visualize view: each session, its running agents, its
//! current step. Dc4 (Q4) rules it list-only; there is no chart.
//! What: [`fetch_live`] reads `mpm.sessions.list`, then `mpm.delegation.list`
//! once per distinct working directory, and `mpm.managed.activity` for the
//! selected session only — over the daemon socket and never HTTP (#10899).
//! [`merge`] (pure) adds the managed sessions `mpm.managed.list` holds and
//! the hook registry lacks (#11104). [`group`] (pure) attaches each live delegation to its session by the
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

use crate::client::{DaemonClient, ManagedSessionSummary, SessionRow};
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

/// A managed `state` word as the status word [`glyph`] reads.
fn managed_status(state: &str) -> String {
    match state {
        "active" => "Active",
        "provisioning" | "starting" => "Starting",
        "paused" => "Paused",
        "detached" => "Detached",
        other => other,
    }
    .to_string()
}

/// One tracker row per session, with no agents yet.
///
/// Why (#11104): `mpm.sessions.list` reads the hook registry only, and a
/// session the session manager spawned is not in it, so the Live tab drew
/// no managed session at all.
/// What: the hook rows in the daemon's order, then each managed session not
/// already listed (same id or same tmux name), in `mpm.managed.list` order.
/// A hook row whose id is a record's `claude_session_id` is the same session:
/// it takes the record's id, name and workdir and keeps its hook status, or
/// is dropped when the record is listed already. Each hook row folds into at
/// most one record. A deleted-slot tombstone is not a session and is skipped.
/// Test: `merge_lists_managed_sessions_the_hook_registry_lacks`,
/// `merge_folds_a_hook_row_into_the_managed_session_it_correlates_with`.
pub fn merge(rows: &[SessionRow], managed: &[ManagedSessionSummary]) -> Vec<LiveSession> {
    let mut sessions: Vec<LiveSession> = rows
        .iter()
        .map(|row| {
            let id = row.id.0.to_string();
            let name = if row.tmux_name.is_empty() {
                id.clone()
            } else {
                row.tmux_name.clone()
            };
            LiveSession {
                id,
                name,
                status: row.status.as_str().to_string(),
                workdir: row.workdir.clone(),
                agents: Vec::new(),
                step: None,
            }
        })
        .collect();
    // #11104: managed sessions live in the session-manager store, not the
    // hook registry; add each one the registry does not already hold.
    let hooks = sessions.len();
    // #11104: a hook row correlated to a managed record is used up: it folds
    // into that one session, or is dropped when the session is listed already.
    let mut used = vec![false; hooks];
    let mut dropped = vec![false; hooks];
    for m in managed.iter().filter(|m| !m.deleted) {
        let name = if m.name.is_empty() { &m.id } else { &m.name };
        let workdir = m
            .cwd
            .clone()
            .filter(|d| !d.is_empty())
            .or_else(|| m.workspace_path.clone())
            .unwrap_or_default();
        let listed = sessions
            .iter()
            .position(|s| s.id == m.id || &s.name == name);
        // #11104: a SessionStart hook row is keyed by Claude's UUID, which
        // `correlate_session_start` stores as the record's `claude_session_id`.
        let correlated = m
            .claude_session_id
            .as_deref()
            .filter(|c| !c.is_empty())
            .and_then(|c| (0..hooks).find(|&i| !used[i] && sessions[i].id.eq_ignore_ascii_case(c)))
            .filter(|&c| Some(c) != listed);
        match (listed, correlated) {
            (Some(_), Some(c)) => (used[c], dropped[c]) = (true, true),
            (Some(_), None) => {}
            (None, Some(c)) => {
                used[c] = true;
                // #11104: the managed record's id, name and workdir win (the id
                // keys the step read, the name keys agent grouping); the hook
                // row keeps its status, since hook events track liveness.
                let s = &mut sessions[c];
                s.id = m.id.clone();
                s.name = name.clone();
                if !workdir.is_empty() {
                    s.workdir = workdir;
                }
            }
            (None, None) => sessions.push(LiveSession {
                id: m.id.clone(),
                name: name.clone(),
                status: managed_status(&m.state),
                workdir,
                agents: Vec::new(),
                step: None,
            }),
        }
    }
    let keep = dropped
        .into_iter()
        .map(|d| !d)
        .chain(std::iter::repeat(true));
    sessions
        .into_iter()
        .zip(keep)
        .filter_map(|(s, keep)| keep.then_some(s))
        .collect()
}

/// Attach records to sessions; a record with no matching session is dropped.
///
/// Test: `group_attaches_agents_by_owner_label`.
pub fn group(mut sessions: Vec<LiveSession>, records: &[Record]) -> LiveSnapshot {
    for s in &mut sessions {
        s.agents = records
            .iter()
            .filter(|r| owner_name(&r.owner) == Some(s.name.as_str()))
            .map(|r| LiveAgent {
                agent: r.agent.clone(),
                status: r.status.clone(),
                age_secs: r.age_secs,
            })
            .collect();
    }
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
    // #11104: a failed managed read fails the fetch; it never draws as an
    // empty managed list.
    let managed = client
        .list_managed_sessions()
        .await
        .map_err(|e| format!("mpm.managed.list: {e}"))?;
    let sessions = merge(&rows, &managed);
    let dirs: BTreeSet<&str> = sessions.iter().map(|s| s.workdir.as_str()).collect();
    let mut records = Vec::new();
    for dir in dirs {
        let listing: Listing = client
            .get("/api/v1/delegations")
            .query(&[("cwd", dir)])
            .send()
            .await
            .map_err(|e| format!("mpm.delegation.list: {e}"))?
            .error_for_status()
            .map_err(|e| format!("mpm.delegation.list: {e}"))?
            .json()
            .await
            .map_err(|e| format!("mpm.delegation.list: {e}"))?;
        records.extend(listing.records);
    }
    let mut snap = group(sessions, &records);
    if let Some(id) = selected
        && let Some(session) = snap.sessions.iter_mut().find(|s| s.id == id)
    {
        // #10966: a failed activity read also draws `step  -`, the same as
        // an unmanaged session; a distinct `?` marker is a known LOW, deferred.
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
