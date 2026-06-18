//! Precedence resolution for the harness manifest (HR-2 / DOC-17).
//!
//! Why: HR-2 mandates a single NORMATIVE precedence —
//! **project override > user config > catalog manifest > compiled-in default** —
//! and `prepare_session` must consume the *resolved* manifest. Centralizing the
//! layer reads and the merge order here keeps that precedence in one auditable
//! place and makes it testable without touching the real `~/.trusty-mpm`.
//! What: [`resolve_manifest`] starts from the compiled-in default
//! ([`super::default::default_manifest`]) and overlays, in ascending precedence,
//! the catalog manifest, the user-config manifest, then the project-override
//! manifest — each via [`HarnessManifest::merge`]. A missing or malformed layer
//! is logged and skipped (never blocks a launch, per the HR-2 error contract).
//! The canonical layer locations are resolved by [`ManifestSources::resolve`]
//! from a project dir + framework root + catalog root.
//! Test: `resolve_uses_default_when_no_layers`, `resolve_project_wins`,
//! `resolve_user_over_catalog`, `resolve_malformed_layer_is_skipped`.

use std::path::{Path, PathBuf};

use super::default::default_manifest;
use super::schema::HarnessManifest;

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
/// What: the project-override path (highest precedence), the user-config path,
/// and the catalog path (lowest of the three; the compiled-in default sits below
/// all of them). A `None` field means "that layer is absent".
/// Test: `manifest_sources_resolve_canonical_paths`.
#[derive(Debug, Clone, Default)]
pub struct ManifestSources {
    /// `<project>/.trusty-mpm/manifest.toml` — highest precedence.
    pub project: Option<PathBuf>,
    /// `~/.trusty-mpm/manifest.toml` — user config.
    pub user: Option<PathBuf>,
    /// `~/.trusty-mpm/catalog/repo/.claude/manifest.toml` — synced catalog.
    pub catalog: Option<PathBuf>,
}

impl ManifestSources {
    /// Resolve the canonical layer paths from a project dir and the framework
    /// and catalog roots.
    ///
    /// Why: the layer locations are conventions (DOC-17 §HR-2); resolving them in
    /// one place keeps `prepare_session` from hard-coding the joins and lets the
    /// catalog source stay configurable (the caller passes whatever
    /// `CatalogSync` used as its catalog root).
    /// What: builds `<project>/.trusty-mpm/manifest.toml`,
    /// `<framework_root>/manifest.toml`, and
    /// `<catalog_root>/repo/.claude/manifest.toml`. Each path is recorded
    /// unconditionally; the resolver tolerates absent files.
    /// Test: `manifest_sources_resolve_canonical_paths`.
    pub fn resolve(project_dir: &Path, framework_root: &Path, catalog_root: &Path) -> Self {
        Self {
            project: Some(project_dir.join(".trusty-mpm").join(MANIFEST_FILE)),
            user: Some(framework_root.join(MANIFEST_FILE)),
            catalog: Some(
                catalog_root
                    .join("repo")
                    .join(".claude")
                    .join(MANIFEST_FILE),
            ),
        }
    }
}

