//! Source file walker for `reindex`.
//!
//! Why: Reindexing a project requires enumerating all source files under a
//! root directory while skipping VCS/build/dependency dirs and binary noise.
//! What: `walk_source_files(root)` returns every source file (filtered by
//! [`SOURCE_EXTS`]) below `root`, honouring `.gitignore` (via the `ignore`
//! crate, same engine ripgrep uses) and skipping any directory whose name
//! appears in [`SKIP_DIRS`], plus [`should_skip_path`] and [`should_skip_content`]
//! for minification / size guards applied at read time.
//! Test: see the unit tests at the bottom.

use std::path::{Path, PathBuf};

/// Source file extensions that the indexer can handle.
///
/// Source code is parsed with tree-sitter (AST-aware chunking). Structured
/// documents (md, yaml, toml, json, xml, txt, log) are handled by the
/// format-aware document chunkers in `crate::core::chunker`.
/// Java-heavy build files (`.gradle`, `.groovy`) fall back to the
/// sliding-window chunker since their tree-sitter grammars aren't compiled in.
#[rustfmt::skip]
pub const SOURCE_EXTS: &[&str] = &[
    // Source code
    "rs", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java",
    "c", "cpp", "h", "hpp", "cs", "rb", "php", "swift", "kt", "kts",
    "scala", "groovy", "gradle", "sh",
    // Documentation
    "md", "mdx",
    // Structured config / data
    "yaml", "yml", "toml", "json", "xml",
    // Plaintext / logs
    "txt", "log",
];

/// Directory names to skip when walking. Matched on basename only.
///
/// Java/Gradle and JS/TS build outputs are aggressively pruned — these directories
/// contain machine-generated code that bloats the index without adding signal.
/// AWS CDK / SAM synth output (`cdk.out`, `.aws-sam`) is especially noisy: each
/// Lambda asset is a full copy of a Python venv with vendored dependencies.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".build",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "vendor",
    ".cargo",
    ".npm",
    ".cache",
    ".pnpm-store",
    ".yarn",
    ".rustup",
    ".tox",
    ".bundle",
    "coverage",
    ".nyc_output",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    // Java / Gradle / Maven build artefacts
    ".gradle",
    ".mvn",
    ".m2",
    "out",
    "bin",
    "classes",
    "generated",
    "generated-sources",
    "generated-test-sources",
    // AWS CDK / SAM synthesized deployment bundles — contain full copies of
    // Python venvs and vendored third-party packages, pure build artefacts.
    "cdk.out",
    "cdk.out2",
    ".aws-sam",
    // Monorepo task-runner caches
    ".turbo",
    // IDE / metadata
    ".idea",
    ".vscode",
    // AI tooling / agent metadata — high-density prose that outranks source code in BM25
    ".claude",
    ".claude-mpm",
    ".open-mpm",
    ".trusty-agents",
    ".cursor",
    ".aider",
    ".continue",
    ".obsidian",
    // Test fixtures / static test data — SQL dumps, sample payloads, golden
    // files. High-volume, machine-shaped data that bloats the index without
    // adding source-code signal. Matched on basename only, so `src/test/
    // resources` stays indexed (we don't skip bare `resources`).
    "fixtures",
    "__fixtures__",
    "testdata",
    "test-data",
    "test_data",
    "testresources",
    "test_resources",
];

/// File names (full basename) to skip unconditionally.
///
/// Why: lock files and dependency manifests carry no semantic value for
/// code search — they list versions/hashes which bloat BM25 vocabulary
/// and produce false positives (e.g. `Cargo.lock` ranking for dependency
/// names rather than source code).
/// What: matched on full file basename only (case-sensitive on the well-
/// known names; the same as how the ecosystems themselves spell them).
/// Test: see `test_skips_lock_files` below.
pub const SKIP_FILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Pipfile.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
];

/// Documentation / metadata file extensions excluded by default (issue #77).
///
/// Why: prose files (Markdown, reStructuredText, AsciiDoc, plain text) contain
/// natural-language descriptions of code that score very high on BM25 — query
/// terms appear verbatim in headings and explanation paragraphs. A benchmark
/// against grep showed `CHANGELOG.md` and other docs polluted 6 of 8 queries.
/// These extensions are skipped at walk time unless the index opts in via
/// `include_docs: true`.
/// What: lowercase extensions (no leading dot) matched against the file
/// extension.
/// Test: `test_default_excludes_markdown_and_changelog` below.
pub const DOC_EXCLUDE_EXTS: &[&str] = &["md", "mdx", "rst", "adoc", "txt"];

/// Filename basename substrings (case-insensitive) excluded by default
/// (issue #77).
///
/// Why: standard metadata files (`CHANGELOG*`, `LICENSE*`, `NOTICE*`) repeat
/// human-readable summaries that outrank source code on BM25. They are
/// uninteresting for code search regardless of extension.
/// What: a lowercased substring of the file basename triggers a match. So
/// `CHANGELOG`, `CHANGELOG.md`, `CHANGELOG-old.txt` all match `"changelog"`.
/// Test: `test_default_excludes_markdown_and_changelog`.
pub const DOC_EXCLUDE_BASENAME_SUBSTRINGS: &[&str] = &["changelog", "license", "notice"];

/// Walk options controlling default-exclusion behaviour (issue #77, #100, #118).
///
/// Why: the original #77 design defaulted `include_docs: false` so prose
/// files never reached BM25 / vector lanes. That design pre-dated the
/// per-mode hard filter in [`crate::core::indexer::docs_penalty::is_allowed_for_mode`],
/// which now (per #77's final form) drops `.md` / README chunks from
/// `SearchMode::Code` results on the way out. With both layers in place,
/// keeping the walker exclusion ALSO active was symmetric only against
/// `code` mode — for `text` mode it produced empty results because there
/// was no prose content to consult. Issue #118 flips the default to
/// `include_docs: true` (Option A from the ticket) so `text` mode works
/// out of the box. Code mode remains clean because `is_allowed_for_mode`
/// continues to reject `.md` / README / LICENSE files. Indexes that
/// genuinely don't want prose chunked at all can still opt out via
/// `include_docs: false` in `trusty-search.yaml`.
///
/// Issue #100 adds `respect_gitignore`: the walker honours `.gitignore`
/// by default (matching ripgrep / fd semantics) and silent partial-index
/// failures from a gitignored subtree dominating the chunk budget are no
/// longer possible. A handful of indexes (e.g. a vendored subtree the
/// operator wants to index on purpose) need to opt out. Keeping the
/// toggle as a struct makes future additions (e.g. `include_configs`)
/// non-breaking.
/// What: a `Clone` struct passed by reference to [`walk_source_files_with_options`].
/// As of v0.8.3 / #118, the default is `include_docs: true`,
/// `respect_gitignore: true`; the legacy [`walk_source_files`] entry
/// point preserves these defaults.
/// Test: `test_default_includes_markdown_and_changelog` (replaces the
///   v0.8.2 `test_default_excludes_markdown_and_changelog`),
///   `test_include_docs_false_excludes_markdown`,
///   `test_walker_honors_gitignore`,
///   `test_walker_respects_disable_flag`,
///   `test_walker_honors_dot_ignore`.
///
/// Issue #1372 adds two hygiene knobs that the daemon resolves from per-index
/// config (`trusty-search.yaml` / `.trusty-search.yaml`) before each walk:
/// `extra_skip_dirs` (extra directory basenames pruned on top of the built-in
/// [`SKIP_DIRS`]) and `data_file_max_bytes` (a tighter size cap applied only to
/// [`DATA_EXTS`] files). Because these carry an owned `Vec<String>`, `WalkOptions`
/// is no longer `Copy`; the orchestrator clones it per subtree.
///
/// The full config stack for #1372 is complete:
///
/// - **Walker defaults** (`DEFAULT_EXTRA_SKIP_DIRS` / `DEFAULT_DATA_FILE_MAX_BYTES`) —
///   this file.
/// - **Per-index config** (`IndexConfig` / `ProjectConfig`) — `core/repo_config.rs`
///   and `core/project_config.rs`; defaults flow into `WalkOptions` at reindex time.
/// - **Runtime edit API** (`GET`/`PATCH /indexes/:id/config`) —
///   `service/server/index_config.rs`; changes persist to `indexes.toml`.
/// - **Dashboard UI** (`IndexConfig.svelte`) — the settings view reads and
///   writes the config API so operators can tune hygiene without touching config files.
///
/// Issue #1554 (walk root in skip-dir path) is addressed by stripping the
/// canonical root prefix before checking components — only segments RELATIVE
/// to the walk root are tested against the skip lists.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// When `true` (default as of v0.8.3 / issue #118), files matching
    /// [`DOC_EXCLUDE_EXTS`] or [`DOC_EXCLUDE_BASENAME_SUBSTRINGS`] are
    /// walked normally. When `false`, they are pruned before they reach
    /// the indexer — preserving the v0.8.2 behaviour for indexes that
    /// explicitly opt out (e.g. via `include_docs: false` in
    /// `trusty-search.yaml`).
    pub include_docs: bool,
    /// When `true` (default), the walker honours `.gitignore`,
    /// `.git/info/exclude`, the global git ignore file, `.ignore`, and
    /// `.rgignore` — the same set of ignore files ripgrep respects. When
    /// `false`, none of these files are consulted and only the hardcoded
    /// [`SKIP_DIRS`] / [`should_skip_path`] filters apply. Default `true` is
    /// the fix for issue #100; the opt-out exists for callers that need to
    /// index a vendored subtree on purpose.
    pub respect_gitignore: bool,
    /// Extra directory basenames pruned on top of the built-in [`SKIP_DIRS`]
    /// (issue #1372). Matched on basename only, identical to `SKIP_DIRS`.
    /// Default [`DEFAULT_EXTRA_SKIP_DIRS`] (`data`, `exports`, `output`,
    /// `reports`, `snapshots`, `results`).
    pub extra_skip_dirs: Vec<String>,
    /// Tighter size cap (bytes) applied only to [`DATA_EXTS`] files
    /// (issue #1372). Resolved from `IndexConfig::data_file_max_bytes`
    /// (`Option<u64>`) to a concrete value at the daemon boundary; the walker
    /// always receives a concrete cap. Default [`DEFAULT_DATA_FILE_MAX_BYTES`]
    /// (64 KiB). Non-data extensions keep the global [`MAX_FILE_BYTES`] cap.
    pub data_file_max_bytes: u64,
}

