//! File discovery + test-file classification for the linter's scope.
//!
//! Why: DOC-38's linkage scope is production source, not test fixtures. Test
//! files legitimately embed synthetic references (`docs/specs/x.md#…`) inside
//! string fixtures; scanning them would produce false "unresolved" failures. The
//! same test/benchmark classification the workspace's line-cap gate uses is
//! applied here, so the two gates agree on what is test code.
//! What: [`is_test_file`] (basename/`/tests/`/`/benches/` classification) and
//! [`discover`] — the repo-relative spec-doc and code-file lists under
//! `docs/specs/**` and `crates/**`, skipping `target/`, hidden dirs, and tests.
//! Test: `super::tests::discover_classifies_tests`.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// Non-Markdown source extensions the linter scans for inline `# Spec References`.
///
/// Why: DOC-38 §3 fixes the set of languages with an inline comment idiom; an
/// extension outside this set has no registered idiom and is skipped (the
/// resolver never guesses — §1.2 G4).
/// What: the extensions covered by `sld::syntax_for_extension`.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "mjs", "cjs", "sh", "bash", "toml", "yaml", "yml",
];

/// The discovered files in scope, split by how they are linted.
///
/// Why: spec docs get the full §4 spec-document checks plus frontmatter
/// resolution, while code files only get inline-reference resolution; separating
/// them keeps the orchestration in [`crate::run`] declarative.
/// What: `spec_docs` are `docs/specs/**/*.md` (excluding the catalog `README.md`);
/// `code_files` are non-test `crates/**` source in [`CODE_EXTENSIONS`]. Paths are
/// repo-relative (from `root`).
/// Test: `super::tests::discover_classifies_tests`.
#[derive(Debug, Default)]
pub struct Discovered {
    /// `docs/specs/**/*.md` spec documents (repo-relative), minus `README.md`.
    pub spec_docs: Vec<PathBuf>,
    /// Non-test `crates/**` code files in [`CODE_EXTENSIONS`] (repo-relative).
    pub code_files: Vec<PathBuf>,
}

/// True when a path is a test or benchmark file (line-cap's classification).
///
/// Why: test fixtures carry synthetic references that must not be resolved;
/// mirroring the line-cap rule keeps "what counts as test code" consistent
/// across the repo's gates.
/// What: returns `true` when the basename is `tests.rs`, ends with `_test.rs` /
/// `_tests.rs`, or the path contains a `/tests/` or `/benches/` directory
/// segment.
/// Test: `super::tests::discover_classifies_tests`.
#[must_use]
pub fn is_test_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name == "tests.rs" || name.ends_with("_test.rs") || name.ends_with("_tests.rs") {
        return true;
    }
    path.components()
        .any(|c| matches!(c.as_os_str().to_str(), Some("tests") | Some("benches")))
}

/// Walk `root` for the spec docs and code files the linter checks.
///
/// Why: the linter's scope (DOC-38 linter directive) is `docs/specs/**/*.md`
/// always and inline blocks across `crates/**`; discovering exactly those keeps
/// the run fast and avoids build-artifact noise.
/// What: recurses `root/docs/specs` for `*.md` (excluding `README.md`) and
/// `root/crates` for non-test [`CODE_EXTENSIONS`] files, skipping `target/` and
/// hidden directories. Returns repo-relative paths, sorted for determinism.
/// Test: `super::tests::discover_classifies_tests`.
#[must_use]
pub fn discover(root: &Path) -> Discovered {
    let mut out = Discovered::default();

    for entry in walk(&root.join("docs/specs")) {
        let rel = relativize(root, &entry);
        if entry.extension().and_then(|e| e.to_str()) == Some("md")
            && entry.file_name().and_then(|n| n.to_str()) != Some("README.md")
        {
            out.spec_docs.push(rel);
        }
    }

    for entry in walk(&root.join("crates")) {
        if is_test_file(&entry) {
            continue;
        }
        let is_code = entry
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| CODE_EXTENSIONS.contains(&e));
        if is_code {
            out.code_files.push(relativize(root, &entry));
        }
    }

    out.spec_docs.sort();
    out.code_files.sort();
    out
}

/// Recursively list files under `dir`, skipping `target/` and hidden dirs.
///
/// Why: a self-contained walk (no `git` dependency at runtime) that avoids build
/// artifacts keeps the linter usable in any checkout and in CI.
/// What: a [`WalkDir`] filtered to files, pruning any directory named `target`
/// or beginning with `.`.
/// Test: covered by `super::tests::discover_classifies_tests`.
fn walk(dir: &Path) -> impl Iterator<Item = PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_str().unwrap_or("");
            !(name == "target" || (name.starts_with('.') && name != "."))
        })
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
}

/// Strip `root` from `path`, yielding a repo-relative path.
///
/// Why: diagnostics and reference lookups key on repo-root-relative paths (§2.1),
/// not absolute ones.
/// What: returns `path` with the `root` prefix removed, or `path` unchanged when
/// it is not under `root`.
/// Test: covered by `super::tests::discover_classifies_tests`.
fn relativize(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}
