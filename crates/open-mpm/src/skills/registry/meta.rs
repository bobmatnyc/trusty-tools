//! Skill metadata record, persisted-index envelope, and scan budget constants
//! (#363 split from `registry/mod.rs`).
//!
//! Why: `SkillMeta` and the `SkillIndex` envelope are the shared data shapes
//! used by both the in-memory registry and the on-disk persistence path;
//! grouping them with the scan-budget constants keeps the registry's `mod.rs`
//! focused on behavior rather than data definitions.
//! What: Defines `SkillMeta`, the versioned `SkillIndex` wrapper, the
//! `SKILL_INDEX_SCHEMA_VERSION` constant, the per-source scan budgets, and the
//! neutral `default_effectiveness` helper.
//! Test: Exercised indirectly by the registry tests in `registry/tests.rs`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Current schema version for the persisted skill effectiveness index (#197).
///
/// Why: When `SkillMeta` gains new required fields, on-disk indexes written by
/// older versions will fail to deserialize. Bumping this constant and writing
/// it into every saved index lets `merge_index` detect stale files quickly and
/// discard them rather than emitting confusing field-missing errors on every
/// startup.
/// What: A monotonically-increasing `u32` embedded in the `SkillIndex` wrapper
/// that wraps the flat `name -> SkillMeta` map. Increment when a breaking
/// schema change is made.
pub const SKILL_INDEX_SCHEMA_VERSION: u32 = 1;

/// Versioned on-disk envelope for the skill effectiveness index (#197).
///
/// Why: Wrapping the flat `HashMap` in a struct with a `schema_version` field
/// lets future readers detect indexes written by older code and discard them
/// gracefully rather than failing with cryptic serde errors.
/// What: Serialized as a JSON object with `schema_version` (u32) and `skills`
/// (the flat `name -> SkillMeta` map).
/// Test: `save_and_load_index_roundtrip`.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct SkillIndex {
    #[serde(default)]
    pub(super) schema_version: u32,
    pub(super) skills: HashMap<String, SkillMeta>,
}

/// Hard cap on `.md` files indexed from a single source directory (#184).
///
/// Why: A user-level skills dir like `~/.claude/skills/` can contain hundreds
/// of skills (claude-mpm bundles 700+) which made startup hang for 30+ minutes
/// while every nested directory was read and parsed. A bounded scan trades
/// completeness on huge external libraries for a predictable startup time.
/// What: Once `MAX_SKILLS_PER_SOURCE` skills have been discovered inside one
/// source root, `visit_dir` stops descending. Operators who want a higher cap
/// can either split the directory or contribute a configurable knob later.
pub const MAX_SKILLS_PER_SOURCE: usize = 50;

/// Threshold for "this looks like an external skill repo, not an open-mpm
/// source" detection (#184).
///
/// Why: claude-mpm and similar projects ship hundreds of `.md` files in flat
/// or shallow layouts; loading them silently costs minutes on cold-cache disks.
/// We bail out early with a WARN so operators see why their skills didn't
/// appear and can opt in explicitly via `skill-sources.toml`.
pub(super) const LARGE_DIR_MD_THRESHOLD: usize = 200;

/// Per-source-root scan timeout (#184).
///
/// Why: Even with the count cap, a pathological filesystem (network mount,
/// symlink loop) could stall startup. A wall-clock budget enforced inside
/// `visit_dir` lets us abandon a misbehaving source and continue with the
/// rest of the registry rather than hang forever.
pub(super) const PER_SOURCE_SCAN_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimal frontmatter-parsed description of one skill.
///
/// Why: The full file body is only needed when an agent actually loads the
/// skill. Listing + tag ranking only need name/description/tags, and keeping
/// a lightweight struct keeps the registry cheap to clone / pass through
/// `Arc`.
/// What: Holds the canonical skill name, human description (may be empty),
/// tag list, and absolute path to the `.md` file on disk.
/// Test: `registry_finds_skills_by_tag`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    /// Human-readable description of the skill. May be empty for skills that
    /// omit the `description` frontmatter key, and defaults to an empty string
    /// when deserializing older index.json files that predate this field
    /// (#216).
    #[serde(default)]
    pub description: String,
    pub tags: Vec<String>,
    pub source_path: PathBuf,
    /// Effectiveness score in `[0.0, 1.0]` used as a multiplier on tag-overlap
    /// rankings (#171). Defaults to `0.5` so newly discovered skills start in
    /// the neutral middle and earn rank up or down via `update_effectiveness`.
    ///
    /// Why: Pure tag overlap can't distinguish a stale, broken skill from a
    /// fresh, useful one. An exponentially-smoothed effectiveness score lets
    /// the system learn from outcomes (e.g., did the run succeed?) without
    /// needing a heavy ML pipeline.
    /// What: A single f32 in `[0.0, 1.0]`; the tag-overlap score is multiplied
    /// by this before sorting, so a low-effectiveness skill with many matching
    /// tags can rank below a high-effectiveness skill with fewer tags.
    /// Test: `effectiveness_score_influences_ranking`,
    /// `skill_meta_deserializes_with_defaults`.
    #[serde(default = "default_effectiveness")]
    pub effectiveness_score: f32,
    /// Total times this skill was injected into a phase prompt (#171).
    ///
    /// Why: Operators benefit from observability into which skills are
    /// pulling weight; this is the simplest counter that surfaces it.
    /// What: Monotonically incremented by `update_skill_usage` after each
    /// workflow run.
    /// Test: `skill_meta_deserializes_with_defaults`.
    #[serde(default)]
    pub use_count: u32,
    /// ISO-8601 UTC timestamp of the most recent injection, or `None` (#171).
    ///
    /// Why: Lets cleanup tooling identify cold skills without scanning logs.
    /// What: Stored as a string to avoid leaking `chrono` types through the
    /// public `SkillMeta` API.
    /// Test: `skill_meta_deserializes_with_defaults`.
    #[serde(default)]
    pub last_used: Option<String>,
}

/// Neutral default effectiveness — new skills start in the middle and earn
/// rank up or down based on actual usage outcomes.
pub(super) fn default_effectiveness() -> f32 {
    0.5
}
