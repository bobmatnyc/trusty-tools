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

    /// Issue #1372: extra directory basenames pruned during the walk on top of
    /// the built-in `walker::SKIP_DIRS`. Default
    /// `walker::DEFAULT_EXTRA_SKIP_DIRS` (`data`, `exports`, `output`,
    /// `reports`, `snapshots`, `results`).
    ///
    /// Why: data-heavy repos over-index thousands of files under data-export
    /// directory trees. These dir names are NOT in the baseline `SKIP_DIRS`
    /// (a few projects keep source under e.g. `output/`), so they are exposed
    /// as an editable per-index default rather than a hardcoded constant — a
    /// project can override or clear the list in `trusty-search.yaml`.
    /// What: a `Vec<String>` of directory basenames. When the field is absent
    /// the serde default supplies the targeted set; the default also
    /// serialises back so users can see and edit it.
    /// Test: `extra_skip_dirs_defaults_and_round_trips` in this module.
    #[serde(default = "crate::service::walker::default_extra_skip_dirs")]
    pub extra_skip_dirs: Vec<String>,

    /// Issue #1372: tighter size cap (bytes) applied only to data-ish file
    /// extensions (`walker::DATA_EXTS`: json/xml/txt/log). `None` ⇒ the built-in
    /// default (`walker::DEFAULT_DATA_FILE_MAX_BYTES`, 64 KiB). Non-data
    /// extensions keep the global `walker::MAX_FILE_BYTES` (1 MiB) cap.
    ///
    /// Why: JSON/XML/TXT/log data exports are usually under 1 MiB so the global
    /// cap lets them all through; a tighter cap prunes the bulk exports while
    /// small config files of the same extension (`package.json`, `tsconfig.json`)
    /// stay indexable.
    /// What: an `Option<u64>` byte count. Defaults to `Some(65_536)` so the
    /// value is discoverable in a freshly-written config.
    /// Test: `data_file_max_bytes_defaults_and_round_trips` in this module.
    #[serde(default = "default_data_file_max_bytes")]
    pub data_file_max_bytes: Option<u64>,
}

/// Shared serde default for `IndexConfig::data_file_max_bytes` (issue #1372):
/// `Some(64 KiB)`. A separate fn (not a closure) because serde's `default`
/// attribute requires a named zero-arg function path.
fn default_data_file_max_bytes() -> Option<u64> {
    Some(crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES)
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
            extra_skip_dirs: crate::service::walker::default_extra_skip_dirs(),
            data_file_max_bytes: default_data_file_max_bytes(),
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
    /// What: stat → read → `serde_yaml::from_str`. Surfaces parse errors via
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
        let cfg: Self = serde_yaml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        // #8922: a bad exclude glob is a load error here, not a runtime purge.
        for index in &cfg.indexes {
            validate_exclude_globs(&index.exclude)
                .map_err(|e| anyhow::anyhow!("{}: index '{}': {e}", path.display(), index.name))?;
        }
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

/// Reject exclude globs that do not parse (#8922).
///
/// Why: at runtime an unparsable glob excludes every path (see
/// [`path_matches_any_glob`]), so one typo would purge a whole index. Callers
/// that accept globs — repo-config load, `POST /indexes`, the config PATCH —
/// refuse it up front instead.
/// What: `Err` naming the first pattern that fails `glob::Pattern::new`.
/// Test: `an_invalid_exclude_glob_is_rejected_at_load`,
/// `create_and_patch_reject_an_invalid_exclude_glob`.
pub fn validate_exclude_globs(globs: &[String]) -> Result<(), String> {
    for pat in globs {
        if let Err(e) = glob::Pattern::new(pat) {
            return Err(format!("invalid exclude glob {pat:?}: {e}"));
        }
    }
    Ok(())
}

/// Record that `pat` failed to parse; `true` only the first time per process.
fn first_report_of_invalid_glob(pat: &str) -> bool {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    SEEN.get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(pat.to_string()))
        .unwrap_or(true)
}

