//! Registering one persona's MCP-derived tools (#7454).
//!
//! Why: three MCP surfaces feed a persona's registry — the `mcp_*` management
//! tools, the statically declared tools of each configured server, and the
//! OpenRPC registry endpoints — and since ADR-0060 all three must be built
//! from the SAME resolved server set, or an assistant-level override would
//! reach one surface and not the others. Keeping the three calls together in
//! one function is what makes that visible; splitting them back across the
//! caller is how they would drift.
//!
//! It also keeps [`super::persona`] under the 500-SLOC production cap, which
//! is the immediate reason this file exists rather than the block staying
//! inline.
//!
//! What: [`register_mcp_tools`] resolves this persona's servers ONCE, registers
//! every executor the three surfaces produce, and returns that resolution
//! alongside the scope vocabulary the endpoints ADVERTISED — unfiltered by the
//! operator's own `scopes` policy, because the dead-grant diagnostic reasons
//! over what could exist, not over what policy admitted (#3987).
//!
//! The resolution comes BACK to the caller rather than staying here, because
//! live discovery is the fourth consumer and runs later in the same turn
//! ([`super::persona`]). Returning it is what makes one read per turn
//! structural: no surface in this path can resolve for itself, because none of
//! them takes a path or an assistant name any more (#7454 review).
//! Test: `crate::mcp::tests::resolve_tests` covers the resolution this consumes;
//! `crate::tools::registry::tests` covers the endpoint build;
//! `tests::the_persona_turn_resolves_mcp_exactly_once` and
//! `tests::the_tool_surfaces_partition_one_resolved_set` pin the single-read
//! invariant.

use std::path::Path;

use crate::mcp::ResolvedMcp;
use crate::tools::ToolRegistry;

/// Register every MCP-derived tool for one persona, and report what it used.
///
/// Why/What: see the module doc. A registry-build failure is warned and
/// skipped rather than propagated — one bad endpoint must not cost the persona
/// its other tools, which is the same posture `ToolRegistryBuilder` takes per
/// endpoint.
/// Test: as above.
pub(super) async fn register_mcp_tools(
    registry: &mut ToolRegistry,
    persona_name: &str,
    project_path: &Path,
) -> (ResolvedMcp, Vec<String>) {
    for tool in crate::tools::mcp_tools::mcp_tool_executors() {
        crate::tools::listener_config::register_external(registry, tool);
    }

    // #7454: every surface below is built from THIS persona's resolved server
    // set, so an assistant-level add, replace or disable reaches the tools the
    // model can actually call. Resolved exactly once per turn — a second read
    // could observe a concurrent `mcp_add` and leave two surfaces disagreeing.
    let resolved = crate::mcp::resolve_for_assistant(Some(persona_name), project_path).await;
    let usable = resolved.usable();
    for tool in crate::tools::mcp_service_tools::mcp_service_tool_executors(&usable) {
        crate::tools::listener_config::register_external(registry, tool);
    }

    // #3987: the returned vocabulary is the UNFILTERED set every endpoint
    // published. The registry keeps only what the operator's `scopes` policy
    // let through; feeding the dead-grant diagnostic that post-policy set would
    // make a deliberate narrowing look like a broken agent grant — see
    // `build_with_scope_vocabulary`.
    let vocabulary = match crate::tools::registry::ToolRegistryBuilder::from_servers(usable)
        .build_with_scope_vocabulary()
        .await
    {
        Ok((executors, vocabulary)) => {
            for tool in executors {
                crate::tools::listener_config::register_external(registry, tool);
            }
            vocabulary
        }
        Err(e) => {
            tracing::warn!("tool registry init failed: {e}");
            Vec::new()
        }
    };
    (resolved, vocabulary)
}

#[cfg(test)]
mod tests {
    use trusty_mcp::config::{McpServerConfig, McpTransport};

    use crate::mcp::extensions::{self, ToolDescriptor};
    use crate::mcp::shared::{GlobalTier, resolve_in};
    use crate::tools::mcp_live::gather_specs;
    use crate::tools::mcp_service_tools::mcp_service_tool_executors;

