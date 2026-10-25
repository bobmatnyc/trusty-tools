//! The `$HOME` walk behind `tm doctor`'s machine-wide settings sweep.
//!
//! Why (#10263): plain `tm doctor` now walks `$HOME` on every run. The shared
//! [`trusty_common::claude_config::discover_claude_settings`] enters
//! `~/Documents` and `~/Desktop`, which are iCloud Drive on macOS, and a read
//! there has frozen a session on the operator's host. It also descends into
//! mount points. Its other callers keep that behaviour; this walk is tm's own.
//! What: the same `.claude/settings*.json` harvest and the same
//! [`SCAN_SKIP_DIRS`], plus two guards: the top-level home folders in
//! [`HOME_SKIP_DIRS`] are never entered, and a directory on a different
//! device (`st_dev`) than `home` is never entered. Symlinks are not followed.
//! Test: the tests module below.

use std::path::{Path, PathBuf};

use trusty_common::claude_config::SCAN_SKIP_DIRS;

/// Top-level `$HOME` folders the walk never enters.
///
/// Why (#10263): macOS syncs these to iCloud or the user's media libraries; a
/// read can block on a network fetch. No project is expected to live directly
/// in them. Matched only at depth one, so `<home>/code/Documents/proj` is
/// still walked.
pub const HOME_SKIP_DIRS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
];

/// Every `.claude/settings.json` and `.claude/settings.local.json` under `home`.
///
/// Why: see the module doc — the doctor's walk must not block on a synced or
/// mounted directory.
/// What: walks `home` up to `max_depth` levels, skipping [`SCAN_SKIP_DIRS`] at
/// any depth, [`HOME_SKIP_DIRS`] directly under `home`, any symlink, and any
/// directory whose device differs from `home`'s. An unreadable `home` device
/// enters nothing below `home`. Returns sorted paths.
/// Test: `walk_skips_synced_home_folders_only_at_the_top_level`,
/// `walk_never_enters_a_directory_on_another_device`.
pub fn discover_home_settings(home: &Path, max_depth: usize) -> Vec<PathBuf> {
    let home_dev = device_of(home);
    discover_home_settings_with(home, max_depth, &|dir| {
        same_device(home_dev, device_of(dir))
    })
}

/// [`discover_home_settings`] with the device check injected.
///
/// Why: a test cannot create a mount point; it supplies the predicate instead.
/// What: `on_home_device(dir)` is asked before entering each directory below
/// `home`; `false` skips it and its subtree.
/// Test: `walk_never_enters_a_directory_on_another_device`.
pub fn discover_home_settings_with(
    home: &Path,
    max_depth: usize,
    on_home_device: &dyn Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    collect(home, max_depth, true, on_home_device, &mut found);
    found.sort();
    found
}

/// `true` only when both devices are known and equal.
///
/// Why: an unknown device cannot be shown to be local, so it is not entered.
/// Test: `same_device_requires_two_known_equal_devices`.
pub fn same_device(home_dev: Option<u64>, dir_dev: Option<u64>) -> bool {
    matches!((home_dev, dir_dev), (Some(h), Some(d)) if h == d)
}

/// The `st_dev` of `path`, without following a symlink; `None` on error.
#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| m.dev())
}

/// No `st_dev` off Unix: every path reports one device, so only the name
/// skip lists guard the walk there.
#[cfg(not(unix))]
fn device_of(path: &Path) -> Option<u64> {
    std::fs::symlink_metadata(path).ok().map(|_| 0)
}

/// Recursive worker for [`discover_home_settings_with`].
fn collect(
    dir: &Path,
    depth_remaining: usize,
    is_home: bool,
    on_home_device: &dyn Fn(&Path) -> bool,
    out: &mut Vec<PathBuf>,
) {
    if dir.file_name().and_then(|n| n.to_str()) == Some(".claude") {
        for name in ["settings.json", "settings.local.json"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                out.push(candidate);
            }
        }
        return;
    }
    if depth_remaining == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        // `DirEntry::file_type` does not follow symlinks, so a link is skipped.
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name != ".claude" && SCAN_SKIP_DIRS.contains(&name) {
            continue;
        }
        // #10263: synced home folders, top level only.
        if is_home && HOME_SKIP_DIRS.contains(&name) {
            continue;
        }
        // #10263: never cross onto another filesystem (mounts, CloudStorage, FUSE).
        if !on_home_device(&path) {
            continue;
        }
        collect(&path, depth_remaining - 1, false, on_home_device, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plant(root: &Path, rel: &str) -> PathBuf {
        let dir = root.join(rel).join(".claude");
        std::fs::create_dir_all(&dir).expect("create .claude");
        let file = dir.join("settings.json");
        std::fs::write(&file, "{}").expect("write settings");
        file
    }

    #[test]
    fn walk_skips_synced_home_folders_only_at_the_top_level() {
        let home = tempfile::tempdir().expect("temp home");
        let documents = plant(home.path(), "Documents/proj");
        let desktop = plant(home.path(), "Desktop/scratch/proj");
        let downloads = plant(home.path(), "Downloads/proj");
        let nested = plant(home.path(), "code/Documents/proj");
        let plain = plant(home.path(), "code/proj");

        let files = discover_home_settings(home.path(), 8);

        for skipped in [&documents, &desktop, &downloads] {
            assert!(!files.contains(skipped), "{skipped:?} returned: {files:?}");
        }
        for kept in [&nested, &plain] {
            assert!(files.contains(kept), "{kept:?} missing: {files:?}");
        }
    }

    #[test]
    fn walk_never_enters_a_directory_on_another_device() {
        let home = tempfile::tempdir().expect("temp home");
        let mounted = plant(home.path(), "mnt/remote/proj");
        let local = plant(home.path(), "code/proj");
        let mount_point = home.path().join("mnt").join("remote");

        let files =
            discover_home_settings_with(home.path(), 8, &|dir: &Path| dir != mount_point.as_path());

        assert!(!files.contains(&mounted), "{mounted:?} returned: {files:?}");
        assert!(files.contains(&local), "{local:?} missing: {files:?}");
    }

    #[cfg(unix)]
    #[test]
    fn walk_does_not_follow_a_symlinked_directory() {
        let home = tempfile::tempdir().expect("temp home");
        let elsewhere = tempfile::tempdir().expect("link target");
        plant(elsewhere.path(), "proj");
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join("link")).expect("symlink");

        let files = discover_home_settings(home.path(), 8);

        assert!(files.is_empty(), "a symlinked tree was walked: {files:?}");
    }

    #[test]
    fn same_device_requires_two_known_equal_devices() {
        assert!(same_device(Some(7), Some(7)));
        assert!(!same_device(Some(7), Some(8)));
        assert!(!same_device(None, Some(7)));
        assert!(!same_device(Some(7), None));
        assert!(!same_device(None, None));
    }
}
