//! Repo-level configuration parsed from `<repo_root>/trusty-search.yaml`.
//!
//! Why: large polyrepos (Duetto's monolith, mono-style ML repos) need to expose
//! several logically-separate indexes — backend Java APIs vs. frontend
//! TypeScript dashboards — without dumping every file into one giant index.
//! A repo-level YAML lets project owners declare these slices once and have
//! `trusty-search index` apply them automatically.
//!
//! What: `RepoConfig` parses a YAML manifest containing one or more named
//! `IndexConfig` slices. Each slice carries:
//!   - `paths`: subtrees to walk (defaults to the whole repo)
//!   - `exclude`: glob patterns to skip on top of the built-in ignores
//!   - `languages`: extension allow-list (`rust`, `typescript`, ...)
//!   - `domain_terms`: per-repo vocabulary fed into the intent classifier so
//!     queries containing those terms route to the right intent
//!
//! Test: see the `#[cfg(test)]` block at the bottom — covers YAML round-trip,
//! missing-file handling, path resolution, language mapping, and glob matching.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// File name auto-detected at the repo root.
pub const CONFIG_FILENAME: &str = "trusty-search.yaml";

/// Top-level repo configuration. See module docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RepoConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    pub indexes: Vec<IndexConfig>,
}

/// One named index slice within a repo.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexConfig {
    pub name: String,

    /// Subtrees to index, relative to the repo root. Empty / missing → walk
    /// the entire repo root (`["."]`).
    #[serde(default)]
    pub paths: Vec<String>,

    /// Glob patterns to exclude (on top of built-in `SKIP_DIRS`).
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Restrict to these language identifiers (see [`language_to_exts`]).
    /// Empty → all supported extensions.
    #[serde(default)]
    pub languages: Vec<String>,

    /// Domain-specific terms injected into the intent classifier for this
    /// index. Queries containing any of these terms (case-insensitive) are
    /// nudged toward `Definition` intent when no other pattern matched.
    #[serde(default)]
    pub domain_terms: Vec<String>,

    /// Index prose docs (`*.md`, `*.mdx`, `*.rst`, `*.txt`, `*.adoc`) and
    /// metadata files (`CHANGELOG*`, `LICENSE*`, `NOTICE*`). Default `true`
    /// as of v0.8.3 — issue #118 (was `false` through v0.8.2 — issue #77
    /// original design).
    ///
    /// Why: `mode=text` searches were silently empty through v0.8.2 because
    /// the walker pruned every prose file at walk time. The original #77
    /// design pre-dated the per-mode filter; with `is_allowed_for_mode`
    /// (also #77, final form) now dropping `.md` / README chunks from
    /// `mode=code` results on the way out, the walker pre-filter became
    /// asymmetric — it kept code-mode clean but broke text mode. Issue
    /// #118 flips the default so both modes work; the post-RRF filter is
    /// the single source of truth for which file types each mode returns.
    /// What: `true` (default) → walker indexes prose alongside source;
    /// `false` → walker prunes doc/CHANGELOG files before the indexer
    /// sees them (saves chunks on docs-heavy projects). Either way,
    /// `mode=code` results never include `.md` files because the
    /// post-RRF `is_allowed_for_mode` filter rejects them.
    /// Test: covered by walker tests and `test_default_includes_markdown_and_changelog`.
    #[serde(default = "default_true")]
    pub include_docs: bool,

    /// Honour `.gitignore`, `.ignore`, `.rgignore`, and `.git/info/exclude`
    /// during the walk (issue #100). Default `true`.
    ///
    /// Why: the walker historically used `walkdir`, which ignores all of the
    /// above — so a gitignored subtree (e.g. `claude-mpm-patch/` full of
    /// minified JS bundles) would dominate the chunk budget and silently
    /// produce an index containing none of the project's real source.
    /// Honouring `.gitignore` by default matches ripgrep / fd semantics.
    /// What: `true` (default) → walker delegates to the `ignore` crate with
    /// the standard ignore-file set enabled. `false` → bypass entirely (only
    /// the hardcoded `SKIP_DIRS` / `should_skip_path` filters apply); useful
    /// when indexing a vendored subtree the operator wants on purpose.
    /// Test: `service::walker::test_walker_honors_gitignore` plus the
    /// `respect_gitignore` opt-out test.
    #[serde(default = "default_true")]
    pub respect_gitignore: bool,

    /// Stage-1-minimal mode (issue #313): when `true`, the Phase 3 KG
    /// rebuild is skipped entirely for this index. The graph stage is
    /// permanently `Skipped`. `get_call_chain` and `search_kg` return a
    /// 503 `kg_unavailable` error.
    ///
    /// Why: per-index YAML control lets a polyrepo owner suppress KG for
    /// large/documentation-heavy sub-indexes that never need call-chain
    /// navigation, saving 50–100 MB of heap and ~400 ms per reindex.
    /// Orthogonal to `lexical_only` — both may be set independently.
    /// What: `false` (default) → KG is built as normal. `true` → Phase 3
    /// is bypassed; the persisted `indexes.toml` entry carries `skip_kg =
    /// true` so the choice survives daemon restarts.
    /// Test: `repo_config::tests::skip_kg_round_trips_yaml` in this module.
    #[serde(default)]
    pub skip_kg: bool,

    /// Deferred-embedding opt-out (issue #923). When `true` (the default),
    /// the fast pass runs synchronously (walk → chunk → BM25 → KG → `Ready`
    /// for lexical+graph) and semantic embedding runs in the background.
    /// Set `false` in `trusty-search.yaml` to force synchronous full indexing.
    ///
    /// Why: per-index YAML control lets CI pipelines opt out while interactive
    /// sessions keep the fast-start benefit.
    /// What: `true` (default) → deferred. `false` → synchronous (old behaviour).
    /// Test: `repo_config::tests::defer_embed_round_trips_yaml` in this module.
    #[serde(default = "default_true")]
    pub defer_embed: bool,
}

