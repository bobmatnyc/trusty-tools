//! Monorepo workspace-member discovery for marker detection (#4760).
//!
//! Why: marker detection probes the PROJECT ROOT. That is correct for a
//! single-package repo and wrong for a monorepo, where the root manifest
//! declares members and the interesting files live inside them. A turborepo or
//! pnpm-workspaces root declares neither `"react"` nor `"next"` and carries no
//! `next.config.*`, so root-only probing loses `react-engineer`,
//! `nextjs-engineer`, and `svelte-engineer` on a Next.js monorepo that had them
//! before #4760 tightened the gates. An Elixir umbrella loses
//! `phoenix-engineer` the same way. Narrowing the gates was authorised for
//! single-package projects that never declared the framework; it was not
//! authorised for monorepos that do.
//! What: [`probe_roots`] returns the project root followed by every workspace
//! member directory the ROOT MANIFEST ITSELF declares — npm/yarn `workspaces`,
//! `pnpm-workspace.yaml` `packages:`, and an Elixir umbrella's `apps_path:`.
//! Declared globs are honoured rather than conventional paths being assumed, so
//! a workspace using `libs/*` or `services/*` is covered without this module
//! guessing. [`ProbeBudget`] bounds the whole walk.
//!
//! **Bounds, and why these numbers.** A launch-path probe must not stall on a
//! pathological repository, so every dimension is capped:
//!
//! | Bound | Value | Rationale |
//! |---|---|---|
//! | Glob depth | one `*` segment, no recursive descent | npm/pnpm globs are overwhelmingly one level (`packages/*`, `apps/*`). Expanding one level makes enumeration O(entries in one directory) and never exponential. A `**` is treated as a single `*`. |
//! | Patterns read | [`MAX_WORKSPACE_PATTERNS`] = 64 | A manifest declaring more member globs than this is malformed, not large. |
//! | Members probed | [`MAX_WORKSPACE_MEMBERS`] = 256 | Bounds directory enumeration. Detection is a boolean OR — one matching member decides the answer — so the marginal member past 256 almost never changes the result. |
//! | Bytes read | [`MAX_WORKSPACE_PROBE_BYTES`] = 16 MiB per detection call | A real 256-member monorepo with few-KB manifests reads well under 1 MiB; 16 MiB leaves headroom while capping a hostile repo at a fraction of a second of I/O. |
//!
//! Exhausting a bound is **fail-closed**: probing stops and the unread members
//! are treated as carrying no marker, exactly as an unreadable file is. It never
//! errors and never blocks a launch.
//! Test: `crates/trusty-mpm/src/core/manifest/workspace_tests.rs`.

use std::cell::Cell;
use std::path::{Path, PathBuf};

/// Maximum member globs read from a single root manifest.
pub(crate) const MAX_WORKSPACE_PATTERNS: usize = 64;

/// Maximum workspace member directories probed for one detection call.
pub(crate) const MAX_WORKSPACE_MEMBERS: usize = 256;

/// Maximum total bytes read while discovering and probing workspace members.
pub(crate) const MAX_WORKSPACE_PROBE_BYTES: u64 = 16 * 1024 * 1024;

/// The largest single file this module will read.
///
/// Why: shared with `project_lang::MAX_MARKER_PROBE_BYTES` in intent — a
/// manifest larger than this is pathological, not large.
/// What: 1 MiB, in bytes.
pub(crate) const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// A shared, decrementing byte allowance for one detection call.
///
/// Why: the walk reads many small files. Capping each file individually still
/// permits `members × per-file-cap` in aggregate, which for 256 members and a
/// 1 MiB cap is 256 MiB — enough to stall a session launch. A single budget
/// threaded through the whole call bounds the aggregate, which is the quantity
/// that actually matters.
/// What: a `Cell<u64>` of remaining bytes. [`ProbeBudget::take`] reserves an
/// amount and reports whether it was available; once exhausted every subsequent
/// request fails, so the walk degrades to "no further markers found".
/// Test: `budget_exhaustion_stops_probing`, `budget_is_shared_across_members`.
#[derive(Debug)]
pub(crate) struct ProbeBudget {
    remaining: Cell<u64>,
}

impl ProbeBudget {
    /// A budget of [`MAX_WORKSPACE_PROBE_BYTES`].
    pub(crate) fn new() -> Self {
        Self {
            remaining: Cell::new(MAX_WORKSPACE_PROBE_BYTES),
        }
    }

