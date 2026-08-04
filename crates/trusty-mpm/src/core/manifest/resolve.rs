//! Precedence resolution for the harness manifest (HR-2 / DOC-17).
//!
//! Why: HR-2 mandates a single NORMATIVE precedence and `prepare_session` must
//! consume the *resolved* manifest. Centralizing the layer reads and the merge
//! order here keeps that precedence in one auditable place and makes it
//! testable without touching the real `~/.trusty-mpm`.
//!
//! #4832 (owner ruling 2026-08-04) narrowed the precedence to
//! **project override > catalog manifest > framework tier > compiled-in
//! default**. The `~/.trusty-mpm/manifest.toml` USER layer is gone: instructions
//! and their manifest are per-project and must always be deployed locally, so
//! there is no user-level manifest surface. This REMOVES a working feature — an
//! operator who kept a home-level `manifest.toml` loses it — and is a ruling,
//! not a bug fix. The project layer also moved from
//! `<project>/.trusty-mpm/manifest.toml` into the `framework/` subdirectory that
//! now holds every project-stable config file.
//! What: [`resolve_manifest`] starts from the compiled-in default
//! ([`super::default::default_manifest`]) and overlays, in ascending precedence,
//! the framework tier, the catalog manifest, then the project-override
//! manifest — each via [`HarnessManifest::merge`]. A missing or malformed layer
//! is logged and skipped (never blocks a launch, per the HR-2 error contract).
//! The canonical layer locations are resolved by [`ManifestSources::resolve`]
//! from a project dir + catalog root.
//! Test: `resolve_uses_default_when_no_layers`, `resolve_project_wins`,
//! `resolve_catalog_over_default`, `resolve_malformed_layer_is_skipped`,
//! `manifest_sources_resolve_canonical_paths`.

use std::path::{Path, PathBuf};

use super::default::default_manifest;
use super::schema::{AgentSet, HarnessManifest};

/// File name of a harness manifest in every layer location.
///
/// Why: the project override, user config, and catalog all use the same file
/// name; naming it once prevents drift.
/// What: `manifest.toml`.
/// Test: `manifest_sources_resolve_canonical_paths`.
pub const MANIFEST_FILE: &str = "manifest.toml";

/// The concrete file paths of the three override layers.
///
/// Why: `prepare_session` (and tests) need to point the resolver at specific
/// files; bundling the three optional paths keeps the resolver signature small
/// and lets tests construct an arbitrary layer set.
/// What: the project-override path (highest precedence) and the catalog path
/// (the compiled-in default and the framework tier sit below both). A `None`
/// field means "that layer is absent". #4832 removed the user layer.
/// Test: `manifest_sources_resolve_canonical_paths`.
#[derive(Debug, Clone, Default)]
pub struct ManifestSources {
    /// `<harness-root>/.trusty-mpm/framework/manifest.toml` — highest precedence.
    pub project: Option<PathBuf>,
    /// `~/.trusty-mpm/catalog/repo/.claude/manifest.toml` — synced catalog.
    pub catalog: Option<PathBuf>,
    /// The framework tier's resolved agent selection (#1941, #4760).
    ///
    /// Why: the bundled `framework-manifest.toml` declares WHICH agents always
    /// deploy and which are gated on a detected language, framework, or
    /// platform; composing that declaration with this project's detected markers
    /// yields the framework-tier selection below every operator-authored layer.
    /// That selection states its gate as an `exclude` list, deliberately — see
    /// [`super::framework::agent_scope_from`] for why an `include` allowlist
    /// would silently drop the `BASE-*` fragments and every catalog-only agent.
    /// [`ManifestSources::resolve`] ALWAYS fills this from
    /// [`super::framework::framework_agent_scope`]; [`resolve_manifest`] applies
    /// it as the lowest override layer (above the compiled default, below the
    /// catalog/user/project manifests) so an explicit `[agents]` override still
    /// wins, per ADR-0025 clause 16. `None` is reachable only by constructing
    /// this struct by hand — it exists so the merge-precedence unit tests below
    /// can isolate a single layer, and means "no framework tier applied".
    pub framework_scope: Option<AgentSet>,
}

