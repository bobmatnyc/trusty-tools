//! One writer for the `statusLine` settings entry, across every tier (#7617).
//!
//! Why: the `💸` savings segment has now disappeared three times (#7209, #7245,
//! #7617), and each investigation had to establish from scratch which of three
//! settings tiers carried the entry and whether the command in it still
//! resolved. Two separate implementations of the same seed-or-heal rule existed
//! —
//! [`crate::core::session_launch::settings::write_status_line`] for the project
//! tier and `standalone::settings_defaults::ensure_settings_defaults` for the
//! tm-owned `CLAUDE_CONFIG_DIR` — and NEITHER covered the user tier
//! (`~/.claude/settings.json`), so a session launched outside the managed driver
//! had no writer at all. The owner's ruling is that the segment is core setup,
//! guaranteed by the framework: one rule, applied to every tier a launch
//! touches. See `CLAUDE.md`, "Common entry point, clean domain demarcation".
//!
//! What: [`ensure_statusline_entry_in`] applies the rule to ONE settings file —
//! seed when the key is absent, repair when the command is stale (an ephemeral
//! build path, or a binary no longer on disk), and never touch a genuine
//! operator customization. Both existing writers delegate here, and
//! [`ensure_statusline_entry_in`] is also what `tm doctor --fix` runs.
//!
//! FAIL-CLOSED on an unreadable or unparseable file: a settings file this cannot
//! read as a JSON object is left exactly as it is and reported as
//! [`StatuslineWrite::Refused`], never overwritten with a fresh object. The one
//! exception is an ABSENT file, which is created — there is nothing there to
//! lose.
//! Test: the `tests` module below.

use std::path::Path;

use crate::core::session_launch::{is_stale_statusline_command, resolve_statusline_command};

/// What one file's worth of [`ensure_statusline_entry_in`] did.
///
/// Why: the doctor `--fix` arm has to say which files it changed and which it
/// declined to, and "declined because the operator customized it" reads very
/// differently from "declined because the file is corrupt". A bool cannot carry
/// that.
/// Test: `a_fresh_file_is_seeded`, `a_customized_entry_is_kept`,
/// `an_unparseable_file_is_refused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatuslineWrite {
    /// The entry was absent and has been written.
    Seeded,
    /// The entry pointed at a stale binary and has been repointed.
    Repaired,
    /// The entry is already correct, or is an operator customization.
    Unchanged,
    /// The file could not be read as a JSON object; nothing was written.
    Refused(String),
}

impl StatuslineWrite {
    /// Whether this outcome actually changed the file.
    ///
    /// What: true for [`StatuslineWrite::Seeded`] and
    /// [`StatuslineWrite::Repaired`].
    /// Test: `a_fresh_file_is_seeded`, `a_stale_entry_is_repaired`.
    pub fn wrote(&self) -> bool {
        matches!(self, Self::Seeded | Self::Repaired)
    }
}

/// Seed or repair `statusLine` in one settings file.
///
/// Why: see the module header — this is the single implementation of the rule
/// every tier is held to.
/// What: reads `settings_path` (an absent file starts from `{}`; an unreadable
/// or non-object one is [`StatuslineWrite::Refused`] and left alone), then
/// inserts `statusLine` when the key is absent, or rewrites only its `command`
/// when [`is_stale_statusline_command`] claims the existing value. Any other
/// existing value is an operator customization and is returned
/// [`StatuslineWrite::Unchanged`] untouched. The file is written back — creating
/// its parent directory if needed — ONLY when something changed, so a steady
/// install costs one read.
/// Test: `a_fresh_file_is_seeded`, `a_stale_entry_is_repaired`,
/// `a_customized_entry_is_kept`, `an_unparseable_file_is_refused`,
/// `seeding_is_idempotent`, `other_keys_survive_the_seed`.
pub fn ensure_statusline_entry_in(settings_path: &Path) -> StatuslineWrite {
    let raw = match std::fs::read_to_string(settings_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        // #7617: an unreadable file is not an empty one. Replacing it with a
        // fresh object would destroy whatever the operator has there.
        Err(err) => return StatuslineWrite::Refused(err.to_string()),
    };

    let mut settings = match raw.as_deref() {
        None => serde_json::json!({}),
        Some(text) if text.trim().is_empty() => serde_json::json!({}),
        Some(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(value) if value.is_object() => value,
            Ok(_) => return StatuslineWrite::Refused("settings root is not an object".to_string()),
            Err(err) => return StatuslineWrite::Refused(err.to_string()),
        },
    };
    let Some(obj) = settings.as_object_mut() else {
        return StatuslineWrite::Refused("settings root is not an object".to_string());
    };

    let outcome = apply_statusline_entry(obj);
    if !outcome.wrote() {
        return outcome;
    }

    if let Some(parent) = settings_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return StatuslineWrite::Refused(err.to_string());
    }
    let serialized = match serde_json::to_string_pretty(&settings) {
        Ok(text) => text,
        Err(err) => return StatuslineWrite::Refused(err.to_string()),
    };
    if let Err(err) = std::fs::write(settings_path, serialized) {
        return StatuslineWrite::Refused(err.to_string());
    }
    outcome
}

/// The seed-or-repair DECISION, applied to an already-parsed settings object.
///
/// Why (#7617): three writers need this rule and only one of them owns the file
/// — `standalone::settings_defaults::ensure_settings_defaults` merges
/// `statusLine` with two other keys into a single atomic write, so it cannot
/// call a function that reads and writes the file itself. Sharing the decision
/// rather than the I/O is what keeps one rule across all three without forcing
/// that writer into two writes.
/// What: inserts the entry when `statusLine` is absent ([`StatuslineWrite::Seeded`]),
/// replaces it when [`is_stale_statusline_command`] claims the existing value
/// ([`StatuslineWrite::Repaired`]), and otherwise leaves an operator
/// customization exactly as it is ([`StatuslineWrite::Unchanged`]). Never
/// returns `Refused` — there is no I/O here to fail.
/// Test: `a_fresh_file_is_seeded`, `a_stale_entry_is_repaired`,
/// `a_customized_entry_is_kept`.
pub fn apply_statusline_entry(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> StatuslineWrite {
    match obj.get("statusLine") {
        None => {
            obj.insert("statusLine".to_string(), statusline_entry());
            StatuslineWrite::Seeded
        }
        Some(existing) if is_stale_statusline_command(existing) => {
            obj.insert("statusLine".to_string(), statusline_entry());
            StatuslineWrite::Repaired
        }
        // A genuine operator customization pointing at a live binary.
        Some(_) => StatuslineWrite::Unchanged,
    }
}

/// The entry every tier gets.
///
/// What: `{"type":"command","command":"<abs tm> statusline","padding":0}`, with
/// the command resolved absolutely by
/// [`resolve_statusline_command`] — a bare `tm statusline` silently renders
/// nothing under Claude Code's minimal `PATH` (#1914).
fn statusline_entry() -> serde_json::Value {
    serde_json::json!({
        "type": "command",
        "command": resolve_statusline_command(),
        "padding": 0
    })
}

#[cfg(test)]
#[path = "statusline_settings_tests.rs"]
mod tests;
