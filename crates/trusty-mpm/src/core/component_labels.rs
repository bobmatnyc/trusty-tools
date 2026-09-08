//! The component labels an issue audit ACCEPTS, derived from the repository's
//! own crates rather than from the harness's seed table (#7123).
//!
//! Why: [`crate::core::policy_labels`] answers "which labels does trusty-mpm
//! CREATE" — the `trusty-mpm` convention label, `ws/<session>`, and whatever
//! `agents.ticketing.extra_labels` adds. #7097's audit reused that answer for a
//! different question, "which labels SATISFY the owning-component rule", and
//! the two are not the same set. On a workspace with ~30 crates the seed table
//! names one of them, so every correctly labelled non-mpm issue audited as
//! `component label FAIL none of [trusty-mpm] present` — #7116, #7128 and
//! #7132–#7140 all did. The owner ruling of 2026-09-06 stands and is the reason
//! this module exists rather than a widening of the seed table: `trusty-mpm` is
//! a component label scoped to `crates/trusty-mpm/`, never a stand-in for "some
//! component label", so an issue owned by another crate has to be able to name
//! that crate and pass.
//!
//! What: [`ComponentLabels`], the accepted set, and [`ComponentLabels::resolve`],
//! which unions the seed table with the workspace's own crate labels — the
//! `[package] name` of every member of the root `Cargo.toml`'s `[workspace]
//! members`, plus the hyphenated spelling of any name carrying a `_`, since a
//! GitHub label name cannot contain one (`trusty_review` is labelled
//! `trusty-review`).
//!
//! The accepted set is deliberately a SUPERSET of the seeded set, and nothing
//! here feeds label creation: `tm issue seed-labels` and session launch still
//! read [`crate::core::policy_labels::policy_labels_configured`], so this
//! module never causes a `gh label create`. A crate label already exists in the
//! repository that owns the crate; seeding thirty of them elsewhere would be a
//! different, unasked-for change.
//!
//! Every filesystem read here is bounded and fail-closed: an unreadable,
//! oversized or malformed manifest contributes no names, leaving the seed table
//! as the accepted set — exactly the pre-#7123 behaviour, never a wider one.
//!
//! Test: `component_labels_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::policy_labels::policy_labels_configured;
use crate::core::trusty_tools_config::ResolvedTicketing;

/// Largest `Cargo.toml` this module will read.
///
/// Why: a manifest past this size is pathological, not large, and the audit
/// must not stall on one.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Maximum workspace members enumerated for one resolve.
///
/// Why: bounds directory enumeration on a hostile or generated repository. A
/// workspace with more members than this is not one whose component labels a
/// FAIL line could usefully name anyway.
const MAX_MEMBERS: usize = 512;

/// The component labels that satisfy the owning-component rule.
///
/// Why: the audit needs one value it can ask "does this label name a
/// component", built in one place, so a caller cannot assemble a different set
/// than the one this module documents.
/// What: an order-preserving, de-duplicated list — the seed table's names
/// first, then the workspace crate labels. [`Self::resolve`] is the derivation;
/// [`Self::from_names`] exists for callers that already hold a set (tests, and
/// any future caller with a non-Cargo source).
/// Test: `resolve_accepts_every_workspace_crate_label`,
/// `from_names_deduplicates`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ComponentLabels {
    names: Vec<String>,
}

impl ComponentLabels {
    /// The accepted set for `ticketing`, widened by the workspace rooted at or
    /// above `start_dir`.
    ///
    /// Why: the audit runs `gh` in the current directory, so the repository
    /// whose issues it reads is the repository whose crates should satisfy the
    /// component rule. Passing the directory in rather than reading it here
    /// keeps the caller in charge of which repository is meant.
    /// What: the names [`policy_labels_configured`] yields with NO session name
    /// — `ws/<session>` is a workstream, not a component — then every crate
    /// label found by walking up from `start_dir` to the Cargo workspace root.
    /// `None`, no workspace root, or an unreadable manifest all yield just the
    /// seed table.
    /// Test: `resolve_accepts_every_workspace_crate_label`,
    /// `resolve_without_a_workspace_is_the_seed_table`,
    /// `resolve_ignores_a_malformed_manifest`.
    #[must_use]
    pub fn resolve(ticketing: &ResolvedTicketing, start_dir: Option<&Path>) -> Self {
        // #7123: `None` for the session — a `ws/<session>` label names the
        // workstream that filed the issue, never the component that owns it.
        let mut names: Vec<String> = policy_labels_configured(ticketing, None)
            .into_iter()
            .map(|l| l.name)
            .collect();
        if let Some(root) = start_dir.and_then(cargo_workspace_root) {
            names.extend(workspace_crate_labels(&root));
        }
        Self::from_names(names)
    }

