//! Project-file scanners for the KG bootstrap seeder.
//!
//! Why: Splitting scanner logic into its own module keeps each file under the
//! 500-SLOC cap and lets unit tests target the scanners directly without
//! pulling in the async entry point or type definitions.
//! What: `scan_project` is the top-level blocking orchestrator; the private
//! per-file functions (`scan_cargo_toml`, `scan_package_json`,
//! `scan_pyproject_toml`, `scan_go_mod`, `scan_claude_md`, `scan_git_config`,
//! `read_origin_url`) each extract triples from one well-known file format.
//! Test: `scan_project_extracts_cargo_facts`,
//! `scan_project_extracts_package_json`,
//! `scan_project_falls_back_to_palace_id_when_no_manifest`,
//! and others in the `tests` sub-module.

use std::path::Path;

use anyhow::Result;

use super::types::{BootstrapTriple, ScannedFile};

/// Blocking scanner: walk well-known files under `root` and extract triples.
///
/// Why: Pulled out as a sync function so the file I/O + TOML/JSON parsing
/// run on a blocking thread (via `spawn_blocking`) and the algorithm itself
/// is trivially unit-testable against fixture directories.
/// What: Returns `(triples, per_file_summary, project_subject)`. The
/// project subject is derived from the first manifest that yields a name;
/// falls back to `fallback_subject` (typically the palace id) when none
/// match.
/// Test: `scan_project_extracts_cargo_facts`,
/// `scan_project_extracts_package_json`,
/// `scan_project_falls_back_to_palace_id_when_no_manifest`.
pub fn scan_project(
    root: &Path,
    fallback_subject: &str,
) -> Result<(Vec<BootstrapTriple>, Vec<ScannedFile>, String)> {
    let mut triples: Vec<BootstrapTriple> = Vec::new();
    let mut summary: Vec<ScannedFile> = Vec::new();
    let mut project_subject: Option<String> = None;

    // 1. Cargo.toml
    let before = triples.len();
    if let Some(name) = scan_cargo_toml(root, &mut triples) {
        project_subject.get_or_insert(name);
    }
    if triples.len() > before {
        summary.push(ScannedFile {
            file: "Cargo.toml".to_string(),
            triples: triples.len() - before,
        });
    }

    // 2. package.json
    let before = triples.len();
    if let Some(name) = scan_package_json(root, &mut triples) {
        project_subject.get_or_insert(name);
    }
    if triples.len() > before {
        summary.push(ScannedFile {
            file: "package.json".to_string(),
            triples: triples.len() - before,
        });
    }

    // 3. pyproject.toml
    let before = triples.len();
    if let Some(name) = scan_pyproject_toml(root, &mut triples) {
        project_subject.get_or_insert(name);
    }
    if triples.len() > before {
        summary.push(ScannedFile {
            file: "pyproject.toml".to_string(),
            triples: triples.len() - before,
        });
    }

    // 4. go.mod
    let before = triples.len();
    if let Some(name) = scan_go_mod(root, &mut triples) {
        project_subject.get_or_insert(name);
    }
    if triples.len() > before {
        summary.push(ScannedFile {
            file: "go.mod".to_string(),
            triples: triples.len() - before,
        });
    }

    // 5. CLAUDE.md — first H1 heading as descriptive name. Does not set
    //    project_subject (the manifest sources are stronger signals) but
    //    contributes a `has_description` triple when the subject is known.
    let before = triples.len();
    scan_claude_md(root, project_subject.as_deref(), &mut triples);
    if triples.len() > before {
        summary.push(ScannedFile {
            file: "CLAUDE.md".to_string(),
            triples: triples.len() - before,
        });
    }

    // 6. .git/config — source repo URL.
    let before = triples.len();
    scan_git_config(root, project_subject.as_deref(), &mut triples);
    if triples.len() > before {
        summary.push(ScannedFile {
            file: ".git/config".to_string(),
            triples: triples.len() - before,
        });
    }

    let subject = project_subject.unwrap_or_else(|| fallback_subject.to_string());

    // Rewrite any triples that used a placeholder subject (only the
    // CLAUDE.md / .git/config scanners are subject-dependent; if no manifest
    // matched, those scanners ran with subject=None and produced nothing, so
    // this is currently a no-op — but keeping the loop makes future scanner
    // additions safe).
    for t in &mut triples {
        if t.subject.is_empty() {
            t.subject = subject.clone();
        }
    }

    Ok((triples, summary, subject))
}

