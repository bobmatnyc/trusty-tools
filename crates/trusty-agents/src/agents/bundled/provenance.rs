//! What this module last WROTE at each bundled path (#3844).
//!
//! Why: the refresh path could not tell a stale deploy from an operator's hand
//! edit. Both are "the bytes on disk differ from the bytes about to be written",
//! so every `cargo install` archived the operator's edit to
//! `<file>.stale.<digest>.bak` and overwrote it. That happened four times in one
//! week and each recurrence silently reverted real configuration — a model
//! routing decision, a `[tools]` block, the `scopes` line that makes gworkspace
//! tools reachable, whole persona sections. A `.bak` file is a recovery path, not
//! a protection; nothing told the operator to go looking for one, and an agent
//! whose memory grants vanish does not error, it just behaves as though it has
//! no memory.
//!
//! Recording the digest of what this module itself wrote is what separates the
//! two cases. On-disk content that still matches the recorded digest is ours to
//! refresh; content that does not is the operator's, and [`super`]'s refresh loop
//! keeps it.
//!
//! What: a `<digest>\t<relative path>` line per bundled file in
//! `<target_dir>/.bundled-provenance`, read as a map and written back whole.
//! [`read`] fails CLOSED — a missing file, an unreadable one, or a line it cannot
//! parse yields no entry for that path, and a path with no entry is treated as
//! the operator's. So the worst case of a corrupt or absent manifest is a
//! bundled template update that waits for `tagent agents repair`, never a lost
//! hand edit.
//!
//! Test: `provenance_round_trips_every_entry`, `missing_manifest_reads_empty`,
//! `unparsable_lines_are_dropped_not_guessed`,
//! `a_path_that_cannot_be_recorded_is_left_unvouched`
//! (this file's own `#[cfg(test)] mod tests`), plus
//! `a_hand_edited_bundled_file_survives_a_stale_stamp_refresh` and the rest of
//! the #3844 set in `tests.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Fixed filename for the manifest, a sibling of `.bundled-stamp` inside the
/// deployed directory. Dot-prefixed so the agent listers' hidden-entry skip
/// (`api::server::projects`'s catalog scan) never surfaces it as an agent.
const PROVENANCE_FILE_NAME: &str = ".bundled-provenance";

/// Recorded digests keyed by the bundle's own relative path (`assistant/agent.toml`).
pub(super) type Recorded = BTreeMap<String, String>;

/// The digest form recorded and compared — full lowercase hex SHA-256.
///
/// Why: the full digest rather than the 16-character prefix
/// [`super::stale_backup_path`] uses. That prefix names a file a human reads;
/// this one decides whether an operator's edit gets overwritten, so it keeps
/// every bit.
/// What: lowercase hex SHA-256 of `bytes`.
/// Test: `provenance_round_trips_every_entry`.
pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Read the manifest at `target_dir/.bundled-provenance`.
///
/// Why: every failure here has to mean "this path is unvouched" rather than
/// "this path is ours" — see the module docs on failing closed. A machine
/// provisioned before #3844 has no manifest at all and reaches this the same
/// way, so its hand-edited files are protected from the first pass onward while
/// its pristine ones get vouched by that pass and refresh normally afterwards.
/// What: reads the file as UTF-8, splits each line on the first tab into
/// `(digest, path)`, and drops any line with no tab or an empty half. Any I/O
/// error, "file does not exist" included, yields an empty map.
/// Test: `provenance_round_trips_every_entry`, `missing_manifest_reads_empty`,
/// `unparsable_lines_are_dropped_not_guessed`.
pub(super) fn read(target_dir: &Path) -> Recorded {
    let Ok(body) = std::fs::read_to_string(target_dir.join(PROVENANCE_FILE_NAME)) else {
        return Recorded::new();
    };
    body.lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(digest, rel)| !digest.is_empty() && !rel.is_empty())
        .map(|(digest, rel)| (rel.to_string(), digest.to_string()))
        .collect()
}

