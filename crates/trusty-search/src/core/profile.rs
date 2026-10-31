//! The `std-v1` collection profile: the standard exclude set and the `code` /
//! `docs` / `excluded` path classes (#10532, epic #10529 section 2).
//!
//! Why: one index per project holds two collections as path classes. Records
//! (an owner queue, dashboards, logs, archives) must never be indexed: a
//! decision query returned the answered item from `local/owner-queue/items`.
//! Every index applied only the excludes its creator happened to send.
//! What: [`STD_V1_GLOBS`] is the standard exclude set, [`PathClass`] names the
//! three classes, and [`resolve`] picks the exclude list for a new index in
//! the order `.trusty-search.yaml`, then the registry override, then the named
//! profile (`std-v1` by default). The result carries a name and a hash of the
//! effective glob list, which are stored on the index and shown by
//! `index_status`, so drift from the standard is visible.
//! Test: this module's tests; `service::server::profile_10532_tests`.

use std::path::Path;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::core::repo_config::path_matches_any_glob;

/// The standard profile's name, and the default for a new index.
pub const STD_V1: &str = "std-v1";

/// The name stored for an index whose excludes came from a caller (a
/// `.trusty-search.yaml`, the registry override, or an explicit request).
pub const CUSTOM: &str = "custom";

/// The `std-v1` exclude globs, matched on the ROOT-RELATIVE path.
///
/// Why: derived from the Architect tree and checked by a `du` and `find` pass
/// over the `rdash`, `apex`, `cto` and `trusty-tools` checkouts (#10532). Each
/// glob that can name source or docs elsewhere is either scoped to the
/// Architect's `local/` tree or kept for a measured payoff:
/// - `owner-queue`, `owner-queue-archive`, `.worktrees`, `*.db`, `*.sqlite*`,
///   `*.lock`, `dashboard*.log`: kept; no source or doc path in the four
///   checkouts carries these names (one `.db` test fixture, lockfiles).
/// - `archive`, `_archive`: kept. cto holds 200 `archive` dirs, 16,190 files,
///   309 MB; apex 258 dirs, 2,608 files. They collide with this repo's
///   `docs/design/archive` (9 archived design docs) and
///   `docs/trusty-agents/_archive` (5), and would drop a code module named
///   `archive`; a PATCH with an explicit list overrides.
/// - `logs`, `backups`, `checkpoints`, `actions.md`, `usage`, `dashboard*`
///   JSON: scoped to `**/local/`. Their payoff is small (cto `logs` 124 files,
///   15 MB; Architect `backups` 0.9 MB, `checkpoints` 4 KB) and they collide:
///   `docs/research/.../01-research/logs` (10 research logs), `src/logs/`,
///   `src/checkpoints/`, `docs/actions.md`, `crates/trusty-agents/src/usage`,
///   `schemas/dashboard/*.json`. `local/usage` is 32 MB in the Architect.
///   (`templates/local/actions.md` in this repo is still excluded.)
///
/// What: record directories, archives, worktrees, databases, lockfiles and
/// logs. `briefs/**` is deliberately absent: briefs are documents.
/// Test: `std_v1_excludes_the_architect_records_and_keeps_source`.
pub const STD_V1_GLOBS: &[&str] = &[
    "**/owner-queue/**",
    "**/owner-queue-archive/**",
    "**/archive/**",
    "**/_archive/**",
    "**/node_modules/**",
    "**/.worktrees/**",
    "**/*.db",
    "**/*.sqlite*",
    "**/*.lock",
    "**/dashboard*.log",
    "**/local/dashboard*/**/*.json",
    "**/local/checkpoints/**",
    "**/local/backups/**",
    "**/local/logs/**",
    "**/local/actions.md",
    "**/local/usage/**",
];

/// Extensions of the `docs` class (design section 2).
const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "rst", "adoc", "txt"];

/// The class of a path inside an index.
///
/// Why: the design names two collections and an excluded class inside one
/// index; callers (health, ranking) need one answer, not three predicates.
/// Test: `classify_names_the_three_classes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClass {
    /// Source and config that ships with code; searched with `mode=code`.
    Code,
    /// Prose (`*.md *.mdx *.rst *.adoc *.txt`); searched with `mode=text`.
    Docs,
    /// Never indexed: matches [`STD_V1_GLOBS`].
    Excluded,
}

