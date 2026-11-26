//! Seal a target directory's last-checkout marker so a build outside the
//! lease cannot pass as the checkout it names (#11661).
//!
//! Why: [`super::stale_guard`] trusted [`LAST_CHECKOUT_MARKER`] alone. A
//! `cargo` run outside the lease from checkout B never rewrites the marker,
//! so after it the marker still names checkout A, the next lease from A read
//! "same checkout", cleared nothing, and cargo judged B's outputs Fresh by
//! mtime — A's gate ran B's binaries.
//! What: when a leased build ends, [`seal_marker`] writes a second marker
//! line, `seal v1 <sha256>`, a digest of every file under every
//! `<profile>/.fingerprint/*/` entry (relative path, size, mtime to the
//! nanosecond). Any build that rebuilds a unit rewrites that unit's
//! fingerprint files, so the digest changes. [`verify`] recomputes it at the
//! next lease; anything but an exact match — no seal line (an old-format or
//! never-sealed marker), a different digest, unreadable fingerprints, or
//! mtimes with no sub-second part (a coarse filesystem, where a rewrite can
//! keep its mtime) — fails, and the caller clears. Equality, never an
//! ordering of times, so clock skew cannot make a stale directory pass.
//! A seal is not written while cargo's lock is held there by another build:
//! the marker is left unsealed and the next lease clears.
//! Cost: one listing of each `.fingerprint` and of each entry in it, and a
//! `stat` per file, at the start and end of a lease; never `deps/`. A
//! checkout's own unleased build also changes the digest, so its next lease
//! clears once and rebuilds its workspace crates.
//! Test: `a_sealed_marker_verifies_until_a_fingerprint_is_rewritten`,
//! `an_unsealed_or_old_marker_never_verifies`,
//! `whole_second_mtimes_never_verify`,
//! `an_unleased_build_of_another_checkout_cannot_pass_as_this_one_11661` in
//! `tests/tm_build_lease.rs`.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::stale_guard::{LAST_CHECKOUT_MARKER, cargo_lock_held, fingerprint_dirs_of};

/// The prefix of the marker's seal line.
pub const SEAL_PREFIX: &str = "seal v1 ";

/// The marker in `dir`: its checkout (first line) and its seal, if any.
#[must_use]
pub fn read_marker(dir: &Path) -> (Option<String>, Option<String>) {
    let Ok(body) = std::fs::read_to_string(dir.join(LAST_CHECKOUT_MARKER)) else {
        return (None, None);
    };
    let mut lines = body.lines();
    let checkout = lines.next().map(|l| l.trim().to_string());
    let seal = lines.find_map(|l| l.trim().strip_prefix(SEAL_PREFIX).map(str::to_string));
    (checkout, seal)
}

