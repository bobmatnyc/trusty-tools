//! Knowledge-graph bootstrap helpers.
//!
//! Why: Issue #60 — after `palace_create`, the knowledge graph (KG) sits at
//! zero triples and there is no auto-discovery path. Users have no idea
//! they're supposed to call `kg_assert` manually before `kg_query` returns
//! anything useful. `kg_bootstrap` closes this gap by scanning well-known
//! project files (`Cargo.toml`, `package.json`, `pyproject.toml`, `CLAUDE.md`,
//! `.git/config`, `go.mod`) and seeding structured triples that describe the
//! project (language, version, source repo, etc.). It also seeds temporal
//! metadata (`created_at`, `bootstrapped_at`) so even an empty project at
//! least has *something* in the KG and a timestamp anchor for future queries.
//! What: A pure-blocking scanner (`scan_project`) returns a flat list of
//! `(subject, predicate, object, provenance)` tuples; the public async entry
//! point `bootstrap_palace` resolves a palace handle, runs the scanner, and
//! asserts each tuple through the existing `KnowledgeGraph::assert` path.
//! Test: Unit tests pin each scanner against fixture directories;
//! `kg_bootstrap` is exercised end-to-end from the MCP tool surface in
//! `tools.rs`.
//!
//! Design notes:
//! - Missing files are NOT errors — every read is best-effort. The scanner
//!   returns whatever triples it could derive and skips the rest with a
//!   debug-level log.
//! - All extracted facts use the user-supplied (or inferred) project name as
//!   the triple subject. When no project name can be derived from manifests,
//!   the palace ID is used as a fallback so the temporal triples still anchor
//!   to a stable subject.
//! - Provenance strings are stable identifiers (`bootstrap:cargo.toml`,
//!   `bootstrap:package.json`, …) so operators can audit which scanner
//!   asserted each triple and retract by source if needed.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use trusty_common::memory_core::store::kg::Triple;

use crate::AppState;

/// A single bootstrap discovery before it becomes a Triple.
///
/// Why: Keeping the scanner output as plain tuples (rather than full
/// `Triple`s) lets the unit tests verify the extraction logic without
/// constructing timestamps or worrying about confidence values. The async
/// caller converts these into `Triple`s with the live `chrono::Utc::now()`
/// timestamp right before assertion.
/// What: Carries subject, predicate, object, and the provenance tag that
/// identifies which scanner produced the fact.
/// Test: Each scanner test asserts the expected `BootstrapTriple`s land in
/// the result list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapTriple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub provenance: String,
}

/// Per-file scan summary returned to the MCP caller.
///
/// Why: Operators want to know *which* files contributed to the bootstrap
/// (and which were absent) without re-running the tool with verbose logging.
/// What: Filename + count of triples it produced; emitted as JSON in the
/// MCP response.
/// Test: `bootstrap_palace_returns_per_file_counts`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScannedFile {
    pub file: String,
    pub triples: usize,
}

/// Aggregate result of a bootstrap run.
///
/// Why: The MCP `kg_bootstrap` tool returns this verbatim so the model (or a
/// human operator) can see exactly what was asserted and which files were
/// scanned.
/// What: Total triple count + per-file summaries + the resolved project
/// subject. `Serialize` so it round-trips into the MCP JSON envelope.
/// Test: `bootstrap_palace_seeds_temporal_metadata_when_no_files`.
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapResult {
    pub palace: String,
    pub project_subject: String,
    pub triples_asserted: usize,
    pub scanned_files: Vec<ScannedFile>,
}