/// The class of `rel_path` (index-relative) under `std-v1`.
pub fn classify(rel_path: &Path) -> PathClass {
    if path_matches_any_glob(rel_path, &std_v1_globs()) {
        return PathClass::Excluded;
    }
    let is_doc = rel_path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| DOC_EXTENSIONS.iter().any(|d| d.eq_ignore_ascii_case(e)));
    if is_doc {
        PathClass::Docs
    } else {
        PathClass::Code
    }
}

/// [`STD_V1_GLOBS`] as owned strings.
pub fn std_v1_globs() -> Vec<String> {
    STD_V1_GLOBS.iter().map(|g| (*g).to_string()).collect()
}

/// A profile name no daemon version defines.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown exclude profile {0:?}; the defined profile is \"std-v1\"")]
pub struct UnknownProfile(pub String);

/// Which input supplied the effective exclude list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A committed `.trusty-search.yaml`.
    RepoConfig,
    /// The registry override (`index.exclude`) or an explicit request list.
    Registry,
    /// The named profile.
    Profile,
}

impl Source {
    /// The wire name `search.project.ensure` reports as `profile_source`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepoConfig => "RepoConfig",
            Self::Registry => "Registry",
            Self::Profile => "Profile",
        }
    }
}

/// The exclude list an index runs with, and what to store about it.
///
/// Test: `a_yaml_list_beats_the_registry_which_beats_the_profile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    /// [`STD_V1`] when the profile supplied the list, else [`CUSTOM`].
    pub name: String,
    /// [`hash`] of `globs`.
    pub hash: String,
    /// The effective exclude globs.
    pub globs: Vec<String>,
    /// Which input won.
    pub source: Source,
}

impl ResolvedProfile {
    /// `std-v1@<hash>`, the form `index_status` shows.
    pub fn label(&self) -> String {
        label(&self.name, &self.hash)
    }
}

/// An exact, caller-supplied list stored as [`CUSTOM`] (an empty list too).
///
/// Why: `PATCH /indexes/:id/config` with `exclude_globs` means exactly that
/// list; unlike create, an empty list clears the excludes instead of
/// falling back to `std-v1`.
/// Test: `a_profile_patch_removes_newly_excluded_files_and_does_not_rebuild` in `profile_10532_tests`.
pub fn custom(globs: Vec<String>) -> ResolvedProfile {
    ResolvedProfile {
        name: CUSTOM.to_string(),
        hash: hash(&globs),
        globs,
        source: Source::Registry,
    }
}

/// `name@hash`.
pub fn label(name: &str, hash: &str) -> String {
    format!("{name}@{hash}")
}

