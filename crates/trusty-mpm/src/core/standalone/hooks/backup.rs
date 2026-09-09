//! Timestamped snapshots of a `settings.json` the hooks writers are about to
//! replace, kept to a bounded set (#7244).
//!
//! Why: #7244 was a bad hook command written into a real project's
//! `.claude/settings.json`, and the only way back was to reconstruct the file
//! by hand. The atomic-write path
//! ([`trusty_common::claude_config::write_json_atomic`]) already keeps a
//! `<path>.bak`, but that is ONE slot overwritten by every writer of that file
//! — the launch that discovers the damage is itself a rewrite, so by the time
//! an operator looks the good copy is gone. A per-rewrite snapshot keeps the
//! last few known states of the file instead of the last one.
//! What: [`snapshot_then_prune`] copies the existing file to
//! `<name>.<YYYYMMDDTHHMMSSZ>.bak` beside it, then deletes all but the newest
//! [`HOOK_SETTINGS_SNAPSHOTS_KEPT`] snapshots of that same file. It is
//! FAIL-CLOSED by contract: it returns the copy's error and the caller must
//! not write. A missing source file yields `Ok(None)` — there is nothing to
//! preserve and the rewrite proceeds.
//! Test: `backup_tests.rs`.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// How many snapshots of one settings file survive a prune.
///
/// Why (#7244): enough to step back past the launch that noticed the damage
/// and the one that caused it, without turning `.claude/` into an archive —
/// every managed launch that changes the file adds one.
/// What: `3`, the count both hooks writers pass to [`snapshot_then_prune`].
/// Test: `snapshot_then_prune_keeps_only_the_newest_three`.
pub(crate) const HOOK_SETTINGS_SNAPSHOTS_KEPT: usize = 3;

/// The extension every snapshot ends with.
const SNAPSHOT_SUFFIX: &str = ".bak";

/// The stamp width of `YYYYMMDDTHHMMSSZ`, in bytes (all ASCII).
const STAMP_LEN: usize = 16;

/// How many snapshots one wall-clock second may hold before the claim gives up.
///
/// Why: the stamp is second-precision, so a burst of rewrites inside one
/// second collides; the `-N` suffix disambiguates. A bound keeps a directory
/// that somehow cannot accept a new file from spinning forever.
/// What: `1000` — far above [`HOOK_SETTINGS_SNAPSHOTS_KEPT`], so the prune
/// always reclaims the names long before this matters.
const MAX_SAME_SECOND_SNAPSHOTS: u32 = 1000;

/// Copy `path` to a timestamped sibling, then prune that file's snapshots to
/// the newest `keep`.
///
/// Why: see the module doc — the hooks writers must be able to hand an
/// operator the file as it was before the rewrite that broke it, and one
/// overwritten `.bak` slot cannot do that. The prune is what keeps the
/// guarantee from costing unbounded disk.
/// What: returns `Ok(None)` when `path` does not exist (nothing to snapshot,
/// the rewrite may proceed). Otherwise claims
/// `<name>.<YYYYMMDDTHHMMSSZ>.bak` — suffixing `-1`, `-2`, … when that second
/// already holds a snapshot — with an exclusive create so two concurrent
/// writers cannot claim one name, copies `path` into it, and returns the
/// snapshot path. Any failure up to and including the copy is returned
/// unchanged and leaves no partial snapshot behind; the CALLER must treat that
/// as fatal to its rewrite. The prune afterwards is best-effort: the snapshot
/// this call owed already exists, so a stale sibling that cannot be removed is
/// a `warn`, not a reason to abandon the write.
/// Test: `snapshot_then_prune_copies_the_existing_file`,
/// `snapshot_then_prune_ignores_a_missing_file`,
/// `snapshot_then_prune_errors_when_the_parent_is_read_only`.
pub(crate) fn snapshot_then_prune(path: &Path, keep: usize) -> io::Result<Option<PathBuf>> {
    snapshot_then_prune_at(path, keep, Utc::now())
}

/// [`snapshot_then_prune`] with the clock supplied by the caller.
///
/// Why: the stamp is the snapshot's identity, so every test of naming,
/// collision suffixing, and prune ordering needs to pin it — reading the real
/// clock would make those assertions race a second boundary.
/// What: as [`snapshot_then_prune`]; `now` supplies the `YYYYMMDDTHHMMSSZ`
/// stamp. Production reads `Utc::now()` one frame up.
/// Test: `snapshot_then_prune_suffixes_a_same_second_collision`,
/// `snapshot_then_prune_keeps_only_the_newest_three`.
fn snapshot_then_prune_at(
    path: &Path,
    keep: usize,
    now: DateTime<Utc>,
) -> io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }

    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let snapshot = claim_snapshot_name(path, &stamp)?;
    if let Err(err) = std::fs::copy(path, &snapshot) {
        // The exclusive create above already published an empty file under
        // this name; leaving it would look like a valid (empty) snapshot.
        let _ = std::fs::remove_file(&snapshot);
        return Err(err);
    }

    prune_snapshots(path, keep);
    Ok(Some(snapshot))
}

/// Create — exclusively — the first free snapshot name for `stamp`.
///
/// Why: `exists()` followed by `copy` is a race two managed launches can lose
/// together, and the loser's snapshot would be silently overwritten by the
/// winner's copy. `create_new` makes claiming the name the same operation as
/// checking it.
/// What: tries `<name>.<stamp>.bak`, then `<name>.<stamp>-1.bak`, … up to
/// [`MAX_SAME_SECOND_SNAPSHOTS`], returning the first name it could create.
/// An `AlreadyExists` moves to the next candidate; any other error is the
/// caller's. A directory sitting on a candidate name also reports
/// `AlreadyExists`, so it is stepped over rather than treated as a failure.
/// Test: `snapshot_then_prune_suffixes_a_same_second_collision`,
/// `snapshot_then_prune_errors_when_the_parent_is_read_only`.
fn claim_snapshot_name(path: &Path, stamp: &str) -> io::Result<PathBuf> {
    for attempt in 0..MAX_SAME_SECOND_SNAPSHOTS {
        let candidate = snapshot_path(path, stamp, attempt);
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
            "{MAX_SAME_SECOND_SNAPSHOTS} snapshots of {} already exist for {stamp}",
            path.display()
        ),
    ))
}

