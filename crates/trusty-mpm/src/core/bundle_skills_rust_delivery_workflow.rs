//! `rust-delivery-workflow` bundled skill (issue #8192) — the Rust delivery
//! PROCESS rules, as distinct from `rust-build-performance`'s inner-loop speed.
//!
//! Why: the delivery rules a Rust agent needs (commit before the gate chain,
//! match the local toolchain to CI's clippy pin, cap concurrent cargo builds,
//! keep gate output to a verdict, batch installs before live verification)
//! lived only in incident prose and one project's `docs/reference/`. Folding
//! them into `rust-build-performance` would overload a skill whose scope is
//! compile time, so they ship as their own flat, single-file skill declared by
//! `rust-engineer`.
//! What: `pub const RUST_DELIVERY_WORKFLOW` — the skill's `SKILL.md` entry
//! point, embedded via `include_str!`. Re-exported by `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `rust_delivery_workflow_skill_is_in_bundle`.

/// `rust-delivery-workflow` skill — declared by `rust-engineer` in its
/// `skills:` frontmatter (issue #8192).
///
/// Why: a skill asset file existing under `src/assets/skills/` is NOT
/// sufficient for it to ship — it must also be registered as a
/// [`crate::core::bundle::BundledArtifact`] in `ALL`, or
/// `deploy_all_skill_tiers` never sees it (the historical orphaned-tm-doctor.md
/// bug documented in `bundle_tm_skills.rs`'s module doc).
/// What: embedded markdown skill file deployed to
/// `skills/rust-delivery-workflow.md`.
/// Test: `bundle_table_is_complete`, `rust_delivery_workflow_skill_is_in_bundle`.
pub const RUST_DELIVERY_WORKFLOW: &str =
    include_str!("../assets/skills/rust-delivery-workflow.md");
