//! The bundled FRAMEWORK-TIER manifest — the declared always-deploy set (#4760).
//!
//! Why: before this module, "which agents always deploy" was not declared
//! anywhere. It was COMPUTED, as the complement of `LANGUAGE_ENGINEERS`:
//! `language_agent_scope` emitted an `exclude` list of the language engineers a
//! project did not match, and everything else deployed by virtue of not being in
//! that one table. Nothing could be retired, because there was no list to remove
//! it from — `ops` has been marked DEPRECATED in `agent-delegation.md` prose
//! since long before this file, with zero code-level effect, and still reached
//! every roster. A declared catalog makes deprecation an edit rather than a wish.
//! What: [`FRAMEWORK_MANIFEST_TOML`] embeds `assets/framework-manifest.toml` —
//! a `HarnessManifest` document in the SAME format, parsed by the SAME
//! [`HarnessManifest::from_toml`], differing only in tier. Its
//! `[agent_categories]` section partitions the bundled catalog into `universal`
//! (no detection), `language` / `framework` / `platform` (marker-gated, each
//! entry carrying its OWN markers since #4765), and `deprecated` (never
//! deployed); its `[skill_categories]` section declares the bundled skill
//! roster. [`parse_framework_manifest`] and [`parse_framework_skills`] validate
//! those declarations against the bundled catalogs; [`agent_scope_from`]
//! composes the agent side with `project_lang`'s marker evaluation into the
//! [`AgentSet`] `ManifestSources::resolve` applies as its lowest override layer.
//!
//! **One authority.** Owner ruling 2026-08-04: "authoritative agent/skill
//! bundling should be in framework-manifest.toml or project manifest.toml.
//! That's it." Membership, category, and gate condition are all manifest fields.
//! No bundled document restates them; `assets/skills/tm.md` and
//! `assets/instructions/sections/agent-delegation.md` point at the manifest, and
//! `tm generate capabilities` renders it mechanically.
//!
//! **Naming caution.** ADR-0025's "four-category agent model" (universal
//! bundled / stack-specific bundled / project custom / user custom) is a
//! different axis from these four DEPLOYMENT categories. ADR-0025 classifies by
//! WHO AUTHORED an agent and WHERE it lives; this file classifies, within
//! ADR-0025's bundled categories 1–2 only, by WHAT GATES its deployment. The two
//! are orthogonal: every stem named in `framework-manifest.toml` is an ADR-0025
//! category-1 or category-2 agent, and this file says nothing about ADR-0025's
//! categories 3–4 (operator-authored agents), which no manifest declares.
//! Test: `crates/trusty-mpm/src/core/manifest/framework_tests.rs`.

use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use super::project_lang::MarkerProbe;
use super::schema::{
    AgentCategories, AgentSet, ContentSource, GatedAgent, HarnessManifest, SkillCategories,
};

/// File name of the bundled framework-tier manifest.
///
/// Why: named once so the error messages, the docs, and any future on-disk
/// materialisation cannot drift.
/// What: `framework-manifest.toml`.
/// Test: `bundled_framework_manifest_is_valid`.
pub const FRAMEWORK_MANIFEST_FILE: &str = "framework-manifest.toml";

/// The bundled framework manifest, embedded at compile time.
///
/// Why: this is the framework's own catalog declaration, not operator input. It
/// travels with the binary so a binary-only install has it, and so a MISSING
/// manifest is a compile error — the loudest failure available — rather than a
/// runtime condition anything could paper over with a fallback.
/// What: the contents of `crates/trusty-mpm/src/assets/framework-manifest.toml`.
/// Test: `bundled_framework_manifest_is_valid`.
const FRAMEWORK_MANIFEST_TOML: &str = include_str!("../../assets/framework-manifest.toml");

