//! `tagent mcp list [--assistant <id>]` — both MCP config tiers (#7454).
//!
//! Why: an assistant's connectors are now the product of TWO files (ADR-0060),
//! and there was no way to see either from a terminal. When an assistant does
//! not reach a server, the first question is which tier dropped it, and the
//! answer has to be readable without opening two TOML files and applying the
//! precedence rule by hand.
//! What: one line per effective server — marker, name, transport, tier, and
//! the reason when it is not usable — followed by any configuration issue with
//! its remedy. `--assistant` layers that assistant's `[mcp]` table; without it
//! the global tier alone is listed, which is what a non-assistant context
//! (a sub-agent, a fresh shell) actually resolves.
//! Test: `tests` below cover the rendering; the resolution itself is covered by
//! `crate::mcp::tests::resolve_tests`.

use anyhow::Result;

use crate::mcp::ResolvedMcp;
use crate::mcp::extensions;

/// Dispatch `tagent mcp …` from an argv tail.
///
/// Why: this crate dispatches subcommand prefixes on argv before the top-level
/// clap parse, so each handler parses its own tail (see
/// `crate::runtime::subcommands`).
/// What: `list` is the only verb — writes go through the shared file directly,
/// the `mcp_*` agent tools, or `PUT /api/assistants/:id/mcp`. An unknown verb
/// prints the usage line and exits non-zero.
/// Test: `parses_the_assistant_flag`.
pub async fn run_mcp_command(args: &[String]) -> Result<()> {
    let verb = args.first().map(String::as_str).unwrap_or("list");
    if verb != "list" {
        anyhow::bail!("usage: tagent mcp list [--assistant <id>]");
    }
    let assistant = parse_assistant(&args[1.min(args.len())..])?;
    let resolved = crate::mcp::resolve_here(assistant.as_deref()).await;
    print!("{}", render(&resolved));
    Ok(())
}

/// Read `--assistant <id>` out of an argv tail.
///
/// Test: `parses_the_assistant_flag`.
fn parse_assistant(args: &[String]) -> Result<Option<String>> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    match first.as_str() {
        "--assistant" | "-a" => args
            .get(1)
            .cloned()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("--assistant needs an assistant id")),
        other if other.starts_with("--assistant=") => {
            Ok(Some(other.trim_start_matches("--assistant=").to_string()))
        }
        other => {
            anyhow::bail!("unknown argument `{other}`; usage: tagent mcp list [--assistant <id>]")
        }
    }
}

/// The listing, including the tier each entry came from.
///
/// Why: "which tier" is the question this command exists to answer, so it is a
/// column rather than something the reader infers from two files. A server that
/// is configured but not usable is LISTED with its reason — omitting it would
/// report a connector as absent when it is one environment variable away.
/// Test: `renders_the_tier_and_the_reason`, `renders_an_empty_global_tier`.
fn render(resolved: &ResolvedMcp) -> String {
    let mut out = String::new();
    match &resolved.assistant {
        Some(name) => out.push_str(&format!("MCP servers for assistant `{name}`\n")),
        None => out.push_str("MCP servers (global tier)\n"),
    }

    if resolved.servers.is_empty() {
        out.push_str("  (none configured)\n");
    }
    for (server, status) in resolved.servers.iter().zip(&resolved.statuses) {
        let marker = if status.usable { "✓" } else { "✗" };
        let tier = match status.tier {
            crate::mcp::McpTier::Global => "global",
            crate::mcp::McpTier::Assistant => "assistant",
        };
        out.push_str(&format!(
            "  {marker} {} [{}] ({tier})\n",
            server.name,
            extensions::transport_label(server),
        ));
        if let Some(reason) = &status.reason {
            out.push_str(&format!("      {reason}\n"));
        }
    }

    for name in &resolved.overrides.disabled {
        out.push_str(&format!(
            "  ✗ {name} (assistant) — disabled for this assistant\n"
        ));
    }

    for issue in &resolved.issues {
        out.push_str(&format!(
            "\n  ! {}: {}\n    {}\n",
            issue.path.display(),
            issue.detail,
            issue.remedy
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistants::mcp::McpOverrides;
    use crate::mcp::shared::{GlobalTier, resolve_in};
    use trusty_mcp::config::{McpServerConfig, McpTransport};

    fn server(name: &str) -> McpServerConfig {
        McpServerConfig::new(
            name,
            McpTransport::Stdio {
                command: format!("{name}-bin"),
                args: Vec::new(),
                env: Default::default(),
            },
        )
    }

    fn tier(servers: Vec<McpServerConfig>) -> GlobalTier {
        GlobalTier {
            servers,
            issues: Vec::new(),
            path: std::path::PathBuf::from("servers.toml"),
        }
    }

    #[test]
    fn parses_the_assistant_flag() {
        assert_eq!(parse_assistant(&[]).unwrap(), None);
        assert_eq!(
            parse_assistant(&["--assistant".into(), "izzie".into()]).unwrap(),
            Some("izzie".to_string())
        );
        assert_eq!(
            parse_assistant(&["--assistant=izzie".into()]).unwrap(),
            Some("izzie".to_string())
        );
        assert!(parse_assistant(&["--assistant".into()]).is_err());
        assert!(parse_assistant(&["--nope".into()]).is_err());
    }

    /// Both tiers are visible in the listing, and a server the assistant added
    /// is labelled as coming from the assistant tier.
    #[test]
    fn renders_the_tier_and_the_reason() {
        let overrides = McpOverrides {
            servers: vec![server("izzie-only")],
            disabled: vec!["granola".to_string()],
        };
        let resolved = resolve_in(
            Some("izzie"),
            tier(vec![server("github"), server("granola")]),
            overrides,
            None,
        );
        let text = render(&resolved);
        assert!(text.contains("assistant `izzie`"), "{text}");
        assert!(text.contains("✓ github [stdio] (global)"), "{text}");
        assert!(text.contains("✓ izzie-only [stdio] (assistant)"), "{text}");
        assert!(
            text.contains("granola (assistant) — disabled for this assistant"),
            "a disabled global server must still be visible: {text}"
        );
    }

    #[test]
    fn renders_an_empty_global_tier() {
        let resolved = resolve_in(None, tier(Vec::new()), McpOverrides::default(), None);
        let text = render(&resolved);
        assert!(text.contains("global tier"), "{text}");
        assert!(text.contains("(none configured)"), "{text}");
    }

    /// A configuration issue is printed with its remedy — a listing that went
    /// quiet on a broken file would read as "you have no connectors".
    #[test]
    fn renders_an_issue_with_its_remedy() {
        let mut global = tier(Vec::new());
        global.issues.push(crate::mcp::McpIssue {
            tier: crate::mcp::McpTier::Global,
            path: std::path::PathBuf::from("/tmp/servers.toml"),
            detail: "could not be read".into(),
            remedy: "fix the TOML".into(),
        });
        let text = render(&resolve_in(None, global, McpOverrides::default(), None));
        assert!(text.contains("/tmp/servers.toml"), "{text}");
        assert!(text.contains("fix the TOML"), "{text}");
    }
}