    /// Build a set from names already in hand, de-duplicated in first-seen
    /// order.
    #[must_use]
    pub fn from_names(names: impl IntoIterator<Item = String>) -> Self {
        let mut out: Vec<String> = Vec::new();
        for name in names {
            if !name.is_empty() && !out.iter().any(|existing| existing == &name) {
                out.push(name);
            }
        }
        Self { names: out }
    }

    /// Whether `label` satisfies the owning-component rule.
    #[must_use]
    pub fn accepts(&self, label: &str) -> bool {
        self.names.iter().any(|n| n == label)
    }

    /// The accepted names, in the order a report should name them.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// The Cargo workspace root at or above `start`, if any.
///
/// Why: the audit is run from anywhere inside a checkout — the repo root, a
/// crate directory, a worktree — and the member list lives only in the root
/// manifest.
/// What: the nearest ancestor (starting with `start` itself) whose `Cargo.toml`
/// declares a `[workspace]` table. `None` when no ancestor does.
/// Test: `workspace_root_is_found_from_a_nested_directory`,
/// `no_workspace_root_outside_a_workspace`.
fn cargo_workspace_root(start: &Path) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let manifest = dir.join("Cargo.toml");
        let Some(raw) = read_bounded(&manifest) else {
            continue;
        };
        if raw
            .parse::<toml::Value>()
            .ok()
            .is_some_and(|v| v.get("workspace").is_some())
        {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Every crate label the workspace rooted at `root` declares.
///
/// Why: the accepted set has to be the workspace's CRATES, so it is derived
/// from each member's `[package] name` rather than from its directory. A
/// directory name would add path segments no repository labels an issue with —
/// this workspace's two nested Tauri members both sit in `…/ui/src-tauri`, and
/// `src-tauri` is not a component. GitHub label names cannot contain `_`, so a
/// package spelled with one is labelled with `-` instead (`trusty_review` is
/// labelled `trusty-review`), and both spellings are accepted.
/// What: expands each `[workspace] members` pattern, then for each existing
/// member directory yields its `[package] name` and, when that name carries a
/// `_`, the hyphenated form. Capped at [`MAX_MEMBERS`]; every read is
/// fail-closed.
/// Test: `resolve_accepts_every_workspace_crate_label`,
/// `an_underscored_package_is_accepted_hyphenated`,
/// `a_nested_member_contributes_no_path_segment`.
fn workspace_crate_labels(root: &Path) -> Vec<String> {
    let Some(raw) = read_bounded(&root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(manifest) = raw.parse::<toml::Value>() else {
        return Vec::new();
    };
    let patterns: Vec<String> = manifest
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let mut labels = Vec::new();
    let mut members = 0usize;
    for pattern in patterns {
        for dir in expand_member(root, &pattern) {
            if members >= MAX_MEMBERS {
                return labels;
            }
            members += 1;
            if let Some(name) = package_name(&dir) {
                if name.contains('_') {
                    labels.push(name.replace('_', "-"));
                }
                labels.push(name);
            }
        }
    }
    labels
}

/// The `[package] name` declared by `dir/Cargo.toml`, if any.
fn package_name(dir: &Path) -> Option<String> {
    let raw = read_bounded(&dir.join("Cargo.toml"))?;
    raw.parse::<toml::Value>()
        .ok()?
        .get("package")?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

/// Expand one `members` pattern into the member directories that exist.
///
/// Why: `crates/*` and `crates/trusty-audit/ui/src-tauri` are both real entries
/// in this workspace's member list, so the expansion has to handle a glob
/// segment and a plain path with the same code.
/// What: walks the pattern segment by segment. A `*` (or `**`) segment replaces
/// each candidate with its subdirectories, sorted so the result is
/// deterministic; every other segment is joined literally. Only directories
/// survive, and the total is capped at [`MAX_MEMBERS`].
/// Test: `member_glob_expands_one_level`, `member_literal_path_is_kept`,
/// `member_pattern_that_matches_nothing_yields_nothing`.
fn expand_member(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut candidates = vec![root.to_path_buf()];
    for segment in pattern.split('/').filter(|s| !s.is_empty() && *s != ".") {
        let mut next = Vec::new();
        for candidate in &candidates {
            if next.len() >= MAX_MEMBERS {
                break;
            }
            if segment == "*" || segment == "**" {
                let Ok(entries) = std::fs::read_dir(candidate) else {
                    continue;
                };
                let mut dirs: Vec<PathBuf> = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect();
                dirs.sort();
                next.extend(dirs);
            } else {
                let joined = candidate.join(segment);
                if joined.is_dir() {
                    next.push(joined);
                }
            }
        }
        next.truncate(MAX_MEMBERS);
        candidates = next;
        if candidates.is_empty() {
            return candidates;
        }
    }
    candidates
}

/// Read a manifest, refusing anything oversized or unreadable.
///
/// Why: one place holds the size cap and the fail-closed rule, so the two
/// callers cannot drift apart on either.
fn read_bounded(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
#[path = "component_labels_tests.rs"]
mod tests;