impl ManifestSources {
    /// Resolve the canonical layer paths from a project dir and the framework
    /// and catalog roots.
    ///
    /// Why: the layer locations are conventions (DOC-17 §HR-2); resolving them in
    /// one place keeps `prepare_session` from hard-coding the joins and lets the
    /// catalog source stay configurable (the caller passes whatever
    /// `CatalogSync` used as its catalog root).
    /// What: builds
    /// `<harness-root>/.trusty-mpm/framework/manifest.toml` and
    /// `<catalog_root>/repo/.claude/manifest.toml`. Each path is recorded
    /// unconditionally; the resolver tolerates absent files.
    ///
    /// #4832: the project layer moved into `framework/`, the project-stable
    /// config directory, and the `framework_root` parameter went with the user
    /// layer it resolved. The harness root — not `project_dir` — anchors the
    /// project path, so a worktree reads (and an operator edits) the ONE
    /// manifest the whole project shares.
    /// Test: `manifest_sources_resolve_canonical_paths`.
    pub fn resolve(project_dir: &Path, catalog_root: &Path) -> Self {
        Self {
            project: Some(
                crate::core::harness_root::framework_dir(project_dir).join(MANIFEST_FILE),
            ),
            catalog: Some(
                catalog_root
                    .join("repo")
                    .join(".claude")
                    .join(MANIFEST_FILE),
            ),
            framework_scope: Some(super::framework::framework_agent_scope(project_dir)),
        }
    }
}

/// Resolve the effective harness manifest by applying the NORMATIVE precedence.
///
/// Why: this is the single seam that implements HR-2's precedence; every consumer
/// (today `prepare_session`, tomorrow HR-3's staleness check) resolves the
/// manifest through here so the layer order can never drift.
/// What: starts from [`default_manifest`] (the floor) and overlays, lowest-to-
/// highest precedence, the framework tier, the catalog layer, then the project
/// layer. #4832 removed the user layer.
/// Each layer is read via [`read_layer`]; a missing file contributes nothing and
/// a malformed file is logged and skipped (the launch never blocks — HR-2 error
/// contract). The returned manifest always has every section populated because
/// the default floor fills any section no layer overrode.
/// Test: `resolve_uses_default_when_no_layers`, `resolve_project_wins`,
/// `resolve_catalog_over_default`, `resolve_malformed_layer_is_skipped`.
pub fn resolve_manifest(sources: &ManifestSources) -> HarnessManifest {
    let mut manifest = default_manifest();

    // The framework tier (#1941, #4760): the lowest override layer, applied on
    // top of the compiled default but below the catalog/user/project manifests so
    // an explicit `[agents]` override always wins. This is where the bundled
    // `framework-manifest.toml`'s declared always-deploy set — composed with this
    // project's detected language/framework/platform markers — enters resolution.
    if let Some(scope) = &sources.framework_scope {
        manifest = manifest.merge(HarnessManifest {
            agents: Some(scope.clone()),
            ..HarnessManifest::default()
        });
    }

    // Lowest-to-highest precedence: catalog → project. #4832 removed the user
    // layer that sat between them.
    if let Some(path) = &sources.catalog
        && let Some(layer) = read_layer(path, "catalog")
    {
        manifest = manifest.merge(layer);
    }
    if let Some(path) = &sources.project {
        match read_layer(path, "project") {
            Some(layer) => manifest = manifest.merge(layer),
            None => {
                if let Some(legacy) = stranded_legacy_manifest(path) {
                    tracing::warn!(
                        "{} is no longer read (#4832 moved the project manifest layer); \
                         move it to {} for it to take effect",
                        legacy.display(),
                        path.display()
                    );
                }
            }
        }
    }

    manifest
}

