//! `/tm` subcommand handlers and their formatting helpers.
//!
//! Why: The per-subcommand `cmd_*` implementations are the bulk of the module;
//! isolating them from the dispatcher in `mod.rs` keeps both files under the
//! 500-line cap.
//! What: One `cmd_*` per `/tm` verb, plus the `render_session_list` /
//! `truncate` / `parse_new_args` / `require_name` helpers they share.
//! Test: Covered by `tm::commands::tests` (the parse paths that don't need a
//! live tmux server).

use std::fmt::Write;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use super::{require_name, truncate};
use crate::tm::manager::TmManager;
use crate::tm::project::{AdapterType, SessionStatus, TmSession};

// ==================== /tm list ====================

pub(super) async fn cmd_list(manager: &Arc<Mutex<TmManager>>, out: &mut String) -> Result<()> {
    let mgr = manager.lock().await;
    let sessions = mgr.list_sessions().await?;
    let projects = mgr.registry.list_projects()?;
    drop(mgr);

    render_session_list(&sessions, &projects, out)
}

pub(super) fn render_session_list(
    sessions: &[TmSession],
    projects: &[crate::tm::project::TmProject],
    out: &mut String,
) -> Result<()> {
    // Filter out Orphaned sessions for the default view; they are pruned on
    // reconcile but may still appear in the list between reconcile passes.
    let visible: Vec<&TmSession> = sessions
        .iter()
        .filter(|s| s.status != SessionStatus::Orphaned)
        .collect();
    let hidden_count = sessions.len() - visible.len();

    writeln!(out, "TM Sessions")?;
    writeln!(
        out,
        "──────────────────────────────────────────────────────────"
    )?;
    if visible.is_empty() {
        writeln!(out, "  (no sessions)")?;
        writeln!(
            out,
            "──────────────────────────────────────────────────────────"
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "  {:<16} {:<14} {:<13} {:<10} LAST ACTIVE",
        "PROJECT", "NAME", "ADAPTER", "STATUS"
    )?;

    let mut running = 0usize;
    let mut paused = 0usize;
    let mut idle = 0usize;
    for s in &visible {
        let project_name = projects
            .iter()
            .find(|p| p.id == s.project_id)
            .map(|p| p.name.as_str())
            .unwrap_or("(unregistered)");
        match s.status {
            SessionStatus::Running => running += 1,
            SessionStatus::Paused => paused += 1,
            SessionStatus::Idle => idle += 1,
            _ => {}
        }
        writeln!(
            out,
            "  {:<16} {:<14} {:<13} {:<10} {}",
            truncate(project_name, 16),
            truncate(&s.name, 14),
            truncate(s.adapter_type.as_str(), 13),
            s.status.to_string(),
            s.last_active_ago()
        )?;
    }
    writeln!(
        out,
        "──────────────────────────────────────────────────────────"
    )?;
    let unique_projects: std::collections::HashSet<&String> =
        visible.iter().map(|s| &s.project_id).collect();
    writeln!(
        out,
        "{} session{} across {} project{}  ({} running, {} paused, {} idle)",
        visible.len(),
        if visible.len() == 1 { "" } else { "s" },
        unique_projects.len(),
        if unique_projects.len() == 1 { "" } else { "s" },
        running,
        paused,
        idle
    )?;
    if hidden_count > 0 {
        writeln!(
            out,
            "({hidden_count} orphaned session{} hidden; run /tm reconcile to clean up)",
            if hidden_count == 1 { "" } else { "s" }
        )?;
    }
    Ok(())
}

// ==================== /tm new ====================

/// Parse `/tm new [name] [-p <path>] [-a <adapter>]`.
///
/// Why: A small, predictable arg parser lets users mix positional name with
/// optional flags without pulling in clap for a single command.
/// What: Walks the tokens once, treating the first non-flag token as `name`;
/// subsequent `-p`/`-a` flags consume the next token as their value.
pub(super) fn parse_new_args(
    rest: &str,
) -> Result<(Option<String>, Option<PathBuf>, Option<AdapterType>)> {
    let mut name: Option<String> = None;
    let mut path: Option<PathBuf> = None;
    let mut adapter: Option<AdapterType> = None;

    let mut iter = rest.split_whitespace();
    while let Some(tok) = iter.next() {
        match tok {
            "-p" | "--path" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("-p needs a value"))?;
                path = Some(PathBuf::from(v));
            }
            "-a" | "--adapter" => {
                let v = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("-a needs a value"))?;
                adapter = Some(AdapterType::from_id(v));
            }
            other if !other.starts_with('-') && name.is_none() => {
                name = Some(other.to_string());
            }
            other => anyhow::bail!("unexpected token '{}'", other),
        }
    }
    Ok((name, path, adapter))
}

