//! Lockfile-resolved dependency versions, per ecosystem (#6794).
//!
//! Why: the inventory used to record whatever the manifest declared, so a
//! `serde = "1"` reached the report — and trusty-audit's OSV stage — as the
//! range `1`, which no advisory database can answer. In a 59-repository run
//! 515 of 1230 rows carried a range rather than a version. The lockfile beside
//! the manifest already holds the answer; reading it is what turns an
//! unscannable row into a coordinate.
//! What: one reader per ecosystem, each returning a [`LockIndex`] — resolved
//! versions per package, the lockfile each came from, and a named warning for
//! any lockfile that existed but did not parse. Nothing here aborts: an
//! unparseable lockfile degrades that ecosystem to declared ranges and says so.
//! Only the resolved `name → version` pairs are kept, so the inventory never
//! grows by the size of the lockfile it read.
//! Test: `deps_tests::{cargo_lock_records_its_source, npm_lock_records_its_source,
//! poetry_lock_resolves_pyproject_ranges, uv_lock_resolves_pyproject_ranges,
//! requirements_pins_resolve_pyproject_ranges,
//! a_malformed_lockfile_warns_and_falls_back}`.

use std::collections::BTreeMap;
use std::path::Path;

/// Resolved versions one ecosystem's lockfiles supplied.
///
/// Why: three facts travel together — what a package resolved to, which file
/// said so, and which lockfiles could not be read at all. Splitting them across
/// three return values loses the pairing.
/// What: `versions` keeps every resolved version per package in lockfile order
/// (Cargo.lock routinely carries several majors of one crate); `sources` names
/// the file each package's versions came from; `warnings` is one line per
/// lockfile that existed and failed to parse.
/// Test: `deps_tests::cargo_lock_records_its_source`.
#[derive(Debug, Default)]
pub(super) struct LockIndex {
    /// Package name → resolved versions, in lockfile order, duplicates kept.
    pub versions: BTreeMap<String, Vec<String>>,
    /// Package name → the lockfile that supplied its versions.
    pub sources: BTreeMap<String, String>,
    /// One line per lockfile that existed but could not be parsed.
    pub warnings: Vec<String>,
}

impl LockIndex {
    /// Record `name` as resolving to `version`, credited to `source`.
    fn insert(&mut self, name: &str, version: &str, source: &str) {
        self.versions
            .entry(name.to_owned())
            .or_default()
            .push(version.to_owned());
        self.sources
            .entry(name.to_owned())
            .or_insert_with(|| source.to_owned());
    }

    /// The warning line an existing-but-unparseable lockfile earns.
    fn unparseable(&mut self, file: &str, ecosystem: &str, cause: &str) {
        self.warnings.push(format!(
            "{file} was found but could not be parsed ({cause}); {ecosystem} dependencies fall \
             back to their declared ranges"
        ));
    }
}

/// One lockfile's text, or `None` when it is not present at `root`.
///
/// An unreadable-but-present file returns its io error so the caller can warn:
/// a lockfile the sweep could not open is exactly the case #6794 forbids
/// swallowing.
fn slurp(root: &Path, name: &str) -> Option<Result<String, String>> {
    let path = root.join(name);
    if !path.is_file() {
        return None;
    }
    Some(std::fs::read_to_string(path).map_err(|e| e.to_string()))
}

// ─── npm ─────────────────────────────────────────────────────────────────────

/// The npm lockfile this reader understands.
pub(super) const NPM_LOCK: &str = "package-lock.json";