/// A pre-#4832 `manifest.toml` sitting unread beside the new location.
///
/// Why: #4832 moved the project layer from `<project>/.trusty-mpm/manifest.toml`
/// into `framework/`, and the file is operator-authored — moving it silently is
/// worse than leaving it, so it is deliberately NOT migrated. But a layer that
/// simply stops being read is invisible: [`read_layer`] logs nothing for an
/// absent file, so an operator's overrides would go dark with no signal at all
/// (#4841 review). Detection is split from the logging so the condition is
/// assertable without capturing a subscriber.
/// What: `Some(legacy_path)` only when the new path is absent AND the legacy
/// sibling (`<new>/../../manifest.toml`) is a file; `None` otherwise — a present
/// new layer means nothing is stranded, whatever else exists.
/// Test: `stranded_legacy_manifest_is_detected`,
/// `stranded_legacy_manifest_is_silent_once_moved`.
fn stranded_legacy_manifest(new_path: &Path) -> Option<PathBuf> {
    if new_path.exists() {
        return None;
    }
    let legacy = new_path
        .parent()
        .and_then(Path::parent)?
        .join(MANIFEST_FILE);
    legacy.is_file().then_some(legacy)
}

/// Read and parse one manifest layer file, tolerating absence and corruption.
///
/// Why: HR-2's error contract says an unresolvable/unreadable layer must never
/// block a launch — a missing file is the common case (no override) and a
/// malformed file must degrade to "skip this layer", not abort.
/// What: returns `Some(manifest)` when the file exists and parses; `None` when it
/// is absent (silent) or malformed (logged at `warn`). `label` names the layer in
/// the warning so operators can find the offending file.
/// Test: `resolve_malformed_layer_is_skipped`, `resolve_project_wins`.
fn read_layer(path: &Path, label: &str) -> Option<HarnessManifest> {
    match std::fs::read_to_string(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(
                "could not read {label} manifest at {}: {err}; skipping layer",
                path.display()
            );
            None
        }
        Ok(raw) => match HarnessManifest::from_toml(&raw) {
            Ok(m) => {
                tracing::debug!("loaded {label} manifest layer from {}", path.display());
                Some(m)
            }
            Err(err) => {
                tracing::warn!(
                    "{label} manifest at {} is malformed: {err}; skipping layer",
                    path.display()
                );
                None
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn manifest_sources_resolve_canonical_paths() {
        // #4832: the project layer lives in `framework/`, and there is no user
        // layer left to resolve.
        let sources =
            ManifestSources::resolve(Path::new("/proj"), Path::new("/home/.trusty-mpm/catalog"));
        assert_eq!(
            sources.project.unwrap(),
            PathBuf::from("/proj/.trusty-mpm/framework/manifest.toml")
        );
        assert_eq!(
            sources.catalog.unwrap(),
            PathBuf::from("/home/.trusty-mpm/catalog/repo/.claude/manifest.toml")
        );
    }

    #[test]
    fn stranded_legacy_manifest_is_detected() {
        // #4841 review: the operator's pre-#4832 file is deliberately not moved
        // for them, so the ONE thing owed is a signal that it stopped being read.
        let tmp = TempDir::new().unwrap();
        let harness = tmp.path().join(".trusty-mpm");
        let legacy = write(&harness, MANIFEST_FILE, "[agents]\n");
        let new_path = harness.join("framework").join(MANIFEST_FILE);

        assert_eq!(
            stranded_legacy_manifest(&new_path),
            Some(legacy),
            "an unread legacy manifest must be detectable so it can be reported"
        );
    }

    #[test]
    fn stranded_legacy_manifest_is_silent_once_moved() {
        // Once the file lives at the new path, the old one (if any) is history —
        // warning on every launch forever would be noise, not a signal.
        let tmp = TempDir::new().unwrap();
        let harness = tmp.path().join(".trusty-mpm");
        write(&harness, MANIFEST_FILE, "[agents]\n");
        let new_path = write(&harness.join("framework"), MANIFEST_FILE, "[agents]\n");

        assert_eq!(stranded_legacy_manifest(&new_path), None);
    }

    #[test]
    fn stranded_legacy_manifest_is_silent_when_there_never_was_one() {
        let tmp = TempDir::new().unwrap();
        let new_path = tmp
            .path()
            .join(".trusty-mpm")
            .join("framework")
            .join(MANIFEST_FILE);

        assert_eq!(stranded_legacy_manifest(&new_path), None);
    }

    #[test]
    fn resolve_ignores_a_user_level_manifest() {
        // #4832 (owner ruling): a `~/.trusty-mpm/manifest.toml` no longer
        // participates. This is a removed feature, asserted so it cannot come
        // back by accident.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        write(
            &home,
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm-teacher\"\n",
        );
        let sources = ManifestSources::resolve(&tmp.path().join("proj"), &tmp.path().join("cat"));
        let m = resolve_manifest(&sources);
        assert_ne!(
            m.style.and_then(|s| s.active),
            Some("trusty-mpm-teacher".to_string()),
            "a user-level manifest must not be read"
        );
    }

    #[test]
    fn resolve_uses_default_when_no_layers() {
        // With every layer pointing at a non-existent file, the resolved
        // manifest must equal the compiled-in default (zero-regression).
        let tmp = TempDir::new().unwrap();
        let sources = ManifestSources {
            project: Some(tmp.path().join("p").join("manifest.toml")),
            catalog: Some(tmp.path().join("c").join("manifest.toml")),
            framework_scope: None,
        };
        assert_eq!(resolve_manifest(&sources), default_manifest());
    }

    #[test]
    fn resolve_project_wins() {
        // The project layer must override the catalog layer for the same
        // section (style here).
        let tmp = TempDir::new().unwrap();
        let catalog = write(
            &tmp.path().join("c"),
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm-research\"\n",
        );
        let project = write(
            &tmp.path().join("p"),
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm\"\n",
        );
        let sources = ManifestSources {
            project: Some(project),
            catalog: Some(catalog),
            framework_scope: None,
        };
        let m = resolve_manifest(&sources);
        assert_eq!(
            m.style.and_then(|s| s.active),
            Some("trusty-mpm".to_string()),
            "project layer must win over catalog"
        );
    }

    #[test]
    fn resolve_catalog_over_default() {
        // The catalog layer must override the compiled-in default when no higher
        // layer is present.
        let tmp = TempDir::new().unwrap();
        let catalog = write(
            &tmp.path().join("c"),
            "manifest.toml",
            "[mcp]\ntrusty_search = false\n",
        );
        let sources = ManifestSources {
            project: None,
            catalog: Some(catalog),
            framework_scope: None,
        };
        let m = resolve_manifest(&sources);
        // The catalog disables search. Because `[mcp]` now merges FIELD-BY-FIELD
        // (not whole-section), the default's `trusty_memory = true` is PRESERVED
        // even though the catalog layer only mentioned `trusty_search`.
        let mcp = m.mcp.expect("mcp section present");
        assert_eq!(mcp.trusty_search, Some(false), "catalog disabled search");
        assert_eq!(
            mcp.trusty_memory,
            Some(true),
            "field-level merge keeps the default's trusty_memory toggle"
        );
    }

    #[test]
    fn resolve_partial_mcp_preserves_other_toggle() {
        // A partial project `[mcp]` override (only trusty_search) must leave the
        // default's trusty_memory toggle intact through the full resolver — the
        // field-level merge regression (whole-section replacement used to null it).
        let tmp = TempDir::new().unwrap();
        let project = write(
            &tmp.path().join("p"),
            "manifest.toml",
            "[mcp]\ntrusty_search = false\n",
        );
        let sources = ManifestSources {
            project: Some(project),
            catalog: None,
            framework_scope: None,
        };
        let mcp = resolve_manifest(&sources).mcp.expect("mcp present");
        assert_eq!(mcp.trusty_search, Some(false));
        assert_eq!(
            mcp.trusty_memory,
            Some(true),
            "project partial [mcp] must not reset the default's trusty_memory"
        );
    }

    #[test]
    fn resolve_malformed_layer_is_skipped() {
        // A malformed layer must be skipped (logged), not abort resolution; the
        // lower layers (and ultimately the default) still apply.
        let tmp = TempDir::new().unwrap();
        let catalog = write(
            &tmp.path().join("c"),
            "manifest.toml",
            "this is not toml {{{",
        );
        let sources = ManifestSources {
            project: None,
            catalog: Some(catalog),
            framework_scope: None,
        };
        // Malformed catalog layer skipped → result equals the default.
        assert_eq!(resolve_manifest(&sources), default_manifest());
    }

    /// Selection outcome for `stem` under a fully resolved manifest.
    ///
    /// Why: the framework tier states its gate as an `exclude` list derived
    /// from the declared categories, so asserting on list SHAPE would pin the
    /// mechanism instead of the behaviour. These tests assert the behaviour:
    /// is this agent selected for deploy?
    fn selected(m: &HarnessManifest, stem: &str) -> bool {
        let agents = m.agents.as_ref().expect("agents section present");
        super::super::schema::selection_matches(stem, &agents.include, &agents.exclude)
    }

    #[test]
    fn resolve_scopes_agents_for_rust_project() {
        // #1941 / #4760: a project whose root has a Cargo.toml (and no manifest
        // overrides) selects rust-engineer, drops the foreign-language engineers,
        // keeps every universal agent, and never selects the deprecated `ops`.
        let proj = TempDir::new().unwrap();
        std::fs::write(proj.path().join("Cargo.toml"), "[package]\n").unwrap();
        let catalog = TempDir::new().unwrap();

        let sources = ManifestSources::resolve(proj.path(), catalog.path());
        assert!(
            sources.framework_scope.is_some(),
            "resolve always applies the framework tier"
        );

        let m = resolve_manifest(&sources);
        assert!(selected(&m, "rust-engineer"), "rust-engineer must survive");
        assert!(
            !selected(&m, "python-engineer"),
            "python-engineer must be dropped from a Rust-only project"
        );
        assert!(selected(&m, "qa"), "universal agents are never stack-gated");
        assert!(
            !selected(&m, "elixir-engineer") && !selected(&m, "phoenix-engineer"),
            "a Rust project gets neither the Elixir nor the Phoenix engineer"
        );
        assert!(
            !selected(&m, "vercel-ops"),
            "a project with no Vercel marker gets no platform agent"
        );
    }

    #[test]
    fn resolve_unknown_project_keeps_every_stack_engineer() {
        // Zero-regression: a project with no recognized stack marker keeps every
        // language and framework engineer, as before #4760. Platform agents have
        // no such fallback, and the deprecated agent stays out either way.
        let proj = TempDir::new().unwrap();
        std::fs::write(proj.path().join("README.md"), "# hi\n").unwrap();
        let catalog = TempDir::new().unwrap();

        let sources = ManifestSources::resolve(proj.path(), catalog.path());
        let m = resolve_manifest(&sources);
        for stem in [
            "rust-engineer",
            "python-engineer",
            "elixir-engineer",
            "react-engineer",
            "phoenix-engineer",
            "tauri-engineer",
            "engineer",
            "qa",
        ] {
            assert!(
                selected(&m, stem),
                "{stem} must deploy to an unknown project"
            );
        }
        assert!(
            !selected(&m, "gcp-ops") && !selected(&m, "vercel-ops"),
            "platform has no unknown-project fallback"
        );
    }

    #[test]
    fn resolve_selects_platform_agent_on_marker() {
        // #4760: a Vercel marker adds vercel-ops, and only vercel-ops.
        let proj = TempDir::new().unwrap();
        std::fs::write(proj.path().join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(proj.path().join("vercel.json"), "{}\n").unwrap();
        let catalog = TempDir::new().unwrap();

        let m = resolve_manifest(&ManifestSources::resolve(proj.path(), catalog.path()));
        assert!(selected(&m, "vercel-ops"));
        assert!(!selected(&m, "gcp-ops"));
    }

    #[test]
    fn project_manifest_agents_override_still_wins() {
        // ADR-0025 clause 16 is preserved: the framework tier is the LOWEST
        // override layer, so a project `[agents]` section still replaces it.
        let tmp = TempDir::new().unwrap();
        let proj_dir = tmp.path().join("p");
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\n").ok();
        let project = write(
            &proj_dir.join(".trusty-mpm"),
            "manifest.toml",
            "[agents]\ninclude = [\"engineer\"]\n",
        );
        let sources = ManifestSources {
            project: Some(project),
            catalog: None,
            framework_scope: Some(super::super::schema::AgentSet {
                include: vec!["qa".to_string()],
                ..Default::default()
            }),
        };
        let m = resolve_manifest(&sources);
        assert!(selected(&m, "engineer"), "project include wins");
        assert!(!selected(&m, "qa"), "framework tier is fully replaced");
    }
}
