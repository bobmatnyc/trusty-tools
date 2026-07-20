//! Human-readable rendering of a [`super::SystemStatusReport`].
//!
//! Why: the CLI's plain-text mode and the `system_status` tool's
//! LLM-facing content both want the same well-formatted summary — one
//! renderer keeps them from drifting apart.
//! What: [`render_text`] — a single multi-section plain-text report.
//! JSON output (`--json` / the tool's structured path) uses
//! `serde_json::to_string_pretty` directly on the report, no renderer needed.
//! Test: `super::tests::render_text_mentions_every_section`.

use super::SystemStatusReport;

/// Render `report` as a plain-text, section-headed summary.
///
/// Why: shared by `tagent system status` (human mode) and the `system_status`
/// tool's `ToolResult::ok` content, so an agent and a human operator read the
/// identical summary.
/// What: one line for the tagent self-identity, then a fixed-width table per
/// section (daemons, MCP servers, credentials), then the registry counts.
/// Test: `super::tests::render_text_mentions_every_section`.
pub fn render_text(report: &SystemStatusReport) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "tagent {} — agent={} model={} runner={}\n",
        report.tagent.version,
        report.tagent.active_agent,
        report.tagent.model,
        report.tagent.runner
    ));

    out.push_str("\nDaemons:\n");
    for d in &report.daemons {
        let state = if d.up { "up" } else { "down" };
        let version = d.version.as_deref().unwrap_or("-");
        let detail = d.detail.as_deref().unwrap_or("");
        out.push_str(&format!(
            "  {:<14} {:<5} version={:<10} {}\n",
            d.name, state, version, detail
        ));
    }

    out.push_str("\nMCP servers:\n");
    if report.mcp_servers.is_empty() {
        out.push_str("  (none configured)\n");
    }
    for m in &report.mcp_servers {
        let state = if !m.enabled {
            "disabled"
        } else if m.reachable {
            "reachable"
        } else {
            "unreachable"
        };
        out.push_str(&format!(
            "  {:<20} {:<12} transport={}\n",
            m.name, state, m.transport
        ));
    }

    out.push_str("\nCredentials (names/tiers only — no values):\n");
    for c in &report.credentials {
        out.push_str(&format!("  {:<12} {}\n", c.provider, c.status));
    }

    out.push_str(&format!(
        "\nAgent registry: {} agents discovered\nSkills: {} skills discovered\n",
        report.agent_registry_count, report.skills_count
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_status::credentials::CredentialStatus;
    use crate::system_status::daemons::DaemonStatus;
    use crate::system_status::{McpServerStatus, TagentSelfStatus};

    /// Why: a formatting regression that silently drops a whole section
    /// (e.g. an empty `Vec` short-circuiting before the header prints) would
    /// otherwise only be caught by eyeballing the live-verify transcript.
    /// Test: itself.
    #[test]
    fn render_text_mentions_every_section() {
        let report = SystemStatusReport {
            tagent: TagentSelfStatus {
                version: "9.9.9".into(),
                active_agent: "assistant".into(),
                model: "anthropic/claude-opus-4-6".into(),
                runner: "subprocess".into(),
            },
            daemons: vec![
                DaemonStatus {
                    name: "trusty-search",
                    up: true,
                    version: Some("0.32.2".into()),
                    detail: Some("12 indexes".into()),
                },
                DaemonStatus {
                    name: "trusty-memory",
                    up: false,
                    version: None,
                    detail: None,
                },
            ],
            mcp_servers: vec![McpServerStatus {
                name: "kuzu-memory".into(),
                transport: "stdio".into(),
                enabled: true,
                reachable: true,
            }],
            credentials: vec![CredentialStatus {
                provider: "openrouter".into(),
                status: "not configured".into(),
            }],
            agent_registry_count: 46,
            skills_count: 12,
        };

        let text = render_text(&report);
        assert!(text.contains("tagent 9.9.9"));
        assert!(text.contains("assistant"));
        assert!(text.contains("trusty-search"));
        assert!(text.contains("up"));
        assert!(text.contains("trusty-memory"));
        assert!(text.contains("down"));
        assert!(text.contains("kuzu-memory"));
        assert!(text.contains("openrouter"));
        assert!(text.contains("46 agents discovered"));
        assert!(text.contains("12 skills discovered"));
    }
}