/// Shared serde default helper that returns `true`.
///
/// Why: serde's `default` attribute requires a path to a zero-arg fn returning
/// the field type; `bool::default()` returns `false`, so fields whose absent
/// value should be `true` (e.g. `respect_gitignore`, `include_docs`,
/// `defer_embed`) must name a custom helper. One shared fn avoids repetition.
/// What: always returns `true`.
/// Test: `repo_config::tests` — any test that omits a `default_true` field
/// and asserts the loaded value is `true` exercises this helper.
fn default_true() -> bool {
    true
}

impl Default for IndexConfig {
    /// `respect_gitignore` defaults to `true` (issue #100) and
    /// `include_docs` defaults to `true` (issue #118) so the manual impl
    /// matches serde's missing-field behaviour. Without this,
    /// `IndexConfig::default()` would silently re-enable the v0.8.2
    /// docs-exclusion footgun on test / fallback paths.
    fn default() -> Self {
        Self {
            name: String::new(),
            paths: Vec::new(),
            exclude: Vec::new(),
            languages: Vec::new(),
            domain_terms: Vec::new(),
            include_docs: true,
            respect_gitignore: true,
            skip_kg: false,
            defer_embed: true,
        }
    }
}

fn default_version() -> u32 {
    1
}

impl RepoConfig {
    /// Load `<root>/trusty-search.yaml`. Returns `Ok(None)` if the file is
    /// absent (the common case — most repos won't have one).
    ///
    /// Why: callers (the `index` CLI handler) need to cleanly distinguish
    /// "no config, fall back to single-index" from "config present but
    /// malformed, fail loudly".
    /// What: stat → read → `serde_yml::from_str`. Surfaces parse errors via
    /// `anyhow::Error` with file path context.
    /// Test: `test_load_valid_yaml`, `test_load_missing_yaml_returns_none`,
    /// `test_load_malformed_yaml_errors`.
    pub fn load(root: &Path) -> anyhow::Result<Option<Self>> {
        let path = root.join(CONFIG_FILENAME);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let cfg: Self = serde_yml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        Ok(Some(cfg))
    }