/// Resolve the effective harness manifest by applying the NORMATIVE precedence.
///
/// Why: this is the single seam that implements HR-2's precedence; every consumer
/// (today `prepare_session`, tomorrow HR-3's staleness check) resolves the
/// manifest through here so the layer order can never drift.
/// What: starts from [`default_manifest`] (the floor) and overlays, lowest-to-
/// highest precedence, the catalog layer, the user layer, then the project layer.
/// Each layer is read via [`read_layer`]; a missing file contributes nothing and
/// a malformed file is logged and skipped (the launch never blocks — HR-2 error
/// contract). The returned manifest always has every section populated because
/// the default floor fills any section no layer overrode.
/// Test: `resolve_uses_default_when_no_layers`, `resolve_project_wins`,
/// `resolve_user_over_catalog`, `resolve_malformed_layer_is_skipped`.
pub fn resolve_manifest(sources: &ManifestSources) -> HarnessManifest {
    let mut manifest = default_manifest();

    // Lowest-to-highest precedence: catalog → user → project.
    if let Some(path) = &sources.catalog
        && let Some(layer) = read_layer(path, "catalog")
    {
        manifest = manifest.merge(layer);
    }
    if let Some(path) = &sources.user
        && let Some(layer) = read_layer(path, "user")
    {
        manifest = manifest.merge(layer);
    }
    if let Some(path) = &sources.project
        && let Some(layer) = read_layer(path, "project")
    {
        manifest = manifest.merge(layer);
    }

    manifest
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
        let sources = ManifestSources::resolve(
            Path::new("/proj"),
            Path::new("/home/.trusty-mpm"),
            Path::new("/home/.trusty-mpm/catalog"),
        );
        assert_eq!(
            sources.project.unwrap(),
            PathBuf::from("/proj/.trusty-mpm/manifest.toml")
        );
        assert_eq!(
            sources.user.unwrap(),
            PathBuf::from("/home/.trusty-mpm/manifest.toml")
        );
        assert_eq!(
            sources.catalog.unwrap(),
            PathBuf::from("/home/.trusty-mpm/catalog/repo/.claude/manifest.toml")
        );
    }

    #[test]
    fn resolve_uses_default_when_no_layers() {
        // With every layer pointing at a non-existent file, the resolved
        // manifest must equal the compiled-in default (zero-regression).
        let tmp = TempDir::new().unwrap();
        let sources = ManifestSources {
            project: Some(tmp.path().join("p").join("manifest.toml")),
            user: Some(tmp.path().join("u").join("manifest.toml")),
            catalog: Some(tmp.path().join("c").join("manifest.toml")),
        };
        assert_eq!(resolve_manifest(&sources), default_manifest());
    }

    #[test]
    fn resolve_project_wins() {
        // The project layer must override the user and catalog layers for the
        // same section (style here).
        let tmp = TempDir::new().unwrap();
        let catalog = write(
            &tmp.path().join("c"),
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm-research\"\n",
        );
        let user = write(
            &tmp.path().join("u"),
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm-teacher\"\n",
        );
        let project = write(
            &tmp.path().join("p"),
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm\"\n",
        );
        let sources = ManifestSources {
            project: Some(project),
            user: Some(user),
            catalog: Some(catalog),
        };
        let m = resolve_manifest(&sources);
        assert_eq!(
            m.style.and_then(|s| s.active),
            Some("trusty-mpm".to_string()),
            "project layer must win over user and catalog"
        );
    }

    #[test]
    fn resolve_user_over_catalog() {
        // When the project layer is absent, the user layer must win over the
        // catalog layer.
        let tmp = TempDir::new().unwrap();
        let catalog = write(
            &tmp.path().join("c"),
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm-research\"\n",
        );
        let user = write(
            &tmp.path().join("u"),
            "manifest.toml",
            "[style]\nactive = \"trusty-mpm-teacher\"\n",
        );
        let sources = ManifestSources {
            project: Some(tmp.path().join("p").join("manifest.toml")), // absent
            user: Some(user),
            catalog: Some(catalog),
        };
        let m = resolve_manifest(&sources);
        assert_eq!(
            m.style.and_then(|s| s.active),
            Some("trusty-mpm-teacher".to_string())
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
            user: None,
            catalog: Some(catalog),
        };
        let m = resolve_manifest(&sources);
        // catalog disabled search; default (memory=true) is preserved because the
        // whole-section replacement carried only trusty_search=false … but the
        // [mcp] section is replaced wholesale, so memory falls back to None →
        // consumer treats None as the default-on. We assert search is the
        // overridden value.
        assert_eq!(m.mcp.unwrap().trusty_search, Some(false));
    }

    #[test]
    fn resolve_malformed_layer_is_skipped() {
        // A malformed layer must be skipped (logged), not abort resolution; the
        // lower layers (and ultimately the default) still apply.
        let tmp = TempDir::new().unwrap();
        let user = write(
            &tmp.path().join("u"),
            "manifest.toml",
            "this is not toml {{{",
        );
        let sources = ManifestSources {
            project: None,
            user: Some(user),
            catalog: None,
        };
        // Malformed user layer skipped → result equals the default.
        assert_eq!(resolve_manifest(&sources), default_manifest());
    }
}