    fn stdio(name: &str) -> McpServerConfig {
        McpServerConfig::new(
            name,
            McpTransport::Stdio {
                command: name.to_string(),
                args: Vec::new(),
                env: Default::default(),
            },
        )
    }

    /// How many lines call something that reads MCP configuration from disk,
    /// ignoring comments.
    ///
    /// The needles are assembled from halves so this function never matches
    /// its own source — one of the files it scans is this one.
    fn resolution_calls(source: &str) -> usize {
        let needles = [
            concat!("resolve_for_", "assistant("),
            concat!("resolve_", "here("),
        ];
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .filter(|line| needles.iter().any(|needle| line.contains(needle)))
            .count()
    }

    /// Why: before this change the same assistant's MCP config was read two or
    /// three times per turn — `register_mcp_tools` resolved, the static tool
    /// builder resolved again internally, and live discovery a third time — so
    /// an `mcp_add` or a disable landing between two reads left the surfaces
    /// disagreeing about what the assistant connects to. This module's doc
    /// claimed all three were built from the SAME resolved set; they were not.
    /// What: the whole persona-chat assembly path holds exactly ONE resolution
    /// call. A second one is what this test exists to catch — thread the
    /// `ResolvedMcp` `register_mcp_tools` returns instead of adding a read.
    #[test]
    fn the_persona_turn_resolves_mcp_exactly_once() {
        let counts = [
            (
                "persona_mcp.rs",
                resolution_calls(include_str!("persona_mcp.rs")),
            ),
            ("persona.rs", resolution_calls(include_str!("persona.rs"))),
        ];
        let total: usize = counts.iter().map(|(_, n)| n).sum();
        assert_eq!(
            total, 1,
            "the persona turn must read MCP configuration once; per file: {counts:?}"
        );
    }

    /// The sub-agent run carries the same invariant: the prompt's MCP section
    /// and the live-discovered tool set come from one resolution, so the model
    /// is never told it can reach a server its registry lacks.
    #[test]
    fn the_subagent_run_resolves_mcp_exactly_once() {
        assert_eq!(
            resolution_calls(include_str!("../../../runtime/subagent_mode.rs")),
            1,
            "the sub-agent run must read MCP configuration once"
        );
    }

    /// Why: one resolution only matters if the surfaces then PARTITION it.
    /// What: one resolved set feeds both tool surfaces — a `discover = true`
    /// server reaches live discovery and nothing else, a statically-declared
    /// server reaches the static path and nothing else, and a disabled server
    /// reaches neither, because both read the same `usable()` rather than
    /// each applying its own filter to its own read.
    #[test]
    fn the_tool_surfaces_partition_one_resolved_set() {
        let mut declared = stdio("static-one");
        extensions::set(
            &mut declared.extensions,
            extensions::TOOLS,
            &vec![ToolDescriptor {
                name: "static_op".to_string(),
                description: "does a thing".to_string(),
            }],
        );
        let mut live = stdio("live-one");
        extensions::set(&mut live.extensions, extensions::DISCOVER, &true);
        let mut off = stdio("switched-off");
        off.enabled = false;

        let global = GlobalTier {
            servers: vec![declared, live, off],
            issues: Vec::new(),
            path: std::path::PathBuf::from("servers.toml"),
        };
        let resolved = resolve_in(Some("izzie"), global, Default::default(), None);
        let usable = resolved.usable();

        let names: Vec<&str> = usable.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["static-one", "live-one"],
            "the disabled server is filtered once, for every surface"
        );

        let executors = mcp_service_tool_executors(&usable);
        let static_tools: Vec<&str> = executors.iter().map(|tool| tool.name()).collect();
        assert_eq!(
            static_tools,
            ["static_op"],
            "the static path sees only its own server"
        );

        let live_specs: Vec<String> = gather_specs(&usable)
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        assert_eq!(
            live_specs,
            ["live-one"],
            "live discovery sees only its own server"
        );
    }
}
