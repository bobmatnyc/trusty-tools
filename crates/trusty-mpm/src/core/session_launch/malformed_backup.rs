//! Preserve a malformed `settings.json` before a writer replaces it.
//!
//! Why (#7780): three `prepare_session` writers read
//! `<project>/.claude/settings.json`, coerced anything that did not parse as a
//! JSON object into an empty `{}`, and wrote that back. A file holding
//! `{ broken` therefore came back as tm's own keys alone — every hand-written
//! `permissions`, `env` and `statusLine` entry gone, with no warning and no copy
//! of what had been there. Simply SKIPPING a malformed file was rejected: the
//! project would then launch with no PM-guard hook (#1977). The owner ruled
//! backup-then-rewrite (2026-09-13), so the bytes are preserved and the rewrite
//! still happens.
//! What: one loader those writers call in place of their inline
//! read-parse-or-`{}`. A JSON object is returned as read. Anything else — an
//! unparseable file, or a valid JSON array/string/number — has its original
//! bytes copied to a timestamped `settings.json.malformed-<stamp>` sibling
//! first, is reported at `warn` naming that copy, and the caller then rewrites
//! from `{}`. A whitespace-only file is the one exception — it holds nothing to
//! preserve, so it is treated as absent (#7789). A file that cannot be read at
//! all, or a copy that cannot be written, is an error: a writer never replaces
//! bytes it could not preserve.
//!
//! #7789: the managed-tier writers
//! [`crate::core::standalone::settings_defaults::ensure_settings_defaults`] and
//! `standalone::hooks::write_project_hooks_with` carried the same
//! coerce-to-`{}` read over `<claude_config_dir>/settings.json`, so they call
//! this loader too rather than growing a second copy of the rule.
//! Test: `malformed_backup_tests.rs`, `tests_malformed_settings_7780.rs`,
//! `core::standalone::tests_malformed_settings_7789`.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::PrepError;

/// The infix every preserved copy carries between the file name and the stamp.
///
/// Why: the name has to read as "this is what your settings file used to be",
/// and must not collide with the `.bak` snapshots
/// [`crate::core::standalone::hooks::backup`] prunes — a malformed original is
/// the one copy that must survive indefinitely.
/// What: `malformed`, rendered as `settings.json.malformed-<stamp>`.
/// Test: `backs_up_unparseable_bytes_under_a_stamped_name`.
const MALFORMED_INFIX: &str = "malformed";

/// How many copies one wall-clock second may hold before the claim gives up.
///
/// Why: the stamp is second-precision, so two rewrites inside one second would
/// collide; the `-N` suffix disambiguates rather than overwriting the first
/// copy. A bound keeps a directory that somehow cannot accept a new file from
/// spinning forever. Mirrors `backup::MAX_SAME_SECOND_SNAPSHOTS`.
/// What: `1000`.
/// Test: `never_overwrites_a_copy_taken_in_the_same_second`.
const MAX_SAME_SECOND_BACKUPS: u32 = 1000;

/// Load the settings object a writer is about to replace, preserving it first
/// when it is not a JSON object.
///
/// Why: see the module doc — this is the whole of #7780's behaviour, in one
/// place, because five writers across two config tiers share the rule and five
/// copies of it is how four of them would keep the old silent-overwrite.
/// What: `{}` for an absent or whitespace-only file (the caller creates it); the
/// parsed value for a JSON object; otherwise the original bytes are copied aside
/// and `{}` is returned so the caller rewrites from scratch. Returns
/// [`PrepError::SettingsBackup`] — and writes nothing — when the file exists but
/// cannot be read, or when the copy cannot be written. `pub(crate)` for the
/// managed-tier callers named in the module doc (#7789); they map the error at
/// their own call site rather than forking the loader.
/// Test: `returns_a_json_object_untouched`, `treats_a_missing_file_as_empty`,
/// `treats_a_whitespace_only_file_as_empty`,
/// `backs_up_unparseable_bytes_under_a_stamped_name`,
/// `backs_up_a_valid_non_object`, `refuses_when_the_copy_cannot_be_written`.
pub(crate) fn load_settings_object(settings_path: &Path) -> Result<Value, PrepError> {
    load_settings_object_at(settings_path, Utc::now())
}

/// Preserve the file a writer is about to replace, without taking its contents.
///
/// Why: a writer that already holds the object it means to write still owes the
/// copy — [`crate::core::session_launch::settings::write_enabled_plugins_with_trust`]
/// planned its merge from a non-object file that was silently discarded. Calling
/// [`load_settings_object`] for its side effect alone reads as dead code to the
/// next editor; DELETING THIS CALL RESTORES #7780's SILENT OVERWRITE for that
/// writer, so the name says what the call is for.
/// What: [`load_settings_object`] with the loaded value dropped — the copy, the
/// warning and the fail-closed refusal are the whole of the contract.
/// Test: `write_enabled_plugins_backs_up_a_malformed_file_before_rewriting_it`,
/// `preserve_if_malformed_copies_aside_and_refuses_when_it_cannot`.
pub(super) fn preserve_if_malformed(settings_path: &Path) -> Result<(), PrepError> {
    load_settings_object(settings_path).map(|_| ())
}