/// Why the bundled framework manifest could not be used.
///
/// Why: requirement #4760.4 — a missing or malformed manifest must fail LOUDLY,
/// never silently fall back to deploying everything and never silently deploy
/// nothing. Every failure mode is a distinct variant so the operator-facing
/// message names the actual defect, and so tests can pin each one. Crucially,
/// NONE of these variants is reachable from "the project matched no marker":
/// that is an ordinary `Ok` with an empty detected set, on a different code path.
/// What: parse failure, a missing/empty section, and each partition-invariant
/// violation.
/// Test: `malformed_manifest_is_an_error`, `missing_section_is_an_error`,
/// `empty_universal_is_an_error`, `unknown_stem_is_an_error`,
/// `undeclared_bundled_agent_is_an_error`, `duplicate_declaration_is_an_error`,
/// `gated_entry_without_markers_is_an_error`,
/// `missing_skill_section_is_an_error`, `unknown_skill_is_an_error`,
/// `undeclared_bundled_skill_is_an_error`, `duplicate_skill_is_an_error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameworkManifestError {
    /// The document is not valid TOML, or does not match the manifest schema.
    #[error("{FRAMEWORK_MANIFEST_FILE} is not a valid manifest document: {0}")]
    Malformed(String),

    /// The document parsed but carries no `[agent_categories]` section.
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} has no [agent_categories] section — \
         the always-deploy set is undeclared"
    )]
    MissingCategories,

    /// `universal` is empty, which would deploy nothing to a project with no
    /// recognised markers.
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} declares an empty `universal` list — \
         that would deploy no agents at all"
    )]
    EmptyUniversal,

    /// A declared stem has no corresponding bundled agent file.
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} declares `{stem}` under `{category}`, \
         but no bundled agent by that name exists"
    )]
    UnknownAgent {
        /// The category list the unknown stem appeared in.
        category: &'static str,
        /// The offending stem.
        stem: String,
    },

    /// A bundled agent is not declared in any category.
    #[error(
        "bundled agent(s) {0:?} are not declared in {FRAMEWORK_MANIFEST_FILE} — \
         add each to exactly one category (use `deprecated` to retire one)"
    )]
    UndeclaredAgents(Vec<String>),

    /// A stem appears in more than one category.
    #[error("{FRAMEWORK_MANIFEST_FILE} declares `{0}` in more than one category")]
    DuplicateDeclaration(String),

    /// A gated entry declares no markers, so it could never be detected and
    /// would never deploy (#4765 — markers are a manifest field now, so the
    /// "gated but ungated" state is representable and must be rejected here).
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} gates `{stem}` on `{category}` detection \
         but declares no markers — it could never deploy"
    )]
    UngatedEntry {
        /// `language`, `framework`, or `platform`.
        category: &'static str,
        /// The offending stem.
        stem: String,
    },

    /// The document parsed but carries no `[skill_categories]` section.
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} has no [skill_categories] section — \
         the bundled skill roster is undeclared"
    )]
    MissingSkillCategories,

    /// A declared skill stem has no corresponding bundled skill file.
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} declares skill `{0}`, \
         but no bundled skill by that name exists"
    )]
    UnknownSkill(String),

    /// A bundled skill is not declared.
    #[error(
        "bundled skill(s) {0:?} are not declared in {FRAMEWORK_MANIFEST_FILE} — \
         add each to [skill_categories]"
    )]
    UndeclaredSkills(Vec<String>),

    /// A skill stem is declared more than once.
    #[error("{FRAMEWORK_MANIFEST_FILE} declares skill `{0}` more than once")]
    DuplicateSkill(String),
}

/// Every bundled agent stem the binary can actually deploy.
///
/// Why: the partition invariant is only meaningful against the real catalog.
/// Deriving the catalog from `core::bundle::ALL` — the same embedded artifact
/// table the installer writes — means a newly added agent that nobody declared
/// fails the invariant loudly instead of quietly never deploying.
/// What: the `agents/<stem>.md` entries of [`crate::core::bundle::ALL`], minus
/// the `BASE-*` foundation templates, which are inheritance fragments rather
/// than dispatchable agents and are never selected by name.
/// Test: `bundled_agent_stems_excludes_foundations`.
pub fn bundled_agent_stems() -> BTreeSet<String> {
    crate::core::bundle::ALL
        .iter()
        .filter_map(|artifact| {
            let file_name = artifact.rel_path.strip_prefix("agents/")?;
            let stem = file_name.strip_suffix(".md")?;
            if crate::core::delegation_authority::is_foundation_file(stem) {
                return None;
            }
            Some(stem.to_string())
        })
        .collect()
}

