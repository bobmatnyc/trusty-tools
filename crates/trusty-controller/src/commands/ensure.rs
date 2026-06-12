//! `tctl ensure` — idempotent per-project auto-config pass (UUC1, DOC-8 §2).
//!
//! Why: The explicit manual entry point for the ensure-project pass that also
//! fires automatically on every claude-mpm launch via a startup hook (DOC-8
//! §4). It patches `.mcp.json`, registers the search index, creates the memory
//! palace, and optionally waits for full readiness.
//!
//! What: Phase-0 stub. Phase-1 will implement the DOC-8 §2 check→act→verify
//! shape, composing `trusty_common::claude_config::patch_mcp_server`,
//! trusty-search `POST /indexes`, and trusty-memory palace-create primitives.
//!
//! Test: `run(false, false)` does not panic.

use crate::output;

/// Handle `tctl ensure [--wait]`.
///
/// Why: Phase-0 entry point for the idempotent project-setup pass.
///
/// What: `wait` blocks until the project reaches `fresh` (for CI / DOC-10
/// harness). `json` selects machine output. Both are stubs in Phase 0.
///
/// Test: Call with both `wait` values and both `json` values; none should panic.
pub fn run(wait: bool, json: bool) {
    let cmd = if wait { "ensure --wait" } else { "ensure" };
    output::print_not_yet_implemented(cmd, json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn does_not_panic() {
        run(false, false);
        run(true, false);
        run(false, true);
    }
}
