//! File-based schema-version stamp (JSON sidecar).
//!
//! Why: many trusty-* stores already keep their durable state next to a
//! per-store directory (`index.redb`, `palace/kg.redb`, …). The simplest stamp
//! storage is a tiny JSON sidecar in that same directory — easy to inspect,
//! easy to write atomically, no extra schema to migrate. This module is the
//! one place that owns the on-disk file layout so every store that opts into
//! it stays compatible.
//!
//! What: two free functions — `read_version_from_file` and
//! `write_version_to_file` — both operating on a JSON object of the form
//! `{ "schema_version": <u32> }`. The write path uses temp-file + rename for
//! atomicity so a crash mid-write cannot truncate the stamp.
//!
//! Test: `file_stamp_roundtrip`, `read_returns_unversioned_when_missing`,
//! `read_returns_unversioned_on_corrupt_payload`,
//! `write_is_atomic_via_tmp_rename`.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::SchemaVersion;

/// On-disk shape of the stamp sidecar.
///
/// Why: keeping the wire format in a dedicated struct (rather than serialising
/// a bare `u32`) leaves room for future additions (timestamps, migration
/// labels) without breaking existing files — every new field would be optional
/// and `#[serde(default)]`-friendly.
/// What: a single `schema_version` field. Serialised as `{ "schema_version":
/// N }`.
/// Test: covered by the round-trip test in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct StampFile {
    schema_version: u32,
}

/// Read the [`SchemaVersion`] from a JSON sidecar at `path`.
///
/// Why: callers need a single line to answer "what version is the store on?"
/// without writing the same `match fs::read → from_slice → unwrap_or` ladder
/// every time. The behaviour is defensive by design — a missing file or a
/// malformed payload are *not* errors, they map to
/// [`SchemaVersion::UNVERSIONED`] so the [`super::MigrationRunner`] runs every
/// step from scratch (which is the correct behaviour for a fresh store *or*
/// for one whose stamp was lost / hand-edited).
/// What: reads `path`, parses it as `{ "schema_version": <u32> }`, and
/// returns the resulting [`SchemaVersion`]. `Ok(UNVERSIONED)` when the file
/// is absent or unparseable; an `Err` is only returned for I/O errors other
/// than `NotFound` (permission denied, etc.).
/// Test: `file_stamp_roundtrip`, `read_returns_unversioned_when_missing`,
/// `read_returns_unversioned_on_corrupt_payload`.
pub fn read_version_from_file(path: &Path) -> Result<SchemaVersion> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SchemaVersion::UNVERSIONED);
        }
        Err(e) => {
            return Err(e).with_context(|| format!("read schema stamp at {}", path.display()));
        }
    };
    match serde_json::from_slice::<StampFile>(&bytes) {
        Ok(stamp) => Ok(SchemaVersion(stamp.schema_version)),
        Err(e) => {
            tracing::warn!(
                "schema stamp at {} is malformed ({e}) — treating as UNVERSIONED",
                path.display()
            );
            Ok(SchemaVersion::UNVERSIONED)
        }
    }
}

/// Write a [`SchemaVersion`] to a JSON sidecar at `path`, atomically.
///
/// Why: the [`super::MigrationRunner`] invokes this after every successful
/// migration step. A crash mid-write must never leave a truncated stamp on
/// disk — the next boot would mis-identify the schema. Temp-file plus
/// rename is the standard cross-platform atomic-write pattern; on POSIX the
/// rename itself is guaranteed atomic, and on Windows it falls back to a
/// best-effort replace which is still safer than a direct write.
/// What: serialises `{ "schema_version": version.0 }` into a sibling
/// `<path>.tmp` file, then renames it over `path`. Creates any missing
/// parent directory components first.
/// Test: `file_stamp_roundtrip`, `write_is_atomic_via_tmp_rename`.
pub fn write_version_to_file(path: &Path, version: SchemaVersion) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent of schema stamp {}", path.display()))?;
    }
    let stamp = StampFile {
        schema_version: version.0,
    };
    let bytes = serde_json::to_vec(&stamp).context("serialize schema stamp")?;
    // Use a `.tmp` sibling instead of `path.with_extension("tmp")` so we don't
    // clobber a stamp whose filename happens to lack an extension (e.g.
    // `schema_version`).
    let tmp = {
        let mut t = path.as_os_str().to_owned();
        t.push(".tmp");
        std::path::PathBuf::from(t)
    };
    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("write temp schema stamp {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename schema stamp into place at {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> std::path::PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-common-stamp-test-{pid}-{nanos}"));
        std::fs::create_dir_all(&p).expect("create scratch dir");
        p
    }

    #[test]
    fn file_stamp_roundtrip() {
        // Why: baseline — write a version, read it back, expect equality.
        let dir = tempdir();
        let path = dir.join("schema_version.json");
        write_version_to_file(&path, SchemaVersion(7)).expect("write succeeds");
        let got = read_version_from_file(&path).expect("read succeeds");
        assert_eq!(got, SchemaVersion(7));
    }

    #[test]
    fn read_returns_unversioned_when_missing() {
        // Why: a brand-new store has no stamp on disk yet. The reader must
        // map that to UNVERSIONED so the runner applies every step.
        let dir = tempdir();
        let path = dir.join("missing.json");
        let got = read_version_from_file(&path).expect("missing file is not an error");
        assert_eq!(got, SchemaVersion::UNVERSIONED);
    }

    #[test]
    fn read_returns_unversioned_on_corrupt_payload() {
        // Why: a hand-edited / truncated stamp must not crash the daemon.
        let dir = tempdir();
        let path = dir.join("schema_version.json");
        std::fs::write(&path, b"this is not json").expect("write garbage");
        let got = read_version_from_file(&path).expect("corrupt file is not an error");
        assert_eq!(got, SchemaVersion::UNVERSIONED);
    }

    #[test]
    fn write_is_atomic_via_tmp_rename() {
        // Why: confirm the documented atomicity pattern (`.tmp` sibling +
        // rename) is what actually happens. Catches a future refactor that
        // accidentally switches to a direct write.
        let dir = tempdir();
        let path = dir.join("schema_version.json");
        write_version_to_file(&path, SchemaVersion(3)).expect("write");
        // The `.tmp` file must not linger after a successful write.
        let tmp = {
            let mut t = path.as_os_str().to_owned();
            t.push(".tmp");
            std::path::PathBuf::from(t)
        };
        assert!(
            !tmp.exists(),
            "temp file should be renamed away, but {} still exists",
            tmp.display()
        );
        assert!(path.exists(), "final stamp file must exist after write");
    }

    #[test]
    fn write_creates_missing_parent_directories() {
        // Why: callers often pass a path under a not-yet-created data dir.
        // The writer must materialise the parent before writing.
        let dir = tempdir();
        let nested = dir.join("a").join("b").join("c");
        let path = nested.join("schema_version.json");
        write_version_to_file(&path, SchemaVersion(1))
            .expect("nested write creates intermediate dirs");
        assert!(path.exists());
    }

    #[test]
    fn overwrite_replaces_existing_stamp() {
        // Why: every successful migration step overwrites the stamp. Confirm
        // the second write replaces the first cleanly (no append, no error).
        let dir = tempdir();
        let path = dir.join("schema_version.json");
        write_version_to_file(&path, SchemaVersion(1)).expect("first write");
        write_version_to_file(&path, SchemaVersion(2)).expect("second write");
        assert_eq!(
            read_version_from_file(&path).expect("read"),
            SchemaVersion(2)
        );
    }
}
