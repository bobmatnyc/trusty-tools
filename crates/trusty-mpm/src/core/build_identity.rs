//! The build id every binary of one `trusty-mpm` build shares (#7822, #7873).
//!
//! Why: `tm doctor`'s `daemon_version` check needs to tell two builds cut under
//! the same semver apart. On 2026-09-13 that hole cost an operator their
//! managed `settings.json`: daemon pid 60859 started before #7789 merged, kept
//! serving the pre-fix settings writer that rewrites without a backup, and
//! reported 1.5.36 — the same 1.5.36 the freshly installed binary reported.
//! Semver is a RELEASE label, not a build label.
//!
//! #7822 filled that gap with a per-FILE fingerprint (`"<mtime>:<size>"` of
//! `std::env::current_exe()`), which #7873 replaced: this package ships TWO
//! `[[bin]]` targets from one source — `tm`, which runs the check, and
//! `trusty-mpm`, which runs the daemon — and one `cargo install` writes them
//! seconds apart. Their file fingerprints therefore always differ (observed:
//! daemon `1789397245:73209856` against installed `1789397221:73195712` right
//! after a supported `tm restart`), so the Warn never cleared on a fully
//! current install and the check taught operators to ignore it.
//!
//! What: [`build_id`] returns the compile-time `TRUSTY_MPM_BUILD_ID` that
//! `build.rs` emits once per package build — identical in both bins, different
//! after a real rebuild. The daemon records it at startup
//! (`daemon_run::run_daemon`) and publishes it on `GET /health`; `tm doctor`
//! compares that against its own. Nothing stats a binary any more, so no
//! comparison can mix a compile-time id with a file fingerprint: the only
//! foreign value left is the `"<mtime>:<size>"` a pre-#7873 daemon still
//! reports, and that daemon genuinely IS an older build, which is the Warn the
//! check exists to raise.
//!
//! Test: the `tests` module below.

/// The build id compiled into this binary.
///
/// Why: see the module doc — the id must identify the BUILD, so that the `tm`
/// bin checking a daemon and the `trusty-mpm` bin running it agree whenever
/// one `cargo install` produced both.
/// What: reads the `TRUSTY_MPM_BUILD_ID` that `build.rs` emitted via
/// `cargo::rustc-env`. `env!` makes its absence a compile error, so this cannot
/// silently degrade to an empty id.
/// Test: `build_id_does_not_vary_with_the_executable_file`,
/// `build_id_is_a_nonempty_decimal_counter`.
pub fn build_id() -> &'static str {
    env!("TRUSTY_MPM_BUILD_ID")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    /// The #7822 fingerprint this module used to return, kept HERE so the
    /// regression test can state what the id must no longer be without leaving
    /// the dead mechanism in production code.
    fn per_file_fingerprint(path: &std::path::Path) -> String {
        let meta = std::fs::metadata(path).expect("metadata of the running test binary");
        let mtime = meta
            .modified()
            .expect("mtime of the running test binary")
            .duration_since(UNIX_EPOCH)
            .expect("mtime after the Unix epoch")
            .as_secs();
        format!("{mtime}:{}", meta.len())
    }

    /// #7873: the defect shape. A per-file fingerprint makes the id a property
    /// of whichever executable the process was launched from, and this package
    /// builds two of them per `cargo install` — so `tm doctor` compared `tm`'s
    /// file against the daemon's `trusty-mpm` file and warned forever. Passing
    /// requires the id to come from the compile-time env, which every target of
    /// one build shares; reverting [`build_id`] to stat `current_exe()` fails
    /// this assertion.
    #[test]
    fn build_id_does_not_vary_with_the_executable_file() {
        let exe = std::env::current_exe().expect("current_exe");
        let fingerprint = per_file_fingerprint(&exe);
        assert_ne!(
            build_id(),
            fingerprint,
            "build id must not be the mtime:size of the executable running it — \
             the `tm` and `trusty-mpm` bins of one build never share one"
        );
    }

    /// The id is a nanosecond counter, so it is non-empty decimal digits and
    /// never the `"<mtime>:<size>"` shape a pre-#7873 daemon reports.
    #[test]
    fn build_id_is_a_nonempty_decimal_counter() {
        let id = build_id();
        assert!(!id.is_empty(), "build.rs must emit a non-empty id");
        assert!(
            id.chars().all(|c| c.is_ascii_digit()),
            "build id should be a decimal counter, got `{id}`"
        );
    }
}
