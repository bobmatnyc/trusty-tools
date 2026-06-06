//! Shared file-type filtering for search and read tools.
//!
//! Why: Multiple tools (SearchCodeTool, GrepFilesTool, SearchDocsTool) each
//! need to decide which files to include. Centralise the logic here so the
//! same skip-list (binaries, build artifacts, vendored deps) applies
//! everywhere — and so adding a new extension once unlocks every search tool.
//! What: Constants for max file size, skip directories, and allowed
//! extensions; plus `should_skip_file` / `should_skip_dir` helpers.
//! Test: See unit tests at the bottom — extension allowlist, directory
//! denylist, and large-file rejection all covered.

use std::path::{Component, Path};

/// Maximum file size to read/index (512 KB).
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

/// Directories to always skip during search/index walks.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".build",
    ".next",
    ".nuxt",
    "out",
    "coverage",
    ".turbo",
    ".cache",
    ".parcel-cache",
    "vendor",
];

/// File extensions that are allowed (code + text/docs).
pub const ALLOWED_EXTENSIONS: &[&str] = &[
    // Systems / compiled
    "rs",
    "c",
    "cpp",
    "cc",
    "cxx",
    "h",
    "hpp",
    "cs",
    "swift",
    "kt",
    "scala",
    "java",
    "go",
    // Scripting / interpreted
    "py",
    "rb",
    "lua",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "mli",
    "clj",
    "cljs",
    "r",
    "jl",
    // Web / frontend
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "vue",
    "svelte",
    "html",
    "htm",
    "css",
    "scss",
    "sass",
    "less",
    // Shell
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    // Data / config
    "toml",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "json5",
    "xml",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    // SQL / data
    "sql",
    "csv",
    "tsv",
    // Docs / text
    "md",
    "mdx",
    "txt",
    "rst",
    "adoc",
    "org",
    // Docker / infra
    "dockerfile",
    "tf",
    "hcl",
    "proto",
];

/// Filenames (no extension) that are still considered text files worth
/// indexing/searching.
const ALLOWED_NO_EXT_NAMES: &[&str] = &[
    "Dockerfile",
    "Makefile",
    "Justfile",
    "Procfile",
    "README",
    "LICENSE",
    "CHANGELOG",
    "CONTRIBUTING",
    ".env",
    ".gitignore",
    ".dockerignore",
    ".editorconfig",
];

/// Returns true if the path should be skipped entirely (binary, too large,
/// inside a skipped directory, or unrecognised type).
///
/// Why: Centralises the "is this a useful text file?" decision so search,
/// grep, and indexing tools all agree on what to walk.
/// What: Checks (in order) — path components against `SKIP_DIRS`, file size
/// against `MAX_FILE_BYTES`, then extension against `ALLOWED_EXTENSIONS`
/// (falling back to `ALLOWED_NO_EXT_NAMES` for files without extensions).
/// Test: `should_skip_file_*` cases below.
pub fn should_skip_file(path: &Path) -> bool {
    // 1. Skip if inside a denied directory anywhere along the path.
    for component in path.components() {
        if let Component::Normal(name) = component
            && let Some(s) = name.to_str()
            && SKIP_DIRS.contains(&s)
        {
            return true;
        }
    }

    // 2. Skip if too large (only check when metadata is available — missing
    //    files are caller's problem).
    if let Ok(meta) = path.metadata()
        && meta.is_file()
        && meta.len() > MAX_FILE_BYTES
    {
        return true;
    }

    // 3. Check extension against allowlist.
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => !ALLOWED_EXTENSIONS.contains(&ext.to_lowercase().as_str()),
        None => {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !ALLOWED_NO_EXT_NAMES.contains(&name)
        }
    }
}

/// Returns true if the directory component name should be skipped during
/// recursive traversal.
///
/// Why: Walkers can avoid descending into vendored / generated trees by
/// pruning at the directory level rather than filtering files individually.
/// What: Checks `SKIP_DIRS`.
/// Test: `should_skip_dir_rejects_git`.
pub fn should_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn should_skip_file_rejects_binary_extensions() {
        assert!(should_skip_file(&PathBuf::from("foo.png")));
        assert!(should_skip_file(&PathBuf::from("bin/app.exe")));
        assert!(should_skip_file(&PathBuf::from("fonts/Inter.woff")));
    }

    #[test]
    fn should_skip_file_allows_code_extensions() {
        // Use names that do not exist on disk so metadata() returns Err and
        // size-check is skipped.
        assert!(!should_skip_file(&PathBuf::from("missing-file.rs")));
        assert!(!should_skip_file(&PathBuf::from("missing-file.py")));
        assert!(!should_skip_file(&PathBuf::from("missing-file.ts")));
        assert!(!should_skip_file(&PathBuf::from("missing-file.md")));
        assert!(!should_skip_file(&PathBuf::from("missing-file.toml")));
    }

    #[test]
    fn should_skip_file_rejects_node_modules() {
        assert!(should_skip_file(&PathBuf::from(
            "project/node_modules/foo/index.js"
        )));
        assert!(should_skip_file(&PathBuf::from("target/debug/build.rs")));
        assert!(should_skip_file(&PathBuf::from(".git/HEAD")));
    }

    #[test]
    fn should_skip_file_rejects_large_files() {
        let dir = std::env::temp_dir().join(format!(
            "trusty-agents-file-filter-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.rs");
        // Write 600 KB of zeros — over the 512 KB threshold.
        let bytes = vec![b'/'; 600 * 1024];
        std::fs::write(&big, &bytes).unwrap();
        assert!(should_skip_file(&big));
        // Cleanup (best-effort).
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_skip_dir_rejects_git() {
        assert!(should_skip_dir(".git"));
        assert!(should_skip_dir("node_modules"));
        assert!(should_skip_dir("target"));
        assert!(!should_skip_dir("src"));
        assert!(!should_skip_dir("docs"));
    }

    #[test]
    fn should_skip_file_allows_dockerfile_without_extension() {
        assert!(!should_skip_file(&PathBuf::from("Dockerfile")));
        assert!(!should_skip_file(&PathBuf::from("Makefile")));
        assert!(!should_skip_file(&PathBuf::from("README")));
    }

    #[test]
    fn should_skip_file_rejects_unknown_no_extension() {
        assert!(should_skip_file(&PathBuf::from("randombinary")));
    }
}
