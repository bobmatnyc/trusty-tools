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
//! What: [`register_mcp_tools`] resolves this persona's servers once, registers
//! every executor the three surfaces produce, and returns the scope vocabulary
//! the endpoints ADVERTISED — unfiltered by the operator's own `scopes` policy,
//! because the dead-grant diagnostic reasons over what could exist, not over
//! what policy admitted (#3987).
//! Test: `crate::mcp::tests::resolve_tests` covers the resolution this consumes;
//! `crate::tools::registry::tests` covers the endpoint build.

use std::path::Path;

use crate::tools::ToolRegistry;

/// Register every MCP-derived tool for one persona, and report the vocabulary.
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
) -> Vec<String> {
    for tool in crate::tools::mcp_tools::mcp_tool_executors() {
        crate::tools::listener_config::register_external(registry, tool);
    }

    // #7454: every surface below is built from THIS persona's resolved server
    // set, so an assistant-level add, replace or disable reaches the tools the
    // model can actually call.
    let resolved = crate::mcp::resolve_for_assistant(Some(persona_name), project_path).await;
    for tool in crate::tools::mcp_service_tools::mcp_service_tool_executors(
        Some(persona_name),
        project_path,
    )
    .await
    {
        crate::tools::listener_config::register_external(registry, tool);
    }

    // #3987: the returned vocabulary is the UNFILTERED set every endpoint
    // published. The registry keeps only what the operator's `scopes` policy
    // let through; feeding the dead-grant diagnostic that post-policy set would
    // make a deliberate narrowing look like a broken agent grant — see
    // `build_with_scope_vocabulary`.
    match crate::tools::registry::ToolRegistryBuilder::from_servers(resolved.usable())
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
    }
}
