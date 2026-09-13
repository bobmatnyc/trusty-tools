//! `self-improvement-loop` bundled skill (issue #7723) — the self-analysis
//! reporting protocol and the fast-loop hypothesis record, moved out of
//! `BASE-AGENT.md` so the mechanics are paid only when an agent's final-report
//! trigger actually fires.
//!
//! Why: `BASE-AGENT.md` is resident in every dispatched agent's first-turn
//! context — 2026-09-13 measurement (epic #7681) found the "Self-Analysis and
//! Improvement Reporting" + "Continuous Self-Improvement — the Fast Loop"
//! sections cost ~7KB there for content that only matters once, at the end of
//! a run. This skill is deliberately NOT added to any agent's `skills:`
//! frontmatter — Claude Code's subagent preload renders every listed skill's
//! full body into the dispatch prompt (the same mechanism
//! `code_critic_declares_batch1_skills` documents), so listing it there would
//! simply relocate the cost rather than cut it. BASE-AGENT keeps a short
//! resident trigger naming this skill in prose; an agent loads it on demand
//! via the `Skill` tool only when finishing a task.
//! What: `pub const SELF_IMPROVEMENT_LOOP` — the skill's `SKILL.md` entry
//! point, embedded via `include_str!`. Re-exported by `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `self_improvement_loop_skill_is_in_bundle`,
//! `self_improvement_loop_skill_carries_the_moved_anchors`.

/// `self-improvement-loop` skill — on-demand only (issue #7723).
///
/// Why: registration in `ALL` (not just the asset file existing) is what
/// makes `deploy_all_skill_tiers` actually ship it — the historical
/// orphaned-tm-doctor.md bug documented in `bundle_tm_skills.rs`'s module doc.
/// What: embedded markdown skill file deployed to
/// `skills/self-improvement-loop.md`.
/// Test: `bundle_table_is_complete`, `self_improvement_loop_skill_is_in_bundle`.
pub const SELF_IMPROVEMENT_LOOP: &str = include_str!("../assets/skills/self-improvement-loop.md");
