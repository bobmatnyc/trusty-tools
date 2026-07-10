//! Deterministic dependency inventory from manifests + lockfiles (wave 3, #2357).
//!
//! Why: "what is this built on, and is it pinned?" is a first-order DD question,
//! and the answer is already on disk — no LLM and no network needed.  Parsing the
//! declared manifests AND their lockfiles gives a `measured` dependency table
//! (declared spec + locked version) the report renders directly; the LLM may then
//! flag staleness as an `inferred` judgement, but the inventory itself is fact.
//! What: [`build_inventory`] probes a checkout root for the supported ecosystems,
//! parses each manifest's direct dependencies and (best-effort) its lockfile's
//! resolved versions, and returns a capped [`DependencyInventory`].  All parsing
//! is lenient: a malformed lockfile degrades to declared-only, never an error.
//! Test: `deps_tests.rs` covers npm (+lock), Cargo (+lock), pyproject, and go.mod.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

/// The maximum number of dependency rows rendered before an "and N more" line.
pub const MAX_ROWS: usize = 30;

/// One declared dependency with its locked version when a lockfile resolved it.
///
/// Why: an acquirer wants both the declared constraint (what the project asks
/// for) and the pinned reality (what a fresh install gets); carrying both makes
/// drift and loose pinning visible.
/// What: `name` is the package; `ecosystem` names the manifest family; `spec` is
/// the declared version constraint (empty when the manifest states none); `locked`
/// is the resolved version from the lockfile, or `None` when unlocked/unparsed.
/// Test: `deps_tests::npm_manifest_and_lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dependency {
    /// Package/crate/module name.
    pub name: String,
    /// Ecosystem label (e.g. `npm`, `cargo`, `pypi`, `go`).
    pub ecosystem: String,
    /// Declared version constraint (empty when none is stated).
    pub spec: String,
    /// Resolved version from the lockfile, if available.
    pub locked: Option<String>,
}

/// The deterministic dependency inventory for one repository.
///
/// Why: the reporter renders this as a `measured` "Dependency Inventory" section;
/// tracking the true total separately from the capped rows lets it print an
/// honest "and N more" line rather than silently dropping dependencies.
/// What: `deps` are the (capped) rows in stable order; `total` is the full count
/// before the cap.
/// Test: `deps_tests::caps_rows_with_overflow_count`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DependencyInventory {
    /// Dependency rows, capped at [`MAX_ROWS`], stable-sorted.
    pub deps: Vec<Dependency>,
    /// Total dependencies discovered across all ecosystems (pre-cap).
    pub total: usize,
}

impl DependencyInventory {
    /// True when no dependencies were discovered.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The count of rows omitted by the [`MAX_ROWS`] cap (0 when all fit).
    pub fn overflow(&self) -> usize {
        self.total.saturating_sub(self.deps.len())
    }
}

/// Build the dependency inventory by probing every supported ecosystem at `root`.
///
/// Why: the single entry point the investigation calls; it never fails (a missing
/// or malformed manifest simply contributes nothing) so it can run unconditionally
/// for a local checkout.
/// What: collects direct dependencies from package.json, Cargo.toml, pyproject.toml,
/// and go.mod, enriches each with a locked version from the matching lockfile when
/// one parses, sorts by (ecosystem, name), and caps to [`MAX_ROWS`].
/// Test: `deps_tests::multi_ecosystem_inventory`.
pub fn build_inventory(root: &Path) -> DependencyInventory {
    let mut all: Vec<Dependency> = Vec::new();
    all.extend(npm_deps(root));
    all.extend(cargo_deps(root));
    all.extend(pypi_deps(root));
    all.extend(go_deps(root));

    all.sort_by(|a, b| {
        a.ecosystem
            .cmp(&b.ecosystem)
            .then_with(|| a.name.cmp(&b.name))
    });
    let total = all.len();
    all.truncate(MAX_ROWS);
    DependencyInventory { deps: all, total }
}

/// Read a root manifest/lockfile as text, or `None` when absent/unreadable.
fn read(root: &Path, name: &str) -> Option<String> {
    let path = root.join(name);
    if !path.is_file() {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

// ─── npm ─────────────────────────────────────────────────────────────────────

/// Parse `package.json` direct deps and enrich with `package-lock.json` versions.
fn npm_deps(root: &Path) -> Vec<Dependency> {
    let Some(text) = read(root, "package.json") else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let locked = npm_lock_versions(root);
    let mut out = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(map) = value.get(key).and_then(|v| v.as_object()) {
            for (name, spec) in map {
                out.push(Dependency {
                    name: name.clone(),
                    ecosystem: "npm".to_string(),
                    spec: spec.as_str().unwrap_or_default().to_string(),
                    locked: locked.get(name).cloned(),
                });
            }
        }
    }
    out
}

/// Best-effort resolved versions from `package-lock.json` (v2/v3 `packages`, or
/// v1 `dependencies`).  Returns an empty map when absent/unparseable.
fn npm_lock_versions(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(text) = read(root, "package-lock.json") else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return out;
    };
    // v2/v3: `packages` keyed by "node_modules/<name>" (root key is "").
    if let Some(pkgs) = value.get("packages").and_then(|v| v.as_object()) {
        for (path, meta) in pkgs {
            if let Some(name) = path.strip_prefix("node_modules/")
                && let Some(ver) = meta.get("version").and_then(|v| v.as_str())
                && !name.contains("/node_modules/")
            {
                out.insert(name.to_string(), ver.to_string());
            }
        }
    }
    // v1: `dependencies` keyed by name.
    if let Some(deps) = value.get("dependencies").and_then(|v| v.as_object()) {
        for (name, meta) in deps {
            if let Some(ver) = meta.get("version").and_then(|v| v.as_str()) {
                out.entry(name.clone()).or_insert_with(|| ver.to_string());
            }
        }
    }
    out
}

