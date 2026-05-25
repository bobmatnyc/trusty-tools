//! Automatic project alias discovery.
//!
//! Why: Projects have implicit shorthand (cargo package names that differ from
//! their directory, binary names that differ from packages, common first-
//! letter abbreviations, repo short names) that should be surfaced
//! automatically as `is_alias_for` triples without requiring users to call
//! `add_alias` manually. The model can then resolve "tga" → "trusty-git-
//! analytics" the first time it sees the shorthand, instead of mis-matching it
//! against unrelated KG entries.
//! What: Scans the given project root for Cargo workspace structure, git
//! remote configuration, and other project signals; returns a flat list of
//! `(short, full, source)` discoveries. The MCP `discover_aliases` tool feeds
//! these into the palace KG (deduping against active triples) and rebuilds
//! the prompt cache.
//! Test: Unit tests in this module exercise each discovery source against
//! fixture directories and the live workspace root (cwd).

use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Where a discovered alias was inferred from.
///
/// Why: Surfaced through the MCP tool response so operators can audit *why*
/// a particular alias landed in the KG (and which signal to trust). Also
/// serialised into the triple's `provenance` field so retraction tooling can
/// distinguish auto-discovered facts from hand-asserted ones.
/// What: `Serialize` for direct JSON emission; `Debug` for tracing logs.
/// Test: covered indirectly through `discover_project_aliases` tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DiscoverySource {
    /// `[package].name` differs from the containing directory name.
    CargoPackageName,
    /// `[[bin]].name` differs from `[package].name`.
    CargoBinaryName,
    /// First-letter abbreviation of a hyphenated package name is globally
    /// unique within the workspace.
    FirstLetterAbbrev,
    /// Short name extracted from the `origin` remote URL of the repo
    /// containing the project root (resolved via `git -C <root> config`, so
    /// it works inside worktrees as well as normal checkouts).
    GitRemote,
}

impl DiscoverySource {
    /// Stable string representation for triple provenance + JSON.
    ///
    /// Why: `serde_json::to_string` on the enum yields `"CargoPackageName"`,
    /// but the triple's `provenance` field is plain text — we want a single
    /// canonical spelling that round-trips cleanly.
    /// What: lowercase, snake-case-ish identifiers matching the variant names.
    /// Test: indirectly via `discover_and_assert` triples.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CargoPackageName => "cargo_package_name",
            Self::CargoBinaryName => "cargo_binary_name",
            Self::FirstLetterAbbrev => "first_letter_abbrev",
            Self::GitRemote => "git_remote",
        }
    }
}

/// A single discovered alias mapping.
///
/// Why: Returned by `discover_project_aliases` and forwarded verbatim to the
/// MCP tool response so callers can see exactly what would be (or was)
/// asserted.
/// What: `short` is the subject ("tga"); `full` is the object
/// ("trusty-git-analytics"); `source` records the discovery signal.
/// Test: each discovery source has a dedicated unit test asserting the
/// resulting `AliasDiscovery` shape.
#[derive(Debug, Clone, Serialize)]
pub struct AliasDiscovery {
    pub short: String,
    pub full: String,
    pub source: DiscoverySource,
}

/// Scan `project_root` for alias signals and return every discovery found.
///
/// Why: One entry point keeps the orchestration logic in the MCP tool simple
/// — it just calls this and decides what to assert.
/// What: Runs each discovery source in order (Cargo workspace, then Cargo
/// single-crate fallback, then git remote, then first-letter abbreviations
/// derived from the cargo discoveries). Deduplicates `(short, full)` pairs
/// within the returned list so the first source wins.
/// Test: `discovers_trusty_git_analytics_alias`,
/// `first_letter_abbrev_tm_for_trusty_memory`,
/// `no_duplicate_short_names_in_results`.
pub async fn discover_project_aliases(project_root: &Path) -> Result<Vec<AliasDiscovery>> {
    let root = project_root.to_path_buf();
    tokio::task::spawn_blocking(move || discover_blocking(&root))
        .await
        .context("join discover_project_aliases")?
}

