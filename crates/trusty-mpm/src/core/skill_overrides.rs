//! Stack-profile `skillOverrides` for the project settings tier (#7751).
//!
//! Why: every agent holding the `Skill` tool, and the PM, pays the skills
//! listing on every turn — about 7.7K tokens at 109 entries. Some of those
//! families cannot be used by a project of a given type: a Rust workspace has no
//! use for a React design system, and a Svelte app has no use for Rust build
//! tuning. Claude Code's `skillOverrides` settings key removes an `"off"` skill
//! from the listing, and a project-tier entry was verified to shrink it. The
//! owner ruling (2026-09-13) settles the rule: tm reads the project's detected
//! stack and turns off the families that stack cannot use, from a table in code.
//! It is not a usage count and not a hand-maintained per-project list. Design:
//! `docs/specs/agent-context-minimization.md` §C and Slice 2.
//! What: [`SKILL_FAMILY_TABLE`] is the one source — each family names its skills
//! and the stacks it is relevant to. [`irrelevant_skills`] resolves the table
//! against a detected stack set, and [`write_skill_overrides`] merges the result
//! into `<project_dir>/.claude/settings.json` at `prepare_session`. Stacks are
//! the engineer stems of [`detected_stack_engineers`], the same detector the
//! "Detected Project Stack" prompt section and the agent roster read.
//! Test: `svelte_project_turns_off_every_family_the_table_marks_irrelevant`,
//! `prepare_session_writes_stack_profile_skill_overrides_for_a_svelte_project`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::core::manifest::framework::detected_stack_engineers;

/// The Claude Code settings key this module writes.
pub const SKILL_OVERRIDES_KEY: &str = "skillOverrides";

/// The `skillOverrides` value that removes a skill from the listing.
pub const OFF: &str = "off";

/// One row of [`SKILL_FAMILY_TABLE`]: a skill family and where it is relevant.
///
/// Why: a family is relevant to a set of stacks rather than irrelevant to one,
/// so a polyglot project keeps a family when ANY of its stacks can use it.
/// What: `skills` are exact `skillOverrides` keys — no wildcard, because only
/// exact names were verified to shrink the listing. `relevant_stacks` are
/// engineer stems from the bundled framework manifest; an empty list means no
/// code stack uses the family.
/// Test: `every_table_stack_is_a_detectable_stem`.
#[derive(Debug)]
pub struct SkillFamily {
    /// A label for reports and test messages; never written to settings.
    pub name: &'static str,
    /// The exact skill names the family turns off.
    pub skills: &'static [&'static str],
    /// The detected-stack stems that keep the family on.
    pub relevant_stacks: &'static [&'static str],
}

/// Stacks that build or serve a web application.
///
/// Why: the browser-facing families are the widest-relevance rows in the table,
/// and a wrong "off" hides a skill the project needs, so the list is generous —
/// every server-side web stack is on it. Only Rust and Dart are left out: their
/// root markers do not signal a web front end.
const WEB_STACKS: &[&str] = &[
    "dotnet-engineer",
    "elixir-engineer",
    "golang-engineer",
    "java-engineer",
    "javascript-engineer",
    "nextjs-engineer",
    "phoenix-engineer",
    "php-engineer",
    "python-engineer",
    "react-engineer",
    "ruby-engineer",
    "svelte-engineer",
    "tauri-engineer",
    "typescript-engineer",
];

