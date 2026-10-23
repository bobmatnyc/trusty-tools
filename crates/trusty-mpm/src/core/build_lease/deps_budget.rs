//! A bounded entry count for a cargo profile's `deps/` directory (#10212).
//!
//! Why: an 844,490-entry `debug/deps` stalled syspolicyd on the Mac and every
//! exec waited behind it. A full readdir of a directory that size IS the
//! stall, so nothing may list one in full to learn that it is too big.
//! What: [`count_deps`] reads at most `line` entries and stops. The answer is
//! the exact count under the line, or "at least the line" once it is reached.
//! A missing directory and an unreadable one are outcomes of their own, never
//! a count of 0. No entry is `stat`ed.
//! Test: the `#[cfg(test)]` suite below.

use std::path::Path;

/// `tm doctor`'s `deps_budget` row warns at this many entries in `<profile>/deps`.
///
/// Provisional until an EVO baseline of a fresh build's count (#10212, Q1a).
pub const DEPS_WARN_ENTRIES: usize = 150_000;

/// The entry count at which a profile directory is to be rotated aside.
///
/// Provisional like [`DEPS_WARN_ENTRIES`]; #10212 Part 1 owns the rotation.
pub const DEPS_ROTATE_ENTRIES: usize = 300_000;

// #10212: the warning has to arrive before a rotation, never after it.
const _: () = assert!(DEPS_WARN_ENTRIES < DEPS_ROTATE_ENTRIES);

/// What a bounded count of one `deps/` directory found.
///
/// Test: `a_dir_under_the_line_reports_its_exact_count`,
/// `the_counter_reads_at_most_the_line_from_a_bigger_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DepsCount {
    /// Fewer entries than the line: this is the exact count.
    Under(usize),
    /// The line was reached and reading stopped there.
    AtLeast(usize),
    /// The directory does not exist.
    Missing,
    /// The directory exists but could not be listed, at open or mid-listing.
    Unreadable(String),
}

/// Count the entries of `dir`, reading no more than `line` of them.
///
/// Why: the doctor row (and #10212 Part 1's rotation) needs "is this over the
/// line" without the full readdir that stalls the host.
/// What: `std::fs::read_dir` through [`count_deps_with`]. `read_dir` fetches in
/// kernel batches, so the cost is bounded by `line`, not by the directory.
/// Test: `the_counter_reads_at_most_the_line_from_a_bigger_dir`,
/// `a_missing_dir_is_missing_not_zero`, `an_unreadable_dir_is_unreadable_not_zero`.
#[must_use]
pub fn count_deps(dir: &Path, line: usize) -> DepsCount {
    count_deps_with(dir, line, std::fs::read_dir)
}

/// [`count_deps`] with the directory opener as a seam, so a test can count reads.
///
/// Test: `an_error_mid_listing_is_unreadable_not_a_short_count`.
fn count_deps_with<'a, I, T>(
    dir: &'a Path,
    line: usize,
    open: impl FnOnce(&'a Path) -> std::io::Result<I>,
) -> DepsCount
where
    I: Iterator<Item = std::io::Result<T>>,
{
    let entries = match open(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return DepsCount::Missing,
        Err(err) => return DepsCount::Unreadable(err.to_string()),
    };
    let mut read = 0;
    // #10212: `take` is the bound; the iterator is never driven past the line.
    for entry in entries.take(line) {
        if let Err(err) = entry {
            return DepsCount::Unreadable(err.to_string());
        }
        read += 1;
    }
    if read >= line {
        DepsCount::AtLeast(line)
    } else {
        DepsCount::Under(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;

    fn dir_with(n: usize) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..n {
            std::fs::write(tmp.path().join(format!("f{i}")), b"").expect("file");
        }
        tmp
    }

    /// The proof the measure is cheap: a dir of ten times the line is read
    /// at most line + 1 times through the seam.
    #[test]
    fn the_counter_reads_at_most_the_line_from_a_bigger_dir() {
        let tmp = dir_with(50);
        let reads = Cell::new(0usize);
        let counted = count_deps_with(tmp.path(), 5, |p| {
            std::fs::read_dir(p).map(|it| it.inspect(|_| reads.set(reads.get() + 1)))
        });
        assert_eq!(counted, DepsCount::AtLeast(5));
        assert!(
            reads.get() <= 6,
            "read {} entries for a line of 5",
            reads.get()
        );
    }

    #[test]
    fn a_dir_under_the_line_reports_its_exact_count() {
        let tmp = dir_with(3);
        assert_eq!(count_deps(tmp.path(), 5), DepsCount::Under(3));
    }

    /// Fail-Open Check: a missing dir is not "0 entries".
    #[test]
    fn a_missing_dir_is_missing_not_zero() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(count_deps(&tmp.path().join("deps"), 5), DepsCount::Missing);
    }

    /// Fail-Open Check: a dir that cannot be opened is not "0 entries".
    #[test]
    fn an_unreadable_dir_is_unreadable_not_zero() {
        let tmp = dir_with(2);
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o000))
            .expect("chmod");
        let counted = count_deps(tmp.path(), 5);
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        assert!(
            matches!(counted, DepsCount::Unreadable(_)),
            "an unreadable dir counted as {counted:?}"
        );
    }

    /// Fail-Open Check: a listing that fails partway is not a short count.
    #[test]
    fn an_error_mid_listing_is_unreadable_not_a_short_count() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let counted = count_deps_with(tmp.path(), 5, |_| {
            Ok(vec![Ok(()), Err(std::io::Error::other("EIO"))].into_iter())
        });
        assert_eq!(counted, DepsCount::Unreadable("EIO".into()));
    }
}
