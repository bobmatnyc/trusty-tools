//! Source file walker for `reindex`.
//!
//! Why: Reindexing a project requires enumerating all source files under a
//! root directory while skipping VCS/build/dependency dirs and binary noise.
//! What: `walk_source_files(root)` returns every source file (filtered by
//! [`SOURCE_EXTS`]) below `root`, skipping any directory whose name appears
//! in [`SKIP_DIRS`], plus [`should_skip_path`] and [`should_skip_content`]
//! for minification / size guards applied at read time.
//! Test: see the unit tests at the bottom.

use std::path::{Path, PathBuf};
use walkdir::WalkDir;

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

/// Walk options controlling default-exclusion behaviour (issue #77).
///
/// Why: most indexes want the default exclusion of `*.md`, `CHANGELOG*`, etc.
/// applied at walk time so docs never enter the BM25 / vector lanes. A small
/// number of indexes (documentation-focused projects, conceptual search) need
/// to opt in via `include_docs: true`. Keeping the toggle as a struct makes
/// future additions (e.g. `include_configs`) non-breaking.
/// What: a `Copy` struct passed to [`walk_source_files_with_options`]. The
/// default is `include_docs: false` (the new behaviour for issue #77); the
/// legacy [`walk_source_files`] entry point preserves the previous behaviour
/// by calling through with `include_docs: false`.
/// Test: `test_default_excludes_markdown_and_changelog`,
///   `test_include_docs_keeps_markdown`.
#[derive(Debug, Clone, Copy, Default)]
pub struct WalkOptions {
    /// When `false` (default), files matching [`DOC_EXCLUDE_EXTS`] or
    /// [`DOC_EXCLUDE_BASENAME_SUBSTRINGS`] are pruned before they reach the
    /// indexer. When `true`, they are walked normally.
    pub include_docs: bool,
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
    walk_source_files_with_options(root, WalkOptions::default())
}

/// Walk `root` recursively, applying the supplied [`WalkOptions`].
///
/// Why: indexes that want documentation indexed (conceptual-search-focused
/// projects, `include_docs: true` in `trusty-search.yaml`) need to bypass the
/// issue #77 default doc exclusion without losing the rest of the walker's
/// hygiene (binary skip, minified detection, SKIP_DIRS pruning).
/// What:
/// - Canonicalises `root` up front (resolves symlink components) so emitted
///   file paths are stable regardless of which alias the caller passed in.
///   When canonicalization fails (e.g. the root vanished mid-call), falls back
///   to the input path so the walker still tries — the daemon's
///   `validate_root_path` is the canonical guard, this is defence in depth.
/// - Follows symlinks during traversal so symlinked subdirectories inside the
///   tree (vendored crates, monorepo aliases) are indexed instead of silently
///   skipped.
/// - Filtered by [`SOURCE_EXTS`].
/// - Skips any directory whose basename is in [`SKIP_DIRS`].
/// - Skips files matching [`should_skip_path`] (minified/binary/large).
/// - When `opts.include_docs` is `false` (default), skips files matching
///   [`is_default_doc_excluded`] (Markdown, CHANGELOG, LICENSE, ...).
/// - Skips entries that fail to stat (does not error out the whole walk).
///
/// Test: `test_default_excludes_markdown_and_changelog`,
/// `test_include_docs_keeps_markdown`,
/// `test_canonicalizes_symlinked_root`,
/// `test_follows_symlinked_subdirectory`.
pub fn walk_source_files_with_options(root: &Path, opts: WalkOptions) -> WalkResult {
    let mut files = Vec::new();
    let mut skipped_dirs = 0usize;

    // Resolve any symlink components so emitted file paths never carry a
    // symlink alias the caller didn't intend. Falls back to the input on
    // canonicalize failure (TOCTOU race, permission error) — the daemon-side
    // `validate_root_path` already canonicalises before storing, so this is
    // belt-and-braces for callers that bypass the HTTP entry point.
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

    let walker = WalkDir::new(&canonical_root).follow_links(true).into_iter();
    let walker = walker.filter_entry(|e| {
        if e.depth() == 0 {
            return true;
        }
        if e.file_type().is_dir() {
            let name = e.file_name().to_string_lossy();
            if SKIP_DIRS.iter().any(|d| *d == name) {
                return false;
            }
        }
        true
    });

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                skipped_dirs += 1;
                continue;
            }
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        // Extension allow-list: only known source extensions enter the indexer.
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !SOURCE_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            continue;
        }
        // Path-level skip: minified filenames, binaries, large files.
        if should_skip_path(path) {
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
        // Issue #77: README.md is now excluded by the default walk options.
        assert!(
            !names.contains(&"README.md".to_string()),
            "Markdown excluded by default (issue #77)"
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
    fn test_default_excludes_markdown_and_changelog() {
        // Issue #77: by default, the walker must skip Markdown docs and
        // CHANGELOG/LICENSE files so prose hits never enter BM25.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# project").unwrap();
        fs::write(root.join("CHANGELOG.md"), "# 1.0.0").unwrap();
        fs::write(root.join("LICENSE"), "MIT").unwrap();
        fs::write(root.join("NOTICE"), "(c)").unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/intro.mdx"), "# intro").unwrap();

        let names: Vec<String> = walk_source_files(root)
            .files
            .iter()
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(!names.contains(&"README.md".to_string()));
        assert!(!names.contains(&"CHANGELOG.md".to_string()));
        assert!(!names.contains(&"LICENSE".to_string()));
        assert!(!names.contains(&"NOTICE".to_string()));
        assert!(!names.contains(&"intro.mdx".to_string()));
    }

    #[test]
    fn test_include_docs_keeps_markdown() {
        // Issue #77: the `include_docs: true` opt-in re-enables the legacy
        // behaviour for doc-focused indexes.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("README.md"), "# project").unwrap();
        fs::write(root.join("CHANGELOG.md"), "# 1.0.0").unwrap();

        let names: Vec<String> =
            walk_source_files_with_options(root, WalkOptions { include_docs: true })
                .files
                .iter()
                .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
                .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"README.md".to_string()));
        assert!(names.contains(&"CHANGELOG.md".to_string()));
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
}
