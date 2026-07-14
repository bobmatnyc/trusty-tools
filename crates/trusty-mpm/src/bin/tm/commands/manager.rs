//! `tm manager status|digest|chat` command handlers (DOC-36 §3.2/§6 phase 1,
//! epic #2109, WI-6 #2583).
//!
//! Why: the thin CLI half of the Layer-3 `/api/v1/manager/*` surface — no
//! logic is duplicated client-side (#2583's acceptance criteria); each verb
//! is a `DaemonClient::manager_*` call plus a pure render function, mirroring
//! `commands/projects/registry.rs`'s `status` verb shape. All the
//! version-skew/degrade handling lives in the client
//! (`client/http_client/manager.rs::manager_digest`/`manager_chat`): a `404`
//! against a daemon that genuinely predates WI-3/WI-4 (#2580/#2581) comes
//! back as `Ok(None)`, which this module turns into the upgrade message
//! below; a daemon-reported error (including a real 404 like an unregistered
//! `scope=project:<name>`, or the inference-unavailable/failed 502/503
//! degrade) comes back as `Ok(Some(outcome))` or `Err` respectively and is
//! rendered/propagated as-is — this module never re-derives that
//! distinction.
//! What: [`status`], [`digest`], [`chat`] — the three dispatch targets
//! `main.rs` routes `Command::Manager` actions to — plus the pure render
//! helpers [`render_status_headline`] and [`render_project_rows`], and
//! [`default_conversation_key`] (the stable per-user key convention documented
//! on `ManagerAction::Chat`).
//! Test: `render_status_headline_*`, `render_project_rows_*`,
//! `default_conversation_key_is_stable_and_prefixed`,
//! `read_message_from_reader_*` in this module's `tests` submodule;
//! live-HTTP coverage (incl. the 404 degrade) lives in
//! `crates/trusty-mpm/tests/manager_cli_client.rs`.

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::http_client::manager::PortfolioStatusWire;

use crate::cli::ManagerAction;

/// Build a `DaemonClient` from the CLI's shared `(reqwest::Client, url)` pair.
fn daemon(client: &reqwest::Client, url: &str) -> DaemonClient {
    DaemonClient::with_client(client.clone(), url.to_string())
}

/// Dispatch a `tm manager <action>` invocation.
///
/// Why: `main.rs` stays a thin bootstrap; all `manager` routing lives here,
/// matching `commands::projects::projects`'s dispatcher shape.
/// What: routes to [`status`], [`digest`], or [`chat`].
/// Test: exercised end-to-end by the daemon integration tests; parse coverage
/// in `tests_manager.rs`.
pub(crate) async fn manager(
    client: &reqwest::Client,
    url: &str,
    action: ManagerAction,
) -> anyhow::Result<()> {
    match action {
        ManagerAction::Status { json } => status(client, url, json).await,
        ManagerAction::Digest { scope, json } => digest(client, url, &scope, json).await,
        ManagerAction::Chat {
            message,
            conversation,
            json,
        } => chat(client, url, message, conversation, json).await,
    }
}

