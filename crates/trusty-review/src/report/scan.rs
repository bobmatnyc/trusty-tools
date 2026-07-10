//! Built-in repository scanning for baseline metrics (#2342.3 / wave 2).
//!
//! Why: owner feedback on the first generated reports was that a bare run
//! produced a mostly-empty template — "it's the project's job to infer these".
//! The repository being inspected IS the data source, so trusty-review must
//! compute baseline metrics DIRECTLY from a local checkout instead of requiring
//! a pre-produced trusty-analyze metrics JSON.  These scanned figures are
//! `measured` provenance (see [`super::provenance`]) and are real source data:
//! the numeric guardrail's allowed-set includes them.  External metrics JSON
//! becomes ENRICHMENT layered on top (see [`super::model`]), not a prerequisite.
//! What: [`RepoScan`] holds the deterministically computed baseline — total LoC
//! (blank-line-excluded), per-language breakdown by file extension, tracked file
//! count, and framework/dependency detection from manifests actually present
//! (package.json, Cargo.toml, pyproject.toml, go.mod).  [`scan_repo`] lists files
//! via `git ls-files` (falling back to a filtered directory walk for non-git
//! paths) and never pulls a heavy dependency — std + serde_json + toml only.
//! Test: `scan_tests.rs` builds a tiny fixture git repo in a temp dir and asserts
//! the LoC, language mix, file count, and detected frameworks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use super::metrics::LanguageLoc;

/// The maximum size (bytes) of a file the scanner will read for LoC counting.
///
/// Why: a technical-DD scan must not stall on a multi-hundred-megabyte vendored
/// blob or a generated lockfile; large files are counted toward the file total
/// but skipped for line counting.
/// What: 4 MiB — comfortably larger than any hand-authored source file.
const MAX_SCAN_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Deterministic baseline metrics computed directly from a local repository.
///
/// Why: a substantive report from a bare run needs real, computed figures; this
/// is the `measured`-provenance foundation the reporter fills before any external
/// metrics enrichment or LLM inference.
/// What: total blank-excluded LoC, tracked file count, the per-language LoC
/// breakdown (largest first), and any frameworks detected from manifests.
/// Test: `scan_tests.rs::scan_counts_loc_and_languages`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RepoScan {
    /// Total lines of code across recognised source files (blank lines excluded).
    pub total_loc: u64,
    /// Number of tracked files considered (whole repo, not just source files).
    pub file_count: u64,
    /// Per-language LoC breakdown, largest first.
    pub by_language: Vec<LanguageLoc>,
    /// Frameworks/dependency manifests detected in the repository root.
    pub frameworks: Vec<Framework>,
}

/// One detected dependency manifest and a compact view of its declared deps.
///
/// Why: an acquirer's first question is "what is this built on"; naming the
/// manifest, the project, and its top declared dependencies answers it from
/// `measured` data alone.
/// What: `manifest` is the filename (e.g. `Cargo.toml`), `name` the project name
/// it declares (empty when absent), and `deps` up to the first several declared
/// dependency names in declared order.
/// Test: `scan_tests.rs::scan_detects_frameworks`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Framework {
    /// The manifest filename the detection came from.
    pub manifest: String,
    /// The project/package name the manifest declares (empty when not stated).
    pub name: String,
    /// A compact list of declared dependency names (capped).
    pub deps: Vec<String>,
}

impl RepoScan {
    /// True when the scan produced no usable signal (no source LoC, no manifest).
    ///
    /// Why: the reporter treats an empty scan as "no measured baseline" so it does
    /// not render a `0`-valued row that reads as real data.
    /// What: true when total LoC is zero and no frameworks were detected.
    /// Test: `scan_tests.rs::scan_non_repo_dir_is_empty`.
    pub fn is_empty(&self) -> bool {
        self.total_loc == 0 && self.frameworks.is_empty()
    }

    /// The primary language names, largest LoC first, up to `n`.
    ///
    /// Why: the tech-stack field wants a compact language list, mirroring
    /// [`super::metrics::AnalyzeMetrics::primary_languages`].
    /// What: returns the first `n` language names from the already-sorted list.
    /// Test: `scan_tests.rs::scan_counts_loc_and_languages`.
    pub fn primary_languages(&self, n: usize) -> Vec<String> {
        self.by_language
            .iter()
            .take(n)
            .map(|l| l.language.clone())
            .collect()
    }
}

