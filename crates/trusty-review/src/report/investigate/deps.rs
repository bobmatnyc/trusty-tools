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
    /// The manifest filenames actually read at the checkout root (#6137).
    ///
    /// Why: `total == 0` has two causes that read identically on the page —
    /// "the manifests declare nothing" and "no manifest was examined" — and
    /// the renderer used to state the first for both, printing "_No
    /// manifest-declared dependencies were found_" for a workspace with 134 of
    /// them. Recording what was read is what lets the section render a named
    /// gap instead of a false clean claim.
    /// Test: `deps_tests::records_the_manifests_it_examined`.
    pub manifests_examined: Vec<String>,
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
/// one parses, sorts by (ecosystem, name), and caps to [`MAX_ROWS`]. Records which
/// of those manifests existed in `manifests_examined` (#6137), so a zero total can
/// be told apart from a root where nothing was read.
/// Test: `deps_tests::{multi_ecosystem_inventory, records_the_manifests_it_examined}`.
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
    DependencyInventory {
        deps: all,
        total,
        manifests_examined: MANIFEST_FILENAMES
            .iter()
            .filter(|name| root.join(name).is_file())
            .map(|name| (*name).to_string())
            .collect(),
    }
}

/// The root manifests [`build_inventory`] probes, in ecosystem order.
const MANIFEST_FILENAMES: &[&str] = &["package.json", "Cargo.toml", "pyproject.toml", "go.mod"];

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

/// Parse `Cargo.toml` dependencies and enrich with `Cargo.lock` versions.
///
/// Reads `[dependencies]` and, since #6137, `[workspace.dependencies]`. A cargo
/// WORKSPACE root declares its shared dependencies only in the latter table, so
/// reading `[dependencies]` alone reported zero for a 134-dependency workspace
/// and the section rendered that as a clean result.
fn cargo_deps(root: &Path) -> Vec<Dependency> {
    let Some(text) = read(root, "Cargo.toml") else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let locked = cargo_lock_versions(root);
    let mut tables: Vec<&toml::map::Map<String, toml::Value>> = Vec::new();
    if let Some(t) = value.get("dependencies").and_then(|v| v.as_table()) {
        tables.push(t);
    }
    if let Some(t) = value
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|v| v.as_table())
    {
        tables.push(t);
    }
    tables
        .into_iter()
        .flatten()
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
            let locked = locked.get(name).map_or_else(Vec::new, |v| v.clone());
            Dependency {
                locked: resolve_locked(&spec_str, &locked),
                name: name.clone(),
                ecosystem: "cargo".to_string(),
                spec: spec_str,
            }
        })
        .collect()
}

/// Every resolved version in `Cargo.lock`, per package name.
///
/// Why: a workspace lockfile routinely carries several versions of one crate —
/// `base64` 0.13.1 alongside 0.22.1, `dashmap` 5.5.3 alongside 6.1.0 — because
/// transitive dependents pin older majors. Keeping only the first entry
/// reported the LOWEST of them (cargo writes `[[package]]` blocks sorted by
/// name then version), so a manifest declaring `base64 = "0.22"` had `0.13.1`
/// printed against it. The list is what [`resolve_locked`] needs to pick the
/// version the declared requirement actually resolves to.
/// What: `name → versions`, in lockfile order, duplicates preserved.
/// Test: `deps_tests::{cargo_locked_version_satisfies_the_declared_req,
/// cargo_locked_prefers_the_highest_satisfying_version}`.
fn cargo_lock_versions(root: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
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
                    .or_default()
                    .push(ver.to_string());
            }
        }
    }
    out
}

/// Pick the locked version a declared requirement resolves to.
///
/// Why: the Locked column answers "what is this build actually using for the
/// version it asked for". With several versions in the lock, that is a semver
/// question, not a first-wins one — and when NONE of them satisfies the
/// declared requirement the honest answer is to say so rather than print an
/// unrelated version as if it were the resolution.
/// What: one locked version resolves to itself. With several, the highest that
/// satisfies `spec` wins; with an empty or unparseable `spec` the highest
/// overall wins; with none satisfying, the cell names every candidate and
/// states that none matches. An unparseable version string is skipped for
/// matching but still named in the no-match text.
/// Test: `deps_tests::{cargo_locked_version_satisfies_the_declared_req,
/// cargo_locked_prefers_the_highest_satisfying_version,
/// cargo_locked_states_when_no_version_satisfies}`.
fn resolve_locked(spec: &str, versions: &[String]) -> Option<String> {
    match versions {
        [] => return None,
        [only] => return Some(only.clone()),
        _ => {}
    }
    let parsed: Vec<(semver::Version, &String)> = versions
        .iter()
        .filter_map(|v| semver::Version::parse(v).ok().map(|p| (p, v)))
        .collect();
    let req = semver::VersionReq::parse(spec.trim()).ok();
    let matching: Vec<&(semver::Version, &String)> = parsed
        .iter()
        .filter(|(v, _)| req.as_ref().is_none_or(|r| r.matches(v)))
        .collect();
    if let Some((_, raw)) = matching.iter().max_by(|a, b| a.0.cmp(&b.0)) {
        return Some((*raw).clone());
    }
    Some(format!("{} (none satisfies {spec})", versions.join(", ")))
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