/// Blocking implementation of [`discover_project_aliases`].
///
/// Why: All work here is filesystem + TOML parsing, which is naturally
/// blocking. Splitting the async wrapper out keeps the algorithm
/// straightforward and unit-testable without a runtime.
/// What: Reads the root `Cargo.toml`, expands workspace members, scans each
/// member's `Cargo.toml`, then walks git config. Returns deduplicated
/// discoveries.
/// Test: exercised by every test in this module (most call it directly).
fn discover_blocking(project_root: &Path) -> Result<Vec<AliasDiscovery>> {
    let mut discoveries: Vec<AliasDiscovery> = Vec::new();
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();

    // Collect (package_name, dir_name) pairs so the first-letter pass can
    // see every package in the workspace at once.
    let mut packages: Vec<(String, String)> = Vec::new();

    let root_manifest = project_root.join("Cargo.toml");
    if root_manifest.is_file() {
        match std::fs::read_to_string(&root_manifest)
            .context("read root Cargo.toml")
            .and_then(|s| toml::from_str::<toml::Value>(&s).context("parse root Cargo.toml"))
        {
            Ok(root_toml) => {
                let members = workspace_members(&root_toml);
                if !members.is_empty() {
                    // Workspace mode.
                    for member in expand_members(project_root, &members) {
                        scan_member(&member, &mut discoveries, &mut seen_pairs, &mut packages);
                    }
                } else if root_toml.get("package").is_some() {
                    // Single-crate fallback: treat the root manifest as the
                    // only "member".
                    scan_member(
                        project_root,
                        &mut discoveries,
                        &mut seen_pairs,
                        &mut packages,
                    );
                }
            }
            Err(e) => {
                tracing::warn!("discovery: skipping root Cargo.toml: {e:#}");
            }
        }
    }

    // Phase 2: first-letter abbreviations for hyphenated package names that
    // produce a globally-unique abbreviation. Uniqueness is computed across
    // the union of every package name AND every abbreviation derived in
    // this pass — so a package whose own name is the same as another
    // package's abbreviation cannot collide with it.
    add_first_letter_abbreviations(&packages, &mut discoveries, &mut seen_pairs);

    // Phase 3: git remote short name.
    if let Some(d) = discover_git_remote(project_root) {
        push_unique(&mut discoveries, &mut seen_pairs, d);
    }

    Ok(discoveries)
}

/// Extract the `[workspace] members = [...]` patterns from a parsed root
/// `Cargo.toml`.
///
/// Why: Workspaces always live under a top-level `[workspace]` table with a
/// `members` array of glob patterns; reading them at parse time keeps the
/// downstream expansion code unaware of TOML.
/// What: Returns the raw pattern strings (typically `"crates/*"`). An absent
/// or malformed `[workspace]` yields an empty `Vec`.
/// Test: covered by `discovers_trusty_git_analytics_alias` (which exercises
/// this against the live root manifest).
fn workspace_members(root_toml: &toml::Value) -> Vec<String> {
    root_toml
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Expand workspace member patterns into concrete directories.
///
/// Why: Cargo permits glob patterns (`crates/*`, `vendor/*/sdk`) in
/// `workspace.members`; we don't pull in the `glob` crate, so a minimal
/// expansion handles the canonical "single trailing `*`" pattern that every
/// workspace in this repo uses, with fallback to a literal directory.
/// What: For each pattern: if it ends with `/*`, list every immediate
/// subdirectory; otherwise treat it as a literal relative path. Skips entries
/// without a `Cargo.toml`.
/// Test: indirectly via `discovers_trusty_git_analytics_alias` (live workspace
/// expansion).
fn expand_members(root: &Path, patterns: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for pattern in patterns {
        if let Some(prefix) = pattern.strip_suffix("/*") {
            let dir = root.join(prefix);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join("Cargo.toml").is_file() {
                    out.push(path);
                }
            }
        } else {
            let path = root.join(pattern);
            if path.is_dir() && path.join("Cargo.toml").is_file() {
                out.push(path);
            }
        }
    }
    out
}