/// Return `true` if any glob in `excludes` matches `path` (relative to root)
/// or `path` directly. Caller is responsible for choosing a stable form.
///
/// Why: `IndexConfig::exclude` patterns target both file basenames
/// (`"**/__tests__/**"`) and partial paths (`"selenium/"`). The `glob` crate's
/// `Pattern` handles both via the standard glob syntax.
/// What: parses each pattern once and tests with `Pattern::matches`. A pattern
/// that fails to parse matches every path (#8922): the exclude config could not
/// be read, so nothing it might cover is admitted. That error is logged once
/// per pattern.
/// Test: `test_glob_match_basic`, `an_unparsable_exclude_glob_excludes_every_path`,
/// `the_invalid_glob_backstop_logs_once`.
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
                // #8922: fail closed — skipping the pattern indexed what it
                // was written to exclude. Entry points reject such globs, so
                // this is a backstop for a persisted config; logged once per
                // pattern, not once per path.
                if first_report_of_invalid_glob(pat) {
                    tracing::error!(
                        "invalid exclude glob {pat:?}: {e} — excluding every path until it is fixed"
                    );
                }
                return true;
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

    /// #8922 fail-open check: an exclude glob that does not parse used to be
    /// skipped, so the files it was written to exclude were indexed. Fails
    /// with the `Err` arm restored to a skip.
    #[test]
    fn an_unparsable_exclude_glob_excludes_every_path() {
        let excludes = vec!["secrets/[unclosed".to_string()];
        assert!(path_matches_any_glob(
            Path::new("/repo/secrets/prod.yaml"),
            &excludes
        ));
        assert!(path_matches_any_glob(
            Path::new("/repo/src/lib.rs"),
            &excludes
        ));
    }

    /// #8922: a repo config whose exclude glob does not parse is a load error,
    /// so it never reaches the daemon as a purge-everything filter. Fails with
    /// the validation loop in `RepoConfig::load` removed.
    #[test]
    fn an_invalid_exclude_glob_is_rejected_at_load() {
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            "version: 1\nindexes:\n  - name: api\n    exclude: [\"secrets/[unclosed\"]\n",
        );
        let err = RepoConfig::load(tmp.path()).expect_err("a bad glob must not load");
        let msg = err.to_string();
        assert!(
            msg.contains("secrets/[unclosed") && msg.contains("'api'"),
            "{msg}"
        );
        assert!(validate_exclude_globs(&["**/ok/**".to_string()]).is_ok());
    }

    /// Counts ERROR events whose message names `needle`.
    struct CountErrors {
        needle: &'static str,
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountErrors {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, _: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    self.0.push_str(&format!("{v:?}"));
                }
            }
            let mut message = Message(String::new());
            event.record(&mut message);
            if *event.metadata().level() == tracing::Level::ERROR && message.0.contains(self.needle)
            {
                self.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    /// #8922: the runtime backstop still excludes every path, but logs the bad
    /// pattern once, not once per path checked. Fails with the
    /// `first_report_of_invalid_glob` check removed (three errors are logged).
    #[test]
    fn the_invalid_glob_backstop_logs_once() {
        use tracing_subscriber::layer::SubscriberExt;
        const PATTERN: &str = "logs-once-8922/[unclosed";
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(CountErrors {
            needle: PATTERN,
            hits: hits.clone(),
        });
        let excludes = vec![PATTERN.to_string()];
        tracing::subscriber::with_default(subscriber, || {
            // An earlier test may have cached this callsite as disabled under
            // the no-op global dispatcher; re-evaluate it under this one.
            tracing::callsite::rebuild_interest_cache();
            for path in ["/repo/a.rs", "/repo/b.rs", "/repo/c/d.py"] {
                assert!(path_matches_any_glob(Path::new(path), &excludes));
            }
        });
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
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

    /// Issue #1372: `extra_skip_dirs` defaults to the targeted data-export set
    /// when absent, an explicit list round-trips, and the default also
    /// serialises so users can see and edit it.
    #[test]
    fn extra_skip_dirs_defaults_and_round_trips() {
        // Default constructor carries the targeted set.
        let cfg = IndexConfig::default();
        assert!(cfg.extra_skip_dirs.contains(&"data".to_string()));
        assert!(cfg.extra_skip_dirs.contains(&"results".to_string()));
        assert_eq!(cfg.extra_skip_dirs.len(), 6);

        // Missing field deserialises to the default set.
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
            loaded.indexes[0]
                .extra_skip_dirs
                .contains(&"exports".to_string()),
            "missing field must default to the targeted set: {:?}",
            loaded.indexes[0].extra_skip_dirs
        );

        // Explicit list round-trips (overrides the default).
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: custom
    extra_skip_dirs: ["archive", "dumps"]
"#,
        );
        let loaded = RepoConfig::load(tmp.path()).unwrap().unwrap();
        assert_eq!(
            loaded.indexes[0].extra_skip_dirs,
            vec!["archive".to_string(), "dumps".to_string()]
        );

        // The default serialises back into a freshly-written config (discoverable).
        let serialized = serde_yaml::to_string(&RepoConfig {
            version: 1,
            indexes: vec![IndexConfig {
                name: "x".into(),
                ..Default::default()
            }],
        })
        .unwrap();
        assert!(
            serialized.contains("extra_skip_dirs"),
            "default must serialise so it is discoverable: {serialized}"
        );
    }

    /// Issue #1372: `data_file_max_bytes` defaults to `Some(64 KiB)`, an
    /// explicit value round-trips, and the default serialises into a fresh
    /// config so it is discoverable.
    #[test]
    fn data_file_max_bytes_defaults_and_round_trips() {
        let cfg = IndexConfig::default();
        assert_eq!(cfg.data_file_max_bytes, Some(65_536));

        // Missing field defaults to Some(64 KiB).
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
        assert_eq!(loaded.indexes[0].data_file_max_bytes, Some(65_536));

        // Explicit value round-trips.
        let tmp = tempdir().unwrap();
        write_yaml(
            tmp.path(),
            r#"
version: 1
indexes:
  - name: custom
    data_file_max_bytes: 131072
"#,
        );
        let loaded = RepoConfig::load(tmp.path()).unwrap().unwrap();
        assert_eq!(loaded.indexes[0].data_file_max_bytes, Some(131_072));

        // Default serialises into a fresh config.
        let serialized = serde_yaml::to_string(&IndexConfig {
            name: "x".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(
            serialized.contains("data_file_max_bytes"),
            "default must serialise: {serialized}"
        );
    }
}