// ─── cargo ───────────────────────────────────────────────────────────────────

/// Parse `Cargo.toml` `[dependencies]` and enrich with `Cargo.lock` versions.
fn cargo_deps(root: &Path) -> Vec<Dependency> {
    let Some(text) = read(root, "Cargo.toml") else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let locked = cargo_lock_versions(root);
    let Some(deps) = value.get("dependencies").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    deps.iter()
        .map(|(name, spec)| {
            let spec_str = match spec {
                toml::Value::String(s) => s.clone(),
                toml::Value::Table(t) => t
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                _ => String::new(),
            };
            Dependency {
                name: name.clone(),
                ecosystem: "cargo".to_string(),
                spec: spec_str,
                locked: locked.get(name).cloned(),
            }
        })
        .collect()
}

/// Resolved versions from `Cargo.lock` (`[[package]] name/version`).
fn cargo_lock_versions(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(text) = read(root, "Cargo.lock") else {
        return out;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return out;
    };
    if let Some(pkgs) = value.get("package").and_then(|v| v.as_array()) {
        for pkg in pkgs {
            if let (Some(name), Some(ver)) = (
                pkg.get("name").and_then(|v| v.as_str()),
                pkg.get("version").and_then(|v| v.as_str()),
            ) {
                out.entry(name.to_string())
                    .or_insert_with(|| ver.to_string());
            }
        }
    }
    out
}

// ─── pypi ────────────────────────────────────────────────────────────────────

/// Parse `pyproject.toml` project/poetry dependencies (declared-only; poetry.lock
/// parsing is best-effort-skipped to avoid a heavy lock format dependency).
fn pypi_deps(root: &Path) -> Vec<Dependency> {
    let Some(text) = read(root, "pyproject.toml") else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // PEP 621 `project.dependencies`: array of requirement strings.
    if let Some(arr) = value
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|v| v.as_array())
    {
        for req in arr.iter().filter_map(|v| v.as_str()) {
            let (name, spec) = split_pep508(req);
            out.push(Dependency {
                name,
                ecosystem: "pypi".to_string(),
                spec,
                locked: None,
            });
        }
    }
    // Poetry `[tool.poetry.dependencies]`: table of name → constraint.
    if let Some(tbl) = value
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|v| v.as_table())
    {
        for (name, spec) in tbl {
            if name.eq_ignore_ascii_case("python") {
                continue;
            }
            let spec_str = match spec {
                toml::Value::String(s) => s.clone(),
                toml::Value::Table(t) => t
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                _ => String::new(),
            };
            out.push(Dependency {
                name: name.clone(),
                ecosystem: "pypi".to_string(),
                spec: spec_str,
                locked: None,
            });
        }
    }
    out
}

/// Split a PEP 508 requirement into `(name, spec)` (e.g. `requests>=2,<3`).
fn split_pep508(req: &str) -> (String, String) {
    let trimmed = req.trim();
    let name: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let spec = trimmed[name.len()..].trim().to_string();
    (name, spec)
}

// ─── go ──────────────────────────────────────────────────────────────────────

/// Parse `go.mod` `require` directives (declared version is the pinned version).
fn go_deps(root: &Path) -> Vec<Dependency> {
    let Some(text) = read(root, "go.mod") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        if line.starts_with("require (") {
            in_block = true;
            continue;
        }
        if in_block && line == ")" {
            in_block = false;
            continue;
        }
        let directive = if in_block {
            Some(line)
        } else {
            line.strip_prefix("require ").map(str::trim)
        };
        if let Some(d) = directive
            && !d.is_empty()
        {
            let mut parts = d.split_whitespace();
            if let Some(name) = parts.next() {
                let ver = parts.next().unwrap_or_default().to_string();
                out.push(Dependency {
                    name: name.to_string(),
                    ecosystem: "go".to_string(),
                    // go.mod pins exactly, so the declared spec IS the locked ver.
                    spec: ver.clone(),
                    locked: if ver.is_empty() { None } else { Some(ver) },
                });
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "deps_tests.rs"]
mod tests;