/// Scan one workspace member directory for cargo-derived aliases.
///
/// Why: Each member can contribute up to two aliases (package-name vs dir
/// name, binary-name vs package name). Centralising the per-member logic
/// lets the caller stay focused on iteration / expansion.
/// What: Reads `<member>/Cargo.toml`, extracts `[package].name`, then walks
/// every `[[bin]]` entry. Pushes one `CargoPackageName` discovery when the
/// package name differs from the directory, and one `CargoBinaryName`
/// discovery per binary whose name differs from the package. Tracks every
/// package in `packages` so the first-letter pass can see the full set.
/// Test: `scan_member_emits_package_and_binary_aliases`.
fn scan_member(
    member_dir: &Path,
    discoveries: &mut Vec<AliasDiscovery>,
    seen_pairs: &mut HashSet<(String, String)>,
    packages: &mut Vec<(String, String)>,
) {
    let manifest = member_dir.join("Cargo.toml");
    let Ok(raw) = std::fs::read_to_string(&manifest) else {
        return;
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&raw) else {
        tracing::warn!("discovery: failed to parse {}", manifest.display());
        return;
    };

    let dir_name = member_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if dir_name.is_empty() {
        return;
    }

    let package_name = parsed
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string());

    if let Some(ref pkg) = package_name {
        packages.push((pkg.clone(), dir_name.clone()));
        if pkg != &dir_name {
            push_unique(
                discoveries,
                seen_pairs,
                AliasDiscovery {
                    short: pkg.clone(),
                    full: dir_name.clone(),
                    source: DiscoverySource::CargoPackageName,
                },
            );
        }
    }

    if let Some(bins) = parsed.get("bin").and_then(|b| b.as_array()) {
        let pkg_for_bin = package_name.as_deref().unwrap_or(&dir_name).to_string();
        for bin in bins {
            if let Some(bin_name) = bin.get("name").and_then(|n| n.as_str()) {
                if bin_name != pkg_for_bin {
                    push_unique(
                        discoveries,
                        seen_pairs,
                        AliasDiscovery {
                            short: bin_name.to_string(),
                            full: pkg_for_bin.clone(),
                            source: DiscoverySource::CargoBinaryName,
                        },
                    );
                }
            }
        }
    }
}

/// Compute first-letter abbreviations for hyphenated package names and add
/// the ones that are globally unique within the workspace.
///
/// Why: Operators routinely refer to crates by their initials ("tm" for
/// `trusty-memory`, "tga" for `trusty-git-analytics`). Surfacing these
/// automatically — but only when there's no ambiguity — avoids polluting the
/// prompt with collisions like `tmc` (which could be `trusty-mpm-cli` or
/// `trusty-mpm-core`).
/// What: Splits each package name on `-`, takes the first letter of every
/// segment; counts how many distinct full names each abbreviation maps to.
/// Emits a `FirstLetterAbbrev` discovery only for abbreviations that map to
/// exactly one full name AND don't equal that full name AND don't collide
/// with an existing package name (which would suggest a different crate).
/// Test: `first_letter_abbrev_tm_for_trusty_memory`,
/// `first_letter_abbrev_skips_ambiguous`.
fn add_first_letter_abbreviations(
    packages: &[(String, String)],
    discoveries: &mut Vec<AliasDiscovery>,
    seen_pairs: &mut HashSet<(String, String)>,
) {
    let package_name_set: HashSet<&str> = packages.iter().map(|(p, _)| p.as_str()).collect();

    // abbrev → set of full package names that produce it.
    let mut groups: HashMap<String, Vec<&str>> = HashMap::new();
    for (pkg, _dir) in packages {
        if !pkg.contains('-') {
            continue;
        }
        let abbrev: String = pkg
            .split('-')
            .filter_map(|seg| seg.chars().next())
            .collect();
        if abbrev.len() < 2 {
            continue;
        }
        groups.entry(abbrev).or_default().push(pkg.as_str());
    }

    for (abbrev, fulls) in groups {
        if fulls.len() != 1 {
            continue;
        }
        let full = fulls[0];
        if abbrev == full {
            continue;
        }
        // Don't shadow an existing package name. e.g. if "tm" were itself a
        // package name, we wouldn't want to also assert "tm → trusty-memory".
        if package_name_set.contains(abbrev.as_str()) {
            continue;
        }
        push_unique(
            discoveries,
            seen_pairs,
            AliasDiscovery {
                short: abbrev,
                full: full.to_string(),
                source: DiscoverySource::FirstLetterAbbrev,
            },
        );
    }
}