/// `tm manager status [--json]` — deterministic cross-project rollup.
pub(crate) async fn status(client: &reqwest::Client, url: &str, json: bool) -> anyhow::Result<()> {
    let status = daemon(client, url).manager_status().await?;
    if json {
        // Field-by-field reconstruction (not a whole-struct `Serialize`),
        // matching `commands/projects/registry.rs::status`'s convention: the
        // client's wire DTOs are intentionally `Deserialize`-only.
        let t = &status.totals;
        let out = serde_json::json!({
            "project_count": status.project_count,
            "totals": {
                "sessions": {
                    "provisioning": t.sessions.provisioning,
                    "active": t.sessions.active,
                    "stopped": t.sessions.stopped,
                    "errored": t.sessions.errored,
                    "decommissioned": t.sessions.decommissioned,
                    "total": t.sessions.total,
                },
                "deliverables": {
                    "proposed": t.deliverables.proposed,
                    "in_progress": t.deliverables.in_progress,
                    "blocked": t.deliverables.blocked,
                    "complete": t.deliverables.complete,
                    "delivered": t.deliverables.delivered,
                    "shipped": t.deliverables.shipped,
                    "total": t.deliverables.total,
                },
                "milestones": {
                    "proposed": t.milestones.proposed,
                    "in_progress": t.milestones.in_progress,
                    "complete": t.milestones.complete,
                    "shipped": t.milestones.shipped,
                    "total": t.milestones.total,
                    "dangling_deliverable_refs": t.milestones.dangling_deliverable_refs,
                },
                "last_activity_at": t.last_activity_at,
            },
            "projects": status.projects.iter().map(|p| serde_json::json!({
                "project_name": p.project_name,
                "repo_url": p.repo_url,
                "sessions": {
                    "provisioning": p.sessions.provisioning,
                    "active": p.sessions.active,
                    "stopped": p.sessions.stopped,
                    "errored": p.sessions.errored,
                    "decommissioned": p.sessions.decommissioned,
                    "total": p.sessions.total,
                },
                "last_activity_at": p.last_activity_at,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!("{}", render_status_headline(&status));
    if status.projects.is_empty() {
        println!("  (no projects registered)");
        return Ok(());
    }
    for line in render_project_rows(&status) {
        println!("{line}");
    }
    Ok(())
}

/// `tm manager digest [--scope <scope>] [--json]` — LLM-authored narrative.
pub(crate) async fn digest(
    client: &reqwest::Client,
    url: &str,
    scope: &str,
    json: bool,
) -> anyhow::Result<()> {
    // `?` propagates a daemon-reported error (incl. a real 404 for an
    // unregistered `scope=project:<name>`) as-is; `None` here means the
    // client feature-detected via `GET /manager/version` that this daemon
    // genuinely predates the digest route.
    let outcome = daemon(client, url).manager_digest(scope).await?;
    let Some(outcome) = outcome else {
        anyhow::bail!(
            "this daemon does not support `manager digest` yet — upgrade the daemon \
             (`tm restart` after `cargo install trusty-mpm --locked`) and try again"
        );
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome.raw)?);
        return Ok(());
    }
    if outcome.narrative.is_empty() {
        println!("(empty digest — the daemon returned no narrative text)");
        return Ok(());
    }
    if outcome.fallback {
        println!("[deterministic fallback — no inference provider configured]");
    }
    println!("{}", outcome.narrative);
    Ok(())
}

/// `tm manager chat [message] [--conversation <key>] [--json]` — one-shot
/// portfolio chat turn.
pub(crate) async fn chat(
    client: &reqwest::Client,
    url: &str,
    message: Option<String>,
    conversation: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let message = match message {
        Some(m) => m,
        None => read_message_from_reader(&mut std::io::stdin())?,
    };
    if message.trim().is_empty() {
        anyhow::bail!("tm manager chat: message must not be empty");
    }
    let conversation_key = conversation.unwrap_or_else(default_conversation_key);

    // Same feature-detected 404 story as `digest` above: `None` only when
    // `GET /manager/version` reports the chat route genuinely unmounted.
    let outcome = daemon(client, url)
        .manager_chat(&conversation_key, &message)
        .await?;
    let Some(outcome) = outcome else {
        anyhow::bail!(
            "this daemon does not support `manager chat` yet — upgrade the daemon \
             (`tm restart` after `cargo install trusty-mpm --locked`) and try again"
        );
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome.raw)?);
        return Ok(());
    }
    if outcome.reply.is_empty() {
        println!("(empty reply — the daemon returned no reply text)");
        return Ok(());
    }
    println!("{}", outcome.reply);
    Ok(())
}

/// Read a one-shot chat message from stdin (piped or terminal, Ctrl-D-terminated).
///
/// Why: `tm manager chat` (issue #2583) takes the message as an OPTIONAL
/// positional (`[message]`); when omitted this is the fallback source rather
/// than launching a full interactive REPL (explicitly deferred to a
/// follow-up, see `ManagerAction::Chat`'s doc).
/// What: reads all of `reader` to a `String` and trims surrounding whitespace.
/// Taking a generic `Read` (rather than calling `std::io::stdin()` directly)
/// keeps this unit-testable against an in-memory buffer.
/// Test: `read_message_from_reader_reads_full_input`,
/// `read_message_from_reader_trims_whitespace`.
fn read_message_from_reader(reader: &mut impl std::io::Read) -> anyhow::Result<String> {
    let mut buf = String::new();
    reader
        .read_to_string(&mut buf)
        .map_err(|e| anyhow::anyhow!("failed to read message from stdin: {e}"))?;
    Ok(buf.trim().to_string())
}

/// The stable per-user conversation key `tm manager chat` defaults to.
///
/// Why: DOC-36 §3.2 keys `/manager/chat` conversations the same way
/// `SessionProxy`'s focus-map does (`client/proxy.rs`) — an opaque stable
/// string, not a fresh key per call. A random per-invocation key would mean
/// every one-shot `tm manager chat` starts a brand-new conversation server
/// side, defeating the point of conversation-keyed state; a fixed
/// process-wide constant would collide across different operators sharing one
/// daemon. Deriving from `$USER`/`$USERNAME` gives each local operator their
/// own durable conversation across repeated invocations on the same machine,
/// without adding a new dependency (no `whoami` crate) or touching disk.
/// `--conversation <key>` (see `ManagerAction::Chat`) overrides this for
/// scripts/tests that need an isolated key.
/// What: `"cli:<user>"`, falling back to `"cli:local"` when neither `$USER`
/// nor `$USERNAME` is set.
/// Test: `default_conversation_key_is_stable_and_prefixed`.
pub(crate) fn default_conversation_key() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local".to_string());
    format!("cli:{user}")
}

/// Render the portfolio headline: project count, summed session/Deliverable/
/// Milestone totals, and the most recent portfolio-wide activity.
///
/// Why: pure function of the parsed [`PortfolioStatusWire`] — the "L2 can't do
/// this" number DOC-36 §3.2 describes, testable without HTTP, matching
/// `commands/projects/registry.rs::render_status`'s style.
/// What: one summary line.
/// Test: `render_status_headline_sums_totals`, `render_status_headline_empty_portfolio`.
pub(crate) fn render_status_headline(status: &PortfolioStatusWire) -> String {
    let t = &status.totals;
    let activity = t
        .last_activity_at
        .map(|d| d.to_rfc3339())
        .unwrap_or_else(|| "never".to_string());
    format!(
        "portfolio: {} project{} — sessions {} total ({} active, {} provisioning, {} stopped, \
         {} errored, {} decommissioned) — deliverables {} total ({} in_progress, {} blocked, \
         {} complete) — milestones {} total — last activity: {activity}",
        status.project_count,
        if status.project_count == 1 { "" } else { "s" },
        t.sessions.total,
        t.sessions.active,
        t.sessions.provisioning,
        t.sessions.stopped,
        t.sessions.errored,
        t.sessions.decommissioned,
        t.deliverables.total,
        t.deliverables.in_progress,
        t.deliverables.blocked,
        t.deliverables.complete,
        t.milestones.total,
    )
}

/// Render the per-project breakdown table, name-sorted (the daemon already
/// sorts; this preserves that order rather than re-sorting).
///
/// Why: `tm manager status` needs the per-project drill-down alongside the
/// headline totals (task requirement: "portfolio totals headline + per-project
/// table"). Plain fixed-width text, matching the crate's dominant
/// deterministic-status render style (no color, no table crate — see
/// `commands/projects/registry.rs::render_status`).
/// What: one row per project: name, session total (active/provisioning/
/// errored breakdown), and last activity.
/// Test: `render_project_rows_one_row_per_project`.
pub(crate) fn render_project_rows(status: &PortfolioStatusWire) -> Vec<String> {
    status
        .projects
        .iter()
        .map(|p| {
            let activity = p
                .last_activity_at
                .map(|d| d.to_rfc3339())
                .unwrap_or_else(|| "never".to_string());
            format!(
                "  {:<20} sessions: {} total ({} active, {} provisioning, {} errored)  last activity: {activity}",
                p.project_name, p.sessions.total, p.sessions.active, p.sessions.provisioning, p.sessions.errored,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use trusty_mpm::client::http_client::manager::{
        DeliverableStatusCountsWire, MilestoneStatusCountsWire, PortfolioTotalsWire,
    };
    use trusty_mpm::client::http_client::projects::{ProjectStatusWire, SessionStateCountsWire};

    use super::*;

    fn ts() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn project(name: &str, active: usize) -> ProjectStatusWire {
        ProjectStatusWire {
            project_name: name.to_string(),
            repo_url: format!("https://github.com/acme/{name}"),
            sessions: SessionStateCountsWire {
                provisioning: 1,
                active,
                stopped: 0,
                errored: 0,
                decommissioned: 0,
                total: active + 1,
            },
            last_activity_at: Some(ts()),
            config: Default::default(),
        }
    }

    fn status_with(projects: Vec<ProjectStatusWire>) -> PortfolioStatusWire {
        let sessions_total: usize = projects.iter().map(|p| p.sessions.total).sum();
        let active_total: usize = projects.iter().map(|p| p.sessions.active).sum();
        PortfolioStatusWire {
            project_count: projects.len(),
            totals: PortfolioTotalsWire {
                sessions: SessionStateCountsWire {
                    provisioning: projects.len(),
                    active: active_total,
                    stopped: 0,
                    errored: 0,
                    decommissioned: 0,
                    total: sessions_total,
                },
                deliverables: DeliverableStatusCountsWire {
                    total: 2,
                    in_progress: 1,
                    complete: 1,
                    ..Default::default()
                },
                milestones: MilestoneStatusCountsWire {
                    total: 1,
                    complete: 1,
                    ..Default::default()
                },
                last_activity_at: Some(ts()),
            },
            projects,
        }
    }

    #[test]
    fn render_status_headline_sums_totals() {
        let status = status_with(vec![project("alpha", 2), project("beta", 1)]);
        let line = render_status_headline(&status);
        assert!(line.contains("2 projects"));
        assert!(line.contains("5 total"), "{line}"); // 3 + 2 sessions
        assert!(line.contains("3 active"), "{line}");
        assert!(line.contains("2 total (1 in_progress"), "{line}");
        assert!(line.contains("2026-07-13"));
    }

    #[test]
    fn render_status_headline_empty_portfolio() {
        let status = status_with(vec![]);
        let line = render_status_headline(&status);
        assert!(line.contains("0 projects"));
        assert!(line.contains("0 total"));
    }

    #[test]
    fn render_status_headline_singular_project_noun() {
        let status = status_with(vec![project("alpha", 1)]);
        let line = render_status_headline(&status);
        assert!(line.contains("1 project "), "{line}");
        assert!(!line.contains("1 projects"));
    }

    #[test]
    fn render_project_rows_one_row_per_project() {
        let status = status_with(vec![project("alpha", 2), project("beta", 0)]);
        let rows = render_project_rows(&status);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("alpha"));
        assert!(rows[0].contains("3 total"));
        assert!(rows[0].contains("2 active"));
        assert!(rows[1].contains("beta"));
        assert!(rows[1].contains("0 active"));
    }

    #[test]
    fn default_conversation_key_is_stable_and_prefixed() {
        // Two calls in the same process/env agree (stability), and the
        // format is the documented `cli:<user>` shape.
        let a = default_conversation_key();
        let b = default_conversation_key();
        assert_eq!(a, b);
        assert!(a.starts_with("cli:"), "{a}");
    }

    #[test]
    fn read_message_from_reader_reads_full_input() {
        let mut reader = std::io::Cursor::new(b"what needs my attention?".to_vec());
        let msg = read_message_from_reader(&mut reader).unwrap();
        assert_eq!(msg, "what needs my attention?");
    }

    #[test]
    fn read_message_from_reader_trims_whitespace() {
        let mut reader = std::io::Cursor::new(b"  hello there  \n".to_vec());
        let msg = read_message_from_reader(&mut reader).unwrap();
        assert_eq!(msg, "hello there");
    }
}
