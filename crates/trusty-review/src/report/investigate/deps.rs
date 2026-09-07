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
//! is lenient: a malformed lockfile degrades to declared-only and names itself in
//! [`DependencyInventory::lockfile_warnings`] (#6794), never an error.
//! Test: `deps_tests.rs` covers npm (+lock), Cargo (+lock), pyproject
//! (+poetry.lock / uv.lock / requirements.txt), and go.mod.

use std::path::Path;

use serde::Serialize;

#[path = "deps_lock.rs"]
mod lock;

/// The maximum number of dependency rows rendered before an "and N more" line.
///
/// #6788: a RENDER cap only. [`DependencyInventory::deps`] holds every row;
/// [`DependencyInventory::rendered`] applies this at table-build time.
pub const MAX_ROWS: usize = 30;

/// One declared dependency with its locked version when a lockfile resolved it.
///
/// Why: an acquirer wants both the declared constraint (what the project asks
/// for) and the pinned reality (what a fresh install gets); carrying both makes
/// drift and loose pinning visible. #6794 adds the two facts a consumer needs
/// to tell those apart mechanically — whether the version is a resolution and
/// which file resolved it — because `serde = "1"` and `serde 1.0.210` reached
/// trusty-audit's OSV stage indistinguishable, and a range is unscannable.
/// What: `name` is the package; `ecosystem` names the manifest family; `spec` is
/// the declared version constraint (empty when the manifest states none);
/// `locked` is the resolved version from the lockfile, or `None` when
/// unlocked/unparsed; `resolved` is true only when `locked` is a single exact
/// version; `source` names the file `locked` came from.
/// Test: `deps_tests::{npm_manifest_and_lock, cargo_lock_records_its_source}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Dependency {
    /// Package/crate/module name.
    pub name: String,
    /// Ecosystem label (e.g. `npm`, `cargo`, `pypi`, `go`).
    pub ecosystem: String,
    /// Declared version constraint (empty when none is stated).
    pub spec: String,
    /// Resolved version from the lockfile, if available.
    pub locked: Option<String>,
    /// True when `locked` is one exact version, not a range or a no-match note.
    ///
    /// #6794: a consumer matching this row against an advisory database needs a
    /// version, and `^1.2` is not one. False is the safe default — an
    /// unresolved row is unassessed, never clean.
    pub resolved: bool,
    /// The file `locked` was read from, e.g. `Cargo.lock` (#6794).
    ///
    /// `None` exactly when `locked` is `None`. Only the filename is kept: the
    /// inventory must not grow by the size of the lockfile it read.
    pub source: Option<String>,
}

impl Dependency {
    /// A declared-only row: no lockfile answered for this package (#6794).
    fn declared(name: String, ecosystem: &str, spec: String) -> Self {
        Self {
            name,
            ecosystem: ecosystem.to_owned(),
            spec,
            locked: None,
            resolved: false,
            source: None,
        }
    }

    /// Apply `index`'s resolution for this row, if it has one (#6794).
    fn resolve_from(mut self, index: &lock::LockIndex) -> Self {
        let key = if self.ecosystem == "pypi" {
            lock::normalize(&self.name)
        } else {
            self.name.clone()
        };
        let Some(versions) = index.versions.get(&key) else {
            return self;
        };
        if let Some(resolution) = lock::resolve(&self.spec, versions) {
            self.resolved = resolution.exact;
            self.locked = Some(resolution.version);
            self.source = index.sources.get(&key).cloned();
        }
        self
    }
}

/// The deterministic dependency inventory for one repository.
///
/// Why: the reporter renders this as a `measured` "Dependency Inventory"
/// section, capped at [`MAX_ROWS`] for readability — but the same struct is
/// serialised into `investigation.json`, which machine consumers read (#6788:
/// trusty-audit's OSV lookup queried 30 of a 134-dependency workspace because
/// the cap was applied before serialisation). Keeping the full inventory here
/// and capping at render time serves both.
/// What: `deps` holds EVERY discovered row in stable order; `total` is that
/// same count; [`Self::rendered`] is the capped view the table draws;
/// `lockfile_warnings` names each lockfile that existed and did not parse.
/// Test: `deps_tests::{inventory_keeps_every_row_past_the_render_cap,
/// rendered_caps_at_max_rows, a_malformed_lockfile_warns_and_falls_back}`.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct DependencyInventory {
    /// Every dependency row discovered, stable-sorted and uncapped (#6788).
    pub deps: Vec<Dependency>,
    /// Total dependencies discovered across all ecosystems.
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
    /// One line per lockfile that was present but could not be parsed (#6794).
    ///
    /// Why: a lockfile that fails to parse silently degrades every row in its
    /// ecosystem to a declared range, and the page then shows those ranges with
    /// nothing saying why. The sweep must not abort for it either — one bad
    /// lockfile in one repository cannot cost the run.
    /// Test: `deps_tests::a_malformed_lockfile_warns_and_falls_back`.
    pub lockfile_warnings: Vec<String>,
}

impl DependencyInventory {
    /// True when no dependencies were discovered.
    pub fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// The rows the markdown table draws: the first [`MAX_ROWS`] of [`Self::deps`].
    ///
    /// Why: the table stays readable at 30 rows while the serialised inventory
    /// keeps all of them (#6788).
    /// What: a slice of at most [`MAX_ROWS`] rows, in the inventory's order.
    /// Test: `deps_tests::rendered_caps_at_max_rows`.
    pub fn rendered(&self) -> &[Dependency] {
        &self.deps[..self.deps.len().min(MAX_ROWS)]
    }

