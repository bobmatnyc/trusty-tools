//! The trust gate over the project tier (#5428).
//!
//! Why: `<project>/.trusty-code/mcp.toml` is REPO-TRACKED content. Honouring
//! an arbitrary `command` from it means anyone who can land a commit — or
//! anyone whose repo you clone — chooses a subprocess this harness spawns with
//! the operator's ambient credentials. That is remote code execution, and it
//! is the same vector `trusty_agents::mcp::shared::merge_mcp_json` gates
//! `.mcp.json` behind (#3266).
//!
//! What: the project tier may say WHICH of the operator's globally configured
//! servers this project uses, never WHAT a server runs. A `Disable` always
//! passes — switching a connector off can only reduce what runs. A `Set` passes
//! only when it is content-equivalent to a global entry of the same name: the
//! identical command, args and env for stdio, the identical url and headers for
//! a remote entry. Name-only matching is NOT sufficient; that would let a
//! project keep a trusted name and swap the command behind it, which is the
//! whole attack. Anything else is refused as an [`McpIssue`] whose remedy names
//! the global file — the operator's own file, which they alone write.
//!
//! // #5428: trust-by-content per the #7422/#3033 ruling.
//!
//! Test: `super::tests::trust_tests` — the whole module.

use std::path::Path;

use trusty_mcp::config::{McpServerConfig, McpServerOverride, McpTransport};

use super::config::{McpIssue, McpTier, ProjectOverrides};

/// What the operator must do to let a refused entry through.
const REMEDY: &str = "Add this server to ~/.trusty-tools/mcp/servers.toml, the file only you \
                      write. A project may choose among the servers configured there and \
                      disable them; it may not introduce or alter a command this harness runs.";

/// Whether two transports describe the SAME connection, byte for byte.
///
/// Why: the trust decision turns on content, not on a name, so this is the
/// whole comparison the gate rests on. A partial comparison — command but not
/// args, url but not headers — would leave exactly the gap the gate exists to
/// close.
/// What: variants must match, and every field within them. `BTreeMap` equality
/// covers `env` and `headers` including key order-independence and arity.
/// Test: `super::tests::trust_tests::an_identical_stdio_entry_is_equivalent`,
/// `super::tests::trust_tests::a_differing_arg_is_not_equivalent`,
/// `super::tests::trust_tests::a_differing_env_value_is_not_equivalent`,
/// `super::tests::trust_tests::http_and_sse_at_the_same_url_are_not_equivalent`.
pub fn transport_equivalent(project: &McpTransport, global: &McpTransport) -> bool {
    match (project, global) {
        (
            McpTransport::Stdio {
                command: pc,
                args: pa,
                env: pe,
            },
            McpTransport::Stdio {
                command: gc,
                args: ga,
                env: ge,
            },
        ) => pc == gc && pa == ga && pe == ge,
        (
            McpTransport::Http {
                url: pu,
                headers: ph,
            },
            McpTransport::Http {
                url: gu,
                headers: gh,
            },
        ) => pu == gu && ph == gh,
        (
            McpTransport::Sse {
                url: pu,
                headers: ph,
            },
            McpTransport::Sse {
                url: gu,
                headers: gh,
            },
        ) => pu == gu && ph == gh,
        _ => false,
    }
}

/// Split the project's table into overrides the resolver may see and refusals.
///
/// Why: the single place the repo-content trust decision is made, so no caller
/// can reach `trusty_mcp::config::resolve` with an ungated project entry.
/// What: every `disabled` name becomes a `Disable` unconditionally. Every
/// `servers` entry becomes a `Set` only when a global entry of the same name
/// exists AND [`transport_equivalent`] holds — which still lets the project
/// flip `enabled`, because that field is outside the transport. Everything else
/// is refused with an [`McpIssue`] and a `tracing::warn!`. `project_path` names
/// the file in the finding and is never read here.
///
/// Disables are emitted BEFORE sets, matching
/// `trusty_agents::assistants::mcp::McpOverrides::as_overrides`, so a name in
/// both lists resolves as the more specific `Set` under the resolver's
/// left-to-right fold.
/// Test: `super::tests::trust_tests::a_matching_set_is_accepted`,
/// `super::tests::trust_tests::a_set_with_a_new_command_is_refused`,
/// `super::tests::trust_tests::a_set_naming_no_global_server_is_refused`,
/// `super::tests::trust_tests::a_set_may_re_enable_a_disabled_global_entry`,
/// `super::tests::trust_tests::a_disable_always_passes`.
pub fn gate(
    global: &[McpServerConfig],
    overrides: &ProjectOverrides,
    project_path: &Path,
) -> (Vec<McpServerOverride>, Vec<McpIssue>) {
    let mut accepted = Vec::with_capacity(overrides.disabled.len() + overrides.servers.len());
    let mut refused = Vec::new();

    for name in &overrides.disabled {
        accepted.push(McpServerOverride::Disable { name: name.clone() });
    }

    for server in &overrides.servers {
        let Some(matching) = global.iter().find(|g| g.name == server.name) else {
            refused.push(refusal(
                project_path,
                &server.name,
                "names a server the shared MCP server file does not define",
            ));
            continue;
        };
        if !transport_equivalent(&server.transport, &matching.transport) {
            refused.push(refusal(
                project_path,
                &server.name,
                "redefines how that server is reached, which a project may not do",
            ));
            continue;
        }
        accepted.push(McpServerOverride::Set(server.clone()));
    }

    (accepted, refused)
}

/// One refusal, logged and returned.
///
/// What: the detail names the server and what it tried, and NOTHING of the
/// transport — an `env` value or an auth header in a log line is the leak the
/// redacted `Debug` on `McpTransport` exists to prevent, and a refusal message
/// must not reintroduce it.
fn refusal(path: &Path, server: &str, what: &str) -> McpIssue {
    tracing::warn!(
        path = %path.display(),
        server = %server,
        "mcp: refusing a project-tier MCP entry that {what}",
    );
    McpIssue {
        tier: McpTier::Project,
        path: path.to_path_buf(),
        detail: format!("the project entry {server:?} {what}; it was not loaded"),
        remedy: REMEDY.to_string(),
    }
}