/// [`load_settings_object`] with the clock supplied by the caller.
///
/// Why: the stamp is the copy's identity, so the naming and same-second
/// collision tests need to pin it — reading the real clock would make them race
/// a second boundary. Mirrors `backup::snapshot_then_prune_at`.
/// What: as [`load_settings_object`]; `now` supplies the `YYYYMMDDTHHMMSSZ`
/// stamp. Production reads `Utc::now()` one frame up.
/// Test: `never_overwrites_a_copy_taken_in_the_same_second`.
fn load_settings_object_at(settings_path: &Path, now: DateTime<Utc>) -> Result<Value, PrepError> {
    let bytes = match std::fs::read(settings_path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(empty_object()),
        // #7780: an unreadable file is refused for the reason
        // `resume_hooks::settings_is_writable_object` gives (#7490) — contents
        // that were never seen cannot be preserved by anything downstream.
        Err(source) => {
            return Err(PrepError::SettingsBackup {
                path: settings_path.to_path_buf(),
                source,
            });
        }
    };

    if let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        && value.is_object()
    {
        return Ok(value);
    }

    // #7789: an empty or whitespace-only file holds nothing to preserve, and
    // the managed hook writer this loader now also serves has always read one
    // as `{}`. A zero-byte copy would be a record of nothing, named as though
    // it were the operator's lost settings.
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(empty_object());
    }

    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let backup =
        copy_aside(settings_path, &bytes, &stamp).map_err(|source| PrepError::SettingsBackup {
            path: settings_path.to_path_buf(),
            source,
        })?;
    tracing::warn!(
        path = %settings_path.display(),
        backup = %backup.display(),
        "settings.json is not a JSON object; copied the original aside and rewrote it (#7780, #7789)"
    );
    Ok(empty_object())
}

/// Copy `bytes` to the first free `<name>.malformed-<stamp>` sibling of `settings_path`.
///
/// What: claims the name with an exclusive create (so two concurrent launches
/// cannot both take it), then fills it. A failed fill removes the empty file it
/// just published — an empty copy would read as a valid, and wrong, record of
/// what the file held.
/// Test: `backs_up_unparseable_bytes_under_a_stamped_name`,
/// `never_overwrites_a_copy_taken_in_the_same_second`.
fn copy_aside(settings_path: &Path, bytes: &[u8], stamp: &str) -> io::Result<PathBuf> {
    let dest = claim_backup_name(settings_path, stamp)?;
    if let Err(err) = std::fs::write(&dest, bytes) {
        let _ = std::fs::remove_file(&dest);
        return Err(err);
    }
    Ok(dest)
}

/// Create — exclusively — the first free copy name for `stamp`.
///
/// Why: `exists()` then `write` is a race two launches can lose together, and
/// the loser's copy would be silently overwritten. `create_new` makes claiming
/// the name the same operation as checking it.
/// What: tries `<name>.malformed-<stamp>`, then `<name>.malformed-<stamp>-1`, …
/// up to [`MAX_SAME_SECOND_BACKUPS`]. An `AlreadyExists` moves to the next
/// candidate; any other error is the caller's. A directory sitting on a
/// candidate name also reports `AlreadyExists`, so it is stepped over rather
/// than treated as a failure.
/// Test: `never_overwrites_a_copy_taken_in_the_same_second`,
/// `refuses_when_the_copy_cannot_be_written`.
fn claim_backup_name(settings_path: &Path, stamp: &str) -> io::Result<PathBuf> {
    for attempt in 0..MAX_SAME_SECOND_BACKUPS {
        let candidate = backup_path(settings_path, stamp, attempt);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{MAX_SAME_SECOND_BACKUPS} preserved copies of {} already exist for {stamp}",
            settings_path.display()
        ),
    ))
}

/// Build the copy path for `settings_path` at `stamp`, `attempt` collisions in.
///
/// What: `attempt` 0 is the bare `<name>.malformed-<stamp>`; every later one
/// appends `-<attempt>` to the stamp.
/// Test: `never_overwrites_a_copy_taken_in_the_same_second`.
fn backup_path(settings_path: &Path, stamp: &str, attempt: u32) -> PathBuf {
    let name = settings_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let suffixed = if attempt == 0 {
        stamp.to_string()
    } else {
        format!("{stamp}-{attempt}")
    };
    settings_path.with_file_name(format!("{name}.{MALFORMED_INFIX}-{suffixed}"))
}

/// A fresh empty JSON object — what a writer starts from when nothing usable
/// was on disk.
fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[cfg(test)]
#[path = "malformed_backup_tests.rs"]
mod tests;
