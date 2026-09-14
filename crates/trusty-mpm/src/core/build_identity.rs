//! Fingerprint of the executable file a process is running (issue #7822).
//!
//! Why: `tm doctor`'s `daemon_version` check compared the running daemon's
//! semver against the installed binary's semver and nothing else, so two
//! DIFFERENT builds that share a version string read as a match. On 2026-09-13
//! that hole cost an operator their managed `settings.json`: daemon pid 60859
//! started before #7789 merged, kept serving the pre-fix settings writer that
//! rewrites without a backup, and reported 1.5.36 — the same 1.5.36 the
//! freshly installed binary reported. `tm doctor` said the daemon matched.
//! Semver is a RELEASE label, not a build label; it cannot distinguish two
//! builds cut under the same version, which is the normal state of a
//! development branch between publishes.
//!
//! What: [`identity_of`] fingerprints a file as `"<mtime_epoch_secs>:<bytes>"`;
//! [`current_exe_identity`] applies it to `std::env::current_exe()`. The daemon
//! captures its own fingerprint ONCE at startup (`daemon_run::run_daemon`) and
//! publishes it on `GET /health`; `tm doctor` fingerprints the binary IT is
//! running and compares. Capturing at startup is the whole mechanism: reading
//! the daemon's own path at `/health` time would stat whatever `cargo install`
//! last wrote, which is precisely the file a stale daemon is NOT running.
//!
//! mtime plus size rather than a content hash: both are one `stat` away, while
//! hashing a multi-megabyte binary would run on every `tm doctor` and at every
//! daemon boot. The only behavioural difference is that a rebuild producing a
//! byte-identical binary still reads as a different build — one extra restart
//! prompt, which is the cheap direction to be wrong in. A git SHA baked in by a
//! `build.rs` was rejected for the opposite reason: cargo caches build-script
//! output, so the SHA silently goes stale, and a crates.io build has no git
//! directory to read at all.
//!
//! Test: the `tests` module below covers a stable re-read, a rewritten file,
//! and a missing path.

use std::path::Path;
use std::time::UNIX_EPOCH;

/// Fingerprint one file as `"<mtime_epoch_secs>:<bytes>"`.
///
/// Why: see the module doc — this is the cheapest identity that changes
/// whenever `cargo install` replaces a binary, which is the event that leaves a
/// running daemon stale.
/// What: one `stat`. Returns `None` when the path cannot be read or its mtime
/// predates the Unix epoch; callers must treat `None` as "cannot tell", never
/// as a match.
/// Test: `identity_is_stable_across_reads`, `identity_changes_when_file_changes`,
/// `identity_is_none_for_a_missing_path`.
pub fn identity_of(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(format!("{mtime}:{}", meta.len()))
}

/// Fingerprint the executable THIS process is running.
///
/// Why: both sides of the #7822 comparison are "the build this process runs" —
/// the daemon records it at startup, `tm doctor` records its own at check time.
/// What: [`identity_of`] over `std::env::current_exe()`; `None` when either
/// step fails.
/// Test: `current_exe_identity_resolves_under_cargo_test`; the fingerprinting
/// itself is covered by [`identity_of`]'s tests.
pub fn current_exe_identity() -> Option<String> {
    identity_of(&std::env::current_exe().ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_across_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("tm");
        std::fs::write(&file, b"build-one").expect("write");
        assert!(identity_of(&file).is_some());
        assert_eq!(identity_of(&file), identity_of(&file));
    }

    /// The case the check exists for: the file at a path is replaced, so the
    /// fingerprint a process captured earlier no longer describes what is on
    /// disk. The two writes differ in LENGTH so the assertion cannot hinge on
    /// filesystem mtime resolution.
    #[test]
    fn identity_changes_when_file_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("tm");
        std::fs::write(&file, b"build-one").expect("write");
        let before = identity_of(&file).expect("first fingerprint");
        std::fs::write(&file, b"build-two-is-longer").expect("rewrite");
        let after = identity_of(&file).expect("second fingerprint");
        assert_ne!(
            before, after,
            "a replaced binary must fingerprint differently"
        );
    }

    #[test]
    fn identity_is_none_for_a_missing_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(identity_of(&dir.path().join("absent")), None);
    }

    /// The daemon and `tm doctor` both go through this entry point; if it
    /// cannot resolve, every verdict degrades to "cannot tell" rather than to a
    /// false `Ok` — but it should resolve on any host that can run a binary.
    #[test]
    fn current_exe_identity_resolves_under_cargo_test() {
        assert!(current_exe_identity().is_some());
    }
}