impl Default for WalkOptions {
    /// Default `include_docs: true` (issue #118), `respect_gitignore: true`
    /// (issue #100), `extra_skip_dirs` = [`DEFAULT_EXTRA_SKIP_DIRS`] and
    /// `data_file_max_bytes` = [`DEFAULT_DATA_FILE_MAX_BYTES`] (issue #1372).
    /// The `code` vs `text` mode filter (`is_allowed_for_mode`) is the single
    /// source of truth for which file types appear in results, so the walker
    /// no longer has to pre-filter.
    fn default() -> Self {
        Self {
            include_docs: true,
            respect_gitignore: true,
            extra_skip_dirs: default_extra_skip_dirs(),
            data_file_max_bytes: DEFAULT_DATA_FILE_MAX_BYTES,
        }
    }
}

/// Return `true` when `path` matches one of the default doc-exclusion rules
/// (issue #77).
///
/// Why: a single predicate shared between the walker and the watcher so a
/// `CHANGELOG.md` write event never sneaks past `path_in_skipped_dir`.
/// What: lowercases the basename once, checks both the extension allowlist
/// and the basename substring list.
/// Test: `test_is_default_doc_excluded`.
pub fn is_default_doc_excluded(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name_lower = name.to_ascii_lowercase();

    if let Some(ext) = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
    {
        if DOC_EXCLUDE_EXTS.contains(&ext.as_str()) {
            return true;
        }
    }

    for needle in DOC_EXCLUDE_BASENAME_SUBSTRINGS {
        if name_lower.contains(needle) {
            return true;
        }
    }
    false
}

/// File extensions that are always binary or non-parseable. These are skipped
/// before the file is opened, so the walker never reads their bytes.
const BINARY_EXTS: &[&str] = &[
    "wasm", "so", "dylib", "dll", "exe", "pdf", "png", "jpg", "jpeg", "gif", "ico", "webp", "zip",
    "tar", "gz", "bz2", "xz", "7z", "rar", "ttf", "otf", "woff", "woff2", "mp3", "mp4", "mov",
    "avi", "mkv", "db", "sqlite", "lock", "pyc", "class", "o", "a",
];

/// Files larger than this are silently skipped. 1 MiB is generous for a source
/// file; anything bigger is almost certainly generated or a data blob.
pub const MAX_FILE_BYTES: u64 = 1_048_576; // 1 MiB

/// Data-ish file extensions subject to the tighter [`WalkOptions::data_file_max_bytes`]
/// cap rather than the global [`MAX_FILE_BYTES`] (issue #1372).
///
/// Why: data-heavy repos (e.g. `cto-reports`) carry thousands of JSON/XML/TXT/log
/// exports that are well under the 1 MiB global cap, so they all pass the walk
/// filter and bloat the index with ~15k non-source files. These four extensions
/// double as legitimate config formats (`package.json`, `tsconfig.json`,
/// small logs) — almost always well under 64 KiB — so a tighter per-extension
/// cap prunes the bulk exports while keeping real config indexable. `yaml`/`yml`/
/// `toml` are deliberately NOT in this set: they are overwhelmingly config, not
/// data dumps, and keep the global 1 MiB cap.
/// What: lowercase extensions (no leading dot) matched against the file extension.
/// Test: `test_data_cap_skips_large_json` and the round-trip walker tests.
pub const DATA_EXTS: &[&str] = &["json", "xml", "txt", "log"];

/// Default tighter size cap (64 KiB) applied to [`DATA_EXTS`] files (issue #1372).
///
/// Why: 64 KiB comfortably covers real config files (a large `package.json` or
/// `tsconfig.json` is a few KiB) while pruning multi-hundred-KiB data exports
/// that the 1 MiB global cap let through. Used as the resolved default when a
/// caller leaves `IndexConfig::data_file_max_bytes` / the wire field unset.
/// What: a `u64` byte count.
/// Test: `test_data_cap_skips_large_json`, `test_data_cap_keeps_small_json`.
pub const DEFAULT_DATA_FILE_MAX_BYTES: u64 = 65_536; // 64 KiB

/// Default extra directory basenames pruned on top of the built-in [`SKIP_DIRS`]
/// (issue #1372).
///
/// Why: common data-export directory names (`data/`, `exports/`, `output/`,
/// `reports/`, `snapshots/`, `results/`) hold machine-generated blobs that bloat
/// the index without source-code signal. They are NOT in the baseline `SKIP_DIRS`
/// because a few projects legitimately keep source under e.g. `build/` — these
/// are surfaced as an editable per-index default so a project can override them.
/// What: lowercase directory basenames; matched the same way as `SKIP_DIRS`.
/// Test: `test_walker_skips_default_data_dirs`.
pub const DEFAULT_EXTRA_SKIP_DIRS: &[&str] = &[
    "data",
    "exports",
    "output",
    "reports",
    "snapshots",
    "results",
];