/// Scan a local repository path for baseline metrics, or `None` if unreadable.
///
/// Why: the single entry point the model builder calls for every local-path
/// repository; a non-existent or empty path yields `None` so the report degrades
/// to declared/inferred data rather than aborting.
/// What: resolves the tracked file list (git first, filtered walk fallback),
/// counts blank-excluded LoC per language by extension, and detects frameworks
/// from root manifests.  Purely deterministic; no network, no LLM.
/// Test: `scan_tests.rs::{scan_counts_loc_and_languages, scan_non_repo_dir_is_empty}`.
pub fn scan_repo(path: &Path) -> Option<RepoScan> {
    if !path.is_dir() {
        return None;
    }
    let files = list_files(path);
    if files.is_empty() {
        return None;
    }

    let mut per_lang: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_loc = 0u64;
    for rel in &files {
        let abs = path.join(rel);
        let Some(lang) = language_for(rel) else {
            continue; // counted in file_count, but not a recognised source file
        };
        let Some(loc) = count_loc(&abs) else {
            continue;
        };
        if loc == 0 {
            continue;
        }
        *per_lang.entry(lang).or_default() += loc;
        total_loc += loc;
    }

    let mut by_language: Vec<LanguageLoc> = per_lang
        .into_iter()
        .map(|(language, loc)| LanguageLoc { language, loc })
        .collect();
    by_language.sort_by(|a, b| b.loc.cmp(&a.loc).then_with(|| a.language.cmp(&b.language)));

    Some(RepoScan {
        total_loc,
        file_count: files.len() as u64,
        by_language,
        frameworks: detect_frameworks(path),
    })
}

/// List tracked repository files relative to `root`.
///
/// Why: `git ls-files` honours `.gitignore` and records exactly the tracked
/// corpus — the right denominator for a DD scan; a non-git path still deserves a
/// substantive report, so a filtered walk is the fallback.
/// What: runs `git -C <root> ls-files`; on any failure walks the tree skipping
/// well-known noise directories (`.git`, `node_modules`, `target`, `dist`, …).
/// Test: `scan_tests.rs::scan_counts_loc_and_languages` (git path).
fn list_files(root: &Path) -> Vec<PathBuf> {
    if let Some(list) = git_ls_files(root) {
        return list;
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

/// Run `git ls-files` and parse its newline-separated output into rel paths.
fn git_ls_files(root: &Path) -> Option<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let files: Vec<PathBuf> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    if files.is_empty() { None } else { Some(files) }
}

/// Directory names never descended into by the filtered-walk fallback.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".svelte-kit",
];

/// Recursively collect files under `dir`, skipping noise directories.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.') {
                continue;
            }
            walk(root, &path, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_path_buf());
        }
    }
}

/// Count blank-excluded lines in a text file, or `None` if unreadable/too large.
///
/// Why: a pragmatic, cheap SLOC proxy — comments are NOT stripped (that would
/// need per-language lexers); blank lines are excluded so whitespace does not
/// inflate the figure.  Documented as a heuristic in the spec.
/// What: skips files above [`MAX_SCAN_FILE_BYTES`] or that are not valid UTF-8;
/// returns the count of lines containing any non-whitespace character.
/// Test: `scan_tests.rs::scan_counts_loc_and_languages`.
fn count_loc(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_SCAN_FILE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.lines().filter(|l| !l.trim().is_empty()).count() as u64)
}

/// Map a file path to a human language name by its extension, or `None`.
///
/// Why: the per-language breakdown groups LoC by language, and only recognised
/// source extensions contribute to the LoC total (data/asset files are excluded
/// so the figure reads as code, not bytes).
/// What: matches the lowercased extension against a curated table of common
/// languages; unknown extensions return `None`.
/// Test: `scan_tests.rs::scan_counts_loc_and_languages`.
fn language_for(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let lang = match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "rb" => "Ruby",
        "php" => "PHP",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" => "C++",
        "cs" => "C#",
        "swift" => "Swift",
        "scala" => "Scala",
        "ex" | "exs" => "Elixir",
        "erl" => "Erlang",
        "sh" | "bash" => "Shell",
        "sql" => "SQL",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS",
        "svelte" => "Svelte",
        "vue" => "Vue",
        "dart" => "Dart",
        "lua" => "Lua",
        "r" => "R",
        "m" | "mm" => "Objective-C",
        "hs" => "Haskell",
        "clj" | "cljs" => "Clojure",
        "toml" | "yaml" | "yml" | "json" => return None, // config/data, not code
        _ => return None,
    };
    Some(lang.to_string())
}