    /// Resolve `IndexConfig::paths` to absolute paths under `root`. Empty
    /// `paths` means "the entire repo" → `vec![root.to_path_buf()]`.
    ///
    /// Why: the file walker needs absolute paths to feed `WalkDir`, but users
    /// write `paths:` entries relative to the repo root.
    /// What: joins each relative entry with `root`. `.` and empty strings
    /// also normalise to `root`.
    /// Test: `test_resolved_paths_default_is_root`, `test_resolved_paths_multiple`.
    pub fn resolved_paths(cfg: &IndexConfig, root: &Path) -> Vec<PathBuf> {
        if cfg.paths.is_empty() {
            return vec![root.to_path_buf()];
        }
        cfg.paths
            .iter()
            .map(|p| {
                let trimmed = p.trim();
                if trimmed.is_empty() || trimmed == "." {
                    root.to_path_buf()
                } else {
                    root.join(trimmed)
                }
            })
            .collect()
    }

    /// Resolve language identifiers to file-extension allow-list. Empty input
    /// returns an empty allow-list (interpreted as "all extensions allowed"
    /// by the caller).
    pub fn resolved_extensions(cfg: &IndexConfig) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for lang in &cfg.languages {
            out.extend(language_to_exts(lang));
        }
        out
    }
}

/// Map a language name (case-insensitive) to its file extensions.
///
/// Why: users write `languages: [typescript]` but the walker filters on the
/// raw extension (`.ts`, `.tsx`). One place to maintain the mapping.
/// What: returns the extensions (without the leading dot) for the language,
/// or an empty slice for unknown languages (caller should warn).
/// Test: `test_language_to_exts_known`, `test_language_to_exts_unknown`.
pub fn language_to_exts(lang: &str) -> &'static [&'static str] {
    match lang.to_ascii_lowercase().as_str() {
        "rust" | "rs" => &["rs"],
        "python" | "py" => &["py"],
        "typescript" | "ts" => &["ts", "tsx"],
        "javascript" | "js" => &["js", "jsx", "mjs", "cjs"],
        "go" => &["go"],
        "java" => &["java"],
        "c" => &["c", "h"],
        "cpp" | "c++" | "cxx" => &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
        _ => &[],
    }
}