/// Persist `recorded` at `target_dir/.bundled-provenance`.
///
/// Why: routed through `crate::state_writer::atomic_write` for the same reason
/// [`super::stamp::write`] is — `delegate_to_agent` spawns a `tagent` per
/// delegation, so concurrent passes are routine, and a torn manifest read back
/// as garbage would decide whether an operator's edit gets overwritten. The
/// caller writes it only after a successful refresh loop and only from inside
/// the pass-level lock, so a pass that fails partway leaves the PRIOR manifest,
/// which vouches for less rather than more.
/// What: one `<digest>\t<path>` line per entry, sorted by path (the map's own
/// order), newline-terminated. An entry whose path holds a tab or a newline is
/// SKIPPED — the format could not round-trip it, and a path this cannot record
/// is one the refresh loop then treats as the operator's.
/// Test: `provenance_round_trips_every_entry`,
/// `a_path_that_cannot_be_recorded_is_left_unvouched`.
pub(super) fn write(target_dir: &Path, recorded: &Recorded) -> Result<()> {
    let mut body = String::new();
    for (rel, digest) in recorded {
        if rel.contains('\t') || rel.contains('\n') {
            tracing::warn!(
                asset = %rel,
                "not recording a bundled path that the provenance format cannot \
                 round-trip; it will be treated as locally modified"
            );
            continue;
        }
        body.push_str(digest);
        body.push('\t');
        body.push_str(rel);
        body.push('\n');
    }
    let path = target_dir.join(PROVENANCE_FILE_NAME);
    crate::state_writer::atomic_write(&path, body.as_bytes()).with_context(|| {
        format!(
            "failed to write the bundled-agent provenance manifest to {}",
            path.display()
        )
    })
}

/// Does `recorded` vouch that `current` is what this module last wrote at `rel`?
///
/// Why: the single question the refresh loop asks before overwriting anything.
/// Named rather than inlined so the fail-closed direction is stated once: no
/// entry, or an entry that does not match, is the operator's content.
/// What: `true` only when `rel` has a recorded digest equal to `current`'s.
/// Test: `a_hand_edited_bundled_file_survives_a_stale_stamp_refresh`,
/// `a_stale_deploy_this_tool_wrote_is_still_refreshed` (tests.rs).
pub(super) fn vouches_for(recorded: &Recorded, rel: &str, current: &[u8]) -> bool {
    recorded
        .get(rel)
        .is_some_and(|known| *known == digest(current))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the manifest is the whole basis for the overwrite decision, so a
    /// value that does not survive a write/read cycle would silently unvouch a
    /// pristine file (a template update that never lands) or vouch an edited
    /// one (the data loss #3844 reports).
    /// Test: itself.
    #[test]
    fn provenance_round_trips_every_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let mut recorded = Recorded::new();
        recorded.insert("assistant/agent.toml".to_string(), digest(b"one"));
        recorded.insert("ctrl.toml".to_string(), digest(b"two"));

        write(tmp.path(), &recorded).unwrap();

        let back = read(tmp.path());
        assert_eq!(back, recorded);
        assert!(vouches_for(&back, "assistant/agent.toml", b"one"));
        assert!(!vouches_for(&back, "assistant/agent.toml", b"edited"));
    }

    /// Why: every machine provisioned before #3844 hits this path, and it must
    /// mean "nothing is vouched" rather than an error that fails the pass.
    /// Test: itself.
    #[test]
    fn missing_manifest_reads_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read(tmp.path()).is_empty());
        assert!(!vouches_for(
            &read(tmp.path()),
            "assistant/agent.toml",
            b"x"
        ));
    }

    /// Why: a truncated or hand-mangled manifest must lose only the lines it
    /// cannot parse, and must never let a half-line be read as a digest — a
    /// wrong digest that happened to match would authorise an overwrite.
    /// Test: itself.
    #[test]
    fn unparsable_lines_are_dropped_not_guessed() {
        let tmp = tempfile::tempdir().unwrap();
        let good = digest(b"kept");
        std::fs::write(
            tmp.path().join(PROVENANCE_FILE_NAME),
            format!("no-tab-at-all\n\t\n\tonly-a-path\n{good}\t\n{good}\tctrl.toml\n"),
        )
        .unwrap();

        let recorded = read(tmp.path());

        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert!(vouches_for(&recorded, "ctrl.toml", b"kept"));
    }

    /// Why: the format is tab-separated and line-oriented, so a path holding
    /// either character cannot round-trip. Skipping it leaves the file
    /// unvouched, which is the safe direction — the alternative is a mangled
    /// line that could vouch for the wrong path.
    /// Test: itself.
    #[test]
    fn a_path_that_cannot_be_recorded_is_left_unvouched() {
        let tmp = tempfile::tempdir().unwrap();
        let mut recorded = Recorded::new();
        recorded.insert("tab\there.toml".to_string(), digest(b"body"));
        recorded.insert("newline\nhere.toml".to_string(), digest(b"body"));
        recorded.insert("fine.toml".to_string(), digest(b"body"));

        write(tmp.path(), &recorded).unwrap();
        let back = read(tmp.path());

        assert_eq!(back.len(), 1, "only the recordable path survives: {back:?}");
        assert!(vouches_for(&back, "fine.toml", b"body"));
        assert!(!vouches_for(&back, "tab\there.toml", b"body"));
    }
}