/// Build the default extra-skip-dir list as owned `String`s (issue #1372).
///
/// Why: serde defaults and `impl Default` for the config structs need an owned
/// `Vec<String>`; [`DEFAULT_EXTRA_SKIP_DIRS`] is a `&[&str]`. One helper keeps
/// the default in a single place so the walker, `IndexConfig`, and `ProjectConfig`
/// never drift.
/// What: maps the static slice to a `Vec<String>`.
/// Test: exercised by the config round-trip tests in `repo_config` / `project_config`.
pub fn default_extra_skip_dirs() -> Vec<String> {
    DEFAULT_EXTRA_SKIP_DIRS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Return `true` when `path` is a [`DATA_EXTS`] file larger than `data_file_max_bytes`
/// (issue #1372).
///
/// Why: layered on top of [`should_skip_path`] (which still enforces the global
/// 1 MiB cap for every extension), this applies the tighter data-file cap only
/// to JSON/XML/TXT/log so bulk data exports are pruned while small config files
/// of the same extension stay indexable. Non-data extensions always return
/// `false` here and keep the global cap.
/// What: lowercases the extension, checks [`DATA_EXTS`] membership, then stats the
/// file and compares its length against `data_file_max_bytes`. A stat failure
/// returns `false` (the walker's `should_skip_path` already handles unreadable
/// files defensively).
/// Test: `test_data_cap_skips_large_json`, `test_data_cap_keeps_small_json`,
/// `test_data_cap_ignores_non_data_ext`.
pub fn exceeds_data_cap(path: &Path, data_file_max_bytes: u64) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !DATA_EXTS.contains(&ext.as_str()) {
        return false;
    }
    match std::fs::metadata(path) {
        Ok(meta) => meta.len() > data_file_max_bytes,
        Err(_) => false,
    }
}

/// If a JS/CSS file's longest line exceeds this and the file has fewer than
/// [`MIN_LINES_FOR_READABLE_JS`] total lines, it is treated as minified.
const MAX_LINE_LEN_FOR_MINIFIED: usize = 500;
/// A readable JS file is expected to have at least this many lines when the
/// longest line is already suspiciously long.
const MIN_LINES_FOR_READABLE_JS: usize = 5;

/// Return `true` when `path` should be silently excluded from indexing based
/// solely on the file name / extension — without reading file contents.
///
/// Covers:
/// - Binary / non-text extensions ([`BINARY_EXTS`]).
/// - Minified or bundled JS/CSS (`*.min.js`, `*.min.css`, `*.bundle.js`,
///   `*.bundle.css`, `*.chunk.js`).
/// - Hashed bundle filenames like `index-ahKOasfG.js` (8+ hex/alnum chars
///   before the `.js` extension, preceded by a `-`).
/// - Files larger than [`MAX_FILE_BYTES`] (requires `fs::metadata` — only
///   called when extension checks pass).
pub fn should_skip_path(path: &Path) -> bool {
    let file_name = match path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n,
        None => return true, // no name → skip
    };

    // Exact-name skip list (lock files, etc.). Case-sensitive: these names
    // are spelled exactly the same way by every tool that produces them.
    if SKIP_FILES.contains(&file_name) {
        return true;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    // Binary / non-text extensions.
    if BINARY_EXTS.iter().any(|b| *b == ext) {
        return true;
    }

    // Minified / bundled filename patterns.
    // *.min.js, *.min.css
    if file_name.ends_with(".min.js")
        || file_name.ends_with(".min.css")
        // *.bundle.js, *.bundle.css
        || file_name.ends_with(".bundle.js")
        || file_name.ends_with(".bundle.css")
        // *.chunk.js
        || file_name.ends_with(".chunk.js")
    {
        return true;
    }

    // Hashed bundle: name like `<stem>-<8+ alnum chars>.<js|css>`.
    // Only applied to JS and CSS since other extensions (e.g. .ts) are never
    // emitted as hashed bundles by bundlers.
    if ext == "js" || ext == "css" {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if is_hashed_bundle_stem(stem) {
                return true;
            }
        }
    }

    // File size guard — avoids reading giant generated files.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX_FILE_BYTES {
            return true;
        }
    }

    false
}

/// Return `true` when any path component of `path` is an excluded directory
/// name from [`SKIP_DIRS`].
///
/// Why: the recursive walker prunes excluded subtrees before descending, but
/// the [`crate::service::watch_loop`] FileWatcher receives raw per-file events
/// and never walks. Without this check a file modified inside `cdk.out/` (or
/// any other build-artefact dir) would still be indexed incrementally,
/// undoing the exclusion that the initial reindex honoured.
/// What: scans every component of `path` against [`SKIP_DIRS`].
/// Test: see `test_path_in_skipped_dir` below.
pub fn path_in_skipped_dir(path: &Path) -> bool {
    path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|name| SKIP_DIRS.contains(&name))
    })
}

/// Return `true` when the *contents* of a JS file look minified even though
/// the filename didn't match a known minified pattern.
///
/// Heuristic: a `.js` (or `.mjs`/`.cjs`) file is treated as minified when it
/// has fewer than [`MIN_LINES_FOR_READABLE_JS`] lines **and** its longest line
/// exceeds [`MAX_LINE_LEN_FOR_MINIFIED`] characters. This catches minified
/// bundles that ship without a `.min.js` suffix.
pub fn should_skip_content(path: &Path, content: &str) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if !matches!(ext.as_str(), "js" | "mjs" | "cjs") {
        return false;
    }

    let line_count = content.lines().count();
    if line_count >= MIN_LINES_FOR_READABLE_JS {
        return false;
    }

    content.lines().any(|l| l.len() > MAX_LINE_LEN_FOR_MINIFIED)
}