/// The maximum number of dependency names recorded per detected framework.
const MAX_DEPS: usize = 8;

/// Detect frameworks/dependency manifests present in the repository root.
///
/// Why: naming the build system and top dependencies is the single most useful
/// `measured` signal for "what is this"; it needs no analyzer, just the manifests
/// already on disk.
/// What: probes for package.json, Cargo.toml, pyproject.toml, and go.mod in the
/// root; each present manifest is parsed for a project name and up to [`MAX_DEPS`]
/// declared dependency names.  A malformed manifest degrades to name-only.
/// Test: `scan_tests.rs::scan_detects_frameworks`.
fn detect_frameworks(root: &Path) -> Vec<Framework> {
    let mut out = Vec::new();
    if let Some(f) = detect_package_json(root) {
        out.push(f);
    }
    if let Some(f) = detect_cargo_toml(root) {
        out.push(f);
    }
    if let Some(f) = detect_pyproject(root) {
        out.push(f);
    }
    if let Some(f) = detect_go_mod(root) {
        out.push(f);
    }
    out
}

/// Read a root manifest file as text, or `None` when absent/unreadable.
fn read_manifest(root: &Path, name: &str) -> Option<String> {
    let path = root.join(name);
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// Detect a Node package from `package.json` (name + `dependencies` keys).
fn detect_package_json(root: &Path) -> Option<Framework> {
    let text = read_manifest(root, "package.json")?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let deps = value
        .get("dependencies")
        .and_then(|v| v.as_object())
        .map(|m| m.keys().take(MAX_DEPS).cloned().collect())
        .unwrap_or_default();
    Some(Framework {
        manifest: "package.json".to_string(),
        name,
        deps,
    })
}

/// Detect a Rust crate from `Cargo.toml` (package name + `dependencies` keys).
fn detect_cargo_toml(root: &Path) -> Option<Framework> {
    let text = read_manifest(root, "Cargo.toml")?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let name = value
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let deps = value
        .get("dependencies")
        .and_then(|v| v.as_table())
        .map(|t| t.keys().take(MAX_DEPS).cloned().collect())
        .unwrap_or_default();
    Some(Framework {
        manifest: "Cargo.toml".to_string(),
        name,
        deps,
    })
}

/// Detect a Python project from `pyproject.toml` (name + declared dependencies).
fn detect_pyproject(root: &Path) -> Option<Framework> {
    let text = read_manifest(root, "pyproject.toml")?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    let project = value.get("project");
    let name = project
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    // PEP 621 `project.dependencies` is an array of requirement strings; keep the
    // bare distribution name (before any version/extras marker).
    let deps = project
        .and_then(|p| p.get("dependencies"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(pep508_name)
                .take(MAX_DEPS)
                .collect()
        })
        .unwrap_or_default();
    Some(Framework {
        manifest: "pyproject.toml".to_string(),
        name,
        deps,
    })
}

/// Extract the bare distribution name from a PEP 508 requirement string.
///
/// Why: `dependencies` lists entries like `requests>=2,<3` or `httpx[cli]`; the
/// report wants the name only.
/// What: takes the leading run of name characters (alphanumeric, `.`, `-`, `_`).
/// Test: exercised by `scan_tests.rs::scan_detects_frameworks`.
fn pep508_name(req: &str) -> String {
    req.trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect()
}

/// Detect a Go module from `go.mod` (module path + `require` directive names).
fn detect_go_mod(root: &Path) -> Option<Framework> {
    let text = read_manifest(root, "go.mod")?;
    let mut name = String::new();
    let mut deps = Vec::new();
    let mut in_require_block = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("module ") {
            name = rest.trim().to_string();
        } else if line.starts_with("require (") {
            in_require_block = true;
        } else if in_require_block && line == ")" {
            in_require_block = false;
        } else if in_require_block {
            if let Some(dep) = line.split_whitespace().next()
                && deps.len() < MAX_DEPS
            {
                deps.push(dep.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("require ")
            && let Some(dep) = rest.split_whitespace().next()
            && deps.len() < MAX_DEPS
        {
            deps.push(dep.to_string());
        }
    }
    Some(Framework {
        manifest: "go.mod".to_string(),
        name,
        deps,
    })
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
