//! In-memory `extends:`-chain composition — no filesystem required.
//!
//! Why: [`builder::compose_agent`] is filesystem-coupled: it scans a
//! `source_dir` for `*.md` files before resolving `extends:`. trusty-code
//! wants to bundle a curated subset of trusty-mpm's agents as
//! `include_str!`-embedded assets (issue #2958, epic #2892 Slice E) — those
//! bytes never touch disk, so there is no `source_dir` to scan. Rather than
//! forking the chain-walk (cycle detection, depth limiting, base-first
//! ordering, frontmatter merge) into a second copy, [`builder::resolve`] and
//! [`builder::render_composed`] were generalised over the
//! `pub(crate)` [`builder::SourceLookup`] trait (Slice E1) so this module
//! only has to supply a new, disk-free implementation of that one seam.
//! What: [`InMemorySources`] is a case-folded `name -> markdown content`
//! map, built via [`build_in_memory_source_map`] from `(name, content)`
//! pairs (e.g. `include_str!` consts). [`compose_agent_in_memory`] resolves
//! an `extends:` chain — including chains through BASE-* templates — entirely
//! against that map and returns the same composed-document shape
//! [`builder::compose_agent`] does for the fs path, with identical behavior
//! for identical content (verified by
//! `fs_and_in_memory_compose_are_byte_equivalent`, below).
//! Test: `cargo test -p trusty-agents-common agents::builder_in_memory`
//! covers single-level extends, a multi-level BASE-template chain, a
//! missing-source error, a cycle error, and fs/in-memory byte-equivalence.

use std::collections::HashMap;

use super::builder::{AgentBuildError, SourceLookup, render_composed, resolve};

/// A case-folded, in-memory index of agent name -> raw markdown content.
///
/// Why: mirrors [`builder::SourceMap`]'s case-insensitive resolution
/// (`BASE-QA.md` on disk vs. an `extends: base-qa` reference) but for
/// embedded assets that have no filesystem path — e.g. trusty-code's
/// `include_str!`-embedded tm agent `.md` bytes. Without case-folding, an
/// embedded `BASE-QA.md` asset registered under its uppercase stem would
/// fail to resolve `extends: base-qa`, even though the fs path handles this
/// transparently via [`builder::build_source_map`].
/// What: wraps a `HashMap<String, String>` keyed by lowercased name. Never
/// constructed directly by external callers in the common case — use
/// [`build_in_memory_source_map`] — but [`InMemorySources::insert`] is
/// exposed for tests and any caller that builds the map incrementally.
/// Test: `compose_in_memory_single_level`, `case_insensitive_in_memory_resolve`.
#[derive(Debug, Clone, Default)]
pub struct InMemorySources(HashMap<String, String>);

impl InMemorySources {
    /// Construct an empty in-memory source map.
    ///
    /// Why: lets callers build a map incrementally (e.g. one `insert` per
    /// embedded asset) when [`build_in_memory_source_map`]'s batch
    /// constructor is less convenient than a loop.
    /// What: returns an `InMemorySources` with no registered names.
    /// Test: `compose_in_memory_single_level` (built via `insert`).
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    /// Register one named markdown source, case-folding the key.
    ///
    /// Why: `extends:` values are conventionally lowercase
    /// (`extends: base-qa`) while BASE template assets are conventionally
    /// named with an uppercase stem (`BASE-QA.md`, `BASE-QA` as a bare
    /// name); case-folding on insert (matching [`builder::build_source_map`]'s
    /// case-folding on scan) lets either spelling resolve the other.
    /// What: inserts `(name.to_lowercase(), content)`, overwriting any prior
    /// entry for the same case-folded name.
    /// Test: `case_insensitive_in_memory_resolve`.
    pub fn insert(&mut self, name: impl Into<String>, content: impl Into<String>) {
        self.0.insert(name.into().to_lowercase(), content.into());
    }
}

impl SourceLookup for InMemorySources {
    fn read_source(&self, name: &str) -> Result<String, AgentBuildError> {
        self.0
            .get(&name.to_lowercase())
            .cloned()
            .ok_or_else(|| AgentBuildError::NotFound(name.to_string()))
    }
}

/// Build an [`InMemorySources`] map from `(name, content)` pairs.
///
/// Why: trusty-code will construct this from ~33 `include_str!` constants
/// (5 BASE templates + 28 coding-relevant agents, issue #2958 Slice E2) —
/// a batch constructor over an iterator is the natural shape for that call
/// site, avoiding 33 individual `insert` calls at each construction point.
/// What: folds `entries` into a fresh [`InMemorySources`] via
/// [`InMemorySources::insert`] (so the same case-folding and last-write-wins
/// semantics apply); an empty iterator yields an empty map.
/// Test: `compose_in_memory_multi_level_base_chain`,
/// `fs_and_in_memory_compose_are_byte_equivalent`.
pub fn build_in_memory_source_map<'a, I>(entries: I) -> InMemorySources
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut map = InMemorySources::new();
    for (name, content) in entries {
        map.insert(name, content);
    }
    map
}

/// Resolve `name`'s `extends:` chain against `sources` and return the
/// composed Markdown document — the disk-free counterpart of
/// [`builder::compose_agent`].
///
/// Why: issue #2958 (epic #2892 Slice E) needs `extends:` resolved against
/// trusty-code's embedded asset map instead of a `source_dir` on disk, so a
/// fresh tcode project can ship a real agent roster with zero filesystem
/// dependency. Reuses [`builder::resolve`] (the generic chain walk) and
/// [`builder::render_composed`] (the merge/join tail) rather than
/// duplicating either — the only new code here is [`InMemorySources`]'s
/// [`SourceLookup`] implementation.
/// What: walks `name`'s `extends:` chain base-first via `sources`, merges
/// frontmatter child-wins (union for `skills:`, override for `tools:`,
/// identically to the fs path), and concatenates bodies base-first. Returns
/// the same [`AgentBuildError`] variants the fs path does — `NotFound` for a
/// name absent from `sources`, `Cycle`/`DepthExceeded` for a malformed chain,
/// `FrontmatterParse` for malformed frontmatter in any chain member — since
/// both paths share [`builder::resolve`] and `builder::split_frontmatter`.
/// Test: `compose_in_memory_single_level`,
/// `compose_in_memory_multi_level_base_chain`,
/// `compose_in_memory_missing_source_errors`, `in_memory_cycle_detection`,
/// `fs_and_in_memory_compose_are_byte_equivalent`.
pub fn compose_agent_in_memory(
    name: &str,
    sources: &InMemorySources,
) -> Result<String, AgentBuildError> {
    let mut visiting = Vec::new();
    let (frontmatters, bodies) = resolve(name, sources, &mut visiting)?;
    Ok(render_composed(&frontmatters, &bodies))
}

#[cfg(test)]
#[path = "builder_in_memory_tests.rs"]
mod tests;