/// Scan `Cargo.toml`. Returns the package/workspace name if extracted.
///
/// Why: Rust projects are the primary trusty-tools target; we want
/// `has_language=Rust`, `has_version`, `has_edition`, `has_rust_version`,
/// and `workspace_member` triples auto-populated so `kg_query` against the
/// project name returns useful context immediately.
/// What: Parses the TOML; emits `(name, has_language, "Rust")` always when
/// the manifest exists, plus version/edition/rust-version/workspace member
/// triples when present.
/// Test: `scan_project_extracts_cargo_facts`.
fn scan_cargo_toml(root: &Path, out: &mut Vec<BootstrapTriple>) -> Option<String> {
    let manifest = root.join("Cargo.toml");
    let raw = std::fs::read_to_string(&manifest).ok()?;
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("bootstrap: parse Cargo.toml failed: {e:#}");
            return None;
        }
    };

    // Workspace root manifests may have no [package] section. Use the
    // workspace.package.name if present; otherwise derive from the dir name.
    let name = parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            parsed
                .get("workspace")
                .and_then(|w| w.get("package"))
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            root.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })?;

    out.push(BootstrapTriple {
        subject: name.clone(),
        predicate: "has_language".to_string(),
        object: "Rust".to_string(),
        provenance: "bootstrap:cargo.toml".to_string(),
    });

    if let Some(version) = parsed
        .get("package")
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    {
        out.push(BootstrapTriple {
            subject: name.clone(),
            predicate: "has_version".to_string(),
            object: version.to_string(),
            provenance: "bootstrap:cargo.toml".to_string(),
        });
    }
    if let Some(edition) = parsed
        .get("package")
        .and_then(|p| p.get("edition"))
        .and_then(|v| v.as_str())
    {
        out.push(BootstrapTriple {
            subject: name.clone(),
            predicate: "has_edition".to_string(),
            object: edition.to_string(),
            provenance: "bootstrap:cargo.toml".to_string(),
        });
    }
    if let Some(rv) = parsed
        .get("package")
        .and_then(|p| p.get("rust-version"))
        .and_then(|v| v.as_str())
    {
        out.push(BootstrapTriple {
            subject: name.clone(),
            predicate: "has_rust_version".to_string(),
            object: rv.to_string(),
            provenance: "bootstrap:cargo.toml".to_string(),
        });
    }

    // Workspace members (capped at 64 to avoid flooding the KG on huge
    // monorepos; bootstrap is a coarse seeder, not an exhaustive index).
    if let Some(members) = parsed
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
    {
        for member in members.iter().take(64) {
            if let Some(s) = member.as_str() {
                out.push(BootstrapTriple {
                    subject: name.clone(),
                    predicate: "has_workspace_member".to_string(),
                    object: s.to_string(),
                    provenance: "bootstrap:cargo.toml".to_string(),
                });
            }
        }
    }

    Some(name)
}

/// Scan `package.json`.
///
/// Why: Node/TypeScript projects are the second most common target. We want
/// `has_language=JavaScript`, `has_version`, and `has_dependency` triples.
/// What: Parses the JSON; emits language/version triples + one
/// `has_dependency` per top-level key in the `dependencies` object (cap 64).
/// Test: `scan_project_extracts_package_json`.
fn scan_package_json(root: &Path, out: &mut Vec<BootstrapTriple>) -> Option<String> {
    let manifest = root.join("package.json");
    let raw = std::fs::read_to_string(&manifest).ok()?;
    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("bootstrap: parse package.json failed: {e:#}");
            return None;
        }
    };
    let name = parsed.get("name").and_then(|n| n.as_str())?.to_string();

    out.push(BootstrapTriple {
        subject: name.clone(),
        predicate: "has_language".to_string(),
        object: "JavaScript".to_string(),
        provenance: "bootstrap:package.json".to_string(),
    });

    if let Some(version) = parsed.get("version").and_then(|v| v.as_str()) {
        out.push(BootstrapTriple {
            subject: name.clone(),
            predicate: "has_version".to_string(),
            object: version.to_string(),
            provenance: "bootstrap:package.json".to_string(),
        });
    }

    if let Some(deps) = parsed.get("dependencies").and_then(|d| d.as_object()) {
        for (k, _) in deps.iter().take(64) {
            out.push(BootstrapTriple {
                subject: name.clone(),
                predicate: "has_dependency".to_string(),
                object: k.clone(),
                provenance: "bootstrap:package.json".to_string(),
            });
        }
    }

    Some(name)
}