/// Parse and fully validate a framework-tier manifest document.
///
/// Why: this is the single place the always-deploy declaration is checked, and
/// it is deliberately a pure function over `(raw, catalog)` so every failure
/// mode is testable without touching the shipped asset. Requirement #4760.4
/// lives here: it returns `Err` — never a permissive default — for a malformed
/// document, a missing section, an empty `universal`, or any violation of the
/// partition invariant.
/// What: parses `raw` with the shared [`HarnessManifest::from_toml`], takes its
/// `[agent_categories]`, and verifies that the five lists (a) name only stems
/// present in `catalog`, (b) name each catalog stem exactly once, (c) never
/// repeat a stem across lists, (d) gate only on markers that exist, and (e)
/// leave `universal` non-empty.
/// Test: every `*_is_an_error` test, plus `bundled_framework_manifest_is_valid`.
pub fn parse_framework_manifest(
    raw: &str,
    catalog: &BTreeSet<String>,
) -> Result<AgentCategories, FrameworkManifestError> {
    let manifest = HarnessManifest::from_toml(raw)
        .map_err(|err| FrameworkManifestError::Malformed(err.to_string()))?;
    let categories = manifest
        .agent_categories
        .ok_or(FrameworkManifestError::MissingCategories)?;

    if categories.universal.is_empty() {
        return Err(FrameworkManifestError::EmptyUniversal);
    }

    // (a) + (b) + (c): the five lists must exactly partition `catalog`.
    let mut declared: HashSet<&str> = HashSet::new();
    for (label, stem) in labelled_stems(&categories) {
        if !catalog.contains(stem) {
            return Err(FrameworkManifestError::UnknownAgent {
                category: label,
                stem: stem.clone(),
            });
        }
        if !declared.insert(stem.as_str()) {
            return Err(FrameworkManifestError::DuplicateDeclaration(stem.clone()));
        }
    }
    let undeclared: Vec<String> = catalog
        .iter()
        .filter(|stem| !declared.contains(stem.as_str()))
        .cloned()
        .collect();
    if !undeclared.is_empty() {
        return Err(FrameworkManifestError::UndeclaredAgents(undeclared));
    }

    // (d): a gated entry with no markers could never deploy.
    for (label, list) in gated_lists(&categories) {
        for entry in list {
            if entry.markers.is_empty() {
                return Err(FrameworkManifestError::UngatedEntry {
                    category: label,
                    stem: entry.stem.clone(),
                });
            }
        }
    }

    Ok(categories)
}