/// Helper: return `true` when `stem` ends with `-<8+ alnum chars>`, which is
/// the naming convention used by Vite, webpack, and Rollup for hashed bundles
/// (e.g. `index-ahKOasfG`, `vendor-1a2b3c4d5e6f7a8b`).
fn is_hashed_bundle_stem(stem: &str) -> bool {
    // Find the last '-' separator.
    let Some(dash_pos) = stem.rfind('-') else {
        return false;
    };
    let hash_part = &stem[dash_pos + 1..];
    // Must be ≥ 8 characters, all alphanumeric.
    hash_part.len() >= 8 && hash_part.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Result of a walk. `skipped_dirs` reports how many entries were pruned by
/// [`SKIP_DIRS`] — useful for surfacing skipped paths in CLI output later.
pub struct WalkResult {
    pub files: Vec<PathBuf>,
    pub skipped_dirs: usize,
}

/// Walk `root` recursively and return every source file under it.
///
/// Why: most callers want the standard, default-excluding walk (no Markdown,
/// no `CHANGELOG`, etc.). Issue #77 added a doc-exclusion default; this entry
/// point preserves the legacy short-name signature while applying that
/// default via [`WalkOptions::default`].
/// What: thin wrapper over [`walk_source_files_with_options`] with
/// `WalkOptions::default()`.
/// Test: every existing walker test exercises this entry point.
pub fn walk_source_files(root: &Path) -> WalkResult {
    walk_source_files_with_options(root, &WalkOptions::default())
}

/// Walk `root` recursively, applying the supplied [`WalkOptions`].
///
/// Why: indexes that want documentation indexed (conceptual-search-focused
/// projects, `include_docs: true` in `trusty-search.yaml`) need to bypass the
/// issue #77 default doc exclusion without losing the rest of the walker's
/// hygiene (binary skip, minified detection, SKIP_DIRS pruning). Issue #100
/// switches the underlying engine from `walkdir` to the `ignore` crate so
/// `.gitignore` is honoured by default — previously a gitignored subtree
/// (e.g. `claude-mpm-patch/` full of minified bundles) would dominate the
/// chunk budget before the walker ever reached the actual source tree,
/// producing a "successful" index containing zero real code.
/// What:
/// - Canonicalises `root` up front (resolves symlink components) so emitted
///   file paths are stable regardless of which alias the caller passed in.
///   When canonicalization fails (e.g. the root vanished mid-call), falls back
///   to the input path so the walker still tries — the daemon's
///   `validate_root_path` is the canonical guard, this is defence in depth.
/// - Drives the walk through [`ignore::WalkBuilder`] (ripgrep's engine) with
///   the standard ignore-file set enabled when `opts.respect_gitignore` is
///   `true` (default): `.gitignore`, `.git/info/exclude`, global gitignore,
///   `.ignore`, `.rgignore`, plus parent-directory ignores. When `false`, the
///   walker behaves as it did before issue #100 — only the hardcoded
///   [`SKIP_DIRS`] / [`should_skip_path`] filters apply.
/// - Follows symlinks during traversal so symlinked subdirectories inside the
///   tree (vendored crates, monorepo aliases) are indexed instead of silently
///   skipped.
/// - Filtered by [`SOURCE_EXTS`].
/// - Belt-and-suspenders skip of any directory whose basename is in
///   [`SKIP_DIRS`] or `opts.extra_skip_dirs`, matched ONLY against path
///   components RELATIVE to the walk root (issue #1554: the root prefix
///   itself is never tested so a repo stored at e.g. `/data/repos/myproject`
///   is not silently excluded because `data` appears in
///   [`DEFAULT_EXTRA_SKIP_DIRS`]).
/// - Skips files matching [`should_skip_path`] (minified/binary/large).
/// - When `opts.include_docs` is `false` (default), skips files matching
///   [`is_default_doc_excluded`] (Markdown, CHANGELOG, LICENSE, ...).
/// - Skips entries that fail to stat (does not error out the whole walk).
///
/// Test: `test_default_excludes_markdown_and_changelog`,
/// `test_include_docs_keeps_markdown`,
/// `test_canonicalizes_symlinked_root`,
/// `test_follows_symlinked_subdirectory`,
/// `test_walker_honors_gitignore`,
/// `test_walker_respects_disable_flag`,
/// `test_walker_honors_dot_ignore`,
/// `test_walker_still_skips_hardcoded_dirs`.
pub fn walk_source_files_with_options(root: &Path, opts: &WalkOptions) -> WalkResult {
    let mut files = Vec::new();
    let mut skipped_dirs = 0usize;

    // Resolve any symlink components so emitted file paths never carry a
    // symlink alias the caller didn't intend. Falls back to the input on
    // canonicalize failure (TOCTOU race, permission error) — the daemon-side
    // `validate_root_path` already canonicalises before storing, so this is
    // belt-and-braces for callers that bypass the HTTP entry point.
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    // Configure the `ignore` crate. `standard_filters(true)` is a shorthand
    // that flips on all the ripgrep-equivalent ignore sources at once
    // (`.gitignore`, `.git/info/exclude`, global gitignore, `.ignore`, hidden
    // files); we then explicitly re-enable each source we care about so a
    // future `ignore` release that changes the standard-filter defaults
    // doesn't silently flip behaviour.
    //
    // `hidden(false)` keeps dotfiles in the walk so e.g. `.github/workflows/*`
    // remain indexable — the original `walkdir`-based walker did not skip
    // hidden entries; the only hidden-directory exclusions came from
    // SKIP_DIRS (`.git`, `.cargo`, ...). Honouring `.gitignore` is what
    // prunes `.venv` / `.cache` / `.aws-sam` on projects that gitignore them.
    let mut builder = ignore::WalkBuilder::new(&canonical_root);
    builder
        .follow_links(true)
        .hidden(false)
        .standard_filters(opts.respect_gitignore)
        .git_ignore(opts.respect_gitignore)
        .git_exclude(opts.respect_gitignore)
        .git_global(opts.respect_gitignore)
        .ignore(opts.respect_gitignore)
        .parents(opts.respect_gitignore)
        .require_git(false);

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                skipped_dirs += 1;
                continue;
            }
        };
        // Directories aren't emitted as walker output, but `ignore` may still
        // yield them when permission errors prevent recursion — skip
        // defensively. The `file_type` is `None` for the root entry on some
        // platforms; treat that as "not a file" (the root itself never has a
        // source extension).
        let is_file = entry.file_type().is_some_and(|ft| ft.is_file());
        if !is_file {
            continue;
        }
        let path = entry.path();
        // Belt-and-suspenders: a project without a `.gitignore` (rare but
        // real, e.g. a `target/` left over from someone else's build) still
        // needs the hardcoded skip list to keep `node_modules` etc. out of
        // the index. Match on any component's basename so a nested
        // `vendor/foo/node_modules/bar.js` is still pruned.
        //
        // Issue #1372: also prune any component whose basename is in the
        // per-index `extra_skip_dirs` set (default: data/exports/output/
        // reports/snapshots/results) so data-export trees never reach the
        // indexer. Matched on basename only, identical to SKIP_DIRS semantics.
        //
        // Fix #1554: strip the canonical root prefix BEFORE checking
        // components so that directory names in the ROOT PATH ITSELF (e.g. a
        // repo stored at `/data/repos/myproject`) never match the skip lists.
        // Only path segments that are RELATIVE to the walk root should be
        // tested — callers have no control over what parent directories their
        // root lives under. `strip_prefix` always succeeds here because
        // `WalkBuilder::new(&canonical_root)` only yields entries that begin
        // with `canonical_root`; the `unwrap_or` is a belt-and-suspenders
        // fallback (e.g. a symlinked entry whose resolved path escapes the
        // root after `follow_links`) that preserves the pre-fix behaviour of
        // checking the full path.
        let rel = path.strip_prefix(&canonical_root).unwrap_or(path);
        if rel
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .any(|seg| SKIP_DIRS.contains(&seg) || opts.extra_skip_dirs.iter().any(|d| d == seg))
        {
            continue;
        }
        // Extension allow-list: only known source extensions enter the indexer.
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !SOURCE_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            continue;
        }
        // Path-level skip: minified filenames, binaries, large files (global
        // 1 MiB cap).
        if should_skip_path(path) {
            continue;
        }
        // Issue #1372: tighter per-extension cap for data-ish files
        // (JSON/XML/TXT/log). Layered on top of the global cap above so a
        // 200 KiB data export is pruned while a small `package.json` of the
        // same extension stays indexable.
        if exceeds_data_cap(path, opts.data_file_max_bytes) {
            continue;
        }
        // Issue #77: default doc/changelog/license exclusion. Skipped before
        // the file is read so prose never enters BM25 / vector lanes.
        if !opts.include_docs && is_default_doc_excluded(path) {
            continue;
        }
        files.push(path.to_path_buf());
    }

    WalkResult {
        files,
        skipped_dirs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_source_files_and_skips_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("node_modules/foo")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("src/lib.py"), "x = 1").unwrap();
        fs::write(root.join("README.md"), "# hi").unwrap();
        fs::write(root.join("target/debug/build.o"), b"\0\0").unwrap();
        fs::write(root.join("node_modules/foo/index.js"), "// no").unwrap();
        fs::write(root.join("binary.bin"), b"\0\0").unwrap();

        let result = walk_source_files(root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"lib.py".to_string()));
        // Issue #118: README.md is now INCLUDED by default — the per-mode
        // filter (`is_allowed_for_mode`) keeps it out of code-mode results
        // but text mode needs it to be indexed.
        assert!(
            names.contains(&"README.md".to_string()),
            "Markdown included by default (issue #118)"
        );
        assert!(!names.contains(&"build.o".to_string()));
        assert!(!names.contains(&"index.js".to_string()));
        assert!(!names.contains(&"binary.bin".to_string()));
    }

    // --- Issue #77 default doc-exclusion tests ---

    #[test]
    fn test_is_default_doc_excluded() {
        // Markdown / RST / AsciiDoc / TXT are excluded by extension.
        assert!(is_default_doc_excluded(Path::new("README.md")));
        assert!(is_default_doc_excluded(Path::new("docs/guide.mdx")));
        assert!(is_default_doc_excluded(Path::new("docs/intro.rst")));
        assert!(is_default_doc_excluded(Path::new("CONTRIBUTING.adoc")));
        assert!(is_default_doc_excluded(Path::new("notes.txt")));
        // CHANGELOG / LICENSE / NOTICE excluded by basename substring,
        // case-insensitive, regardless of extension.
        assert!(is_default_doc_excluded(Path::new("CHANGELOG")));
        assert!(is_default_doc_excluded(Path::new("CHANGELOG.md")));
        assert!(is_default_doc_excluded(Path::new("changelog.txt")));
        assert!(is_default_doc_excluded(Path::new("LICENSE")));
        assert!(is_default_doc_excluded(Path::new("License.txt")));
        assert!(is_default_doc_excluded(Path::new("NOTICE")));
        // Normal source files are not flagged.
        assert!(!is_default_doc_excluded(Path::new("src/main.rs")));
        assert!(!is_default_doc_excluded(Path::new("src/lib.py")));
        assert!(!is_default_doc_excluded(Path::new("Cargo.toml")));
    }

    #[test]
    fn test_default_includes_markdown_and_changelog() {
        // Issue #118: by default, the walker now INCLUDES Markdown docs
        // and CHANGELOG-with-extension files so `mode=text` searches have
        // content to return. The per-mode filter (`is_allowed_for_mode`)
        // keeps these chunks out of `mode=code` results, so code-search
        // ranking is unaffected.
        //
        // Note: extension-less LICENSE / NOTICE files are still skipped
        // because the walker's [`SOURCE_EXTS`] allow-list keys on
        // extension. That is a pre-existing limitation independent of
        // this ticket — `LICENSE.md` and `CHANGELOG.md` are the
        // canonical extensioned forms that v0.8.3 now indexes.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# project").unwrap();
        fs::write(root.join("CHANGELOG.md"), "# 1.0.0").unwrap();
        fs::write(root.join("LICENSE.md"), "MIT").unwrap();
        fs::write(root.join("NOTICE.txt"), "(c)").unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/intro.mdx"), "# intro").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(
            names.contains(&"README.md".to_string()),
            "README.md included by default (issue #118): {names:?}"
        );
        assert!(names.contains(&"CHANGELOG.md".to_string()));
        assert!(names.contains(&"LICENSE.md".to_string()));
        assert!(names.contains(&"NOTICE.txt".to_string()));
        assert!(names.contains(&"intro.mdx".to_string()));
    }

    /// Issue #118 acceptance: a fresh reindex of a tempdir with a
    /// `README.md`, `CHANGELOG.md`, and one `lib.rs` produces walk entries
    /// for BOTH the source file and the docs. Pins the contract that
    /// flipping the walker default actually feeds docs into the indexer
    /// pipeline.
    ///
    /// The mode-filter side of the acceptance (`mode=text` returns docs;
    /// `mode=code` returns only `lib.rs`) is covered end-to-end by
    /// `core::indexer::tests::test_mode_filter_code_returns_only_source`
    /// and `test_mode_filter_text_returns_only_prose_and_named_docs`,
    /// which exercise `is_allowed_for_mode` against a seeded indexer.
    #[test]
    fn test_issue_118_acceptance_walks_both_source_and_docs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("lib.rs"), "pub fn entrypoint() {}").unwrap();
        fs::write(
            root.join("README.md"),
            "# Project\nInstall via cargo install trusty-search.",
        )
        .unwrap();
        fs::write(root.join("CHANGELOG.md"), "## 0.8.3\n- fix #118").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"lib.rs".to_string()),
            "source must be walked: {names:?}"
        );
        assert!(
            names.contains(&"README.md".to_string()),
            "README.md must be walked under v0.8.3 default (issue #118): {names:?}"
        );
        assert!(
            names.contains(&"CHANGELOG.md".to_string()),
            "CHANGELOG.md must be walked under v0.8.3 default (issue #118): {names:?}"
        );
    }

    #[test]
    fn test_include_docs_false_excludes_markdown() {
        // Issue #118: the `include_docs: false` opt-out preserves the
        // v0.8.2 walker behaviour for indexes that genuinely don't want
        // prose chunked at all (saves chunks on docs-heavy projects, or
        // restores the legacy walk-time exclusion when the operator
        // configured it explicitly in `trusty-search.yaml`).
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# project").unwrap();
        fs::write(root.join("CHANGELOG.md"), "# 1.0.0").unwrap();

        let names: Vec<String> = walk_source_files_with_options(
            root,
            &WalkOptions {
                include_docs: false,
                ..WalkOptions::default()
            },
        )
        .files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.contains(&"README.md".to_string()));
        assert!(!names.contains(&"CHANGELOG.md".to_string()));
    }

    // --- should_skip_path tests ---

    #[test]
    fn test_skips_min_js() {
        assert!(should_skip_path(Path::new("foo.min.js")));
        assert!(should_skip_path(Path::new("path/to/app.min.js")));
    }

    #[test]
    fn test_skips_min_css() {
        assert!(should_skip_path(Path::new("styles.min.css")));
    }

    #[test]
    fn test_skips_bundle_js() {
        assert!(should_skip_path(Path::new("app.bundle.js")));
        assert!(should_skip_path(Path::new("vendor.bundle.css")));
    }

    #[test]
    fn test_skips_chunk_js() {
        assert!(should_skip_path(Path::new("runtime.chunk.js")));
    }

    #[test]
    fn test_skips_hashed_bundle() {
        // 8+ alphanumeric characters after the last '-'
        assert!(should_skip_path(Path::new("index-ahKOasfG.js")));
        assert!(should_skip_path(Path::new("vendor-1a2b3c4d5e6f7a8b.js")));
        assert!(should_skip_path(Path::new("src/assets/main-AbCdEfGh.js")));
    }

    #[test]
    fn test_hashed_bundle_too_short_not_skipped() {
        // Only 7 chars after '-' — not long enough to be a hash.
        assert!(!should_skip_path(Path::new("foo-abcdefg.js")));
    }

    #[test]
    fn test_keeps_normal_js() {
        assert!(!should_skip_path(Path::new("utils.js")));
        assert!(!should_skip_path(Path::new("main.js")));
        assert!(!should_skip_path(Path::new("src/components/button.js")));
    }

    #[test]
    fn test_skips_node_modules_dir() {
        // Path-based dir skipping happens in the walker; should_skip_path
        // only looks at the file name, not ancestor dirs. But we verify that
        // the walker does exclude node_modules content end-to-end.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::create_dir_all(root.join("node_modules/lodash")).unwrap();
        fs::write(
            root.join("node_modules/lodash/index.js"),
            "module.exports={}",
        )
        .unwrap();
        fs::write(root.join("real.js"), "export const x = 1;").unwrap();

        let result = walk_source_files(root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            !names.contains(&"index.js".to_string()),
            "node_modules must be excluded"
        );
        assert!(names.contains(&"real.js".to_string()));
    }

    #[test]
    fn test_skips_large_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let big_path = tmp.path().join("huge.js");
        // Write just over 1 MiB.
        let big_content = "x".repeat((MAX_FILE_BYTES + 1) as usize);
        fs::write(&big_path, big_content.as_bytes()).unwrap();
        assert!(should_skip_path(&big_path));
    }

    #[test]
    fn test_keeps_small_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let small_path = tmp.path().join("small.js");
        fs::write(&small_path, b"const x = 1;").unwrap();
        assert!(!should_skip_path(&small_path));
    }

    // --- Issue #1372: data-file cap + extra skip dirs ---

    /// A data-ish file (json/xml/txt/log) larger than the 64 KiB default cap
    /// is pruned by `exceeds_data_cap`, even though it is well under the global
    /// 1 MiB `MAX_FILE_BYTES` cap.
    #[test]
    fn test_data_cap_skips_large_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for ext in ["json", "xml", "txt", "log"] {
            let p = tmp.path().join(format!("export.{ext}"));
            // 100 KiB — over 64 KiB cap, under 1 MiB global cap.
            fs::write(&p, "x".repeat(100 * 1024)).unwrap();
            assert!(
                exceeds_data_cap(&p, DEFAULT_DATA_FILE_MAX_BYTES),
                "{ext}: large data file must exceed the 64 KiB cap"
            );
            // The global path-skip (1 MiB) does NOT catch it — proving the
            // tighter data cap is what prunes it.
            assert!(
                !should_skip_path(&p),
                "{ext}: 100 KiB file is under the global 1 MiB cap"
            );
        }
    }

    /// A small data-ish config file (e.g. `package.json`, `tsconfig.json`) stays
    /// indexable under the 64 KiB cap.
    #[test]
    fn test_data_cap_keeps_small_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for name in ["package.json", "config.xml", "notes.txt", "app.log"] {
            let p = tmp.path().join(name);
            fs::write(&p, "{\"a\":1}").unwrap();
            assert!(
                !exceeds_data_cap(&p, DEFAULT_DATA_FILE_MAX_BYTES),
                "{name}: small data file must stay indexable"
            );
        }
    }

    /// Non-data extensions (`.rs`, `.md`, `.toml`) are never subject to the
    /// data cap — only the global 1 MiB cap applies. A 100 KiB `.rs`/`.md`/
    /// `.toml` file (over the 64 KiB data cap, under the 1 MiB global cap)
    /// must still be indexed.
    #[test]
    fn test_data_cap_ignores_non_data_ext() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for ext in ["rs", "md", "toml"] {
            let p = tmp.path().join(format!("big.{ext}"));
            fs::write(&p, "x".repeat(100 * 1024)).unwrap();
            assert!(
                !exceeds_data_cap(&p, DEFAULT_DATA_FILE_MAX_BYTES),
                "{ext}: non-data extension must ignore the data cap"
            );
            assert!(
                !should_skip_path(&p),
                "{ext}: 100 KiB file is under the global 1 MiB cap and must be kept"
            );
        }
    }

    /// End-to-end: the walker prunes large data files but keeps small config
    /// files of the same extension, and keeps large non-data source files.
    #[test]
    fn test_walker_applies_data_cap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        // Large data exports — pruned by the 64 KiB cap.
        fs::write(root.join("dump.json"), "x".repeat(100 * 1024)).unwrap();
        fs::write(root.join("feed.xml"), "x".repeat(100 * 1024)).unwrap();
        // Small config — kept.
        fs::write(root.join("package.json"), "{\"name\":\"x\"}").unwrap();
        // Large non-data source / config — kept (only the 1 MiB cap applies).
        fs::write(
            root.join("Cargo.toml"),
            format!("# {}", "x".repeat(100 * 1024)),
        )
        .unwrap();
        fs::write(
            root.join("big.rs"),
            format!("// {}\nfn x() {{}}", "y".repeat(100 * 1024)),
        )
        .unwrap();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            !names.contains(&"dump.json".to_string()),
            "large json pruned: {names:?}"
        );
        assert!(
            !names.contains(&"feed.xml".to_string()),
            "large xml pruned: {names:?}"
        );
        assert!(
            names.contains(&"package.json".to_string()),
            "small json kept: {names:?}"
        );
        assert!(
            names.contains(&"Cargo.toml".to_string()),
            "large toml kept: {names:?}"
        );
        assert!(
            names.contains(&"big.rs".to_string()),
            "large rs kept: {names:?}"
        );
        assert!(
            names.contains(&"main.rs".to_string()),
            "source kept: {names:?}"
        );
    }

    /// The walker prunes files under each default extra-skip dir
    /// (`data`/`exports`/`output`/`reports`/`snapshots`/`results`) while keeping
    /// normal source elsewhere.
    #[test]
    fn test_walker_skips_default_data_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in DEFAULT_EXTRA_SKIP_DIRS {
            let d = root.join(dir);
            fs::create_dir_all(&d).unwrap();
            // Use a source extension to prove the DIRECTORY is pruned, not the
            // extension.
            fs::write(d.join("trapped.rs"), "fn t() {}").unwrap();
        }
        fs::write(root.join("kept.rs"), "fn k() {}").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"kept.rs".to_string()),
            "normal source kept: {names:?}"
        );
        assert!(
            !names.contains(&"trapped.rs".to_string()),
            "files inside default data dirs must be pruned: {names:?}"
        );
    }

    /// An empty `extra_skip_dirs` (explicit override) means the default data
    /// dirs are NOT pruned — proving the list is honoured rather than hardcoded.
    #[test]
    fn test_walker_extra_skip_dirs_overridable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let d = root.join("reports");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("kept.rs"), "fn k() {}").unwrap();

        let opts = WalkOptions {
            extra_skip_dirs: Vec::new(),
            ..WalkOptions::default()
        };
        let names: Vec<String> = walk_source_files_with_options(root, &opts)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"kept.rs".to_string()),
            "empty extra_skip_dirs must NOT prune reports/: {names:?}"
        );
    }

    /// Regression test for issue #1554: repos stored under a root path whose
    /// absolute path CONTAINS a segment matching `extra_skip_dirs` or `SKIP_DIRS`
    /// (e.g. `/data/repos/myproject`, where `data` is in `DEFAULT_EXTRA_SKIP_DIRS`)
    /// must still have their files walked. Only path segments RELATIVE to the
    /// walk root should be tested against the skip lists; the root prefix itself
    /// must never be matched.
    ///
    /// Before the fix the component check ran over the full absolute path, so
    /// any repo stored under e.g. `/data/…` returned 0 files — all walked
    /// paths started with `/data/…` and the `data` segment hit `extra_skip_dirs`.
    #[test]
    fn test_walk_root_containing_skip_dir_name_still_indexes_files() {
        // Why: reproduces the #1554 regression. A root path whose absolute form
        // contains one of the DEFAULT_EXTRA_SKIP_DIRS names as a component
        // (here `data`) must still produce a non-empty walk.
        // What: creates a tempdir, nests it under a `data/` subdirectory (to
        // mimic e.g. `/data/repos/myproject`), then walks from that inner root.
        // Test: asserts that the walk finds source files rather than 0.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // Simulate a tree like: <tmp>/data/repos/myproject/src/main.rs
        // `data` is in DEFAULT_EXTRA_SKIP_DIRS; it must NOT prune files when
        // it appears in the WALK ROOT PATH, only when it appears under the root.
        let project_dir = root.join("data").join("repos").join("myproject");
        fs::create_dir_all(project_dir.join("src")).unwrap();
        fs::write(project_dir.join("src").join("main.rs"), "fn main() {}").unwrap();
        fs::write(project_dir.join("src").join("lib.py"), "x = 1").unwrap();

        // Walk from the project root (which has "data" in its absolute path).
        let result = walk_source_files(&project_dir);
        assert!(
            !result.files.is_empty(),
            "walk must find files even when root path contains a skip-dir name \
             (issue #1554): got 0 files for root {:?}",
            project_dir
        );
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"main.rs".to_string()),
            "main.rs must be found under a root with 'data' in the path: {names:?}"
        );
        assert!(
            names.contains(&"lib.py".to_string()),
            "lib.py must be found under a root with 'data' in the path: {names:?}"
        );

        // Sanity: a `data/` SUBDIRECTORY *under* the root is still pruned.
        let trapped_dir = project_dir.join("data");
        fs::create_dir_all(&trapped_dir).unwrap();
        fs::write(trapped_dir.join("trapped.rs"), "fn trapped() {}").unwrap();
        let result2 = walk_source_files(&project_dir);
        let names2: Vec<String> = result2
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            !names2.contains(&"trapped.rs".to_string()),
            "data/ SUBDIR under root must still be pruned: {names2:?}"
        );
    }

    // --- should_skip_content tests ---

    #[test]
    fn test_skip_content_detects_minified_js() {
        // A single line longer than 500 chars with fewer than 5 total lines.
        let minified = "a".repeat(501);
        assert!(should_skip_content(Path::new("bundle.js"), &minified));
    }

    #[test]
    fn test_skip_content_allows_normal_js() {
        let normal = "const x = 1;\nconst y = 2;\nconst z = 3;\nconst w = 4;\nconst v = 5;\n";
        assert!(!should_skip_content(Path::new("app.js"), normal));
    }

    #[test]
    fn test_skip_content_ignores_non_js() {
        // Even with a very long line, non-JS files are not skipped by this check.
        let long_line = "x".repeat(1000);
        assert!(!should_skip_content(Path::new("data.rs"), &long_line));
        assert!(!should_skip_content(Path::new("query.py"), &long_line));
    }

    #[test]
    fn test_skip_content_mjs_cjs() {
        let minified = "a".repeat(501);
        assert!(should_skip_content(Path::new("mod.mjs"), &minified));
        assert!(should_skip_content(Path::new("mod.cjs"), &minified));
    }

    // --- SKIP_FILES / lock file tests ---

    #[test]
    fn test_skips_lock_files() {
        assert!(should_skip_path(Path::new("Cargo.lock")));
        assert!(should_skip_path(Path::new("project/Cargo.lock")));
        assert!(should_skip_path(Path::new("package-lock.json")));
        assert!(should_skip_path(Path::new("yarn.lock")));
        assert!(should_skip_path(Path::new("pnpm-lock.yaml")));
        assert!(should_skip_path(Path::new("poetry.lock")));
        assert!(should_skip_path(Path::new("Pipfile.lock")));
        assert!(should_skip_path(Path::new("Gemfile.lock")));
        assert!(should_skip_path(Path::new("composer.lock")));
        assert!(should_skip_path(Path::new("go.sum")));
    }

    #[test]
    fn test_does_not_skip_non_lock_named_files() {
        // Case-sensitive: "cargo.lock" (lowercase) is not Cargo.lock, but the
        // `.lock` extension is still in BINARY_EXTS so it would be skipped
        // anyway. Verify a clearly unrelated filename passes through.
        assert!(!should_skip_path(Path::new("main.rs")));
        assert!(!should_skip_path(Path::new("locked_file.rs")));
        assert!(!should_skip_path(Path::new("my-cargo-locker.py")));
    }

    #[test]
    fn test_walker_skips_lock_files_in_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("Cargo.lock"), "# lock").unwrap();
        fs::write(root.join("package-lock.json"), "{}").unwrap();
        fs::write(root.join("yarn.lock"), "# lock").unwrap();
        fs::write(root.join("real.rs"), "fn main() {}").unwrap();

        let result = walk_source_files(root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"real.rs".to_string()));
        assert!(!names.contains(&"Cargo.lock".to_string()));
        assert!(!names.contains(&"package-lock.json".to_string()));
        assert!(!names.contains(&"yarn.lock".to_string()));
    }

    // --- SKIP_DIRS new entries ---

    #[test]
    fn test_walker_skips_new_skip_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in [".cache", ".npm", ".build", ".pnpm-store", ".yarn", ".tox"] {
            let d = root.join(dir);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("trapped.rs"), "fn x() {}").unwrap();
        }
        fs::write(root.join("kept.rs"), "fn k() {}").unwrap();

        let result = walk_source_files(root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"kept.rs".to_string()));
        assert!(
            !names.contains(&"trapped.rs".to_string()),
            "files inside new SKIP_DIRS must be excluded"
        );
    }

    #[test]
    fn test_walker_skips_cdk_and_sam_dirs() {
        // Issue #129: AWS CDK / SAM synth output must be pruned by default.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in ["cdk.out", "cdk.out2", ".aws-sam", ".turbo", ".mvn"] {
            // Nest a file deep inside, mimicking a synthesized Lambda venv.
            let d = root.join(dir).join("asset.abc123/python/lib");
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("vendored.py"), "import boto3").unwrap();
        }
        fs::write(root.join("handler.py"), "def handler(): pass").unwrap();

        let result = walk_source_files(root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"handler.py".to_string()),
            "real source must be kept"
        );
        assert!(
            !names.contains(&"vendored.py".to_string()),
            "files inside cdk.out / .aws-sam / .turbo / .mvn must be excluded"
        );
    }

    #[test]
    fn test_walker_skips_fixture_and_test_data_dirs() {
        // Issue #130: test fixtures / static test data must be pruned by
        // default — SQL dumps and sample payloads bloat the index.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in [
            "fixtures",
            "__fixtures__",
            "testdata",
            "test-data",
            "test_data",
            "testresources",
            "test_resources",
        ] {
            let d = root.join(dir);
            fs::create_dir_all(&d).unwrap();
            // .sql is already excluded by the SOURCE_EXTS allowlist; use a
            // source extension to prove the directory itself is pruned.
            fs::write(d.join("sample.py"), "x = 1").unwrap();
        }
        // Basename matching keeps src/test/resources indexed (no bare
        // `resources` entry), so a file there must survive the walk.
        let kept_resources = root.join("src/test/resources");
        fs::create_dir_all(&kept_resources).unwrap();
        fs::write(kept_resources.join("config.py"), "y = 2").unwrap();
        fs::write(root.join("handler.py"), "def handler(): pass").unwrap();

        let result = walk_source_files(root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"handler.py".to_string()),
            "real source must be kept"
        );
        assert!(
            names.contains(&"config.py".to_string()),
            "src/test/resources must stay indexed (basename `resources` not skipped)"
        );
        assert!(
            !names.contains(&"sample.py".to_string()),
            "files inside fixture / test-data dirs must be excluded"
        );
    }

    #[test]
    fn test_skip_dirs_contains_fixture_entries() {
        // Issue #130 acceptance: fixture / test-data dirs are default exclusions.
        for required in [
            "fixtures",
            "__fixtures__",
            "testdata",
            "test-data",
            "test_data",
            "testresources",
            "test_resources",
        ] {
            assert!(
                SKIP_DIRS.contains(&required),
                "SKIP_DIRS missing required fixture entry: {required}"
            );
        }
    }

    #[test]
    fn test_sql_extension_excluded_by_allowlist() {
        // Issue #130: .sql files carry no source-code signal. They are
        // excluded by the SOURCE_EXTS allowlist — `sql` is intentionally
        // absent — so no explicit SKIP_FILES / extension entry is needed.
        assert!(
            !SOURCE_EXTS.iter().any(|e| e.eq_ignore_ascii_case("sql")),
            "`sql` must not be in SOURCE_EXTS — SQL is excluded by the allowlist"
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("schema.sql"), "CREATE TABLE t (id INT);").unwrap();
        fs::write(root.join("real.rs"), "fn main() {}").unwrap();

        let result = walk_source_files(root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"real.rs".to_string()));
        assert!(
            !names.contains(&"schema.sql".to_string()),
            ".sql files must be excluded by the SOURCE_EXTS allowlist"
        );
    }

    #[test]
    fn test_skip_dirs_contains_build_artifact_entries() {
        // Issue #129 acceptance: AWS CDK / SAM and common build dirs must be
        // default exclusions (no per-project .trusty-search-ignore needed).
        for required in [
            "cdk.out",
            "cdk.out2",
            ".aws-sam",
            ".turbo",
            ".mvn",
            ".gradle",
            "node_modules",
            ".venv",
            "venv",
            "__pycache__",
            ".next",
            "dist",
            "build",
            "target",
            "vendor",
        ] {
            assert!(
                SKIP_DIRS.contains(&required),
                "SKIP_DIRS missing required build-artifact entry: {required}"
            );
        }
    }

    #[test]
    fn test_skip_dirs_contains_required_entries() {
        // Issue #12 acceptance: these directory names must be in the skip list.
        for required in [
            "node_modules",
            "target",
            "vendor",
            ".git",
            ".cargo",
            ".npm",
            "dist",
            "build",
            ".build",
            "__pycache__",
            ".venv",
            "venv",
            ".next",
            ".nuxt",
            "coverage",
            ".nyc_output",
        ] {
            assert!(
                SKIP_DIRS.contains(&required),
                "SKIP_DIRS missing required entry: {required}"
            );
        }
    }

    #[test]
    fn test_path_in_skipped_dir() {
        // Issue #129: per-file watcher events must respect SKIP_DIRS.
        assert!(path_in_skipped_dir(Path::new(
            "project/cdk.out/asset.abc/python/handler.py"
        )));
        assert!(path_in_skipped_dir(Path::new(".aws-sam/build/app.py")));
        assert!(path_in_skipped_dir(Path::new(
            "repo/node_modules/lodash/x.js"
        )));
        assert!(path_in_skipped_dir(Path::new("repo/.turbo/cache/x.js")));
        // Normal source paths are not flagged.
        assert!(!path_in_skipped_dir(Path::new("src/handler.py")));
        assert!(!path_in_skipped_dir(Path::new("project/src/main.rs")));
    }

    #[test]
    fn test_skip_content_multiline_js_not_skipped() {
        // 5+ lines, even if one line is long.
        let content = format!("line1\nline2\nline3\nline4\nline5\n{}\n", "x".repeat(600));
        assert!(!should_skip_content(Path::new("ok.js"), &content));
    }

    // --- Symlink resolution tests (issue: indexed-paths-mismatch) ---

    /// When the walk root is itself a symlink to the real directory, every
    /// emitted file path must start with the canonical (real) directory, not
    /// the symlink alias. This is the daemon-side guard against the bug where
    /// indexing `/Users/me/Projects/foo` (a symlink to `/Volumes/Disk/...`)
    /// stored paths under the symlink alias and search from the canonical
    /// mount returned zero hits.
    #[cfg(unix)]
    #[test]
    fn test_canonicalizes_symlinked_root() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        // `tempfile::tempdir()` on macOS already returns a `/var/folders/...`
        // path whose `/var` is a symlink to `/private/var`. Canonicalise once
        // up front so the expected-prefix check below doesn't trip on the OS's
        // own symlink rather than ours.
        let real_root = std::fs::canonicalize(tmp.path()).expect("canonicalize real root");
        fs::write(real_root.join("main.rs"), "fn main() {}").unwrap();

        // Create a sibling symlink that points at `real_root`, then walk via
        // the symlink path. Every emitted file must live under `real_root`.
        let parent = real_root.parent().expect("tempdir has parent");
        let link_path = parent.join(format!(
            "trusty-search-symlink-{}",
            std::process::id() // unique per test run
        ));
        // Best-effort cleanup of any stale link from a previous run.
        let _ = std::fs::remove_file(&link_path);
        symlink(&real_root, &link_path).expect("create symlink");

        let result = walk_source_files(&link_path);
        // Always clean up the symlink, even on test failure.
        let _ = std::fs::remove_file(&link_path);

        assert!(!result.files.is_empty(), "walker emitted no files");
        for f in &result.files {
            assert!(
                f.starts_with(&real_root),
                "emitted file {f:?} does not start with canonical root {real_root:?}",
            );
            assert!(
                !f.starts_with(&link_path),
                "emitted file {f:?} carries the symlink alias instead of canonical path",
            );
        }
    }

    /// When a symlinked subdirectory inside the tree points at a directory of
    /// source files, those files must be indexed. Historically `follow_links`
    /// was disabled so vendored / monorepo-aliased subtrees were silently
    /// skipped.
    #[cfg(unix)]
    #[test]
    fn test_follows_symlinked_subdirectory() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize root");

        // Real source dir lives outside the walked root; we link it in.
        let extern_dir = tempfile::tempdir().expect("extern tempdir");
        let extern_root = std::fs::canonicalize(extern_dir.path()).expect("canonicalize extern");
        fs::write(extern_root.join("linked.rs"), "fn linked() {}").unwrap();

        // Plain file inside the root so we have a baseline.
        fs::write(root.join("local.rs"), "fn local() {}").unwrap();
        // Symlinked subdirectory: walking should descend into it.
        symlink(&extern_root, root.join("vendored")).expect("symlink subdir");

        let result = walk_source_files(&root);
        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"local.rs".to_string()),
            "baseline local file missing: {names:?}",
        );
        assert!(
            names.contains(&"linked.rs".to_string()),
            "file inside symlinked subdir was not indexed: {names:?}",
        );
    }

    // --- Issue #100: .gitignore honoured by default ----------------------

    /// The walker must honour `.gitignore`. Without this, a project that
    /// gitignores a noisy subtree (e.g. `claude-mpm-patch/` full of minified
    /// JS bundles) sees that subtree dominate the chunk budget and the real
    /// source tree is silently dropped — the failure mode described in
    /// issue #100. The fix swaps `walkdir` for the `ignore` crate
    /// (ripgrep's engine).
    #[test]
    fn test_walker_honors_gitignore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        // A `.gitignore` that excludes the `excluded/` subtree.
        fs::write(root.join(".gitignore"), "excluded/\n").unwrap();
        fs::create_dir_all(root.join("excluded")).unwrap();
        fs::create_dir_all(root.join("included")).unwrap();
        fs::write(root.join("excluded/foo.rs"), "fn foo() {}").unwrap();
        fs::write(root.join("included/bar.rs"), "fn bar() {}").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"bar.rs".to_string()),
            "included file dropped: {names:?}"
        );
        assert!(
            !names.contains(&"foo.rs".to_string()),
            "gitignored file not pruned: {names:?}"
        );
    }

    /// `respect_gitignore: false` restores the legacy walk-everything
    /// behaviour for callers that want to index a vendored subtree
    /// intentionally.
    #[test]
    fn test_walker_respects_disable_flag() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join(".gitignore"), "excluded/\n").unwrap();
        fs::create_dir_all(root.join("excluded")).unwrap();
        fs::create_dir_all(root.join("included")).unwrap();
        fs::write(root.join("excluded/foo.rs"), "fn foo() {}").unwrap();
        fs::write(root.join("included/bar.rs"), "fn bar() {}").unwrap();

        let opts = WalkOptions {
            include_docs: false,
            respect_gitignore: false,
            ..WalkOptions::default()
        };
        let names: Vec<String> = walk_source_files_with_options(root, &opts)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"bar.rs".to_string()),
            "included file dropped: {names:?}"
        );
        assert!(
            names.contains(&"foo.rs".to_string()),
            "respect_gitignore=false still pruned gitignored file: {names:?}"
        );
    }

    /// `.ignore` (ripgrep's non-git ignore file) is honoured by the same
    /// machinery as `.gitignore`. Projects use it to add walker-only
    /// exclusions that they don't want committed to `.gitignore`.
    #[test]
    fn test_walker_honors_dot_ignore() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join(".ignore"), "excluded/\n").unwrap();
        fs::create_dir_all(root.join("excluded")).unwrap();
        fs::create_dir_all(root.join("included")).unwrap();
        fs::write(root.join("excluded/foo.rs"), "fn foo() {}").unwrap();
        fs::write(root.join("included/bar.rs"), "fn bar() {}").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"bar.rs".to_string()),
            "included file dropped: {names:?}"
        );
        assert!(
            !names.contains(&"foo.rs".to_string()),
            ".ignore file not honoured: {names:?}"
        );
    }

    /// The hardcoded [`SKIP_DIRS`] list remains the floor: projects without
    /// any `.gitignore` (a fresh `cargo new` that never ran git, a vendored
    /// blob the operator dropped in raw) still get `target/` and
    /// `node_modules/` pruned. This guards against regressions where
    /// flipping the ignore engine accidentally relied on the gitignore to
    /// carry those exclusions.
    #[test]
    fn test_walker_still_skips_hardcoded_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        // No `.gitignore` — `ignore` has nothing to honour. The hardcoded
        // SKIP_DIRS filter must still drop these subtrees.
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join("node_modules/foo")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::write(root.join("target/debug/build.rs"), "fn b() {}").unwrap();
        fs::write(root.join("node_modules/foo/index.js"), "// pkg").unwrap();
        // `.git` doesn't contain source extensions, but planting one
        // proves the skip still applies even when extensions match.
        fs::write(root.join(".git/objects/blob.rs"), "fn g() {}").unwrap();
        fs::write(root.join("real.rs"), "fn real() {}").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(
            names.contains(&"real.rs".to_string()),
            "real source dropped: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "build.rs"),
            "target/ leaked into walk: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "index.js"),
            "node_modules/ leaked into walk: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "blob.rs"),
            ".git/ leaked into walk: {names:?}"
        );
    }
}