/// Scan `pyproject.toml`.
///
/// Why: Python projects use PEP-621 `[project]` metadata; surfacing the
/// language tag + version + `requires-python` makes Python repos legible to
/// the KG without manual assertions.
/// What: Parses the TOML; emits language/version/requires-python triples
/// when the `[project]` table is present.
/// Test: `scan_project_extracts_pyproject`.
fn scan_pyproject_toml(root: &Path, out: &mut Vec<BootstrapTriple>) -> Option<String> {
    let manifest = root.join("pyproject.toml");
    let raw = std::fs::read_to_string(&manifest).ok()?;
    let parsed: toml::Value = match toml::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("bootstrap: parse pyproject.toml failed: {e:#}");
            return None;
        }
    };
    let project = parsed.get("project")?;
    let name = project.get("name").and_then(|n| n.as_str())?.to_string();

    out.push(BootstrapTriple {
        subject: name.clone(),
        predicate: "has_language".to_string(),
        object: "Python".to_string(),
        provenance: "bootstrap:pyproject.toml".to_string(),
    });

    if let Some(v) = project.get("version").and_then(|v| v.as_str()) {
        out.push(BootstrapTriple {
            subject: name.clone(),
            predicate: "has_version".to_string(),
            object: v.to_string(),
            provenance: "bootstrap:pyproject.toml".to_string(),
        });
    }
    if let Some(rp) = project.get("requires-python").and_then(|v| v.as_str()) {
        out.push(BootstrapTriple {
            subject: name.clone(),
            predicate: "requires_python".to_string(),
            object: rp.to_string(),
            provenance: "bootstrap:pyproject.toml".to_string(),
        });
    }

    Some(name)
}

/// Scan `go.mod` for the module name.
///
/// Why: Go projects encode their canonical name on the `module` line of
/// `go.mod`; surfacing it as the project subject lets Go repos opt into the
/// same KG shape as Rust/Node/Python.
/// What: Reads `go.mod`, extracts the `module <name>` directive, and emits
/// `(name, has_language, "Go")` plus `(name, has_module_path, <name>)`.
/// Test: `scan_project_extracts_go_mod`.
fn scan_go_mod(root: &Path, out: &mut Vec<BootstrapTriple>) -> Option<String> {
    let raw = std::fs::read_to_string(root.join("go.mod")).ok()?;
    let module = raw
        .lines()
        .find_map(|line| line.trim().strip_prefix("module "))
        .map(|s| s.trim().to_string())?;
    if module.is_empty() {
        return None;
    }
    out.push(BootstrapTriple {
        subject: module.clone(),
        predicate: "has_language".to_string(),
        object: "Go".to_string(),
        provenance: "bootstrap:go.mod".to_string(),
    });
    out.push(BootstrapTriple {
        subject: module.clone(),
        predicate: "has_module_path".to_string(),
        object: module.clone(),
        provenance: "bootstrap:go.mod".to_string(),
    });
    Some(module)
}

/// Scan `CLAUDE.md` for the first H1 heading; attach as project description.
///
/// Why: Trusty-* projects use `CLAUDE.md` as the canonical orientation
/// document; the first H1 line is invariably the project name/tagline and
/// makes a good `has_description` triple.
/// What: Walks lines, finds the first `# Title` heading, strips the prefix,
/// and pushes a `has_description` triple under `subject` (when known).
/// Test: `scan_project_extracts_claude_md_h1`.
fn scan_claude_md(root: &Path, subject: Option<&str>, out: &mut Vec<BootstrapTriple>) {
    let Some(subject) = subject else {
        // No project subject yet — skip; we don't want orphan triples.
        return;
    };
    let Ok(raw) = std::fs::read_to_string(root.join("CLAUDE.md")) else {
        return;
    };
    if let Some(h1) = raw.lines().find_map(|line| {
        let t = line.trim_start();
        t.strip_prefix("# ")
            .filter(|rest| !rest.is_empty())
            .map(|s| s.trim().to_string())
    }) {
        out.push(BootstrapTriple {
            subject: subject.to_string(),
            predicate: "has_description".to_string(),
            object: h1,
            provenance: "bootstrap:claude.md".to_string(),
        });
    }
}