/// The stack-to-skill-family table — the only source of tm's `skillOverrides`.
///
/// Why: owner ruling 2026-09-13 (#7751) — the "never-used" families are read
/// off the project's stack profile, from a table in code.
/// What: a family is turned off for a project when none of the project's
/// detected stacks is in its `relevant_stacks`. Plugin families (`aws-agents:*`,
/// `aws-core:*`) are absent on purpose: the managed config's `enabledPlugins`
/// already turns those plugins off (#7422), which removes their skills too.
/// Test: `rust_project_turns_off_every_family_the_table_marks_irrelevant`,
/// `svelte_project_turns_off_every_family_the_table_marks_irrelevant`,
/// `every_table_stack_is_a_detectable_stem`.
pub const SKILL_FAMILY_TABLE: &[SkillFamily] = &[
    SkillFamily {
        name: "rust-build",
        skills: &["rust-build-performance"],
        // `tauri-engineer` preloads this skill, so a Tauri project keeps it.
        relevant_stacks: &["rust-engineer", "tauri-engineer"],
    },
    SkillFamily {
        // #7751 review round 1: `claude-in-chrome` was listed here and is not a
        // skill file in any skill root — it is the Chrome MCP server
        // (`mcp__claude-in-chrome__*`). A `skillOverrides` key that names no
        // skill turns nothing off; keeping a browser MCP server away from a
        // non-web project is a `tools:`/MCP gate, not this table.
        name: "web-app",
        skills: &["web-performance-optimization", "webapp-testing"],
        relevant_stacks: WEB_STACKS,
    },
    SkillFamily {
        name: "react-design-system",
        skills: &["duetto-design-system"],
        relevant_stacks: &["nextjs-engineer", "react-engineer"],
    },
    SkillFamily {
        name: "spreadsheets",
        // The skill's recipes are openpyxl, pandas, and the `xlsx` npm package.
        skills: &["xlsx"],
        relevant_stacks: &[
            "javascript-engineer",
            "python-engineer",
            "typescript-engineer",
        ],
    },
    SkillFamily {
        name: "voice-media",
        skills: &["breeze-voice"],
        relevant_stacks: &[],
    },
    SkillFamily {
        name: "knowledge-base-ingest",
        skills: &["cto-kb-ingest"],
        relevant_stacks: &[],
    },
];

/// Table skills the framework manifest does not bundle (#7751 review round 1).
///
/// Why: `skillOverrides` addresses every skill root Claude Code lists, not only
/// tm's bundle, so a name missing from the bundled roster is not by itself a
/// typo. It does have to be a name somebody checked, because a misspelt entry
/// turns nothing off and still reports success.
/// What: each name here was verified on 2026-09-13 to exist as a skill
/// directory under `~/.claude/skills` AND under
/// `~/.trusty-tools/trusty-mpm/claude-config/skills` on the owner's host.
/// Every other table skill must appear in the bundled roster.
/// Test: `every_table_skill_is_bundled_or_vouched_for`.
pub const UNBUNDLED_TABLE_SKILLS: &[&str] =
    &["breeze-voice", "cto-kb-ingest", "duetto-design-system"];

/// The skills [`SKILL_FAMILY_TABLE`] turns off for a project with `stacks`.
///
/// Why: the pure half of the rule, so both directions of the table are testable
/// without a filesystem.
/// What: the skills of every family whose `relevant_stacks` shares no stem with
/// `stacks`. An empty `stacks` turns nothing off.
/// Test: `rust_project_turns_off_every_family_the_table_marks_irrelevant`,
/// `unknown_stack_turns_nothing_off`.
pub fn irrelevant_skills(stacks: &BTreeSet<String>) -> BTreeSet<&'static str> {
    // #7751: an unknown stack shares no stem with any family, so without this
    // guard it would turn EVERY family off. The detector also answers empty when
    // it fails, and a failure must never hide a skill the project needs.
    if stacks.is_empty() {
        return BTreeSet::new();
    }
    SKILL_FAMILY_TABLE
        .iter()
        .filter(|family| {
            !family
                .relevant_stacks
                .iter()
                .any(|stem| stacks.contains(*stem))
        })
        .flat_map(|family| family.skills.iter().copied())
        .collect()
}

/// What one [`write_skill_overrides`] call did.
#[derive(Debug, PartialEq, Eq)]
pub enum SkillOverridesOutcome {
    /// No stack was detected, so nothing was turned off and no file was touched.
    NoStack,
    /// Every planned entry was already present; the file was not rewritten.
    Unchanged,
    /// The named skills were added as `"off"`.
    Written(Vec<String>),
    /// The settings file, or its `skillOverrides` value, was not a JSON object;
    /// it was left byte-for-byte as found.
    Skipped(String),
}