/// Run the bootstrap scan against a palace.
///
/// Why: Single async entry point that the MCP dispatcher (and the
/// auto-bootstrap hook in `palace_create`) calls. Encapsulates path
/// resolution, scanning, triple construction, and KG assertion.
/// What: Resolves `project_path` (caller-supplied), runs the blocking
/// scanner, seeds temporal metadata triples, and asserts every discovery
/// through `handle.kg.assert(...)`. Returns a summary of what was written.
/// Test: `bootstrap_palace_seeds_temporal_metadata_when_no_files`,
/// `bootstrap_palace_scans_cargo_toml`.
pub async fn bootstrap_palace(
    state: &AppState,
    palace_id: &str,
    project_path: Option<&Path>,
) -> Result<BootstrapResult> {
    let handle = state
        .registry
        .open_palace(
            &state.data_root,
            &trusty_common::memory_core::palace::PalaceId::new(palace_id),
        )
        .with_context(|| format!("open palace {palace_id}"))?;

    // Choose the scan root. When the caller did not supply a project path,
    // we still scan the palace's own data dir so `CLAUDE.md` or other
    // operator-placed files inside the palace are picked up.
    let scan_root: PathBuf = match project_path {
        Some(p) => p.to_path_buf(),
        None => handle
            .data_dir
            .clone()
            .unwrap_or_else(|| state.data_root.join(palace_id)),
    };
    let palace_id_owned = palace_id.to_string();

    let (triples, scanned_files, project_subject) =
        tokio::task::spawn_blocking(move || scan_project(&scan_root, &palace_id_owned))
            .await
            .context("join scan_project")??;

    // Seed temporal metadata (always present, even for empty projects).
    let now = chrono::Utc::now();
    let mut all = triples;
    all.push(BootstrapTriple {
        subject: project_subject.clone(),
        predicate: "bootstrapped_at".to_string(),
        object: now.to_rfc3339(),
        provenance: "bootstrap:temporal".to_string(),
    });
    // `created_at` is only inserted when the palace doesn't yet have one;
    // re-running bootstrap must not lie about when the palace first came
    // into being. The KG's temporal layer would close the prior interval
    // and the new interval would carry a misleading `valid_from`. Check
    // `query_active` before writing.
    let existing = handle
        .kg
        .query_active(&project_subject)
        .await
        .context("kg.query_active for created_at check")?;
    if !existing.iter().any(|t| t.predicate == "created_at") {
        all.push(BootstrapTriple {
            subject: project_subject.clone(),
            predicate: "created_at".to_string(),
            object: now.to_rfc3339(),
            provenance: "bootstrap:temporal".to_string(),
        });
    }

    let mut asserted = 0usize;
    for bt in &all {
        let triple = Triple {
            subject: bt.subject.clone(),
            predicate: bt.predicate.clone(),
            object: bt.object.clone(),
            valid_from: now,
            valid_to: None,
            confidence: 1.0,
            provenance: Some(bt.provenance.clone()),
        };
        handle
            .kg
            .assert(triple)
            .await
            .with_context(|| format!("kg.assert {} {}", bt.subject, bt.predicate))?;
        asserted += 1;
    }

    Ok(BootstrapResult {
        palace: palace_id.to_string(),
        project_subject,
        triples_asserted: asserted,
        scanned_files,
    })
}

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

/// Hint string returned by `kg_query` when the palace KG is empty.
///
/// Why: Issue #60 — when a user calls `kg_query` against a brand-new palace
/// they get an empty triples array with no indication that `kg_bootstrap` /
/// `kg_assert` even exist. A short hint embedded in the response solves
/// this with one line of code at the call site.
/// What: Static string, kept in this module so tests can pin it.
/// Test: `kg_query_emits_hint_when_palace_empty` in `tools.rs`.
pub const KG_EMPTY_HINT: &str =
    "Knowledge graph is empty. Run kg_bootstrap to seed it from project files, \
     or use kg_assert to add triples manually.";

/// Convenience: count active triples across an entire palace.
///
/// Why: `kg_query` is per-subject, so to determine "is the KG empty?" the
/// `kg_query` handler needs a separate broader check. Centralising the
/// emptiness check here keeps the hint logic in one place and lets future
/// changes (e.g. counting across closets) live alongside their consumer.
/// What: Returns `Ok(true)` iff the palace has zero triples for the queried
/// subject AND the broader "is_anything_asserted" check is empty. Practical
/// emptiness: we treat the palace as empty if the queried subject returned
/// no triples — this is the user's signal that something is wrong, even if
/// other subjects have data.
/// Test: covered indirectly through `kg_query_emits_hint_when_palace_empty`.
pub fn is_kg_empty_for_subject(triples: &[Triple]) -> bool {
    triples.is_empty()
}