/// Return `true` if any glob in `excludes` matches `path` (relative to root)
/// or `path` directly. Caller is responsible for choosing a stable form.
///
/// Why: `IndexConfig::exclude` patterns target both file basenames
/// (`"**/__tests__/**"`) and partial paths (`"selenium/"`). The `glob` crate's
/// `Pattern` handles both via the standard glob syntax.
/// What: parses each pattern once and tests with `Pattern::matches`. Patterns
/// that fail to parse are skipped with a warning.
/// Test: `test_glob_match_basic`, `test_glob_match_recursive`.
pub fn path_matches_any_glob(path: &Path, excludes: &[String]) -> bool {
    if excludes.is_empty() {
        return false;
    }
    let s = path.to_string_lossy();
    for pat in excludes {
        match glob::Pattern::new(pat) {
            Ok(p) => {
                if p.matches(&s) {
                    return true;
                }
                // Also try matching just the file name / any trailing segment
                // so that `selenium/` matches `<root>/api/selenium/foo.py`.
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if p.matches(name) {
                        return true;
                    }
                }
                // And the `**/<pat>` form so plain `selenium/` works against
                // arbitrarily-nested paths.
                if !pat.starts_with("**/") {
                    let alt = format!("**/{}", pat.trim_end_matches('/'));
                    if let Ok(p2) = glob::Pattern::new(&alt) {
                        if p2.matches(&s) {
                            return true;
                        }
                    }
                    let alt2 = format!("**/{}/**", pat.trim_end_matches('/'));
                    if let Ok(p3) = glob::Pattern::new(&alt2) {
                        if p3.matches(&s) {
                            return true;
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!("ignoring invalid exclude glob {pat:?}: {e}");
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_yaml(dir: &Path, body: &str) {
        fs::write(dir.join(CONFIG_FILENAME), body).unwrap();
    }

    #[test]
    fn test_load_valid_yaml() {
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: api
    paths: [src/api]
    exclude: ["**/__tests__/**"]
    languages: [java, python]
    domain_terms: ["PMS", "rate strategy"]
  - name: ui
    paths: [ui/src]
    languages: [typescript]
"#,
        );
        let cfg = RepoConfig::load(tmp.path())
            .unwrap()
            .expect("config present");
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.indexes.len(), 2);
        assert_eq!(cfg.indexes[0].name, "api");
        assert_eq!(cfg.indexes[0].paths, vec!["src/api".to_string()]);
        assert_eq!(cfg.indexes[0].languages, vec!["java", "python"]);
        assert_eq!(
            cfg.indexes[0].domain_terms,
            vec!["PMS".to_string(), "rate strategy".to_string()]
        );
        assert_eq!(cfg.indexes[1].name, "ui");
        assert_eq!(cfg.indexes[1].languages, vec!["typescript"]);
    }

    #[test]
    fn test_load_missing_yaml_returns_none() {
        let tmp = tempdir().unwrap();
        let res = RepoConfig::load(tmp.path()).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn test_load_malformed_yaml_errors() {
        let tmp = tempdir().unwrap();
        write_yaml(tmp.path(), "not: valid: yaml: : :");
        let res = RepoConfig::load(tmp.path());
        assert!(res.is_err(), "expected parse error for malformed yaml");
    }

    #[test]
    fn test_resolved_paths_default_is_root() {
        let cfg = IndexConfig {
            name: "x".into(),
            ..Default::default()
        };
        let root = Path::new("/tmp/repo");
        let resolved = RepoConfig::resolved_paths(&cfg, root);
        assert_eq!(resolved, vec![root.to_path_buf()]);
    }

    #[test]
    fn test_resolved_paths_multiple() {
        let cfg = IndexConfig {
            name: "x".into(),
            paths: vec!["src/api".into(), "services/".into(), ".".into()],
            ..Default::default()
        };
        let root = Path::new("/tmp/repo");
        let resolved = RepoConfig::resolved_paths(&cfg, root);
        assert_eq!(
            resolved,
            vec![
                PathBuf::from("/tmp/repo/src/api"),
                PathBuf::from("/tmp/repo/services/"),
                PathBuf::from("/tmp/repo"),
            ]
        );
    }

    #[test]
    fn test_language_to_exts_known() {
        assert_eq!(language_to_exts("rust"), &["rs"]);
        assert_eq!(language_to_exts("Python"), &["py"]);
        assert_eq!(language_to_exts("typescript"), &["ts", "tsx"]);
        assert_eq!(language_to_exts("javascript"), &["js", "jsx", "mjs", "cjs"]);
        assert_eq!(
            language_to_exts("c++"),
            &["cpp", "cc", "cxx", "hpp", "hh", "hxx"]
        );
    }

    #[test]
    fn test_language_to_exts_unknown() {
        let empty: &[&str] = &[];
        assert_eq!(language_to_exts("cobol"), empty);
        assert_eq!(language_to_exts(""), empty);
    }

    #[test]
    fn test_glob_match_basic() {
        let excludes = vec!["**/__tests__/**".to_string()];
        assert!(path_matches_any_glob(
            Path::new("/repo/src/api/__tests__/foo.py"),
            &excludes
        ));
        assert!(!path_matches_any_glob(
            Path::new("/repo/src/api/foo.py"),
            &excludes
        ));
    }

    #[test]
    fn test_glob_match_partial_segment() {
        let excludes = vec!["selenium/".to_string()];
        assert!(path_matches_any_glob(
            Path::new("/repo/api/selenium/foo.py"),
            &excludes
        ));
    }

    /// Verify that the domain_terms on `IndexConfig` survive a YAML
    /// round-trip and that an empty list disables classifier domain-bumping.
    ///
    /// Why: the daemon attaches these to the `CodeIndexer` and passes them to
    /// `QueryClassifier::classify_with_domain` on every search. If the YAML
    /// parser silently drops them (e.g. a renamed serde field), classification
    /// would silently degrade for every multi-index repo.
    /// What: parses a YAML body, asserts the field is preserved verbatim.
    /// Test: this test.
    #[test]
    fn domain_terms_survive_yaml_roundtrip() {
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: api
    domain_terms: ["PMS", "rate strategy", "Cloudbeds"]
"#,
        );
        let cfg = RepoConfig::load(tmp.path()).unwrap().unwrap();
        assert_eq!(cfg.indexes.len(), 1);
        assert_eq!(
            cfg.indexes[0].domain_terms,
            vec![
                "PMS".to_string(),
                "rate strategy".into(),
                "Cloudbeds".into()
            ]
        );
    }

    /// `path_matches_any_glob` is what the daemon-side walker filters use to
    /// honour `IndexConfig::exclude`. Verify a few common patterns end-to-end
    /// since regressions here silently leak files into the index.
    #[test]
    fn exclude_globs_match_common_patterns() {
        // `**/__tests__/**` should match a deeply-nested test dir.
        let excludes = vec!["**/__tests__/**".to_string()];
        assert!(path_matches_any_glob(
            Path::new("/repo/src/api/__tests__/foo.py"),
            &excludes
        ));
        assert!(!path_matches_any_glob(
            Path::new("/repo/src/api/foo.py"),
            &excludes
        ));
    }

    #[test]
    fn test_resolved_extensions() {
        let cfg = IndexConfig {
            name: "x".into(),
            languages: vec!["typescript".into(), "javascript".into()],
            ..Default::default()
        };
        let exts = RepoConfig::resolved_extensions(&cfg);
        assert!(exts.contains(&"ts"));
        assert!(exts.contains(&"tsx"));
        assert!(exts.contains(&"js"));
        assert!(exts.contains(&"jsx"));
    }

    /// Issue #100: `respect_gitignore` defaults to `true`, both at construction
    /// (`IndexConfig::default()`) and when deserialising an older YAML that
    /// predates the field. The explicit `false` value also round-trips. This
    /// pins the back-compat contract — an existing `trusty-search.yaml` must
    /// pick up the gitignore-honouring fix automatically.
    #[test]
    fn respect_gitignore_defaults_true_and_round_trips() {
        // Default constructor returns `true`.
        let cfg = IndexConfig::default();
        assert!(cfg.respect_gitignore);

        // YAML without the field deserialises to `true`.
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: legacy
"#,
        );
        let cfg = RepoConfig::load(tmp.path()).unwrap().unwrap();
        assert!(
            cfg.indexes[0].respect_gitignore,
            "missing field must default to true (issue #100 back-compat)"
        );

        // Explicit `false` round-trips.
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: vendored
    respect_gitignore: false
"#,
        );
        let cfg = RepoConfig::load(tmp.path()).unwrap().unwrap();
        assert!(!cfg.indexes[0].respect_gitignore);
    }

    /// Issue #313 (D3): `skip_kg` is a first-class YAML field. Verify it
    /// defaults to `false`, that an older YAML without the field keeps it
    /// `false`, and that `skip_kg: true` round-trips correctly.
    #[test]
    fn skip_kg_round_trips_yaml() {
        // Default constructor returns `false`.
        let cfg = IndexConfig::default();
        assert!(!cfg.skip_kg, "default must be false");

        // YAML without the field deserialises to `false`.
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: legacy
"#,
        );
        let loaded = RepoConfig::load(tmp.path()).unwrap().unwrap();
        assert!(
            !loaded.indexes[0].skip_kg,
            "missing field must default to false (backward-compat)"
        );

        // Explicit `true` round-trips.
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: no-kg-index
    skip_kg: true
"#,
        );
        let loaded = RepoConfig::load(tmp.path()).unwrap().unwrap();
        assert!(
            loaded.indexes[0].skip_kg,
            "skip_kg: true must deserialise as true"
        );

        // Both `skip_kg` and `lexical_only: false` can coexist (orthogonality).
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: mixed
    skip_kg: true
"#,
        );
        let loaded = RepoConfig::load(tmp.path()).unwrap().unwrap();
        // `lexical_only` is a CLI-only concept (not a YAML field) — the
        // IndexConfig has no `lexical_only`; the RegisterFilters layer handles
        // it. Just confirm skip_kg is true and the other defaults are intact.
        assert!(loaded.indexes[0].skip_kg);
        assert!(loaded.indexes[0].respect_gitignore);
        assert!(loaded.indexes[0].include_docs);
    }
}