/// Resolved npm versions from `package-lock.json` (v2/v3 `packages`, or v1
/// `dependencies`).
///
/// Test: `deps_tests::npm_lock_records_its_source`.
pub(super) fn npm(root: &Path) -> LockIndex {
    let mut index = LockIndex::default();
    let Some(text) = slurp(root, NPM_LOCK) else {
        return index;
    };
    let text = match text {
        Ok(text) => text,
        Err(e) => {
            index.unparseable(NPM_LOCK, "npm", &e);
            return index;
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => value,
        Err(e) => {
            index.unparseable(NPM_LOCK, "npm", &e.to_string());
            return index;
        }
    };
    // v2/v3: `packages` keyed by "node_modules/<name>" (root key is "").
    if let Some(pkgs) = value.get("packages").and_then(|v| v.as_object()) {
        for (path, meta) in pkgs {
            if let Some(name) = path.strip_prefix("node_modules/")
                && let Some(ver) = meta.get("version").and_then(|v| v.as_str())
                && !name.contains("/node_modules/")
            {
                index.insert(name, ver, NPM_LOCK);
            }
        }
    }
    // v1: `dependencies` keyed by name.
    if let Some(deps) = value.get("dependencies").and_then(|v| v.as_object()) {
        for (name, meta) in deps {
            if let Some(ver) = meta.get("version").and_then(|v| v.as_str())
                && !index.versions.contains_key(name)
            {
                index.insert(name, ver, NPM_LOCK);
            }
        }
    }
    index
}

// ─── cargo ───────────────────────────────────────────────────────────────────

/// The cargo lockfile this reader understands.
pub(super) const CARGO_LOCK: &str = "Cargo.lock";

/// Every resolved version in `Cargo.lock`, per package name.
///
/// Why: a workspace lockfile routinely carries several versions of one crate —
/// `base64` 0.13.1 alongside 0.22.1 — because transitive dependents pin older
/// majors. Keeping only the first entry reported the LOWEST of them, so a
/// manifest declaring `base64 = "0.22"` had `0.13.1` printed against it.
/// What: `name → versions`, in lockfile order, duplicates preserved, for
/// [`resolve`] to pick from.
/// Test: `deps_tests::{cargo_locked_version_satisfies_the_declared_req,
/// cargo_locked_prefers_the_highest_satisfying_version}`.
pub(super) fn cargo(root: &Path) -> LockIndex {
    let mut index = LockIndex::default();
    let Some(text) = slurp(root, CARGO_LOCK) else {
        return index;
    };
    let text = match text {
        Ok(text) => text,
        Err(e) => {
            index.unparseable(CARGO_LOCK, "cargo", &e);
            return index;
        }
    };
    let value = match toml::from_str::<toml::Value>(&text) {
        Ok(value) => value,
        Err(e) => {
            index.unparseable(CARGO_LOCK, "cargo", &e.to_string());
            return index;
        }
    };
    for pkg in value
        .get("package")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if let (Some(name), Some(ver)) = (
            pkg.get("name").and_then(|v| v.as_str()),
            pkg.get("version").and_then(|v| v.as_str()),
        ) {
            index.insert(name, ver, CARGO_LOCK);
        }
    }
    index
}

// ─── pypi ────────────────────────────────────────────────────────────────────

/// The pypi pin sources this reader understands, in precedence order.
///
/// Why: a project pins in exactly one of these in practice, and when it has
/// more than one the lockfile proper is the stronger claim — `requirements.txt`
/// is often a partial export. First hit per package wins ([`LockIndex::insert`]
/// keeps the first `source`), so the order here is the precedence.
pub(super) const PYPI_LOCKS: &[&str] = &["poetry.lock", "uv.lock", "requirements.txt"];

/// Resolved pypi versions from `poetry.lock`, `uv.lock`, or `requirements.txt`.
///
/// Why: `pyproject.toml` states ranges and nothing else, so before #6794 every
/// python row in the inventory was unresolved — the single largest share of the
/// 515 unscannable rows the issue counts.
/// What: the two lock formats are TOML `[[package]] name/version` arrays;
/// `requirements.txt` contributes only its `==` pins, since a `>=` line is a
/// range and not a resolution. Names are normalised ([`normalize`]) because
/// PyPI treats `-`, `_` and case as equivalent while the files do not.
/// Test: `deps_tests::{poetry_lock_resolves_pyproject_ranges,
/// uv_lock_resolves_pyproject_ranges, requirements_pins_resolve_pyproject_ranges}`.
pub(super) fn pypi(root: &Path) -> LockIndex {
    let mut index = LockIndex::default();
    for file in PYPI_LOCKS {
        let Some(text) = slurp(root, file) else {
            continue;
        };
        let text = match text {
            Ok(text) => text,
            Err(e) => {
                index.unparseable(file, "pypi", &e);
                continue;
            }
        };
        if *file == "requirements.txt" {
            absorb_requirements(&mut index, &text, file);
            continue;
        }
        match toml::from_str::<toml::Value>(&text) {
            Ok(value) => absorb_toml_lock(&mut index, &value, file),
            Err(e) => index.unparseable(file, "pypi", &e.to_string()),
        }
    }
    index
}