/// Scan the git origin URL for the project rooted at `root`.
///
/// Why: Tying a project to its source repo URL is the single highest-signal
/// fact for downstream tooling (link to issues, find upstream, etc.). The
/// canonical source is `[remote "origin"] url = …`, but its physical location
/// depends on whether `root` is a normal checkout or a git worktree:
///
/// - Normal checkout: `.git/` is a directory; the config lives in
///   `<root>/.git/config`.
/// - Worktree: `.git` is a *file* containing `gitdir: <pointer>` to the
///   parent repo's `.git/worktrees/<name>/` dir; the `[remote]` section lives
///   in the parent's `.git/config`, not anywhere reachable by joining
///   `<root>/.git/config`.
///
/// Issue #113: the previous implementation only handled the first case and
/// silently dropped the `source_repo` triple in any worktree-based checkout.
///
/// What: First shells out to `git -C <root> config --get remote.origin.url`,
/// which natively resolves the `.git`-file pointer for us. Falls back to a
/// direct file scan of `<root>/.git/config` when `git` is unavailable on PATH
/// (matters for the fixture-based unit tests, which fabricate a `.git/config`
/// file in a tempdir without a real repo). Emits a
/// `(subject, source_repo, url)` triple when a URL is found.
/// Test: `scan_project_extracts_git_origin` (file fallback path),
/// `tools::tests::kg_bootstrap_seeds_workspace_facts` (git-CLI path,
/// exercised inside worktrees).
fn scan_git_config(root: &Path, subject: Option<&str>, out: &mut Vec<BootstrapTriple>) {
    let Some(subject) = subject else { return };
    let Some(url) = read_origin_url(root) else {
        return;
    };
    out.push(BootstrapTriple {
        subject: subject.to_string(),
        predicate: "source_repo".to_string(),
        object: url,
        provenance: "bootstrap:git.config".to_string(),
    });
}

/// Resolve `remote.origin.url` for the repo rooted at `root`, transparent to
/// worktree vs. normal-checkout layout.
///
/// Why: Centralises the worktree-vs-checkout indirection in one place so the
/// bootstrap scanner stays readable. See `scan_git_config` for the full
/// reasoning behind the two-strategy approach.
/// What: (1) tries `git -C <root> config --get remote.origin.url`, which
/// works equally well in worktrees, normal checkouts, and submodules; (2)
/// falls back to a manual INI scan of `<root>/.git/config` for environments
/// without a `git` binary (notably the fixture-based unit tests in this
/// module). Returns `None` if neither path yields a non-empty URL.
/// Test: `read_origin_url_returns_none_for_non_git_dir`,
/// `scan_project_extracts_git_origin` (file fallback).
fn read_origin_url(root: &Path) -> Option<String> {
    // Strategy 1: ask git directly. This is the only path that handles
    // worktrees correctly without us re-implementing `gitdir:` resolution.
    if let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("config")
        .arg("--get")
        .arg("remote.origin.url")
        .output()
    {
        if output.status.success() {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !url.is_empty() {
                return Some(url);
            }
        }
    }

    // Strategy 2: direct INI scan of `<root>/.git/config`. Only useful for
    // fixture tests that fabricate a `.git/config` in a tempdir; real-world
    // worktrees will never reach this branch because the file read fails
    // (the worktree `.git` is a file, not a directory).
    let raw = std::fs::read_to_string(root.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed == "[remote \"origin\"]";
            continue;
        }
        if in_origin {
            if let Some(rest) = trimmed.strip_prefix("url") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    let url = rest.trim().to_string();
                    if !url.is_empty() {
                        return Some(url);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod scan_tests;
