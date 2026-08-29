//! What counts as EVIDENCE rather than a top-N filler row (#6082).
//!
//! Why: a hybrid search always returns its top-N. A query that finds nothing
//! therefore returns near-zero scores instead of returning nothing, and the
//! ranking presented those rows as headline evidence. The graded self-audit of
//! 2026-08-21 caught both failure modes in one dimension: "authentication &
//! secrets" led with `crates/trusty-search/src/service/call_chain/tests.rs` at
//! score 0.02 — a call-graph TEST file promoted as password-hashing evidence —
//! while the actual middleware, `crates/trusty-agents/src/api/server/auth.rs`,
//! went unread. Two more findings cited test files for production-behaviour
//! claims.
//!
//! The same audit graded "dependencies" INADEQUATE for the opposite reason: it
//! selected three semantic hits and never read `Cargo.toml`, then reported "no
//! manifest-declared dependencies" for a 134-dependency workspace. Dependency
//! manifests are not a semantic question — they are enumerable by name, so
//! [`dependency_manifests`] enumerates them and the search lane never gets the
//! chance to miss them.
//!
//! What: three rules, applied where the per-dimension evidence list is built.
//!
//! | Rule | Effect |
//! |---|---|
//! | [`MIN_EVIDENCE_SHARE`] | a hit far below its own query's best hit is noise, and is dropped |
//! | [`demoted_for`] | a test file sorts after every production file, except under "test coverage" |
//! | [`dependency_manifests`] | the build manifests present in the tree lead the dependencies dimension |
//!
//! Test: `super::quality_tests`.

use std::path::Path;

use super::evidence::FileEvidence;

/// Share of its own query's best hit below which a hit is a filler row.
///
/// Why RELATIVE rather than absolute (#6082): the floor shipped as an absolute
/// `0.15`, calibrated against a run whose filler scored 0.02 and whose signal
/// scored 0.6–0.9. The endpoint the discovery leg actually calls does not
/// produce scores on that scale. `search.query` fuses its lanes
/// with Reciprocal Rank Fusion — `Σ weight / (60 + rank)` over lane weights that
/// sum to one — so the highest score any hit can carry is `1/61 ≈ 0.0164`, an
/// order of magnitude BELOW the floor. Every hit of every query failed it, on
/// every run, and the discovery leg returned nothing whatever the index held:
/// the 2026-08-22 dogfood audit read 85 840 indexed chunks and wrote zero
/// search-derived evidence rows. An absolute constant cannot be right for both
/// score spaces, and a floor that can reject a whole result set is a floor that
/// will.
///
/// Why 0.5: measured against its own query's best hit, the rule is scale-free —
/// it drops the 0.02 filler beside a 0.9 hit exactly as the absolute floor did,
/// and keeps the head of an RRF list. The head is kept by construction: `best`
/// always clears `best * 0.5`, so no query can be silently emptied by this rule
/// again. Still deliberately not tunable, for the reason the absolute constant
/// was not.
pub const MIN_EVIDENCE_SHARE: f32 = 0.5;

/// The dimension for which a test file IS the evidence.
pub const TEST_DIMENSION: &str = "test coverage";

/// The dimension whose evidence is enumerable rather than searchable.
pub const DEPENDENCY_DIMENSION: &str = "dependencies";

/// The floor one query's hits are judged against: a share of its own best hit.
///
/// Why: see [`MIN_EVIDENCE_SHARE`]. Taking `best` as an argument is what keeps
/// the rule scale-free, and what makes "this filter emptied the result set"
/// unreachable rather than merely unlikely.
/// What: `best * MIN_EVIDENCE_SHARE` for a finite positive best; `0.0` for
/// anything else, so a query whose scores are all zero, negative or `NaN` is
/// judged by [`is_evidence`]'s own finiteness check alone rather than by a
/// floor derived from a number that means nothing.
/// Test: `super::quality_tests::the_floor_scales_with_the_best_hit`.
#[must_use]
pub fn evidence_floor(best: f32) -> f32 {
    if best.is_finite() && best > 0.0 {
        best * MIN_EVIDENCE_SHARE
    } else {
        0.0
    }
}

/// True when this hit is worth ranking, against `floor` for the same query.
///
/// A `NaN` score fails this, which is the safe direction: the daemon should
/// never send one, and a comparison against `NaN` is false in both directions.
/// `floor` comes from [`evidence_floor`] over the same query's hits.
/// Test: `super::quality_tests::{rrf_scale_hits_clear_a_relative_floor,
/// a_filler_row_beside_a_real_hit_is_dropped}`.
#[must_use]
pub fn is_evidence(score: f32, floor: f32) -> bool {
    score.is_finite() && score >= floor
}

/// True when `path` should sort behind every production file for `dimension`.
///
/// Why: a test file names the thing it tests, so it matches the query that
/// looks for that thing — and under every dimension but one it is evidence
/// ABOUT the production file rather than evidence itself. A report that cites
/// `call_chain/tests.rs` for "password hashing" has read the wrong file. Under
/// [`TEST_DIMENSION`] the relationship inverts and test files are first-class.
/// What: demotion, never exclusion — a repository whose only auth-adjacent file
/// is a test should still surface it, below anything else it has.
/// Test: `super::quality_tests::{a_test_file_is_demoted_for_a_production_dimension,
/// a_test_file_is_first_class_for_test_coverage}`.
#[must_use]
pub fn demoted_for(dimension: &str, path: &str) -> bool {
    dimension != TEST_DIMENSION && is_test_path(path)
}

