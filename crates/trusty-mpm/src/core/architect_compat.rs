//! Legacy "supervisor" names the Architect still answers to (D07, #9755).
//!
//! Why: the fleet role "supervisor" is renamed "Architect". Its names are
//! persisted or stamped outside this binary — the `.trusty-mpm.toml` profile
//! value, the launch stamp a running `claude` carries across a `tm` upgrade,
//! the stored `SessionKind`, the instruction content paths and the output
//! style id. A reader that misses one of them resolves the Architect to the
//! PM profile and denies its writes, so every legacy name is a failure
//! branch, not a convenience.
//! What: the old and new value of each name, and one predicate per name that
//! accepts both. Writers keep emitting the legacy value for one release (owner
//! answers Q1 and R3): an older `tm` reads only the legacy value, and would
//! resolve the Architect to PM on the new one. Removing this module in the
//! release after next is one grep for `DEPRECATED(D07)`.
//! Test: `architect_compat_tests.rs` — each alias has a test that goes red
//! when the alias is removed.

use std::ffi::OsStr;

/// `profile` and launch-stamp value naming the Architect.
pub const ARCHITECT_PROFILE_ID: &str = "architect";

/// The pre-D07 `profile` and launch-stamp value. Still the value written.
// DEPRECATED(D07): stop writing it the release after #9755 ships; stop
// reading it one release later.
pub const LEGACY_SUPERVISOR_PROFILE_ID: &str = "supervisor";

/// Output-style id of the Architect once the content bundle is renamed.
pub const ARCHITECT_OUTPUT_STYLE_ID: &str = "trusty-mpm-architect";

/// The pre-D07 output-style id. Still the deployed and selected id: the
/// content file `output-styles/trusty-mpm-supervisor.md` is not moved (D2d).
// DEPRECATED(D07): select the new id once the content rename is released.
pub const LEGACY_SUPERVISOR_OUTPUT_STYLE_ID: &str = "trusty-mpm-supervisor";

/// Stored `SessionKind` wire names before D07. Still the names written.
// DEPRECATED(D07): see `canonical_kind_wire`.
pub const LEGACY_SUPERVISOR_KIND: &str = "supervisor";
/// See [`LEGACY_SUPERVISOR_KIND`].
pub const LEGACY_SUPERVISOR_AUX_KIND: &str = "supervisor_aux";

/// The user-config table holding the Architect grant and session name.
pub const ARCHITECT_TABLE: &str = "architect";

/// The pre-D07 name of [`ARCHITECT_TABLE`]. Still the table written when no
/// `[architect]` exists (owner answer Q7).
// DEPRECATED(D07): see `MpmConfig::architect_grant`.
pub const LEGACY_SUPERVISOR_TABLE: &str = "supervisor";

/// The table a writer edits: `[architect]` when the file has one, else the
/// legacy `[supervisor]`.
///
/// Why: a write to a table the reader ignores is lost — with both present
/// `[architect]` wins (Q7), and an older `tm` reads only `[supervisor]`.
/// Test: `the_writer_targets_the_architect_table_once_it_exists`,
/// `an_existing_architect_table_gains_the_entry`.
pub fn grant_table_to_write(has_architect_table: bool) -> &'static str {
    if has_architect_table {
        ARCHITECT_TABLE
    } else {
        LEGACY_SUPERVISOR_TABLE
    }
}

/// Whether a trimmed, lower-cased `profile` value names the Architect.
///
/// Why: `profile_from_config` resolves any unknown value to PM, so a value
/// this predicate misses is a silent demotion.
/// Test: `both_profile_values_name_the_architect`.
pub fn is_architect_profile_id(value: &str) -> bool {
    // DEPRECATED(D07): the legacy arm.
    value == ARCHITECT_PROFILE_ID || value == LEGACY_SUPERVISOR_PROFILE_ID
}

/// Whether a launch stamp names the Architect.
///
/// Why: a running Architect keeps the stamp it was launched with across a
/// `tm` upgrade, and its hooks run the new binary.
/// What: exact match, no trimming: the stamp is written by `tm`, never typed.
/// Test: `both_stamps_name_the_architect`.
pub fn is_architect_stamp(stamp: Option<&OsStr>) -> bool {
    stamp.is_some_and(|s| {
        // DEPRECATED(D07): the legacy arm.
        s == OsStr::new(ARCHITECT_PROFILE_ID) || s == OsStr::new(LEGACY_SUPERVISOR_PROFILE_ID)
    })
}

/// Whether an output-style id is the Architect's style.
///
/// Why: a PM session must never select the Architect style under either id —
/// it carries no delegation floor (#8453).
/// Test: `a_pm_project_cannot_select_the_architect_style_by_its_new_id`.
pub fn is_architect_output_style_id(id: &str) -> bool {
    // DEPRECATED(D07): the legacy arm.
    id == ARCHITECT_OUTPUT_STYLE_ID || id == LEGACY_SUPERVISOR_OUTPUT_STYLE_ID
}

/// The pre-D07 content path of an Architect instruction file, if any.
///
/// Why: the content bundle is released apart from the binary (#9012), and its
/// `instructions/supervisor/` directory and style file stay where they are
/// until a later content release (owner ruling D2d). The binary asks for the
/// new path and falls back to this one.
/// What: `architect/<f>` → `supervisor/<f>`, and the Architect style file to
/// the legacy style file; every other path → `None`.
/// Test: `a_source_with_only_legacy_supervisor_sections_still_loads`.
pub fn legacy_instruction_path(rel: &str) -> Option<String> {
    // DEPRECATED(D07): drop once the content release carries `architect/`.
    if let Some(file) = rel.strip_prefix("architect/") {
        return Some(format!("supervisor/{file}"));
    }
    (rel == "output-styles/trusty-mpm-architect.md")
        .then(|| "output-styles/trusty-mpm-supervisor.md".to_owned())
}

/// The stored `SessionKind` wire name `wire` reads as.
///
/// Why: owner answer Q1 — the store keeps writing `supervisor` /
/// `supervisor_aux`, because an older daemon reads anything else as
/// `Unknown` and an older `tm fleet init` then fails registration. A newer
/// writer's `architect` / `architect_aux` must still read as the Architect.
/// What: the new names map to the legacy names; every other value is
/// returned unchanged.
/// Test: `a_stored_architect_kind_reads_as_the_architect`.
pub fn canonical_kind_wire(wire: &str) -> &str {
    match wire {
        // DEPRECATED(D07): the new-name arms, until writers flip.
        "architect" => LEGACY_SUPERVISOR_KIND,
        "architect_aux" => LEGACY_SUPERVISOR_AUX_KIND,
        other => other,
    }
}

#[cfg(test)]
#[path = "architect_compat_tests.rs"]
mod tests;