pub(super) async fn cmd_new(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let (name_opt, path_opt, adapter_opt) = match parse_new_args(rest) {
        Ok(v) => v,
        Err(e) => {
            writeln!(out, "/tm new: {e}")?;
            return Ok(());
        }
    };

    let name = name_opt.unwrap_or_else(|| format!("tm-{}", chrono::Utc::now().timestamp()));
    let path = match path_opt {
        Some(p) => p,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let mgr = manager.lock().await;
    match mgr.new_session(&name, &path, adapter_opt).await {
        Ok(session) => {
            writeln!(
                out,
                "Created session '{}' ({}) in {}",
                session.name,
                session.adapter_type,
                session.project_path.display()
            )?;
        }
        Err(e) => {
            writeln!(out, "/tm new: failed: {e:#}")?;
        }
    }
    Ok(())
}

// ==================== /tm attach ====================

pub(super) async fn cmd_attach(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let name = match require_name(rest, "attach", out)? {
        Some(n) => n,
        None => return Ok(()),
    };
    let mgr = manager.lock().await;
    match mgr.attach_instructions(&name) {
        Ok(cmd) => {
            writeln!(out, "To attach, run in a new terminal:")?;
            writeln!(out, "  {cmd}")?;
        }
        Err(e) => {
            writeln!(out, "/tm attach: {e:#}")?;
        }
    }
    Ok(())
}

// ==================== /tm pause / resume / kill ====================

pub(super) async fn cmd_pause(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let name = match require_name(rest, "pause", out)? {
        Some(n) => n,
        None => return Ok(()),
    };
    let mgr = manager.lock().await;
    match mgr.pause_session(&name).await {
        Ok(()) => writeln!(out, "Paused session '{name}'")?,
        Err(e) => writeln!(out, "Cannot pause '{name}': {e:#}")?,
    }
    Ok(())
}

pub(super) async fn cmd_resume(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let name = match require_name(rest, "resume", out)? {
        Some(n) => n,
        None => return Ok(()),
    };
    let mgr = manager.lock().await;
    match mgr.resume_session(&name).await {
        Ok(()) => writeln!(out, "Resumed session '{name}'")?,
        Err(e) => writeln!(out, "Cannot resume '{name}': {e:#}")?,
    }
    Ok(())
}

pub(super) async fn cmd_kill(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let name = match require_name(rest, "kill", out)? {
        Some(n) => n,
        None => return Ok(()),
    };
    let mgr = manager.lock().await;
    match mgr.kill_session(&name).await {
        Ok(()) => writeln!(out, "Killed session '{name}'")?,
        Err(e) => writeln!(out, "/tm kill: {e:#}")?,
    }
    Ok(())
}

// ==================== /tm send ====================

pub(super) async fn cmd_send(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let (name, message) = match rest.split_once(char::is_whitespace) {
        Some((n, m)) if !m.trim().is_empty() => (n.trim().to_string(), m.trim().to_string()),
        _ => {
            writeln!(out, "usage: /tm send <name> <message>")?;
            return Ok(());
        }
    };
    let mgr = manager.lock().await;
    match mgr.send_message(&name, &message).await {
        Ok(()) => writeln!(out, "Sent message to '{name}'")?,
        Err(e) => writeln!(out, "/tm send: {e:#}")?,
    }
    Ok(())
}

// ==================== /tm capture ====================

pub(super) async fn cmd_capture(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let mut iter = rest.split_whitespace();
    let name = match iter.next() {
        Some(n) => n.to_string(),
        None => {
            writeln!(out, "usage: /tm capture <name> [lines]")?;
            return Ok(());
        }
    };
    let lines: u32 = iter.next().and_then(|s| s.parse().ok()).unwrap_or(50);

    let mgr = manager.lock().await;
    match mgr.capture_pane(&name, lines).await {
        Ok(content) => {
            writeln!(out, "─── {name} (last {lines} lines) ──────────────")?;
            // Trim trailing newlines so the closing rule sits flush.
            let body = content.trim_end_matches('\n');
            writeln!(out, "{body}")?;
            writeln!(out, "────────────────────────────────────────────")?;
        }
        Err(e) => writeln!(out, "/tm capture: {e:#}")?,
    }
    Ok(())
}

// ==================== /tm detect ====================

pub(super) async fn cmd_detect(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    let name = match require_name(rest, "detect", out)? {
        Some(n) => n,
        None => return Ok(()),
    };
    let mgr = manager.lock().await;
    match mgr.detect_adapter(&name).await {
        Ok((kind, conf)) => {
            writeln!(out, "Detected: {} (confidence: {:.2})", kind, conf)?;
        }
        Err(e) => writeln!(out, "/tm detect: {e:#}")?,
    }
    Ok(())
}

// ==================== /tm reconcile ====================

pub(super) async fn cmd_reconcile(manager: &Arc<Mutex<TmManager>>, out: &mut String) -> Result<()> {
    let mgr = manager.lock().await;
    writeln!(out, "Reconciling with tmux...")?;
    match mgr.reconcile().await {
        Ok(report) => {
            for s in &report.added {
                writeln!(
                    out,
                    "  + discovered: {} ({}, {})",
                    s.name,
                    s.adapter_type,
                    s.project_path.display()
                )?;
            }
            for name in &report.orphaned {
                writeln!(out, "  ~ orphaned:   {}", name)?;
            }
            for s in &report.updated {
                writeln!(out, "  = updated:    {}", s.name)?;
            }
            if report.added.is_empty() && report.orphaned.is_empty() && report.updated.is_empty() {
                writeln!(out, "  (no changes)")?;
            }
            writeln!(
                out,
                "Done: {} added, {} orphaned, {} updated",
                report.added.len(),
                report.orphaned.len(),
                report.updated.len()
            )?;
        }
        Err(e) => {
            writeln!(out, "Reconcile failed: {e:#}")?;
            writeln!(
                out,
                "Hint: ensure tmux is installed and on PATH (`which tmux`)."
            )?;
        }
    }
    Ok(())
}

// ==================== /tm status ====================

pub(super) async fn cmd_status(
    manager: &Arc<Mutex<TmManager>>,
    rest: &str,
    out: &mut String,
) -> Result<()> {
    if rest.is_empty() {
        return cmd_list(manager, out).await;
    }
    let name = rest.split_whitespace().next().unwrap_or("").to_string();
    let mgr = manager.lock().await;
    let session = match mgr.registry.get_session_by_name(&name)? {
        Some(s) => s,
        None => {
            writeln!(out, "session '{name}' not found")?;
            return Ok(());
        }
    };
    let project = mgr.registry.get_project(&session.project_id)?;
    let adapter = mgr.adapters.get(session.adapter_type.as_str());
    let can_pause = adapter.as_ref().map(|a| a.can_pause()).unwrap_or(false);
    drop(mgr);

    writeln!(out, "Session: {}", session.name)?;
    if let Some(p) = project {
        writeln!(out, "  Project:  {} ({})", p.name, p.path.display())?;
    } else {
        writeln!(out, "  Project:  (unregistered)")?;
    }
    writeln!(out, "  Adapter:  {}", session.adapter_type)?;
    writeln!(out, "  Status:   {}", session.status)?;
    writeln!(
        out,
        "  Created:  {}",
        session.created_at.format("%Y-%m-%d %H:%M UTC")
    )?;
    writeln!(out, "  Active:   {}", session.last_active_ago())?;
    writeln!(out, "  Can pause: {}", if can_pause { "yes" } else { "no" })?;
    Ok(())
}

// ==================== /tm projects ====================

pub(super) async fn cmd_projects(manager: &Arc<Mutex<TmManager>>, out: &mut String) -> Result<()> {
    let mgr = manager.lock().await;
    let projects = mgr.list_projects().await?;
    drop(mgr);

    writeln!(out, "TM Projects")?;
    writeln!(
        out,
        "──────────────────────────────────────────────────────"
    )?;
    if projects.is_empty() {
        writeln!(out, "  (no projects)")?;
        writeln!(
            out,
            "──────────────────────────────────────────────────────"
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "  {:<14} {:<28} {:<14} SESSIONS",
        "NAME", "PATH", "FRAMEWORK"
    )?;
    for p in &projects {
        let running = p
            .sessions
            .iter()
            .filter(|s| matches!(s.status, SessionStatus::Running))
            .count();
        let total = p.sessions.len();
        let summary = if total == 0 {
            "0".to_string()
        } else {
            format!("{} ({} running)", total, running)
        };
        writeln!(
            out,
            "  {:<14} {:<28} {:<14} {}",
            truncate(&p.name, 14),
            truncate(&p.path.display().to_string(), 28),
            truncate(&p.framework.display(), 14),
            summary
        )?;
    }
    writeln!(
        out,
        "──────────────────────────────────────────────────────"
    )?;
    writeln!(
        out,
        "{} project{}",
        projects.len(),
        if projects.len() == 1 { "" } else { "s" }
    )?;
    Ok(())
}
