//! `documentation-style` bundled skill (issue #2911) — SLD-grounded
//! per-artifact-type documentation conventions.
//!
//! Why: `docs/specs/spec-linked-documentation.md` (DOC-38) Annex B names
//! authoring the `spec-linked-docs` (and `spec-authoring`) skills as
//! follow-up F2. This module ships the consolidation of that brief: one
//! bundled skill covering the four-axis Why/What/Test + opt-in Spec
//! References inline model, plus per-artifact-type guides (spec, README,
//! file-level, class, method/function, block/inline) — split into its own
//! file (rather than folded into an existing batch-1 module) to keep the
//! addition isolable and independently reviewable, and to stay well under
//! the 500-SLOC production cap.
//! What: `pub const` strings for the skill's entry `SKILL.md` and its six
//! `references/*.md` files, embedded at compile time via `include_str!`.
//! Re-exported by `bundle.rs`.
//! Test: `bundle_tests.rs` — `bundle_table_is_complete`,
//! `documentation_style_skill_is_in_bundle`,
//! `documentation_style_references_land_on_disk`.

/// `documentation-style` skill entry point (issue #2911).
///
/// Why: registration in [`crate::core::bundle_all::ALL`] (not just the asset
/// file existing under `src/assets/skills/`) is what makes
/// `deploy_all_skill_tiers` actually ship it — the tm-doctor.md orphaning
/// lesson (`bundle_tm_skills.rs`'s module doc) applies here too.
/// What: embedded markdown skill file deployed to
/// `skills/documentation-style.md`.
/// Test: `bundle_table_is_complete`, `documentation_style_skill_is_in_bundle`.
pub const DOCUMENTATION_STYLE: &str = include_str!("../assets/skills/documentation-style.md");

/// `documentation-style` reference file `references/spec.md` (issue #2911).
///
/// Why: progressive disclosure — Claude Code loads this file on demand
/// alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside
/// `documentation-style`'s `SKILL.md` at
/// `skills/documentation-style/references/spec.md`.
/// Test: `bundle_table_is_complete`, `documentation_style_references_land_on_disk`.
pub const DOCUMENTATION_STYLE_SPEC: &str =
    include_str!("../assets/skills/documentation-style/references/spec.md");

/// `documentation-style` reference file `references/readme.md` (issue #2911).
///
/// Why: progressive disclosure — Claude Code loads this file on demand
/// alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside
/// `documentation-style`'s `SKILL.md` at
/// `skills/documentation-style/references/readme.md`.
/// Test: `bundle_table_is_complete`, `documentation_style_references_land_on_disk`.
pub const DOCUMENTATION_STYLE_README: &str =
    include_str!("../assets/skills/documentation-style/references/readme.md");

/// `documentation-style` reference file `references/file-level.md` (issue #2911).
///
/// Why: progressive disclosure — Claude Code loads this file on demand
/// alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside
/// `documentation-style`'s `SKILL.md` at
/// `skills/documentation-style/references/file-level.md`.
/// Test: `bundle_table_is_complete`, `documentation_style_references_land_on_disk`.
pub const DOCUMENTATION_STYLE_FILE_LEVEL: &str =
    include_str!("../assets/skills/documentation-style/references/file-level.md");

/// `documentation-style` reference file `references/class.md` (issue #2911).
///
/// Why: progressive disclosure — Claude Code loads this file on demand
/// alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside
/// `documentation-style`'s `SKILL.md` at
/// `skills/documentation-style/references/class.md`.
/// Test: `bundle_table_is_complete`, `documentation_style_references_land_on_disk`.
pub const DOCUMENTATION_STYLE_CLASS: &str =
    include_str!("../assets/skills/documentation-style/references/class.md");

/// `documentation-style` reference file `references/method-function.md`
/// (issue #2911).
///
/// Why: progressive disclosure — Claude Code loads this file on demand
/// alongside the entry-point `SKILL.md`, not up front. Method and function
/// guidance ship as one file (not two) — their documentation contract is
/// identical, differing only in whether the callable is bound to a type; see
/// the PR description for this call.
/// What: embedded markdown reference file deployed alongside
/// `documentation-style`'s `SKILL.md` at
/// `skills/documentation-style/references/method-function.md`.
/// Test: `bundle_table_is_complete`, `documentation_style_references_land_on_disk`.
pub const DOCUMENTATION_STYLE_METHOD_FUNCTION: &str =
    include_str!("../assets/skills/documentation-style/references/method-function.md");

/// `documentation-style` reference file `references/block-inline.md`
/// (issue #2911).
///
/// Why: progressive disclosure — Claude Code loads this file on demand
/// alongside the entry-point `SKILL.md`, not up front.
/// What: embedded markdown reference file deployed alongside
/// `documentation-style`'s `SKILL.md` at
/// `skills/documentation-style/references/block-inline.md`.
/// Test: `bundle_table_is_complete`, `documentation_style_references_land_on_disk`.
pub const DOCUMENTATION_STYLE_BLOCK_INLINE: &str =
    include_str!("../assets/skills/documentation-style/references/block-inline.md");