/// A failure reading or writing the project settings file.
#[derive(Debug, thiserror::Error)]
pub enum SkillOverridesError {
    /// The settings file exists but could not be read.
    #[error("read {path}: {source}")]
    Read {
        /// The settings file.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
    /// The merged settings could not be written.
    #[error("write {path}: {message}")]
    Write {
        /// The settings file.
        path: PathBuf,
        /// The atomic writer's error chain.
        message: String,
    },
}

/// Merge this project's stack-profile `skillOverrides` into its settings (#7751).
///
/// Why: `.claude/settings.json` is the project tier the listing was verified
/// against, and it is often a tracked, user-edited file, so the write merges.
/// What: detects the stack with [`detected_stack_engineers`] and hands it to
/// [`write_skill_overrides_for`].
/// Test: `prepare_session_writes_stack_profile_skill_overrides_for_a_rust_project`,
/// `prepare_session_on_an_unknown_stack_writes_no_skill_overrides`.
pub fn write_skill_overrides(
    project_dir: &Path,
) -> Result<SkillOverridesOutcome, SkillOverridesError> {
    write_skill_overrides_for(project_dir, &detected_stack_engineers(project_dir))
}

/// [`write_skill_overrides`] against an explicit stack set.
///
/// Why: the detector reads marker files, so the merge rules are tested here
/// against a pinned stack set.
/// What, in order:
/// - the whole read → mutate → write cycle runs under
///   [`crate::core::claude_json_guard::lock`], the in-process mutex every other
///   read-modify-write of this file already holds (#4072, #7617);
/// - an empty `stacks` returns [`SkillOverridesOutcome::NoStack`] before reading
///   anything. The detector also answers empty when it fails, so a failure hides
///   no skill;
/// - a missing file starts from `{}`; a file or `skillOverrides` value that is
///   not a JSON object is left alone with a warning;
/// - each planned skill is inserted as `"off"` only when the key is absent, so
///   every existing key and every user entry wins;
/// - nothing is written when nothing was added, so a repeat run leaves the file
///   byte-identical; otherwise the file is written atomically.
///
/// The "left alone" rule above is this function's own behaviour, reachable from
/// a direct call. It is NOT what a launch does: `prepare_session` runs
/// `session_launch::settings::merge_settings` first, which replaces a settings
/// file it cannot parse with a fresh object (#7780), so by the time this runs
/// the file always parses.
///
/// Test: `unknown_stack_turns_nothing_off`,
/// `user_entries_and_foreign_keys_survive_the_merge`,
/// `malformed_settings_are_left_untouched`,
/// `second_write_is_byte_identical`,
/// `a_concurrent_statusline_write_loses_no_skill_overrides`.
pub fn write_skill_overrides_for(
    project_dir: &Path,
    stacks: &BTreeSet<String>,
) -> Result<SkillOverridesOutcome, SkillOverridesError> {
    let planned = irrelevant_skills(stacks);
    if planned.is_empty() {
        return Ok(SkillOverridesOutcome::NoStack);
    }

    // #7751 review round 1 (HIGH): held across the read AND the write below.
    // `write_json_atomic` stops a torn file, not a LOST UPDATE — a sibling
    // writer of this same `.claude/settings.json` that read before this store
    // republishes its own pre-read snapshot and drops these keys. Same line,
    // same mutex, same reason as `statusline_settings::ensure_statusline_entry_in`
    // and `claude_md_excludes::add_exclude` (#4072, #7617). No caller holds it
    // already: `prepare_session_inner` calls this outside every guarded seeder,
    // and the mutex is not reentrant.
    let _guard = crate::core::claude_json_guard::lock();

    let path = project_dir.join(".claude").join("settings.json");
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) if value.is_object() => value,
            _ => return Ok(skip(&path, "the file is not a JSON object")),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            serde_json::Value::Object(serde_json::Map::new())
        }
        Err(source) => return Err(SkillOverridesError::Read { path, source }),
    };

    let Some(overrides) = settings
        .as_object_mut()
        .map(|obj| {
            obj.entry(SKILL_OVERRIDES_KEY)
                .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        })
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(skip(
            &path,
            "its `skillOverrides` value is not a JSON object",
        ));
    };

    let mut added = Vec::new();
    for skill in planned {
        if !overrides.contains_key(skill) {
            overrides.insert(
                skill.to_string(),
                serde_json::Value::String(OFF.to_string()),
            );
            added.push(skill.to_string());
        }
    }
    if added.is_empty() {
        return Ok(SkillOverridesOutcome::Unchanged);
    }

    trusty_common::claude_config::write_json_atomic(&path, &settings).map_err(|err| {
        SkillOverridesError::Write {
            path: path.clone(),
            message: format!("{err:#}"),
        }
    })?;
    Ok(SkillOverridesOutcome::Written(added))
}

/// Warn that `path` was left alone, and say so in the outcome.
fn skip(path: &Path, reason: &str) -> SkillOverridesOutcome {
    let message = format!(
        "left {} untouched: {reason}; no stack-profile skillOverrides written",
        path.display()
    );
    tracing::warn!("{message}");
    SkillOverridesOutcome::Skipped(message)
}

#[cfg(test)]
#[path = "skill_overrides_tests.rs"]
mod tests;
