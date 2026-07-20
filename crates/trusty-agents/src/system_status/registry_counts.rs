//! Agent registry + skills counts for `system_status` (epic #3052).
//!
//! Why: the harness already logs `agent registry loaded from hierarchical
//! search paths count=N` and a skills-index count at startup
//! (`runtime::predispatch::build_registries`) — this module reuses the exact
//! same loader calls (`AgentRegistry::load` over `agent_search_paths`,
//! `SkillRegistry::from_sources` over a project-local `SkillSourceRegistry`)
//! so the counts `system_status` reports can never drift from what startup
//! already discovered. It deliberately skips `predispatch`'s background
//! remote-source git refresh (`ensure_remote_sources`) and index-effectiveness
//! merge — those are startup-time enrichments, not part of the discovery
//! count itself, and a manual on-demand status call should not trigger a
//! network fetch.
//! What: [`counts`] returns `(agent_count, skills_count)`, running the
//! (local-filesystem-only) agent walk on the blocking pool since it is
//! synchronous directory IO.
//! Test: `super::tests::counts_do_not_panic_on_a_fresh_tempdir`.

use std::path::Path;

/// Discover the agent registry and local skill sources, returning their
/// counts.
///
/// Why: single entry point so `system_status::gather` doesn't need to know
/// which loader belongs to which subsystem.
/// What: `agent_search_paths`/`AgentRegistry::load` walk `.trusty-agents/agents/`
/// hierarchically exactly as startup does; `SkillSourceRegistry::load` +
/// `SkillRegistry::from_sources` scan the already-cloned/cached local skill
/// sources for `project_root` without touching the network.
/// Test: `super::tests::counts_do_not_panic_on_a_fresh_tempdir`.
pub async fn counts(project_root: &Path) -> (usize, usize) {
    let bundled = crate::default_bundled_config_dir();
    let agent_count = {
        let paths = crate::agents::registry::agent_search_paths(&bundled);
        tokio::task::spawn_blocking(move || {
            crate::agents::registry::AgentRegistry::load(&paths).len()
        })
        .await
        .unwrap_or(0)
    };

    let skills_count = {
        let project_root = project_root.to_path_buf();
        let bundled = bundled.clone();
        tokio::task::spawn_blocking(move || {
            let source_registry = crate::skills::sources::SkillSourceRegistry::load(&project_root);
            crate::skills::registry::SkillRegistry::from_sources(
                &source_registry,
                &bundled.join("skills"),
            )
            .len()
        })
        .await
        .unwrap_or(0)
    };

    (agent_count, skills_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: `system_status` must degrade gracefully in a directory with no
    /// `.trusty-agents/` at all (e.g. an empty non-repo dir, as the live
    /// verify step uses) — zero counts, never a panic.
    /// Test: itself.
    #[tokio::test]
    async fn counts_do_not_panic_on_a_fresh_tempdir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (agents, skills) = counts(tmp.path()).await;
        // Bundled agents/skills still resolve from the binary's own bundled
        // config dir, so counts may be non-zero — the contract under test is
        // "does not panic / hang", not a specific count.
        let _ = (agents, skills);
    }
}