/// The digest of every fingerprint file in `dir`.
///
/// # Errors
///
/// A fingerprint directory or file that cannot be read, or fingerprint files
/// whose mtimes all lack a sub-second part (a coarse filesystem).
pub fn stamp(dir: &Path) -> Result<String, String> {
    let unreadable = |p: &Path, e: std::io::Error| format!("{} unreadable: {e}", p.display());
    let mut rows = Vec::new();
    let mut fine_grained = false;
    let fingerprints = fingerprint_dirs_of(dir).map_err(|e| unreadable(dir, e))?;
    for fp in fingerprints {
        for entry in std::fs::read_dir(&fp).map_err(|e| unreadable(&fp, e))? {
            let entry = entry.map_err(|e| unreadable(&fp, e))?;
            if !entry.file_type().map_err(|e| unreadable(&fp, e))?.is_dir() {
                continue;
            }
            let unit = entry.path();
            for file in std::fs::read_dir(&unit).map_err(|e| unreadable(&unit, e))? {
                let file = file.map_err(|e| unreadable(&unit, e))?;
                let meta = file.metadata().map_err(|e| unreadable(&file.path(), e))?;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .ok_or_else(|| format!("{} has no usable mtime", file.path().display()))?;
                fine_grained |= mtime.subsec_nanos() != 0;
                let rel = file.path();
                let rel = rel.strip_prefix(dir).unwrap_or(&rel).display().to_string();
                rows.push(format!("{rel}\0{}\0{}", meta.len(), mtime.as_nanos()));
            }
        }
    }
    if !rows.is_empty() && !fine_grained {
        return Err("fingerprint mtimes have whole-second resolution".to_string());
    }
    rows.sort();
    let mut hash = Sha256::new();
    for row in &rows {
        hash.update(row.as_bytes());
        hash.update(b"\n");
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// `Ok` only when `seal` matches the fingerprints in `dir` now.
///
/// # Errors
///
/// Why the directory is not trusted: no seal, a mismatch, or [`stamp`]'s error.
pub fn verify(dir: &Path, seal: Option<&str>) -> Result<(), String> {
    let seal = seal.ok_or("its record carries no seal")?;
    if stamp(dir)? == seal {
        Ok(())
    } else {
        Err("its fingerprints changed since that checkout's last lease".to_string())
    }
}

/// Seal `dir`'s marker for `checkout` once its leased build has ended.
///
/// What: nothing when the marker names another checkout. Unsealed (the
/// checkout line alone) when cargo's lock is held there or [`stamp`] fails;
/// sealed otherwise. `Ok(true)` when a seal was written.
///
/// # Errors
///
/// The marker could not be written.
pub fn seal_marker(dir: &Path, checkout: &Path) -> Result<bool, String> {
    let current = checkout.display().to_string();
    if read_marker(dir).0.as_deref() != Some(current.as_str()) {
        return Ok(false);
    }
    let seal = if cargo_lock_held(dir) {
        None
    } else {
        stamp(dir).ok()
    };
    let body = match &seal {
        Some(s) => format!("{current}\n{SEAL_PREFIX}{s}\n"),
        None => current,
    };
    let marker = dir.join(LAST_CHECKOUT_MARKER);
    std::fs::write(&marker, body)
        .map_err(|e| format!("could not write {}: {e}", marker.display()))?;
    Ok(seal.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn unit_file(dir: &Path, mtime: SystemTime) -> std::path::PathBuf {
        let unit = dir.join("debug/.fingerprint/widget-0123456789abcdef");
        std::fs::create_dir_all(&unit).expect("unit");
        let file = unit.join("lib-widget");
        std::fs::write(&file, "0123456789abcdef").expect("write");
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|f| f.set_modified(mtime))
            .expect("mtime");
        file
    }

    fn marked(checkout: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tmp");
        std::fs::write(tmp.path().join(LAST_CHECKOUT_MARKER), checkout).expect("marker");
        tmp
    }

    #[test]
    fn a_sealed_marker_verifies_until_a_fingerprint_is_rewritten() {
        let tmp = marked("/work/a");
        let t = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        let file = unit_file(tmp.path(), t);
        assert_eq!(seal_marker(tmp.path(), Path::new("/work/a")), Ok(true));
        let (checkout, seal) = read_marker(tmp.path());
        assert_eq!(checkout.as_deref(), Some("/work/a"));
        assert_eq!(verify(tmp.path(), seal.as_deref()), Ok(()));
        // Another build rewrites the unit: same name, same size, new mtime.
        unit_file(tmp.path(), t + Duration::from_nanos(1));
        assert!(file.exists());
        assert!(verify(tmp.path(), seal.as_deref()).is_err());
    }

    #[test]
    fn an_unsealed_or_old_marker_never_verifies() {
        let tmp = marked("/work/a");
        unit_file(tmp.path(), SystemTime::now());
        let (_, seal) = read_marker(tmp.path());
        assert_eq!(seal, None, "the old format carries no seal");
        assert!(verify(tmp.path(), seal.as_deref()).is_err());
        assert_eq!(
            seal_marker(tmp.path(), Path::new("/work/b")),
            Ok(false),
            "another checkout's marker is never sealed"
        );
        assert_eq!(read_marker(tmp.path()).1, None);
    }

    #[test]
    fn whole_second_mtimes_never_verify() {
        let tmp = marked("/work/a");
        unit_file(
            tmp.path(),
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        );
        assert!(stamp(tmp.path()).is_err());
        assert_eq!(seal_marker(tmp.path(), Path::new("/work/a")), Ok(false));
        assert!(verify(tmp.path(), read_marker(tmp.path()).1.as_deref()).is_err());
    }
}