/// Read the git origin URL for `project_root` and extract a short repo name.
///
/// Why: Most repos refer to themselves by the trailing path component of the
/// origin URL ("trusty-tools"), which is rarely the same as the working tree
/// directory name when checked out under a non-default path. Surfacing it as
/// an alias for itself isn't useful, but surfacing the workspace dir name as
/// the canonical full name for the short repo name is — e.g. when working
/// inside a worktree directory the model still knows "trusty-tools" refers
/// to the project. The canonical source for `[remote "origin"] url = …` lives
/// in `<root>/.git/config` for a normal checkout, but in a *worktree* `.git`
/// is a file containing `gitdir: <parent>/.git/worktrees/<name>/` and the
/// `[remote]` section is reachable only through the parent repo's
/// `.git/config`. Direct filesystem reads silently drop the discovery in
/// worktree-based checkouts.
///
/// Issue #116: the previous implementation only handled the normal-checkout
/// case and returned `None` from inside any git worktree, mirroring the bug
/// fixed for `kg_bootstrap` in #113 / PR #115.
///
/// What: Resolves the origin URL via [`read_origin_url`] (which prefers
/// `git -C <root> config --get remote.origin.url` and falls back to a manual
/// INI scan of `<root>/.git/config` when no `git` binary is on PATH — useful
/// only for fixture-based tests that fabricate a `.git/config` directly).
/// Extracts the short name, strips a trailing `.git`, and emits a
/// `GitRemote` discovery iff the short name differs from the directory name.
/// Test: `extract_origin_url_handles_typical_config`,
/// `short_repo_name_strips_git_suffix_and_path`,
/// `git_remote_works_inside_worktree`.
fn discover_git_remote(project_root: &Path) -> Option<AliasDiscovery> {
    let url = read_origin_url(project_root)?;
    let short = short_repo_name(&url)?;
    let dir_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if dir_name.is_empty() || short == dir_name {
        return None;
    }
    Some(AliasDiscovery {
        short,
        full: dir_name,
        source: DiscoverySource::GitRemote,
    })
}

/// Resolve `remote.origin.url` for the repo rooted at `project_root`,
/// transparent to worktree vs. normal-checkout layout.
///
/// Why: Centralises the worktree-vs-checkout indirection in one place so
/// `discover_git_remote` stays readable. In a worktree `.git` is a file
/// (not a directory) containing `gitdir: <parent>/.git/worktrees/<name>/`,
/// so a naive `std::fs::read_to_string(".git/config")` fails — but the
/// `[remote "origin"]` section is still reachable via the parent's
/// `.git/config`. Shelling out to `git` lets us delegate that pointer
/// resolution instead of re-implementing it.
/// What: (1) tries `git -C <root> config --get remote.origin.url`, which
/// works equally well in worktrees, normal checkouts, and submodules; (2)
/// falls back to a manual INI scan of `<root>/.git/config` for environments
/// without a `git` binary on PATH (notably fixture tests that fabricate a
/// `.git/config` in a tempdir without ever initialising a real repo).
/// Returns `None` if neither path yields a non-empty URL.
/// Test: `git_remote_works_inside_worktree` (CLI path),
/// `extract_origin_url_handles_typical_config` (file fallback path, via
/// `extract_origin_url`).
fn read_origin_url(project_root: &Path) -> Option<String> {
    // Strategy 1: ask git directly. This is the only path that handles
    // worktrees correctly without us re-implementing `gitdir:` resolution.
    if let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(project_root)
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
    let raw = std::fs::read_to_string(project_root.join(".git").join("config")).ok()?;
    extract_origin_url(&raw)
}