/// Short, order-independent digest of an exclude list.
///
/// What: the first 8 hex digits of SHA-256 over the sorted, de-duplicated
/// globs joined by `\n`. Reordering a list keeps its hash; adding one glob
/// changes it, which is what makes drift from `std-v1` visible.
/// Test: `the_hash_ignores_order_and_tracks_content`.
pub fn hash(globs: &[String]) -> String {
    let mut sorted: Vec<&str> = globs.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let digest = Sha256::digest(sorted.join("\n").as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn non_empty(list: Option<&[String]>) -> Option<&[String]> {
    list.filter(|l| !l.is_empty())
}

/// Choose the exclude list for a new index.
///
/// Why: a committed `.trusty-search.yaml` states what its authors want, the tm
/// registry record is the operator's override, and `std-v1` is the default for
/// everything that states nothing (design section 2, "Where the profile lives").
/// What: the first non-empty of `repo_config`, `registry` wins and is named
/// [`CUSTOM`]; otherwise `profile` (default [`STD_V1`]) supplies the list. An
/// empty list counts as absent, so `exclude_globs: []` still gets `std-v1`.
/// Errors: [`UnknownProfile`] for a profile name this daemon does not define,
/// even when a higher-priority list wins, so a typo is never silent.
/// Test: `a_yaml_list_beats_the_registry_which_beats_the_profile`,
/// `an_unknown_profile_is_refused_even_when_an_override_wins`.
pub fn resolve(
    repo_config: Option<&[String]>,
    registry: Option<&[String]>,
    profile: Option<&str>,
) -> Result<ResolvedProfile, UnknownProfile> {
    let named = profile.map(str::trim).filter(|p| !p.is_empty());
    if let Some(name) = named {
        if name != STD_V1 {
            return Err(UnknownProfile(name.to_string()));
        }
    }
    let (globs, name, source) = if let Some(list) = non_empty(repo_config) {
        (list.to_vec(), CUSTOM, Source::RepoConfig)
    } else if let Some(list) = non_empty(registry) {
        (list.to_vec(), CUSTOM, Source::Registry)
    } else {
        (std_v1_globs(), STD_V1, Source::Profile)
    };
    Ok(ResolvedProfile {
        hash: hash(&globs),
        name: name.to_string(),
        globs,
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_string()).collect()
    }

    /// The Architect records go; source, briefs and schemas stay.
    #[test]
    fn std_v1_excludes_the_architect_records_and_keeps_source() {
        let globs = std_v1_globs();
        for gone in [
            "local/owner-queue/items/x.json",
            "owner-queue/items/x.json",
            "local/owner-queue-archive/2026/y.json",
            "local/archive/old.md",
            "local/actions.md",
            "local/usage/2026-10.jsonl",
            "docs/design/archive/README.md",
            "local/dashboard-serve.log",
            "local/dashboard-apex/state/a.json",
            "local/logs/run.txt",
            "local/backups/a.json",
            "local/checkpoints/c.json",
            ".worktrees/feat/src/lib.rs",
            "briefs/data.db",
            "Cargo.lock",
        ] {
            assert!(
                path_matches_any_glob(Path::new(gone), &globs),
                "{gone} must be excluded"
            );
        }
        for kept in [
            "src/lib.rs",
            "docs/adr/0001.md",
            "local/briefs/plan.md",
            "crates/trusty-agents/src/usage/mod.rs",
            "crates/trusty-architect/src/poller/steps/usage/a.rs",
            "schemas/dashboard/widget.json",
            "package.json",
            "src/logs/mod.rs",
            "src/checkpoints/store.rs",
            "src/backups/mod.rs",
            "docs/actions.md",
            "docs/research/harness/01-research/logs/README.md",
        ] {
            assert!(
                !path_matches_any_glob(Path::new(kept), &globs),
                "{kept} must stay indexed"
            );
        }
    }

    #[test]
    fn classify_names_the_three_classes() {
        assert_eq!(classify(Path::new("src/main.rs")), PathClass::Code);
        assert_eq!(classify(Path::new("Cargo.toml")), PathClass::Code);
        for doc in ["README.md", "docs/a.MDX", "x.rst", "y.adoc", "z.txt"] {
            assert_eq!(classify(Path::new(doc)), PathClass::Docs, "{doc}");
        }
        assert_eq!(
            classify(Path::new("local/owner-queue/items/a.md")),
            PathClass::Excluded
        );
    }

    #[test]
    fn the_hash_ignores_order_and_tracks_content() {
        let a = hash(&s(&["a/**", "b/**"]));
        assert_eq!(a, hash(&s(&["b/**", "a/**", "a/**"])));
        assert_ne!(a, hash(&s(&["a/**", "b/**", "c/**"])));
        assert_eq!(a.len(), 8);
    }

    /// `.trusty-search.yaml`, then the registry override, then `std-v1`.
    #[test]
    fn a_yaml_list_beats_the_registry_which_beats_the_profile() {
        let yaml = s(&["yaml/**"]);
        let reg = s(&["registry/**"]);
        let all = resolve(Some(&yaml), Some(&reg), Some(STD_V1)).expect("resolves");
        assert_eq!(
            (all.source, all.globs.as_slice()),
            (Source::RepoConfig, yaml.as_slice())
        );
        assert_eq!(all.name, CUSTOM);
        let two = resolve(None, Some(&reg), None).expect("resolves");
        assert_eq!(
            (two.source, two.globs.as_slice()),
            (Source::Registry, reg.as_slice())
        );
        let none = resolve(Some(&[]), Some(&[]), None).expect("resolves");
        assert_eq!(none.source, Source::Profile);
        assert_eq!(none.name, STD_V1);
        assert_eq!(none.globs, std_v1_globs());
        assert_eq!(none.label(), format!("std-v1@{}", hash(&std_v1_globs())));
    }

    #[test]
    fn an_unknown_profile_is_refused_even_when_an_override_wins() {
        let yaml = s(&["yaml/**"]);
        let err = resolve(Some(&yaml), None, Some("std-v9")).expect_err("unknown");
        assert_eq!(err, UnknownProfile("std-v9".into()));
    }

    /// Every shipped glob parses, or an index created with it would be held.
    #[test]
    fn every_std_v1_glob_parses() {
        crate::core::repo_config::validate_exclude_globs(&std_v1_globs()).expect("valid globs");
    }
}