/// Every declared stem paired with the TOML key that declared it.
///
/// Why: the partition passes must walk ALL five lists — the two plain stem lists
/// and the three gated ones — and naming them once keeps a newly added category
/// from being silently skipped by one pass.
/// What: `(key, stem)` pairs, in declaration order.
/// Test: covered by every partition-invariant test.
fn labelled_stems(categories: &AgentCategories) -> Vec<(&'static str, &String)> {
    let mut out: Vec<(&'static str, &String)> = categories
        .universal
        .iter()
        .map(|stem| ("universal", stem))
        .collect();
    for (label, list) in gated_lists(categories) {
        out.extend(list.iter().map(|entry| (label, &entry.stem)));
    }
    out.extend(
        categories
            .deprecated
            .iter()
            .map(|stem| ("deprecated", stem)),
    );
    out
}

/// The three marker-gated category lists paired with their TOML key.
///
/// Why/What/Test: as [`labelled_stems`], for the passes that need the markers
/// rather than only the stem.
fn gated_lists(categories: &AgentCategories) -> [(&'static str, &Vec<GatedAgent>); 3] {
    [
        ("language", &categories.language),
        ("framework", &categories.framework),
        ("platform", &categories.platform),
    ]
}

/// Every bundled skill stem the binary can actually deploy.
///
/// Why: the skill roster gets the same guarantee the agent roster does — a
/// bundled skill nobody declared is a hard error, not a silent default.
/// What: the top-level `skills/<stem>.md` entries of [`crate::core::bundle::ALL`].
/// A nested `skills/<name>/references/*.md` is a skill's own reference material,
/// not a catalog entry, and is excluded — the same predicate
/// `tm generate capabilities` uses for its skill count.
/// Test: `bundled_skill_stems_excludes_reference_files`.
pub fn bundled_skill_stems() -> BTreeSet<String> {
    crate::core::bundle::ALL
        .iter()
        .filter_map(|artifact| {
            let rest = artifact.rel_path.strip_prefix("skills/")?;
            if rest.contains('/') {
                return None;
            }
            Some(rest.strip_suffix(".md")?.to_string())
        })
        .collect()
}

/// Parse and fully validate a framework-tier manifest's SKILL roster (#4765).
///
/// Why: the owner ruling makes `framework-manifest.toml` the authority for skill
/// bundling as well as agent bundling. Validating exhaustively against the real
/// bundle is what makes the declaration load-bearing rather than decorative:
/// adding a `skills/*.md` row to `bundle_all.rs` without declaring it here fails
/// the build's test gate.
/// What: pure over `(raw, catalog)`, like [`parse_framework_manifest`]. Returns
/// `Err` for a malformed document, a missing `[skill_categories]` section, an
/// unknown stem, a duplicate, or any bundled skill left undeclared.
/// Test: `bundled_skill_roster_is_valid`, `missing_skill_section_is_an_error`,
/// `unknown_skill_is_an_error`, `undeclared_bundled_skill_is_an_error`,
/// `duplicate_skill_is_an_error`.
pub fn parse_framework_skills(
    raw: &str,
    catalog: &BTreeSet<String>,
) -> Result<SkillCategories, FrameworkManifestError> {
    let manifest = HarnessManifest::from_toml(raw)
        .map_err(|err| FrameworkManifestError::Malformed(err.to_string()))?;
    let skills = manifest
        .skill_categories
        .ok_or(FrameworkManifestError::MissingSkillCategories)?;

    let mut declared: HashSet<&str> = HashSet::new();
    for stem in &skills.universal {
        if !catalog.contains(stem) {
            return Err(FrameworkManifestError::UnknownSkill(stem.clone()));
        }
        if !declared.insert(stem.as_str()) {
            return Err(FrameworkManifestError::DuplicateSkill(stem.clone()));
        }
    }
    let undeclared: Vec<String> = catalog
        .iter()
        .filter(|stem| !declared.contains(stem.as_str()))
        .cloned()
        .collect();
    if !undeclared.is_empty() {
        return Err(FrameworkManifestError::UndeclaredSkills(undeclared));
    }

    Ok(skills)
}

/// The validated skill roster declared by the bundled framework manifest.
///
/// Why/What/Test: [`parse_framework_skills`] over [`FRAMEWORK_MANIFEST_TOML`]
/// and the real [`bundled_skill_stems`] catalog. Test:
/// `bundled_skill_roster_is_valid`.
pub fn framework_skill_categories() -> Result<SkillCategories, FrameworkManifestError> {
    parse_framework_skills(FRAMEWORK_MANIFEST_TOML, &bundled_skill_stems())
}

/// The validated categories declared by the bundled framework manifest.
///
/// Why/What/Test: [`parse_framework_manifest`] over [`FRAMEWORK_MANIFEST_TOML`]
/// and the real [`bundled_agent_stems`] catalog. Test:
/// `bundled_framework_manifest_is_valid`.
pub fn framework_agent_categories() -> Result<AgentCategories, FrameworkManifestError> {
    parse_framework_manifest(FRAMEWORK_MANIFEST_TOML, &bundled_agent_stems())
}

/// Compose declared categories with detected markers into a deploy selection.
///
/// Why: this is the single, auditable statement of how the always-deploy set and
/// the marker-gated sets combine. Keeping it a pure function of
/// `(categories, project_dir)` means the composition can be tested against
/// synthetic categories, so a test that a deprecated agent does not deploy
/// cannot pass merely because the real manifest happens to omit it.
/// What: builds an [`AgentSet`] that leaves `include` empty (no allowlist) and
/// states the gate as an explicit `exclude`, derived ENTIRELY from the declared
/// categories:
///
/// ```text
/// exclude = deprecated
///         u ((language u framework) \ detected stacks)
///         u (platform \ detected platforms)
/// ```
///
/// A `universal` stem appears in no term, so nothing can exclude it — that is
/// what makes the manifest the source of truth for the always-deploy set.
///
/// Two deliberate asymmetries:
///
/// * **Unknown project type** (no stack marker recognised at all — e.g. the
///   daemon's own framework root): the stack term contributes nothing, so every
///   `language` and `framework` engineer still deploys, exactly as
///   `language_agent_scope` behaved before #4760. Zero regression.
/// * **Platform has no such fallback.** A project with no platform marker
///   excludes every `platform` stem. That is the intended behaviour change
///   #4760 carries: `gcp-ops`/`vercel-ops` used to deploy everywhere.
///
/// **Why an exclusion and not an `include` allowlist.** An allowlist naming only
/// the bundled stems would silently drop anything the SOURCE DIRECTORY carries
/// that the manifest does not name — the five `BASE-*` inheritance fragments,
/// and every agent a `ContentSource::Catalog` checkout adds. Deploying less than
/// asked, silently, is the failure class this work exists to remove, not one to
/// introduce. The exhaustiveness invariant in [`parse_framework_manifest`] buys
/// what the allowlist would have: a bundled agent missing from the manifest is a
/// hard error, so it can never be forgotten into deploying.
/// Test: `universal_agents_deploy_with_no_markers`,
/// `language_engineers_still_gate_on_detection`,
/// `deprecated_agent_never_deploys`, `platform_agent_absent_without_marker`,
/// `platform_agent_deploys_with_marker`,
/// `unknown_project_keeps_every_stack_engineer`,
/// `undeclared_source_agents_still_deploy`.
pub fn agent_scope_from(categories: &AgentCategories, project_dir: &Path) -> AgentSet {
    // One probe — so the stack and platform questions share ONE read budget and
    // one workspace-member resolution, not two of each.
    let probe = MarkerProbe::new(project_dir);
    let mut stacks = probe.detect(&categories.language);
    stacks.extend(probe.detect(&categories.framework));
    let platforms = probe.detect(&categories.platform);

    // Declared-as-retired: excluded unconditionally, for every project.
    let mut exclude: BTreeSet<String> = categories.deprecated.iter().cloned().collect();

    // Stack-gated: drop the ones this project's markers did not select. With NO
    // marker at all, drop none — an unknown project keeps the full roster.
    if !stacks.is_empty() {
        exclude.extend(
            categories
                .language
                .iter()
                .chain(categories.framework.iter())
                .filter(|entry| !stacks.contains(&entry.stem))
                .map(|entry| entry.stem.clone()),
        );
    }

    // Platform-gated: no fallback — an undetected platform is always dropped.
    exclude.extend(
        categories
            .platform
            .iter()
            .filter(|entry| !platforms.contains(&entry.stem))
            .map(|entry| entry.stem.clone()),
    );

    AgentSet {
        include: Vec::new(),
        exclude: exclude.into_iter().collect(),
        ignore_staleness: Vec::new(),
        source: ContentSource::Bundled,
    }
}

/// The stack engineers `project_dir`'s markers select, per the bundled manifest.
///
/// Why: `core::stack_profile` primes the PM prompt with the project's actual
/// stack and must read the SAME declaration the deployer reads — a prompt that
/// names `rust-engineer` for a project that never received it is the drift #1971
/// exists to prevent. Before #4765 it imported `project_lang`'s marker table
/// directly; that table is now a manifest field, so this is the entry point.
/// What: the union of the declared `language` and `framework` stems whose
/// markers are present. An unusable manifest yields an EMPTY set, which
/// `stack_profile_section` renders as its neutral "detect before routing"
/// block — the safe answer for a prompt. The DEPLOY path does not share that
/// leniency: [`framework_agent_scope`] refuses to resolve at all.
/// Test: `detected_stack_engineers_matches_the_manifest`,
/// `core::stack_profile::tests::detected_rust_lists_rust_engineer`.
pub fn detected_stack_engineers(project_dir: &Path) -> BTreeSet<String> {
    let Ok(categories) = framework_agent_categories() else {
        return BTreeSet::new();
    };
    let probe = MarkerProbe::new(project_dir);
    let mut detected = probe.detect(&categories.language);
    detected.extend(probe.detect(&categories.framework));
    detected
}

/// The framework-tier agent selection for `project_dir`.
///
/// Why: the one entry point `ManifestSources::resolve` calls.
///
/// **Panics** — deliberately, per requirement #4760.4 — when the bundled
/// manifest is unusable. [`FRAMEWORK_MANIFEST_TOML`] is a compile-time constant,
/// so an `Err` here means the SHIPPED asset is corrupt: a programmer error and a
/// build-integrity failure, not operator input, which is exactly the case this
/// project's "no `unwrap()` in library code" rule reserves `expect`-style aborts
/// for. The alternatives are both forbidden by the requirement — returning an
/// empty selection deploys nothing, and returning the permissive default deploys
/// everything, each silently. It is unreachable in a valid build:
/// `bundled_framework_manifest_is_valid` fails the test suite before merge if
/// the asset ever stops validating.
/// What: [`framework_agent_categories`] then [`agent_scope_from`].
/// Test: `bundled_framework_manifest_is_valid` (the guard),
/// `framework_agent_scope_selects_universal_agents`.
pub fn framework_agent_scope(project_dir: &Path) -> AgentSet {
    match framework_agent_categories() {
        Ok(categories) => agent_scope_from(&categories, project_dir),
        Err(err) => {
            tracing::error!("bundled {FRAMEWORK_MANIFEST_FILE} is unusable: {err}");
            panic!(
                "bundled {FRAMEWORK_MANIFEST_FILE} is unusable and no agent set can be \
                 resolved from it: {err}"
            );
        }
    }
}

#[cfg(test)]
#[path = "framework_tests.rs"]
mod tests;