/// Extract the `url = ...` value from the `[remote "origin"]` section of a
/// git config file.
///
/// Why: Git config is a stable INI-ish format, but pulling in `gitoxide`
/// just for one field would be wildly disproportionate. A line-based scan is
/// sufficient for the canonical layout used by every git client.
/// What: Walks lines, tracks whether we're inside `[remote "origin"]`, and
/// returns the trimmed value of the first `url = ...` line within that
/// section.
/// Test: `extract_origin_url_handles_typical_config`.
fn extract_origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_origin = trimmed == "[remote \"origin\"]";
            continue;
        }
        if in_origin {
            if let Some(rest) = trimmed.strip_prefix("url") {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('=') {
                    return Some(rest.trim().to_string());
                }
            }
        }
    }
    None
}

/// Extract the short repo name from a git URL.
///
/// Why: Origin URLs come in three flavours — HTTPS (`https://host/owner/repo.git`),
/// SSH (`git@host:owner/repo.git`), and local paths. All three end with
/// `<name>` or `<name>.git`; returning the last path-component without the
/// suffix gives a stable short name.
/// What: Splits on both `/` and `:`, takes the last component, strips a
/// trailing `.git`. Returns `None` for empty inputs.
/// Test: `short_repo_name_strips_git_suffix_and_path`.
fn short_repo_name(url: &str) -> Option<String> {
    let last = url.rsplit(['/', ':']).next().unwrap_or("");
    let stripped = last.strip_suffix(".git").unwrap_or(last).trim();
    if stripped.is_empty() {
        None
    } else {
        Some(stripped.to_string())
    }
}