/// Absorb a `[[package]] name/version` TOML lock (poetry.lock, uv.lock).
fn absorb_toml_lock(index: &mut LockIndex, value: &toml::Value, source: &str) {
    for pkg in value
        .get("package")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        if let (Some(name), Some(ver)) = (
            pkg.get("name").and_then(|v| v.as_str()),
            pkg.get("version").and_then(|v| v.as_str()),
        ) {
            index.insert(&normalize(name), ver, source);
        }
    }
}

/// Absorb the `==` pins of a `requirements.txt`, ignoring every other line.
///
/// A `>=`, `~=` or bare name is a range or no constraint at all, and recording
/// it as a resolution would invent the fact this module exists to measure.
fn absorb_requirements(index: &mut LockIndex, text: &str, source: &str) {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() || line.starts_with('-') {
            continue;
        }
        // Strip an environment marker or extras hash tail before the split.
        let line = line.split(';').next().unwrap_or("").trim();
        let Some((name, version)) = line.split_once("==") else {
            continue;
        };
        let name = name.split('[').next().unwrap_or("").trim();
        let version = version.split_whitespace().next().unwrap_or("").trim();
        if name.is_empty() || version.is_empty() {
            continue;
        }
        index.insert(&normalize(name), version, source);
    }
}

/// PyPI's own name equivalence: case-insensitive, `_` and `.` fold to `-`.
///
/// Test: `deps_tests::poetry_lock_resolves_pyproject_ranges`.
pub(super) fn normalize(name: &str) -> String {
    name.trim()
        .to_ascii_lowercase()
        .replace(['_', '.'], "-")
        .trim_matches('-')
        .to_owned()
}

// ─── resolution ──────────────────────────────────────────────────────────────

/// One dependency's resolved version, and whether it is an exact pin.
///
/// Why: #6794 asks the report to distinguish a resolved version from a declared
/// range, so the boolean travels with the string rather than being re-derived
/// by every reader of it.
/// What: `version` is what the Locked cell shows; `exact` is false when no
/// lockfile answered, or when several locked versions exist and none satisfies
/// the declared requirement.
/// Test: `deps_tests::cargo_locked_states_when_no_version_satisfies`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Resolution {
    /// The text the Locked column renders.
    pub version: String,
    /// True when `version` is a single exact version a lockfile resolved.
    pub exact: bool,
}

/// Pick the locked version a declared requirement resolves to.
///
/// Why: the Locked column answers "what is this build actually using for the
/// version it asked for". With several versions in the lock that is a semver
/// question, not a first-wins one — and when NONE of them satisfies the
/// declared requirement the honest answer is to say so rather than print an
/// unrelated version as if it were the resolution.
/// What: one locked version resolves to itself. With several, the highest that
/// satisfies `spec` wins; with an empty or unparseable `spec` the highest
/// overall wins; with none satisfying, the cell names every candidate, states
/// that none matches, and reports `exact: false` so the row is not offered to a
/// vulnerability database as a coordinate (#6794).
/// Test: `deps_tests::{cargo_locked_version_satisfies_the_declared_req,
/// cargo_locked_prefers_the_highest_satisfying_version,
/// cargo_locked_states_when_no_version_satisfies}`.
pub(super) fn resolve(spec: &str, versions: &[String]) -> Option<Resolution> {
    match versions {
        [] => return None,
        [only] => {
            return Some(Resolution {
                version: only.clone(),
                exact: true,
            });
        }
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
        return Some(Resolution {
            version: (*raw).clone(),
            exact: true,
        });
    }
    Some(Resolution {
        version: format!("{} (none satisfies {spec})", versions.join(", ")),
        exact: false,
    })
}