/// Build the snapshot path for `path` at `stamp`, `attempt` collisions in.
///
/// What: `attempt` 0 is the bare `<name>.<stamp>.bak`; every later one appends
/// `-<attempt>` to the stamp, so the `.bak` extension stays last.
/// Test: `snapshot_then_prune_suffixes_a_same_second_collision`.
fn snapshot_path(path: &Path, stamp: &str, attempt: u32) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let suffixed = if attempt == 0 {
        stamp.to_string()
    } else {
        format!("{stamp}-{attempt}")
    };
    path.with_file_name(format!("{name}.{suffixed}{SNAPSHOT_SUFFIX}"))
}

/// Delete all but the newest `keep` snapshots of `path`.
///
/// Why: without it every managed launch that changes the file leaves another
/// copy in the project's `.claude/` forever.
/// What: lists the parent directory, keeps only names
/// [`snapshot_order_key`] accepts for THIS file, orders them by stamp then
/// collision index, and removes everything before the last `keep`. Best-effort
/// throughout — an unreadable directory or an undeletable entry is logged at
/// `warn` and does not fail the caller, because the snapshot it owed already
/// exists. Sorting on the parsed collision index rather than raw bytes keeps
/// the eleventh snapshot of one second from sorting before the second.
/// Test: `snapshot_then_prune_keeps_only_the_newest_three`,
/// `snapshot_then_prune_leaves_foreign_backups_alone`.
fn prune_snapshots(path: &Path, keep: usize) {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let name = name.to_string_lossy().into_owned();

    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                "cannot prune settings snapshots in {}: {err}",
                parent.display()
            );
            return;
        }
    };

    let mut found: Vec<(String, u32, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        if let Some((stamp, index)) = snapshot_order_key(&name, &entry_name) {
            found.push((stamp, index, entry.path()));
        }
    }

    if found.len() <= keep {
        return;
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, _, stale) in found.iter().take(found.len() - keep) {
        if let Err(err) = std::fs::remove_file(stale) {
            tracing::warn!("cannot remove stale snapshot {}: {err}", stale.display());
        }
    }
}

/// Whether `entry_name` is a snapshot of `settings_name` this module wrote.
///
/// Why: the prune's inclusion rule and the assertions that count snapshots
/// have to be the SAME rule, or a test can pass by counting a file the prune
/// would never have touched — `.claude/` also holds the atomic writer's own
/// `settings.json.bak`. Three test modules across two module trees assert on
/// snapshot counts, so the alternative is three copies of the shape drifting
/// apart from the one the prune actually applies. Production reads the ordering
/// key directly and never calls this, hence `#[cfg(test)]`.
/// What: [`snapshot_order_key`] reduced to a yes/no.
/// Test: `snapshot_order_key_rejects_foreign_names`,
/// `write_project_hooks_snapshots_the_file_it_replaces`.
#[cfg(test)]
pub(crate) fn is_snapshot_of(settings_name: &str, entry_name: &str) -> bool {
    snapshot_order_key(settings_name, entry_name).is_some()
}

/// Read `entry_name`'s ordering key when it is a snapshot of `settings_name`.
///
/// Why: the prune deletes files, so its predicate must be exact. `.claude/`
/// also holds `settings.json.bak` (the atomic writer's own single-slot backup)
/// and transient `settings.json.bak.<pid>.<seq>` / `settings.json.tmp.<pid>.<seq>`
/// staging names — none of which this module created and none of which it may
/// remove.
/// What: `Some((stamp, collision index))` only when `entry_name` is exactly
/// `<settings_name>.<stamp>.bak` or `<settings_name>.<stamp>-<n>.bak`, with
/// `stamp` shaped `YYYYMMDDTHHMMSSZ` (8 digits, `T`, 6 digits, `Z`) and `n` a
/// run of digits. Everything else is `None`.
/// Test: `snapshot_then_prune_leaves_foreign_backups_alone`,
/// `snapshot_order_key_rejects_foreign_names`.
fn snapshot_order_key(settings_name: &str, entry_name: &str) -> Option<(String, u32)> {
    let prefix = format!("{settings_name}.");
    let middle = entry_name
        .strip_prefix(&prefix)?
        .strip_suffix(SNAPSHOT_SUFFIX)?;

    let (stamp, index) = match middle.split_once('-') {
        Some((stamp, suffix)) => {
            if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (stamp, suffix.parse::<u32>().ok()?)
        }
        None => (middle, 0),
    };

    if !is_stamp(stamp) {
        return None;
    }
    Some((stamp.to_string(), index))
}

/// Whether `stamp` is a `YYYYMMDDTHHMMSSZ` UTC stamp.
///
/// What: exactly [`STAMP_LEN`] ASCII bytes — 8 digits, `T`, 6 digits, `Z`. The
/// field VALUES are not range-checked: this only has to separate our own
/// generated names from foreign ones, and every name it accepts was written by
/// [`snapshot_path`].
/// Test: `snapshot_order_key_rejects_foreign_names`.
fn is_stamp(stamp: &str) -> bool {
    let bytes = stamp.as_bytes();
    bytes.len() == STAMP_LEN
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
}

#[cfg(test)]
#[path = "backup_tests.rs"]
mod tests;