    /// The count of rows the [`MAX_ROWS`] render cap omits (0 when all fit).
    ///
    /// #6788: measured against the RENDERED rows — `deps` now carries the
    /// full inventory, so `total - deps.len()` would always be 0.
    pub fn overflow(&self) -> usize {
        self.total.saturating_sub(self.rendered().len())
    }

    /// How many rows carry an exact lockfile-resolved version (#6794).
    ///
    /// Why: "how much of this inventory is scannable" is the question the OSV
    /// stage's coverage line answers, and it is a property of the inventory
    /// rather than of any one consumer.
    /// Test: `deps_tests::poetry_lock_resolves_pyproject_ranges`.
    pub fn resolved_count(&self) -> usize {
        self.deps.iter().filter(|d| d.resolved).count()
    }
}

/// Build the dependency inventory by probing every supported ecosystem at `root`.
///
/// Why: the single entry point the investigation calls; it never fails (a missing
/// or malformed manifest simply contributes nothing) so it can run unconditionally
/// for a local checkout.
/// What: collects direct dependencies from package.json, Cargo.toml, pyproject.toml,
/// and go.mod, enriches each with a locked version from the matching lockfile when
/// one parses ([`lock`]), and sorts by (ecosystem, name). Every row is kept
/// (#6788); the [`MAX_ROWS`] cap is applied by [`DependencyInventory::rendered`]
/// at table-build time. Records which of those manifests existed in
/// `manifests_examined` (#6137), so a zero total can be told apart from a root
/// where nothing was read, and which lockfiles failed to parse in
/// `lockfile_warnings` (#6794).
/// Test: `deps_tests::{multi_ecosystem_inventory, records_the_manifests_it_examined,
/// inventory_keeps_every_row_past_the_render_cap, a_malformed_lockfile_warns_and_falls_back}`.
pub fn build_inventory(root: &Path) -> DependencyInventory {
    let mut warnings = Vec::new();
    let mut all: Vec<Dependency> = Vec::new();
    all.extend(npm_deps(root, &mut warnings));
    all.extend(cargo_deps(root, &mut warnings));
    all.extend(pypi_deps(root, &mut warnings));
    all.extend(go_deps(root));
    all.sort_by(|a, b| {
        a.ecosystem
            .cmp(&b.ecosystem)
            .then_with(|| a.name.cmp(&b.name))
    });
    // #6788: no truncation here — the snapshot must carry every row.
    let total = all.len();
    DependencyInventory {
        deps: all,
        total,
        manifests_examined: MANIFEST_FILENAMES
            .iter()
            .filter(|name| root.join(name).is_file())
            .map(|name| (*name).to_string())
            .collect(),
        lockfile_warnings: warnings,
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
fn npm_deps(root: &Path, warnings: &mut Vec<String>) -> Vec<Dependency> {
    let Some(text) = read(root, "package.json") else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let index = lock::npm(root);
    warnings.extend(index.warnings.iter().cloned());
    let mut out = Vec::new();
    for key in ["dependencies", "devDependencies"] {
        if let Some(map) = value.get(key).and_then(|v| v.as_object()) {
            for (name, spec) in map {
                out.push(
                    Dependency::declared(
                        name.clone(),
                        "npm",
                        spec.as_str().unwrap_or_default().to_string(),
                    )
                    .resolve_from(&index),
                );
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
fn cargo_deps(root: &Path, warnings: &mut Vec<String>) -> Vec<Dependency> {
    let Some(text) = read(root, "Cargo.toml") else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let index = lock::cargo(root);
    warnings.extend(index.warnings.iter().cloned());
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
            Dependency::declared(name.clone(), "cargo", toml_spec(spec)).resolve_from(&index)
        })
        .collect()
}

/// The declared version constraint a TOML dependency value states.
///
/// A bare string IS the constraint; a table states it under `version`; anything
/// else (a bare path or git dependency) declares no version at all.
fn toml_spec(spec: &toml::Value) -> String {
    match spec {
        toml::Value::String(s) => s.clone(),
        toml::Value::Table(t) => t
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

// ─── pypi ────────────────────────────────────────────────────────────────────

/// Parse `pyproject.toml` project/poetry dependencies and enrich them with the
/// pins in `poetry.lock`, `uv.lock`, or `requirements.txt` (#6794).
fn pypi_deps(root: &Path, warnings: &mut Vec<String>) -> Vec<Dependency> {
    let Some(text) = read(root, "pyproject.toml") else {
        return Vec::new();
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return Vec::new();
    };
    let index = lock::pypi(root);
    warnings.extend(index.warnings.iter().cloned());
    let mut out = Vec::new();
    // PEP 621 `project.dependencies`: array of requirement strings.
    if let Some(arr) = value
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|v| v.as_array())
    {
        for req in arr.iter().filter_map(|v| v.as_str()) {
            let (name, spec) = split_pep508(req);
            out.push(Dependency::declared(name, "pypi", spec).resolve_from(&index));
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
            out.push(
                Dependency::declared(name.clone(), "pypi", toml_spec(spec)).resolve_from(&index),
            );
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
                let mut dep = Dependency::declared(name.to_string(), "go", ver.clone());
                // go.mod pins exactly, so the declared spec IS the locked ver
                // and the manifest itself is the source (#6794).
                if !ver.is_empty() {
                    dep.locked = Some(ver);
                    dep.resolved = true;
                    dep.source = Some("go.mod".to_string());
                }
                out.push(dep);
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "deps_tests.rs"]
mod tests;
