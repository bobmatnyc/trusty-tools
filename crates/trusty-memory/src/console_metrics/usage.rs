//! The daemon's own RAM and disk usage, for the `console_metrics` payload
//! (#6928).
//!
//! Why: the console's memory tab is display-only, and the two things it
//! displays are how much RAM this daemon holds and how much disk the palace
//! store occupies. Both are facts only the daemon can state correctly: the
//! footprint because `task_info` answers for the calling task alone, and the
//! disk figure because the daemon owns `data_root` and the console has no
//! Cargo edge on this crate to learn where that is.
//! What: [`sample`] reads the process footprint plus its heap / file-backed /
//! compressed split, and walks `data_root` for its allocated size.
//! [`palace_disk_bytes`] does the same for one palace directory.
//! Test: `usage_sample_reports_a_footprint_and_a_disk_figure`,
//! `palace_disk_bytes_counts_a_palace_directory`,
//! `console_metrics_reports_ram_and_disk_usage`.
//!
//! # Why the walk is affordable on a poll
//!
//! `dir_allocated_bytes` stats every file under the root. The palace store is
//! a few files per palace — two redb databases and a last-used stamp — so the
//! reference host measured 1,425 files across 102 palaces. That is a walk of
//! milliseconds, not the `target/`-scale tree
//! `trusty_mpm::disk::DirSizeIndex` exists to cache. It runs on the same
//! blocking pool as the per-palace counts, so it never touches the runtime.

use std::path::Path;

use trusty_common::sys_metrics::dir_allocated_bytes;

/// What this daemon is using, right now.
///
/// Why the breakdown is three `Option`s rather than three zeros: "not
/// reported" and "zero bytes" are different facts, and the console renders
/// them differently — an absent field says the OS does not supply the split,
/// a zero says it does and the number is zero.
/// What: `ram_bytes` is the physical footprint (`ri_phys_footprint`), the same
/// counter the console's Services row reads for this pid. `disk_bytes` is the
/// allocated size of `data_root`, the figure `du -s` reports.
/// Test: `usage_sample_reports_a_footprint_and_a_disk_figure`.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DaemonUsage {
    /// Physical footprint in bytes, or `None` where the OS has no such counter.
    pub(super) ram_bytes: Option<u64>,
    /// Anonymous dirty pages — heap and stacks.
    pub(super) ram_heap_bytes: Option<u64>,
    /// File-backed pages, the mmapped vector and redb stores included.
    pub(super) ram_file_backed_bytes: Option<u64>,
    /// Pages held by the macOS memory compressor.
    pub(super) ram_compressed_bytes: Option<u64>,
    /// Allocated bytes under `data_root`.
    pub(super) disk_bytes: u64,
}

/// Measure this daemon's RAM footprint and its store's disk usage.
///
/// Why it takes `data_root` rather than the whole `AppState`: this runs on the
/// blocking pool alongside the palace counts, and a `Path` is all it needs —
/// keeping it that narrow is what lets it be tested against a fixture
/// directory with no daemon at all.
/// What: one `proc_pid_rusage` call, one `task_info` call, one bounded
/// directory walk. Every failure degrades to `None` or to the partial total;
/// nothing here can fail the poll.
/// Test: `usage_sample_reports_a_footprint_and_a_disk_figure`.
pub(super) fn sample(data_root: &Path) -> DaemonUsage {
    let breakdown = trusty_common::sys_metrics::self_memory_breakdown();
    DaemonUsage {
        ram_bytes: process_footprint_bytes(),
        ram_heap_bytes: breakdown.map(|b| b.heap_bytes),
        ram_file_backed_bytes: breakdown.map(|b| b.file_backed_bytes),
        ram_compressed_bytes: breakdown.map(|b| b.compressed_bytes),
        disk_bytes: dir_allocated_bytes(data_root),
    }
}

/// One palace directory's allocated size.
///
/// Why separate from [`sample`]: it runs per palace inside the same
/// aggregation loop, and only the first `MAX_PALACES_IN_REPORT` rows need it.
/// Test: `palace_disk_bytes_counts_a_palace_directory`.
pub(super) fn palace_disk_bytes(data_dir: &Path) -> u64 {
    dir_allocated_bytes(data_dir)
}

/// This process's physical footprint, where the OS reports one.
///
/// macOS-only because `ri_phys_footprint` is: on every other target the
/// console shows the field as absent rather than substituting an RSS reading
/// that means something different (repo memory `macos-phys-footprint-vs-ps-rss`).
fn process_footprint_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        trusty_common::sys_metrics::physical_footprint_bytes(std::process::id())
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the payload's two headline figures are produced here, and a
    /// swapped or absent field would surface as a plausible-looking zero on
    /// the console rather than as an error.
    /// Test: this is the test.
    #[test]
    fn usage_sample_reports_a_footprint_and_a_disk_figure() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(tmp.path().join("kg.redb"), vec![0u8; 40_000]).expect("write");

        let usage = sample(tmp.path());
        assert!(
            usage.disk_bytes >= 40_000,
            "disk_bytes must cover the fixture file, got {}",
            usage.disk_bytes
        );

        #[cfg(target_os = "macos")]
        {
            let ram = usage.ram_bytes.expect("macOS reports a footprint");
            assert!(ram > 1024 * 1024, "a live process holds more than 1 MB");
            assert!(
                usage.ram_heap_bytes.is_some(),
                "macOS supplies the heap/file-backed/compressed split"
            );
        }
    }

    /// Why: the per-palace column is the disk breakdown the tab drills into,
    /// so it must measure the palace's own directory and nothing else.
    /// Test: this is the test.
    #[test]
    fn palace_disk_bytes_counts_a_palace_directory() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let palace = tmp.path().join("scratch");
        std::fs::create_dir(&palace).expect("mkdir");
        std::fs::write(palace.join("kg.redb"), vec![0u8; 8_000]).expect("write");
        // A sibling palace must not leak into this one's figure.
        let other = tmp.path().join("other");
        std::fs::create_dir(&other).expect("mkdir");
        std::fs::write(other.join("kg.redb"), vec![0u8; 900_000]).expect("write");

        let measured = palace_disk_bytes(&palace);
        assert!(measured >= 8_000, "got {measured}");
        assert!(measured < 900_000, "a sibling palace leaked in: {measured}");
    }

    /// An absent palace directory measures zero rather than failing the poll.
    #[test]
    fn palace_disk_bytes_is_zero_for_a_missing_directory() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        assert_eq!(palace_disk_bytes(&tmp.path().join("gone")), 0);
    }
}