/// Helper: bubble up the bootstrap result as the MCP JSON envelope expects.
///
/// Why: `tools.rs` keeps the dispatcher branches small; converting the
/// `BootstrapResult` into a `serde_json::Value` here keeps the JSON shape
/// owned by this module and stable for tests.
/// What: Serialises the result via serde and wraps any failure in
/// `anyhow::Error` with context.
/// Test: round-tripped via the MCP dispatcher test.
pub fn result_to_json(r: &BootstrapResult) -> Result<serde_json::Value> {
    serde_json::to_value(r).map_err(|e| anyhow!("serialize BootstrapResult: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&p, content).expect("write");
    }

    /// Why: Pin the Cargo.toml scanner against a realistic single-crate
    /// manifest. Covers name/version/edition/rust-version extraction.
    #[test]
    fn scan_project_extracts_cargo_facts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "Cargo.toml",
            r#"
[package]
name = "demo-crate"
version = "1.2.3"
edition = "2021"
rust-version = "1.88"
"#,
        );
        let (triples, summary, subject) =
            scan_project(tmp.path(), "fallback").expect("scan_project");
        assert_eq!(subject, "demo-crate");
        assert!(summary.iter().any(|s| s.file == "Cargo.toml"));

        let has = |p: &str, o: &str| {
            triples
                .iter()
                .any(|t| t.subject == "demo-crate" && t.predicate == p && t.object == o)
        };
        assert!(has("has_language", "Rust"));
        assert!(has("has_version", "1.2.3"));
        assert!(has("has_edition", "2021"));
        assert!(has("has_rust_version", "1.88"));
    }

    /// Why: Workspace manifests have no `[package]` section but a
    /// `[workspace]` table with members; the scanner must still produce
    /// workspace-member triples and fall back to the directory name for
    /// the subject.
    #[test]
    fn scan_project_extracts_workspace_members() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("trusty-tools");
        fs::create_dir_all(&root).expect("mkdir");
        write(
            &root,
            "Cargo.toml",
            r#"
[workspace]
members = ["crates/foo", "crates/bar"]
resolver = "2"
"#,
        );
        let (triples, _summary, subject) = scan_project(&root, "fallback").expect("scan_project");
        assert_eq!(subject, "trusty-tools");
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_workspace_member" && t.object == "crates/foo"));
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_workspace_member" && t.object == "crates/bar"));
    }

    /// Why: package.json is the JS/TS entry point; pin name/version + a
    /// `has_dependency` triple per top-level dep key.
    #[test]
    fn scan_project_extracts_package_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "package.json",
            r#"{
  "name": "my-app",
  "version": "0.5.0",
  "dependencies": {
    "react": "^18.0.0",
    "lodash": "^4.0.0"
  }
}"#,
        );
        let (triples, _summary, subject) = scan_project(tmp.path(), "fb").expect("scan");
        assert_eq!(subject, "my-app");
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_language" && t.object == "JavaScript"));
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_version" && t.object == "0.5.0"));
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_dependency" && t.object == "react"));
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_dependency" && t.object == "lodash"));
    }

    /// Why: pyproject.toml uses PEP-621 `[project]` table; confirm
    /// language/version/requires-python triples land.
    #[test]
    fn scan_project_extracts_pyproject() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "pyproject.toml",
            r#"
[project]
name = "pydemo"
version = "2.0.1"
requires-python = ">=3.10"
"#,
        );
        let (triples, _summary, subject) = scan_project(tmp.path(), "fb").expect("scan");
        assert_eq!(subject, "pydemo");
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_language" && t.object == "Python"));
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_version" && t.object == "2.0.1"));
        assert!(triples
            .iter()
            .any(|t| t.predicate == "requires_python" && t.object == ">=3.10"));
    }

    /// Why: Go modules name themselves in `go.mod`; confirm module-name
    /// extraction + language tag.
    #[test]
    fn scan_project_extracts_go_mod() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "go.mod",
            "module github.com/example/widget\n\ngo 1.22\n",
        );
        let (triples, _summary, subject) = scan_project(tmp.path(), "fb").expect("scan");
        assert_eq!(subject, "github.com/example/widget");
        assert!(triples
            .iter()
            .any(|t| t.predicate == "has_language" && t.object == "Go"));
    }

    /// Why: CLAUDE.md's first H1 becomes the project description; pin the
    /// extractor against a typical heading + leading frontmatter.
    #[test]
    fn scan_project_extracts_claude_md_h1() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "Cargo.toml",
            r#"
[package]
name = "demo"
version = "0.1.0"
"#,
        );
        write(
            tmp.path(),
            "CLAUDE.md",
            "\n\n# Demo Project — orientation guide\n\nSome body text.\n",
        );
        let (triples, _summary, _subject) = scan_project(tmp.path(), "fb").expect("scan");
        assert!(triples.iter().any(|t| t.subject == "demo"
            && t.predicate == "has_description"
            && t.object == "Demo Project — orientation guide"));
    }

    /// Why: .git/config is the canonical source-repo URL; confirm extraction
    /// across SSH-style URLs.
    #[test]
    fn scan_project_extracts_git_origin() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write(
            tmp.path(),
            "Cargo.toml",
            r#"
[package]
name = "demo"
version = "0.1.0"
"#,
        );
        write(
            tmp.path(),
            ".git/config",
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = git@github.com:example/demo.git\n",
        );
        let (triples, _summary, _) = scan_project(tmp.path(), "fb").expect("scan");
        assert!(
            triples
                .iter()
                .any(|t| t.predicate == "source_repo"
                    && t.object == "git@github.com:example/demo.git")
        );
    }

    /// Why: When no manifest matches, the fallback subject (palace id) must
    /// be returned so temporal triples still have a stable anchor.
    #[test]
    fn scan_project_falls_back_to_palace_id_when_no_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (triples, summary, subject) = scan_project(tmp.path(), "my-palace").expect("scan");
        assert_eq!(subject, "my-palace");
        assert!(triples.is_empty());
        assert!(summary.is_empty());
    }
}