    /// A budget of exactly `bytes` — for tests that need to force exhaustion.
    #[cfg(test)]
    pub(crate) fn with_bytes(bytes: u64) -> Self {
        Self {
            remaining: Cell::new(bytes),
        }
    }

    /// Reserve `bytes`, returning `false` when the budget cannot cover it.
    pub(crate) fn take(&self, bytes: u64) -> bool {
        let left = self.remaining.get();
        if bytes > left {
            self.remaining.set(0);
            return false;
        }
        self.remaining.set(left - bytes);
        true
    }
}

/// Read a file if it is regular, within [`MAX_MANIFEST_BYTES`], and affordable.
///
/// Why: every read in this module goes through one place so the size cap, the
/// shared budget, and the fail-closed rule cannot drift apart.
/// What: `None` unless `path` is a regular file no larger than
/// [`MAX_MANIFEST_BYTES`], the budget covers its length, and it is valid UTF-8.
/// Test: `oversized_manifest_is_not_read`, `budget_exhaustion_stops_probing`.
pub(crate) fn read_bounded(path: &Path, budget: &ProbeBudget) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST_BYTES {
        return None;
    }
    if !budget.take(meta.len()) {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The directories marker detection should probe: the root, then its members.
///
/// Why: the single entry point detection calls, so "where do we look" is stated
/// once. The root is always first, so a single-package project behaves exactly
/// as it did before this module existed.
/// What: `[project_dir]` followed by every declared workspace member directory
/// that exists, de-duplicated, capped at [`MAX_WORKSPACE_MEMBERS`], in
/// deterministic (sorted-per-pattern) order.
/// Test: `npm_workspaces_array_members`, `pnpm_workspace_members`,
/// `elixir_umbrella_members`, `single_package_project_probes_only_root`.
pub(crate) fn probe_roots(project_dir: &Path, budget: &ProbeBudget) -> Vec<PathBuf> {
    let mut roots = vec![project_dir.to_path_buf()];
    for member in workspace_members(project_dir, budget) {
        // `roots` carries the project root plus members, so the ceiling is
        // MAX_WORKSPACE_MEMBERS members = MAX_WORKSPACE_MEMBERS + 1 entries.
        if roots.len() > MAX_WORKSPACE_MEMBERS {
            break;
        }
        if !roots.contains(&member) {
            roots.push(member);
        }
    }
    roots
}

/// Every workspace member directory declared by `project_dir`'s root manifest.
///
/// Why/What: unions the three declaration formats, expands each declared glob,
/// and keeps only paths that exist and are directories. Reading the DECLARATION
/// rather than assuming `packages/*` means a workspace using any other layout is
/// covered without this module guessing at conventions.
/// Test: `npm_workspaces_object_form`, `unknown_root_declares_no_members`.
fn workspace_members(project_dir: &Path, budget: &ProbeBudget) -> Vec<PathBuf> {
    let mut patterns = Vec::new();
    patterns.extend(npm_workspace_patterns(project_dir, budget));
    patterns.extend(pnpm_workspace_patterns(project_dir, budget));
    patterns.extend(elixir_umbrella_patterns(project_dir, budget));
    patterns.truncate(MAX_WORKSPACE_PATTERNS);

    let mut members = Vec::new();
    for pattern in patterns {
        for dir in expand_member_glob(project_dir, &pattern) {
            if members.len() >= MAX_WORKSPACE_MEMBERS {
                return members;
            }
            members.push(dir);
        }
    }
    members
}

/// npm/yarn `workspaces` from `package.json` — array form or `{ packages: [] }`.
///
/// Why: both spellings are in the wild (yarn classic uses the object form when
/// it also declares `nohoist`), and a monorepo using the object form is not
/// meaningfully different from one using the array form.
/// What: parses `package.json` as JSON and returns the string entries of either
/// `workspaces` (array) or `workspaces.packages` (array). A malformed or absent
/// file yields no patterns.
/// Test: `npm_workspaces_array_members`, `npm_workspaces_object_form`,
/// `malformed_package_json_declares_no_members`.
fn npm_workspace_patterns(project_dir: &Path, budget: &ProbeBudget) -> Vec<String> {
    let raw = match read_bounded(&project_dir.join("package.json"), budget) {
        Some(raw) => raw,
        None => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let list = match json.get("workspaces") {
        Some(serde_json::Value::Array(a)) => a.clone(),
        Some(serde_json::Value::Object(o)) => match o.get("packages") {
            Some(serde_json::Value::Array(a)) => a.clone(),
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    list.iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// pnpm `packages:` entries from `pnpm-workspace.yaml`.
///
/// Why: pnpm declares members in a separate file, so a pnpm monorepo's
/// `package.json` says nothing about its members.
/// What: a deliberately minimal reader for the one shape this file has — a
/// `packages:` key followed by `- <glob>` list items. It reads a declaration in
/// a fixed format; it does not attempt general YAML. Quotes are stripped and a
/// `#` comment tail is ignored. Anything it cannot understand yields no
/// patterns, which is fail-closed.
/// Test: `pnpm_workspace_members`, `pnpm_workspace_ignores_other_keys`.
fn pnpm_workspace_patterns(project_dir: &Path, budget: &ProbeBudget) -> Vec<String> {
    let raw = match read_bounded(&project_dir.join("pnpm-workspace.yaml"), budget) {
        Some(raw) => raw,
        None => return Vec::new(),
    };
    let mut patterns = Vec::new();
    let mut in_packages = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(item) = trimmed.strip_prefix("- ").or(trimmed.strip_prefix("-")) {
            if in_packages {
                let item = item.split('#').next().unwrap_or("").trim();
                let item = item.trim_matches(|c| c == '\'' || c == '"').trim();
                if !item.is_empty() {
                    patterns.push(item.to_string());
                }
            }
            continue;
        }
        // A non-list line at any indentation ends the `packages:` block.
        in_packages = trimmed == "packages:";
    }
    patterns
}

/// An Elixir umbrella's member directory from `mix.exs`'s `apps_path:`.
///
/// Why: an umbrella project's root `mix.exs` declares no dependencies of its
/// own — every app lives under `apps_path`, so a root-only probe never sees
/// `{:phoenix,` in an umbrella.
/// What: finds `apps_path:` and takes its quoted value, returning `<value>/*`.
/// `mix.exs` is Elixir source rather than a data format, so this reads the one
/// declaration it needs rather than attempting to evaluate the file.
/// Test: `elixir_umbrella_members`, `plain_mix_project_declares_no_members`.
fn elixir_umbrella_patterns(project_dir: &Path, budget: &ProbeBudget) -> Vec<String> {
    let raw = match read_bounded(&project_dir.join("mix.exs"), budget) {
        Some(raw) => raw,
        None => return Vec::new(),
    };
    for line in raw.lines() {
        let Some(rest) = line.split_once("apps_path:") else {
            continue;
        };
        let rest = rest.1.trim_start();
        let quote = match rest.chars().next() {
            Some(c @ ('"' | '\'')) => c,
            _ => continue,
        };
        if let Some(value) = rest[1..].split(quote).next()
            && !value.is_empty()
        {
            return vec![format!("{value}/*")];
        }
    }
    Vec::new()
}

/// Expand one declared member glob into existing directories under `root`.
///
/// Why: a declared pattern is usually a glob (`packages/*`), and expanding it
/// against the real filesystem is what turns a declaration into probe targets.
/// What: splits on `/` and walks literal segments until the first containing
/// `*`; that segment's position is expanded to the matching DIRECT child
/// directories (matched with the manifest's own glob vocabulary via
/// [`super::schema::matches_any`]). Segments after the glob are ignored — only
/// the member root is needed, and honouring them would require recursive
/// descent this deliberately does not do. `**` is treated as a single `*`. A
/// pattern with no glob resolves to one literal directory. A pattern containing
/// `..` is REJECTED so a declaration cannot walk outside the project.
/// Test: `glob_expands_one_level`, `literal_member_path`,
/// `pattern_with_parent_escape_is_rejected`, `double_star_is_one_level`.
fn expand_member_glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    if pattern.contains("..") {
        return Vec::new();
    }
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let glob_at = segments.iter().position(|s| s.contains('*'));

    let Some(idx) = glob_at else {
        // No glob: one literal member directory.
        let path = segments.iter().fold(root.to_path_buf(), |p, s| p.join(s));
        return if path.is_dir() {
            vec![path]
        } else {
            Vec::new()
        };
    };

    let parent = segments[..idx]
        .iter()
        .fold(root.to_path_buf(), |p, s| p.join(s));
    // `**` carries no more meaning here than `*` — see the doc comment.
    let seg = segments[idx].replace("**", "*");
    let patterns = [seg];

    let Ok(entries) = std::fs::read_dir(&parent) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| super::schema::matches_any(n, &patterns))
        })
        .map(|e| e.path())
        .collect();
    // Deterministic order so a capped walk is reproducible.
    out.sort();
    out
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
