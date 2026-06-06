//! Code analysis tool surface (#373).
//!
//! Why: Ports the core capabilities of `mcp-vector-search`'s analysis
//! collectors (complexity, smells, coupling, dependency cycles, call graphs)
//! into native Rust on top of `symgraph`. Lets the LLM reason about code
//! quality structurally without shelling out.
//! What: Six tools — `analyze_file`, `analyze_project`,
//! `get_complexity_hotspots`, `find_smells`, `check_circular_dependencies`,
//! `trace_execution_flow`. Each implements `ToolExecutor`. The public
//! `analysis_tools()` function returns the canonical bundle for registration.
//! Test: Each tool has a unit test against an inline tempdir fixture.

pub mod analyze_file;
pub mod analyze_project;
pub mod ast_walker;
pub mod circular_deps;
pub mod hotspots;
pub mod metrics;
pub mod smells;
pub mod trace_flow;

use std::sync::Arc;

use crate::tools::traits::ToolExecutor;

pub use analyze_file::AnalyzeFileTool;
pub use analyze_project::AnalyzeProjectTool;
pub use circular_deps::CheckCircularDependenciesTool;
pub use hotspots::GetComplexityHotspotsTool;
pub use smells::FindSmellsTool;
pub use trace_flow::TraceExecutionFlowTool;

/// Build the canonical analysis-tool bundle.
///
/// Why: One call to register all six analysis tools on an agent registry.
/// What: Returns a `Vec<Arc<dyn ToolExecutor>>` containing every tool in
/// `src/tools/analysis/`.
/// Test: `analysis_tools_returns_six` below.
pub fn analysis_tools() -> Vec<Arc<dyn ToolExecutor>> {
    vec![
        Arc::new(AnalyzeFileTool),
        Arc::new(AnalyzeProjectTool),
        Arc::new(GetComplexityHotspotsTool),
        Arc::new(FindSmellsTool),
        Arc::new(CheckCircularDependenciesTool),
        Arc::new(TraceExecutionFlowTool),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_tools_returns_six() {
        let tools = analysis_tools();
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for n in [
            "analyze_file",
            "analyze_project",
            "get_complexity_hotspots",
            "find_smells",
            "check_circular_dependencies",
            "trace_execution_flow",
        ] {
            assert!(names.contains(&n), "missing tool: {n}");
        }
    }
}
