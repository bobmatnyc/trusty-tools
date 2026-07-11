//! `tm projects deliverables` subtree: list / add / show / set-status (#2381).
//!
//! Why: the CLI over the Deliverable CRUD API (§10.8). `set-status` is the one
//! verb with a state machine behind it (§10.3, enforced daemon-side by #2380):
//! an illegal transition returns a structured 409 the client parses into
//! [`SetStatusError::Rejected`]; this handler renders those legal next states so
//! the operator can self-correct instead of seeing a bare "409 Conflict".
//! What: [`dispatch`] routes a [`DeliverablesAction`]; the per-verb handlers are
//! thin `DaemonClient` calls with human/`--json` rendering; [`render_rejection`]
//! is the pure, tested formatter for the illegal-transition surface.
//! Test: `render_rejection_lists_allowed`, `render_rejection_terminal` in the
//! `tests` submodule; live HTTP via the daemon's `deliverable_routes::tests`.

use trusty_mpm::client::DaemonClient;
use trusty_mpm::client::http_client::deliverables::{CreateDeliverableArgs, SetStatusError};
use trusty_mpm::deliverable::{Deliverable, DeliverableStatus};

use crate::cli::DeliverablesAction;

/// Build a `DaemonClient` from the CLI's shared `(reqwest::Client, url)` pair.
fn daemon(client: &reqwest::Client, url: &str) -> DaemonClient {
    DaemonClient::with_client(client.clone(), url.to_string())
}

/// Route a `tm projects deliverables <action>` invocation.
pub(crate) async fn dispatch(
    client: &reqwest::Client,
    url: &str,
    action: DeliverablesAction,
) -> anyhow::Result<()> {
    match action {
        DeliverablesAction::List {
            project,
            json,
            status,
        } => {
            let deliverables = daemon(client, url)
                .list_deliverables(&project, status.map(Into::into))
                .await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&deliverables)?);
            } else if deliverables.is_empty() {
                println!("no deliverables for project '{project}'");
            } else {
                for d in &deliverables {
                    println!("{}", render_deliverable_line(d));
                }
            }
            Ok(())
        }
        DeliverablesAction::Add {
            project,
            name,
            kind,
            estimate,
            description,
            spec_ref,
            ticket_ref,
        } => {
            let args = CreateDeliverableArgs {
                name,
                description,
                kind: kind.into(),
                estimated_effort: estimate.into(),
                ticket_ref,
                spec_ref,
            };
            let created = daemon(client, url)
                .create_deliverable(&project, &args)
                .await?;
            println!("created deliverable {}", created.id);
            println!("{}", render_deliverable_line(&created));
            Ok(())
        }
        DeliverablesAction::Show { project, id, json } => {
            let d = daemon(client, url).get_deliverable(&project, &id).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                for line in render_deliverable_detail(&d) {
                    println!("{line}");
                }
            }
            Ok(())
        }
        DeliverablesAction::SetStatus {
            project,
            id,
            status,
        } => set_status(client, url, &project, &id, status.into()).await,
    }
}

/// Handle `set-status`, rendering the structured 409 clearly on rejection.
///
/// Why: the illegal-transition case is the whole point of #2380's structured
/// error — the operator must SEE the legal next states. On rejection this prints
/// [`render_rejection`] to stderr and returns an error (non-zero exit) so scripts
/// can detect the failure; a genuine transport/404 error propagates unchanged.
/// What: calls [`DaemonClient::set_deliverable_status`]; on
/// [`SetStatusError::Rejected`] prints the guidance and errors out; on success
/// prints the new status; on [`SetStatusError::Other`] propagates the error.
/// Test: rendering covered by `render_rejection_lists_allowed`; live 409 path by
/// the daemon route test.
async fn set_status(
    client: &reqwest::Client,
    url: &str,
    project: &str,
    id: &str,
    status: DeliverableStatus,
) -> anyhow::Result<()> {
    match daemon(client, url)
        .set_deliverable_status(project, id, status)
        .await
    {
        Ok(updated) => {
            println!("deliverable {} is now '{}'", updated.id, updated.status);
            Ok(())
        }
        Err(SetStatusError::Rejected {
            from,
            to,
            allowed_next,
        }) => {
            eprintln!("{}", render_rejection(&from, &to, &allowed_next));
            anyhow::bail!("status transition rejected");
        }
        Err(SetStatusError::Other(e)) => Err(e),
    }
}

/// Render an illegal-transition rejection as an operator-facing message (#2380).
///
/// Why: this is the surface the task calls out — the operator must see WHICH
/// states are legal next. A pure function of the three parts keeps it testable.
/// What: a two-line message naming the rejected `from → to` and the legal next
/// states (or "none (terminal state)" when the from-state is terminal).
/// Test: `render_rejection_lists_allowed`, `render_rejection_terminal`.
pub(crate) fn render_rejection(from: &str, to: &str, allowed_next: &[String]) -> String {
    let allowed = if allowed_next.is_empty() {
        "none (terminal state)".to_string()
    } else {
        allowed_next.join(", ")
    };
    format!(
        "error: cannot transition deliverable from '{from}' to '{to}'\n  legal next states from '{from}': {allowed}"
    )
}

/// Render one Deliverable as a compact `id  status  kind  tier  name` line.
fn render_deliverable_line(d: &Deliverable) -> String {
    format!(
        "{}\t{}\t{:?}\t{:?}\t{}",
        d.id, d.status, d.kind, d.estimated_effort, d.name
    )
}

/// Render a Deliverable's full detail as human lines.
fn render_deliverable_detail(d: &Deliverable) -> Vec<String> {
    let mut lines = vec![
        format!("{} — {}", d.id, d.name),
        format!("  project: {}", d.project_name),
        format!("  status: {}", d.status),
        format!("  kind: {:?}   estimate: {:?}", d.kind, d.estimated_effort),
    ];
    if let Some(t) = &d.ticket_ref {
        lines.push(format!("  ticket: {t}"));
    }
    if let Some(s) = &d.spec_ref {
        lines.push(format!("  spec: {s}"));
    }
    if !d.description.is_empty() {
        lines.push(format!("  description: {}", d.description));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_rejection_lists_allowed() {
        let msg = render_rejection("proposed", "complete", &["in-progress".to_string()]);
        assert!(msg.contains("proposed"), "{msg}");
        assert!(msg.contains("complete"), "{msg}");
        assert!(msg.contains("in-progress"), "{msg}");
        assert!(msg.contains("legal next states"), "{msg}");
    }

    #[test]
    fn render_rejection_terminal() {
        // A terminal from-state has no legal successors.
        let msg = render_rejection("shipped", "complete", &[]);
        assert!(msg.contains("none (terminal state)"), "{msg}");
    }

    #[test]
    fn render_deliverable_line_has_status_and_name() {
        let d = Deliverable {
            id: trusty_mpm::deliverable::DeliverableId::new(),
            project_name: "widget".into(),
            name: "OAuth2 flow".into(),
            description: String::new(),
            kind: trusty_mpm::deliverable::DeliverableKind::Feature,
            ticket_ref: None,
            spec_ref: None,
            status: DeliverableStatus::Proposed,
            estimated_effort: trusty_mpm::deliverable::EstimationTier::L,
            created_at: chrono::Utc::now(),
            target_date: None,
        };
        let line = render_deliverable_line(&d);
        assert!(line.contains("proposed"), "{line}");
        assert!(line.contains("OAuth2 flow"), "{line}");
    }
}
