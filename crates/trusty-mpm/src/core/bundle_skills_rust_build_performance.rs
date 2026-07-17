//! `rust-build-performance` bundled skill (per Bob directive 2026-07-17) —
//! dev-loop + dependency-graph Rust build hygiene.
//!
//! Why: Rust build-performance guidance (cargo check inner loop, `--timings`
//! measurement, dependency/feature graph trimming, incremental-compilation
//! preservation, sccache) was previously undocumented for the Rust-family
//! agents (`rust-engineer`, `tauri-engineer`), so each one either invented ad
//! hoc advice or skipped the topic. A single bundled skill both agents
//! declare in `skills:` keeps the guidance in one place. Flat, single-file
//! skill — small enough (~200 lines) that no `references/` split is needed,
//! unlike the multi-file skills (`documentation-style`, `software-patterns`)
//! elsewhere in this bundle.
//! What: `pub const RUST_BUILD_PERFORMANCE` — the skill's `SKILL.md` entry
//! point, embedded via `include_str!`. Re-exported by `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `rust_build_performance_skill_is_in_bundle`.

/// `rust-build-performance` skill — declared by `rust-engineer` and
/// `tauri-engineer` in their `skills:` frontmatter (per Bob directive
/// 2026-07-17).
///
/// Why: a skill asset file existing under `src/assets/skills/` is NOT
/// sufficient for it to ship — it must also be registered as a
/// [`crate::core::bundle::BundledArtifact`] in `ALL`, or
/// `deploy_all_skill_tiers` never sees it (the historical orphaned-tm-doctor.md
/// bug documented in `bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to
/// `skills/rust-build-performance.md`.
/// Test: `bundle_table_is_complete`, `rust_build_performance_skill_is_in_bundle`.
pub const RUST_BUILD_PERFORMANCE: &str = include_str!("../assets/skills/rust-build-performance.md");
