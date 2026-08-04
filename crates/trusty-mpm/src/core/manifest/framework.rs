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
//! (no detection), `language` / `framework` / `platform` (marker-gated), and
//! `deprecated` (never deployed). [`parse_framework_manifest`] validates that
//! partition against the bundled catalog and the marker tables;
//! [`agent_scope_from`] composes it with `project_lang`'s detection into the
//! [`AgentSet`] `ManifestSources::resolve` applies as its lowest override layer.
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

use super::project_lang::{
    detected_engineers, detected_platforms, language_engineer_stems, platform_agent_stems,
};
use super::schema::{AgentCategories, AgentSet, ContentSource, HarnessManifest};

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
/// `stack_stem_without_markers_is_an_error`,
/// `platform_stem_without_markers_is_an_error`.
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

    /// A `language`/`framework` stem has no row in `LANGUAGE_ENGINEERS`, so it
    /// could never be detected and would never deploy.
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} gates `{stem}` on `{category}` detection, \
         but it has no marker row in LANGUAGE_ENGINEERS — it could never deploy"
    )]
    UndetectableStack {
        /// `language` or `framework`.
        category: &'static str,
        /// The offending stem.
        stem: String,
    },

    /// A `platform` stem has no row in `PLATFORM_AGENTS`.
    #[error(
        "{FRAMEWORK_MANIFEST_FILE} gates `{0}` on platform detection, \
         but it has no marker row in PLATFORM_AGENTS — it could never deploy"
    )]
    UndetectablePlatform(String),
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
    for (label, list) in labelled_lists(&categories) {
        for stem in list {
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
    }
    let undeclared: Vec<String> = catalog
        .iter()
        .filter(|stem| !declared.contains(stem.as_str()))
        .cloned()
        .collect();
    if !undeclared.is_empty() {
        return Err(FrameworkManifestError::UndeclaredAgents(undeclared));
    }

    // (d): a gated stem with no marker row could never deploy.
    let stack_markers = language_engineer_stems();
    for (label, list) in [
        ("language", &categories.language),
        ("framework", &categories.framework),
    ] {
        for stem in list {
            if !stack_markers.contains(stem.as_str()) {
                return Err(FrameworkManifestError::UndetectableStack {
                    category: label,
                    stem: stem.clone(),
                });
            }
        }
    }
    let platform_markers = platform_agent_stems();
    for stem in &categories.platform {
        if !platform_markers.contains(stem.as_str()) {
            return Err(FrameworkManifestError::UndetectablePlatform(stem.clone()));
        }
    }

    Ok(categories)
}

/// The five category lists paired with their TOML key, in declaration order.
///
/// Why: three validation passes iterate the same five lists; naming them once
/// keeps a newly added category from being silently skipped by one pass.
/// What: `(key, list)` pairs for `universal`, `language`, `framework`,
/// `platform`, `deprecated`.
/// Test: covered by every partition-invariant test.
fn labelled_lists(categories: &AgentCategories) -> [(&'static str, &Vec<String>); 5] {
    [
        ("universal", &categories.universal),
        ("language", &categories.language),
        ("framework", &categories.framework),
        ("platform", &categories.platform),
        ("deprecated", &categories.deprecated),
    ]
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
    let stacks = detected_engineers(project_dir);
    let platforms = detected_platforms(project_dir);

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
                .filter(|stem| !stacks.contains(stem.as_str()))
                .cloned(),
        );
    }

    // Platform-gated: no fallback — an undetected platform is always dropped.
    exclude.extend(
        categories
            .platform
            .iter()
            .filter(|stem| !platforms.contains(stem.as_str()))
            .cloned(),
    );

    AgentSet {
        include: Vec::new(),
        exclude: exclude.into_iter().collect(),
        ignore_staleness: Vec::new(),
        source: ContentSource::Bundled,
    }
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