/// True when the path is a test or benchmark file.
///
/// Mirrors this repo's own SLOC-cap classification (`docs/reference/sloc-cap.md`)
/// and extends it to the non-Rust conventions an audited repository may use.
/// Deliberately a local copy rather than a shared entry point: the cap script is
/// shell, trusty-review's is private to its selector, and a third consumer is
/// the point at which consolidating becomes worth its own change (#6082).
#[must_use]
pub fn is_test_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    let base = p.rsplit('/').next().unwrap_or(&p);
    p.split('/')
        .any(|seg| matches!(seg, "tests" | "test" | "__tests__" | "spec" | "benches"))
        || base == "tests.rs"
        || base.ends_with("_test.rs")
        || base.ends_with("_tests.rs")
        || base.ends_with("_test.go")
        || base.ends_with("_test.py")
        || base.starts_with("test_")
        || base.contains(".test.")
        || base.contains(".spec.")
}

/// Build manifests this enumerates, in the order a reader wants them.
const MANIFEST_NAMES: &[&str] = &[
    "Cargo.toml",
    "deny.toml",
    "package.json",
    "pnpm-workspace.yaml",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "Gemfile",
    "composer.json",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

/// Directories that hold someone else's manifests, never this repository's.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    ".venv",
    "venv",
    "third_party",
];

/// Depth below the checkout root this descends.
const MAX_DEPTH: usize = 4;

/// Manifests one repository may contribute, at most.
///
/// Why: a workspace with 21 members has 22 `Cargo.toml` files, and handing all
/// of them to the investigation spends the dependencies dimension's whole share
/// on near-identical member manifests. The root manifests carry the dependency
/// graph; the members carry which crate uses what.
const MAX_MANIFESTS: usize = 12;

/// The build manifests present in `checkout`, root-first.
///
/// Why: the self-audit reported "no manifest-declared dependencies" for a
/// workspace with 134 of them, because the dependencies dimension was answered
/// by semantic search and search never returned `Cargo.toml`. Nothing about
/// finding a build manifest needs a search index or an LLM — the filenames are
/// known — so this enumerates them and they LEAD the dimension. Search still
/// contributes; it just no longer decides whether the manifest is read.
/// What: a bounded walk of `checkout` to [`MAX_DEPTH`], skipping
/// [`SKIPPED_DIRS`], keeping files named in [`MANIFEST_NAMES`], ordered
/// shallowest-first then by path so the root manifests lead, capped at
/// [`MAX_MANIFESTS`]. Repo-relative paths, with a reason naming the enumeration
/// rather than a query, because no query found them.
///
/// # Postconditions
/// Never panics. An unreadable directory contributes nothing and stops nothing.
///
/// Test: `super::quality_tests::{dependency_manifests_lead_with_the_root_manifest,
/// dependency_manifests_skip_vendored_trees, dependency_manifests_are_capped}`.
#[must_use]
pub fn dependency_manifests(checkout: &Path) -> Vec<FileEvidence> {
    let mut found: Vec<(usize, String)> = Vec::new();
    for entry in walkdir::WalkDir::new(checkout)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_skipped(e))
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !MANIFEST_NAMES.iter().any(|m| *m == name) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(checkout) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        found.push((rel.matches('/').count(), rel));
    }
    // Shallowest first so the root manifests lead, then by path for determinism.
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    found.truncate(MAX_MANIFESTS);
    found
        .into_iter()
        .map(|(_, path)| FileEvidence {
            reason: format!("build manifest present in the repository ({path})"),
            path,
        })
        .collect()
}

/// Put the repository's build manifests at the head of the dependencies
/// dimension, creating the dimension when search found nothing for it.
///
/// Why: the dimension is STRUCTURAL, not semantic. The self-audit's three
/// semantic hits were real files that mention dependencies, and none of them was
/// a manifest — so the report concluded there were no manifest-declared
/// dependencies at all. Leading with the manifests makes that conclusion
/// unreachable, and costs one directory walk.
/// What: prepends [`dependency_manifests`] to the dimension's file list, drops
/// any semantic hit naming a path already led with, and truncates to `cap`.
/// Semantic hits keep their order behind the manifests. Does nothing when the
/// repository has no manifest — a tree with no build file is a real state, and
/// inventing an entry for it would be the same lie in the other direction.
///
/// # Postconditions
/// Never panics. Every other dimension is left byte-identical.
///
/// Test: `super::quality_tests::{manifests_lead_the_dependencies_dimension,
/// a_dependencies_dimension_is_created_when_search_found_none,
/// a_tree_with_no_manifest_is_left_alone}`.
pub fn lead_with_manifests(
    dimensions: &mut Vec<super::evidence::DimensionEvidence>,
    checkout: &Path,
    cap: usize,
) {
    let manifests = dependency_manifests(checkout);
    if manifests.is_empty() || cap == 0 {
        return;
    }
    let led: Vec<String> = manifests.iter().map(|m| m.path.clone()).collect();
    match dimensions
        .iter_mut()
        .find(|d| d.dimension == DEPENDENCY_DIMENSION)
    {
        Some(existing) => {
            existing.files.retain(|f| !led.contains(&f.path));
            let mut files = manifests;
            files.append(&mut existing.files);
            files.truncate(cap);
            existing.files = files;
        }
        None => {
            let mut files = manifests;
            files.truncate(cap);
            dimensions.push(super::evidence::DimensionEvidence {
                dimension: DEPENDENCY_DIMENSION.to_owned(),
                files,
            });
        }
    }
}

/// True when this directory is one [`SKIPPED_DIRS`] names.
fn is_skipped(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return false;
    }
    let name = entry.file_name().to_string_lossy();
    SKIPPED_DIRS.iter().any(|d| *d == name)
}

#[cfg(test)]
#[path = "quality_tests.rs"]
mod quality_tests;
