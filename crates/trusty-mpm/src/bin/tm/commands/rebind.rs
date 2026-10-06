//! `tm sessions rebind <id-or-name> [--tmux <session>]` and `--all` (#9313).
//!
//! Why: after a tmux server replacement a live session's record names a dead
//! pane and server. The daemon rebinds an unambiguous match on its own; this
//! verb covers the rest — a renamed session, a same-server relaunch, or a
//! check across the fleet. It updates records only; no Claude is touched.
//! What: [`session_rebind`] POSTs the daemon's rebind route and prints one
//! line per session via [`render_row`].
//! Test: `cli_parses_sessions_rebind`, `cli_parses_sessions_rebind_all`;
//! rendering by `render_row_names_each_outcome`.

use serde::Deserialize;

/// One record's rebind result, as the daemon reports it.
#[derive(Debug, Deserialize)]
pub(crate) struct RebindRow {
    id: String,
    name: String,
    outcome: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    pane_id: Option<String>,
    #[serde(default)]
    tmux_server: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RebindAll {
    results: Vec<RebindRow>,
}

/// The line printed for one row.
///
/// What: `<name> (<id>): <outcome>`, with the new pane and server for
/// `rebound` and the daemon's reason for every other outcome.
/// Test: `render_row_names_each_outcome`.
pub(crate) fn render_row(row: &RebindRow) -> String {
    let head = format!("{} ({})", row.name, row.id);
    match row.outcome.as_str() {
        "rebound" => format!(
            "{head}: rebound to pane {} on tmux server {}",
            row.pane_id.as_deref().unwrap_or("?"),
            row.tmux_server.as_deref().unwrap_or("?")
        ),
        "current" => format!("{head}: already bound to its live pane"),
        "no_match" => format!("{head}: no match — {}; record unchanged", row.detail),
        "ambiguous" => format!("{head}: ambiguous — {}; record unchanged", row.detail),
        other => format!("{head}: {other} — {}", row.detail),
    }
}

/// Run `tm sessions rebind`.
///
/// Why: see the module doc.
/// What: with `id`, resolves it to a managed id, POSTs
/// `/api/v1/sessions/managed/{id}/rebind` (`?tmux=` when given), prints the
/// row, and fails for any outcome but `rebound` or `current`. With no `id`
/// (`--all`), POSTs `/api/v1/sessions/managed/rebind`, prints every row, and
/// fails only when a row is `error`. A non-success status carries the
/// daemon's message.
/// Test: `cli_parses_sessions_rebind`, `cli_parses_sessions_rebind_all`.
pub(crate) async fn session_rebind(
    daemon: &trusty_mpm::client::DaemonClient,
    id: Option<String>,
    tmux: Option<String>,
) -> anyhow::Result<()> {
    let Some(target) = id else {
        let resp = daemon
            .post("/api/v1/sessions/managed/rebind")
            .send()
            .await?;
        let all: RebindAll = checked(resp).await?.json().await?;
        if all.results.is_empty() {
            println!("no active or stopped sessions to rebind");
        }
        for row in &all.results {
            println!("{}", render_row(row));
        }
        let errors = all.results.iter().filter(|r| r.outcome == "error").count();
        if errors > 0 {
            anyhow::bail!("{errors} session(s) could not be checked");
        }
        return Ok(());
    };
    let id = super::managed_route::resolve_managed_match(daemon, &target)
        .await
        .ok_or_else(|| anyhow::anyhow!("managed session '{target}' not found"))?;
    let mut req = daemon.post(format!("/api/v1/sessions/managed/{id}/rebind"));
    if let Some(name) = tmux.as_deref() {
        req = req.query(&[("tmux", name)]);
    }
    let row: RebindRow = checked(req.send().await?).await?.json().await?;
    let line = render_row(&row);
    match row.outcome.as_str() {
        "rebound" | "current" => {
            println!("{line}");
            Ok(())
        }
        _ => {
            eprintln!("error: {line}");
            Err(anyhow::anyhow!("session '{target}' was not rebound"))
        }
    }
}

/// `resp` when it succeeded; otherwise an error carrying the daemon's body.
async fn checked(
    resp: trusty_mpm::client::DaemonResponse,
) -> anyhow::Result<trusty_mpm::client::DaemonResponse> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    anyhow::bail!("daemon returned {status}: {}", body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(outcome: &str, detail: &str) -> RebindRow {
        RebindRow {
            id: "id-1".into(),
            name: "tm-dogfood".into(),
            outcome: outcome.into(),
            detail: detail.into(),
            pane_id: Some("%1".into()),
            tmux_server: Some("66166:1791300000".into()),
        }
    }

    /// #9313: each outcome is named, with the new pane or the reason.
    #[test]
    fn render_row_names_each_outcome() {
        assert_eq!(
            render_row(&row("rebound", "")),
            "tm-dogfood (id-1): rebound to pane %1 on tmux server 66166:1791300000"
        );
        assert!(render_row(&row("current", "")).ends_with("already bound to its live pane"));
        assert!(render_row(&row("no_match", "gone")).contains("no match — gone; record unchanged"));
        assert!(
            render_row(&row("ambiguous", "2 panes"))
                .contains("ambiguous — 2 panes; record unchanged")
        );
        assert!(render_row(&row("error", "tmux down")).contains("error — tmux down"));
    }
}
