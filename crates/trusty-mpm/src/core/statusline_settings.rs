//! One writer for the `statusLine` settings entry, across every tier (#7617).
//!
//! Why: the `💸` savings segment has now disappeared three times (#7209, #7245,
//! #7617), and each investigation had to establish from scratch which of three
//! settings tiers carried the entry and whether the command in it still
//! resolved. Two separate implementations of the same seed-or-heal rule existed
//! — `session_launch::settings::write_status_line` for the project
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

use trusty_common::claude_config::write_json_atomic;

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
/// [`StatuslineWrite::Unchanged`] untouched. The file is written back ONLY when
/// something changed, so a steady install costs one read.
///
/// # Write safety
///
/// This writes `~/.claude/settings.json` and a project's `.claude/settings.json`
/// — the file class `write_json_atomic` exists for, because a half-written one
/// bricks Claude Code. Two guarantees, and neither is optional here since
/// `session_launch::ensure_status_line` runs this on every launch AND every
/// resume, so two sessions on one machine race:
///
/// 1. **The publish is atomic, and the prior bytes are backed up** —
///    [`write_json_atomic`] stages under a per-call name and renames, so a
///    reader sees one writer's complete payload or the other's, never a splice
///    (#4077), and leaves `<path>.bak` behind.
/// 2. **The read-modify-write cycle is serialised** — the whole load → mutate →
///    store runs under [`crate::core::claude_json_guard::lock`], the same
///    in-process mutex the `.claude.json` seeders take (#4072). Atomicity alone
///    stops corruption but not a LOST UPDATE: two writers that both read the
///    pre-seed file would each publish their own complete copy, and the second
///    would drop whatever the first added.
///
/// Test: `a_fresh_file_is_seeded`, `a_stale_entry_is_repaired`,
/// `a_customized_entry_is_kept`, `an_unparseable_file_is_refused`,
/// `seeding_is_idempotent`, `other_keys_survive_the_seed`,
/// `a_repair_backs_up_the_prior_bytes`,
/// `a_repair_preserves_operator_fields`.
pub fn ensure_statusline_entry_in(settings_path: &Path) -> StatuslineWrite {
    // #7617: held across the read AND the write below — see "Write safety".
    let _guard = crate::core::claude_json_guard::lock();
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

    // #7617: staged-then-renamed, with `<path>.bak` taken first. Creates the
    // parent directory itself, so no separate `create_dir_all` is needed — and
    // on failure it leaves `settings_path` byte-for-byte as it was.
    match write_json_atomic(settings_path, &settings) {
        Ok(()) => outcome,
        Err(err) => StatuslineWrite::Refused(err.to_string()),
    }
}

/// The backup [`ensure_statusline_entry_in`] leaves behind, when it wrote one.
///
/// Why (#7617, critic MEDIUM 4): `tm doctor --fix` must report the backup an
/// operator can undo from, and the module's own rule is "back up before
/// overwriting". [`write_json_atomic`] takes it and does not return its path,
/// and `trusty_common`'s own `backup_path` is private, so the one place that
/// spelling is re-derived is here rather than at the repair site.
/// What: `<path>.bak`, the name `write_json_atomic` publishes onto. Note it
/// exists only when `path` existed BEFORE the write — seeding a brand-new
/// settings file backs up nothing, because there was nothing to lose.
/// Test: `a_repair_backs_up_the_prior_bytes`,
/// `statusline_repair_backs_up_before_repointing`.
pub fn backup_of(settings_path: &Path) -> std::path::PathBuf {
    let mut name = settings_path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    settings_path.with_file_name(name)
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
/// repoints it when [`is_stale_statusline_command`] claims the existing value
/// ([`StatuslineWrite::Repaired`]), and otherwise leaves an operator
/// customization exactly as it is ([`StatuslineWrite::Unchanged`]). Never
/// returns `Refused` — there is no I/O here to fail.
///
/// A REPAIR IS A REPOINT, NOT A RESET (#7617, critic MEDIUM 3): only `command`
/// is rewritten, in place, so an operator's `padding` — and any key Claude Code
/// adds to this entry that this code has never heard of — survives. Replacing
/// the whole object would silently reset both, and a status bar that loses its
/// padding on every stale-binary heal is a worse trade than the one it fixes.
/// The pre-#7617 project-tier writer patched only `command` for exactly this
/// reason; consolidating the rule must not quietly drop that.
///
/// The wholesale-insert arm is reachable only for a `statusLine` that is not a
/// JSON object. `is_stale_statusline_command` requires `type == "command"` and a
/// string `command`, so no real input takes it — it is the defensive branch
/// #1914's review asked for, kept so a future loosening of that predicate cannot
/// turn into a silent no-op.
/// Test: `a_fresh_file_is_seeded`, `a_stale_entry_is_repaired`,
/// `a_customized_entry_is_kept`, `a_repair_preserves_operator_fields`.
pub fn apply_statusline_entry(
    obj: &mut serde_json::Map<String, serde_json::Value>,
) -> StatuslineWrite {
    match obj.get("statusLine") {
        None => {
            obj.insert("statusLine".to_string(), statusline_entry());
            StatuslineWrite::Seeded
        }
        Some(existing) if is_stale_statusline_command(existing) => {
            match obj
                .get_mut("statusLine")
                .and_then(serde_json::Value::as_object_mut)
            {
                Some(entry) => {
                    entry.insert(
                        "command".to_string(),
                        serde_json::Value::String(resolve_statusline_command()),
                    );
                }
                None => {
                    obj.insert("statusLine".to_string(), statusline_entry());
                }
            }
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