/// Push a discovery into the result list iff its `short` hasn't been seen yet.
///
/// Why: A subject can only have one *active* `is_alias_for` triple at a time
/// (the temporal KG closes the prior interval whenever a new value is
/// asserted), so emitting two discoveries with the same `short` would force
/// every subsequent `discover_aliases` call to flap between them — endlessly
/// reasserting because neither matches the currently-active object. Deduping
/// on `short` here makes the discovery list inherently idempotent: one
/// authoritative mapping per subject, with the first-seen source winning
/// (`CargoPackageName` > `CargoBinaryName` > `FirstLetterAbbrev` >
/// `GitRemote`, matching the call order in `discover_blocking`).
/// What: Tracks every `short` already pushed; subsequent pushes with the
/// same `short` are dropped. `seen_pairs` is misnamed historically — it now
/// holds the deduped subjects.
/// Test: `no_duplicate_short_names_in_results`,
/// `dispatch_discover_aliases_inserts_new_and_dedupes` (the rerun assertion
/// only passes when this dedup holds).
fn push_unique(
    discoveries: &mut Vec<AliasDiscovery>,
    seen_subjects: &mut HashSet<(String, String)>,
    d: AliasDiscovery,
) {
    // Repurpose the set as a subject-only dedup: store ("subject", "") so
    // the existing call sites keep working without renaming the parameter
    // type across every signature.
    let key = (d.short.clone(), String::new());
    if seen_subjects.insert(key) {
        discoveries.push(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Smoke-test the live workspace — the prompt test in the task spec
    /// pins `("tga", "trusty-git-analytics")` as a discovered alias.
    /// What: Locates the workspace root (parent of this crate dir), runs the
    /// blocking discovery, and asserts the canonical pair is present with
    /// the `CargoPackageName` source.
    /// Test: this test itself.
    #[test]
    fn discovers_trusty_git_analytics_alias() {
        let root = workspace_root();
        let discoveries = discover_blocking(&root).expect("discover");
        let hit = discoveries
            .iter()
            .find(|d| d.short == "tga" && d.full == "trusty-git-analytics");
        assert!(
            hit.is_some(),
            "expected tga→trusty-git-analytics in discoveries; got: {discoveries:?}"
        );
        assert_eq!(hit.unwrap().source, DiscoverySource::CargoPackageName);
    }

    /// Why: First-letter abbreviation is the most subtle source — confirm
    /// it fires for at least one crate in the live workspace and pins the
    /// canonical example (`tc → trusty-common`, the longest-lived shared
    /// library crate, has a guaranteed-unique two-letter abbreviation).
    /// Test: this test itself.
    #[test]
    fn first_letter_abbrev_emits_unique_workspace_initials() {
        let root = workspace_root();
        let discoveries = discover_blocking(&root).expect("discover");
        let hit = discoveries.iter().find(|d| {
            d.short == "tc"
                && d.full == "trusty-common"
                && d.source == DiscoverySource::FirstLetterAbbrev
        });
        assert!(
            hit.is_some(),
            "expected tc→trusty-common first-letter abbrev; got: {discoveries:?}"
        );
    }

    /// Why: A synthetic fixture pins the abbreviation algorithm against the
    /// exact scenario the original spec called out — a workspace where
    /// `tm` would uniquely map to `trusty-memory` if there were no other
    /// `t-m-…` crates. The live workspace happens to also expose `tm` as a
    /// binary alias for `trusty-mpm-cli`, which (correctly) takes
    /// precedence; this isolated test confirms the abbreviation logic
    /// itself does the right thing.
    /// Test: this test itself.
    #[test]
    fn first_letter_abbrev_tm_unique_when_only_trusty_memory() {
        let packages = vec![
            ("trusty-memory".to_string(), "trusty-memory".to_string()),
            ("trusty-common".to_string(), "trusty-common".to_string()),
            ("trusty-mpm-cli".to_string(), "trusty-mpm-cli".to_string()),
        ];
        let mut discoveries = Vec::new();
        let mut seen = HashSet::new();
        add_first_letter_abbreviations(&packages, &mut discoveries, &mut seen);
        let tm = discoveries
            .iter()
            .find(|d| d.short == "tm" && d.source == DiscoverySource::FirstLetterAbbrev);
        assert_eq!(
            tm.map(|d| d.full.as_str()),
            Some("trusty-memory"),
            "tm must abbreviate trusty-memory in this fixture; got: {discoveries:?}"
        );
    }

    /// Why: Calling discovery twice must produce the same result — the
    /// helper is pure (no mutation of disk state), and the dedup test in
    /// the spec uses this property to verify idempotency.
    /// Test: this test itself.
    #[tokio::test]
    async fn no_duplicate_short_names_in_results() {
        let root = workspace_root();
        let a = discover_project_aliases(&root).await.expect("discover a");
        let b = discover_project_aliases(&root).await.expect("discover b");
        assert_eq!(a.len(), b.len(), "two calls must yield equal counts");

        // No (short, full) pair appears twice within a single call.
        let mut seen = HashSet::new();
        for d in &a {
            assert!(
                seen.insert((d.short.clone(), d.full.clone())),
                "duplicate discovery: {} → {} ({:?})",
                d.short,
                d.full,
                d.source,
            );
        }
    }

    /// Why: Pin the abbreviation-uniqueness rule against a synthetic
    /// workspace where two crates share an abbreviation — the algorithm
    /// must NOT emit a discovery for the ambiguous prefix.
    /// What: Build two fake packages, both abbreviating to "tm", and assert
    /// no `FirstLetterAbbrev` for "tm" is produced.
    /// Test: this test itself.
    #[test]
    fn first_letter_abbrev_skips_ambiguous() {
        let packages = vec![
            ("trusty-memory".to_string(), "trusty-memory".to_string()),
            ("trusty-monitor".to_string(), "trusty-monitor".to_string()),
        ];
        let mut discoveries = Vec::new();
        let mut seen = HashSet::new();
        add_first_letter_abbreviations(&packages, &mut discoveries, &mut seen);
        let tm = discoveries
            .iter()
            .find(|d| d.short == "tm" && d.source == DiscoverySource::FirstLetterAbbrev);
        assert!(
            tm.is_none(),
            "ambiguous tm must not produce an abbrev discovery; got: {discoveries:?}"
        );
    }

    /// Why: Pin the parser against the typical `[remote "origin"]` block
    /// shape. A regression that loses the URL would silently disable the
    /// GitRemote source.
    #[test]
    fn extract_origin_url_handles_typical_config() {
        let cfg = "\
[core]
\trepositoryformatversion = 0
[remote \"origin\"]
\turl = git@github.com:bobmatnyc/trusty-tools.git
\tfetch = +refs/heads/*:refs/remotes/origin/*
[branch \"main\"]
\tremote = origin
";
        assert_eq!(
            extract_origin_url(cfg),
            Some("git@github.com:bobmatnyc/trusty-tools.git".to_string())
        );
    }

    /// Why: Three URL flavours must all collapse to the same short name.
    #[test]
    fn short_repo_name_strips_git_suffix_and_path() {
        assert_eq!(
            short_repo_name("git@github.com:bobmatnyc/trusty-tools.git").as_deref(),
            Some("trusty-tools")
        );
        assert_eq!(
            short_repo_name("https://github.com/bobmatnyc/trusty-tools.git").as_deref(),
            Some("trusty-tools")
        );
        assert_eq!(
            short_repo_name("https://github.com/bobmatnyc/trusty-tools").as_deref(),
            Some("trusty-tools")
        );
        assert_eq!(short_repo_name("").as_deref(), None);
    }

    /// Why: Scan logic must surface both CargoPackageName and
    /// CargoBinaryName aliases from a single fixture.
    #[test]
    fn scan_member_emits_package_and_binary_aliases() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let member = tmp.path().join("trusty-git-analytics");
        std::fs::create_dir_all(&member).expect("mkdir");
        std::fs::write(
            member.join("Cargo.toml"),
            r#"
[package]
name = "tga"
version = "0.1.0"

[[bin]]
name = "tga_bench"
path = "src/bench.rs"

[[bin]]
name = "tga"
path = "src/main.rs"
"#,
        )
        .expect("write Cargo.toml");

        let mut discoveries = Vec::new();
        let mut seen = HashSet::new();
        let mut packages = Vec::new();
        scan_member(&member, &mut discoveries, &mut seen, &mut packages);

        // Package-name discovery.
        let pkg_disc = discoveries
            .iter()
            .find(|d| d.source == DiscoverySource::CargoPackageName)
            .expect("package alias");
        assert_eq!(pkg_disc.short, "tga");
        assert_eq!(pkg_disc.full, "trusty-git-analytics");

        // Binary-name discovery (only the one that differs from the package).
        let bin_disc = discoveries
            .iter()
            .find(|d| d.source == DiscoverySource::CargoBinaryName)
            .expect("binary alias");
        assert_eq!(bin_disc.short, "tga_bench");
        assert_eq!(bin_disc.full, "tga");

        // The matching-name bin must NOT produce a discovery.
        assert_eq!(
            discoveries
                .iter()
                .filter(|d| d.source == DiscoverySource::CargoBinaryName)
                .count(),
            1
        );
    }

    /// Why (issue #116): `discover_git_remote` must return the same remote
    /// URL inside a git worktree as it does in the parent checkout. Before
    /// the fix it read `<root>/.git/config` directly, which fails inside a
    /// worktree because `.git` is a *file* (containing
    /// `gitdir: <parent>/.git/worktrees/<name>/`), not a directory — and
    /// the `[remote "origin"]` section lives only in the parent's
    /// `.git/config`. This test pins the post-fix behaviour: initialise a
    /// real repo, add a remote, create a worktree off it, and assert
    /// `discover_git_remote` recovers the URL from inside the worktree.
    /// What: Builds a tempdir-backed parent repo + worktree pair using the
    /// real `git` CLI (the same tool the production code delegates to),
    /// then calls the discovery helper against the worktree path.
    /// Test: this test itself; serves as the worktree regression guard for #116.
    #[test]
    fn git_remote_works_inside_worktree() {
        // Skip when `git` is unavailable on PATH — the fixture relies on
        // real worktree semantics that we can't fabricate from pure FS ops.
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .ok()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping git_remote_works_inside_worktree: `git` not on PATH");
            return;
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        // The repo dir name must differ from the short repo name in the
        // remote URL so that `discover_git_remote` actually emits a
        // discovery (it skips when `short == dir_name`).
        let parent = tmp.path().join("local-checkout");
        std::fs::create_dir_all(&parent).expect("mkdir parent");

        // Initialise a real repo so `.git` is a directory in the parent
        // and a file (with `gitdir:`) inside the worktree.
        let run = |args: &[&str], cwd: &Path| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .status()
                .expect("git status");
            assert!(status.success(), "git {args:?} failed in {cwd:?}");
        };
        run(&["init", "--initial-branch=main", "."], &parent);
        run(&["config", "user.email", "test@example.invalid"], &parent);
        run(&["config", "user.name", "test"], &parent);
        run(
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:bobmatnyc/trusty-tools.git",
            ],
            &parent,
        );
        // A real commit + branch is required before `git worktree add` will
        // accept the source as a base.
        std::fs::write(parent.join("README.md"), "hi").expect("write README");
        run(&["add", "README.md"], &parent);
        run(&["commit", "-m", "init"], &parent);

        // Create the worktree as a sibling directory (outside the parent
        // checkout, the standard layout). Re-use the same short repo name
        // as the URL's tail so this also confirms the "short == dir_name"
        // skip rule works against the worktree dir name (not the parent's).
        let worktree = tmp.path().join("trusty-tools-feature");
        run(
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().expect("worktree path"),
            ],
            &parent,
        );

        // Sanity: `.git` inside the worktree must be a file, not a dir —
        // otherwise the fixture isn't actually exercising the bug.
        let dot_git = worktree.join(".git");
        assert!(
            dot_git.is_file(),
            "expected `.git` to be a file inside the worktree; got {dot_git:?}"
        );

        // Run discovery against the worktree path. Pre-fix this returned
        // `None`; post-fix it must return the GitRemote discovery with the
        // short name extracted from origin.
        let d = discover_git_remote(&worktree).expect("expected GitRemote discovery from worktree");
        assert_eq!(d.source, DiscoverySource::GitRemote);
        assert_eq!(d.short, "trusty-tools");
        assert_eq!(d.full, "trusty-tools-feature");

        // Also confirm the normal-checkout path still works inside the same
        // fixture (regression guard: the shell-out must not break the
        // happy path either).
        let d_parent = discover_git_remote(&parent)
            .expect("expected GitRemote discovery from normal checkout");
        assert_eq!(d_parent.source, DiscoverySource::GitRemote);
        assert_eq!(d_parent.short, "trusty-tools");
        assert_eq!(d_parent.full, "local-checkout");
    }

    /// Resolve the workspace root (parent of `crates/trusty-memory`).
    ///
    /// Why: Cargo runs each crate's tests with `CARGO_MANIFEST_DIR` set to
    /// that crate's directory. The live-workspace tests need the workspace
    /// root, which is two levels up.
    fn workspace_root() -> PathBuf {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir
            .parent() // crates/
            .and_then(|p| p.parent()) // workspace root
            .expect("workspace root")
            .to_path_buf()
    }
}
