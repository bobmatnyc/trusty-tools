use super::*;

/// A slot directory with fingerprints for one workspace crate and one dependency.
fn slot() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for unit in [
        "debug/.fingerprint/trusty-mpm-0123456789abcdef",
        "debug/.fingerprint/trusty-mpm-fedcba9876543210",
        "debug/.fingerprint/trusty-mpm-extra-0123456789abcdef",
        "debug/.fingerprint/serde-0123456789abcdef",
        "aarch64-apple-darwin/debug/.fingerprint/trusty-mpm-00000000000000aa",
    ] {
        std::fs::create_dir_all(dir.path().join(unit)).expect("mkdir");
    }
    dir
}

fn exists(root: &Path, rel: &str) -> bool {
    root.join(rel).exists()
}

#[test]
fn a_different_checkout_clears_only_workspace_fingerprints() {
    let slot = slot();
    std::fs::write(slot.path().join(LAST_CHECKOUT_MARKER), "/wt/a").expect("marker");
    let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/b"), || {
        Ok(vec!["trusty-mpm".into()])
    })
    .expect("cleared");
    assert_eq!(
        got,
        Invalidation::Cleared {
            previous: Some("/wt/a".into()),
            removed: 3,
            all: false
        }
    );
    let root = slot.path();
    assert!(!exists(
        root,
        "debug/.fingerprint/trusty-mpm-0123456789abcdef"
    ));
    assert!(!exists(
        root,
        "aarch64-apple-darwin/debug/.fingerprint/trusty-mpm-00000000000000aa"
    ));
    assert!(
        exists(root, "debug/.fingerprint/serde-0123456789abcdef"),
        "deps stay warm"
    );
    assert!(
        exists(root, "debug/.fingerprint/trusty-mpm-extra-0123456789abcdef"),
        "a different package sharing a prefix is not this one"
    );
    let marker = std::fs::read_to_string(root.join(LAST_CHECKOUT_MARKER)).expect("marker");
    assert_eq!(marker, "/wt/b");
}

#[test]
fn the_same_checkout_touches_nothing() {
    let slot = slot();
    std::fs::write(slot.path().join(LAST_CHECKOUT_MARKER), "/wt/a").expect("marker");
    let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/a"), || {
        panic!("the package list is not needed for the same checkout")
    })
    .expect("ok");
    assert_eq!(got, Invalidation::SameCheckout);
    assert!(exists(
        slot.path(),
        "debug/.fingerprint/trusty-mpm-0123456789abcdef"
    ));
}

#[test]
fn a_missing_marker_clears() {
    let slot = slot();
    let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/a"), || {
        Ok(vec!["trusty-mpm".into()])
    })
    .expect("cleared");
    assert!(
        matches!(
            got,
            Invalidation::Cleared {
                previous: None,
                removed: 3,
                ..
            }
        ),
        "{got:?}"
    );
}

#[test]
fn an_unreadable_package_list_clears_every_fingerprint() {
    let slot = slot();
    let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/a"), || {
        Err("cargo metadata failed".into())
    })
    .expect("cleared");
    assert!(
        matches!(
            got,
            Invalidation::Cleared {
                removed: 5,
                all: true,
                ..
            }
        ),
        "{got:?}"
    );
    assert!(!exists(
        slot.path(),
        "debug/.fingerprint/serde-0123456789abcdef"
    ));
}

/// #8261 round 3: a fingerprint that cannot be removed fails the whole
/// invalidation, and the marker still names the previous checkout.
#[test]
fn a_failed_removal_is_an_error_and_keeps_the_marker() {
    use std::os::unix::fs::PermissionsExt;
    let slot = slot();
    std::fs::write(slot.path().join(LAST_CHECKOUT_MARKER), "/wt/a").expect("marker");
    let parent = slot.path().join("debug/.fingerprint");
    std::fs::create_dir(parent.join("trusty-mpm-0123456789abcdef/inner")).expect("mkdir");
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/b"), || {
        Ok(vec!["trusty-mpm".into()])
    });
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).expect("restore");
    let err = got.expect_err("a fingerprint that survives must fail the call");
    assert!(err.contains("could not clear stale fingerprint"), "{err}");
    let marker = std::fs::read_to_string(slot.path().join(LAST_CHECKOUT_MARKER)).expect("m");
    assert_eq!(marker, "/wt/a", "the marker is unchanged");
}

#[test]
#[serial_test::serial(build_slot_fds)] // #8736: probes after a release; see `slots::tests`.
fn a_held_cargo_lock_marks_the_directory_busy() {
    use std::os::fd::AsRawFd;
    let slot = slot();
    assert!(!cargo_lock_held(slot.path()), "no lock file");
    let lock = slot.path().join("debug/.cargo-lock");
    std::fs::write(&lock, "").expect("lock file");
    assert!(!cargo_lock_held(slot.path()), "an unheld lock");
    let held = std::fs::File::open(&lock).expect("open");
    // SAFETY: `held` owns a valid descriptor.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    assert!(cargo_lock_held(slot.path()), "a held lock");
    // #8850: a child another test thread spawns holds a copy of this open
    // file description until its exec, so a bare close can leave the flock
    // held. The clone stands in for that copy; `LOCK_UN` releases the lock
    // on the description itself, as `slots::unlock` does in production.
    let inherited = held.try_clone().expect("dup");
    // SAFETY: `held` owns a valid descriptor.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(held);
    assert!(!cargo_lock_held(slot.path()), "released");
    drop(inherited);
}

#[test]
#[serial_test::serial(build_slot_fds)] // #8736: spawns `cargo`; see `slots::tests`.
fn workspace_packages_lists_this_workspace() {
    let names = workspace_packages(
        &trusty_common::test_harness::test_repo_root().expect("resolve the checkout"),
    )
    .expect("metadata");
    assert!(names.iter().any(|n| n == "trusty-mpm"), "{names:?}");
    assert!(
        !names.iter().any(|n| n == "serde"),
        "--no-deps lists no registry crate"
    );
}

#[test]
fn the_checkout_root_is_found_from_a_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".git"), "gitdir: /elsewhere").expect("linked .git");
    let sub = dir.path().join("crates/x");
    std::fs::create_dir_all(&sub).expect("mkdir");
    assert_eq!(checkout_root(&sub), dir.path());
}

/// #9045: entries in the regression slot's `debug/deps` — the incident slot held
/// 458,794; this many is enough to show the guard never reads them.
const LARGE_DEPS: usize = 20_000;

/// [`RealFs`] that records every path the guard asks about.
#[derive(Default)]
struct CountingFs {
    listed: std::cell::RefCell<Vec<PathBuf>>,
    visited: std::cell::RefCell<Vec<PathBuf>>,
}

impl SlotFs for CountingFs {
    fn child_dirs(&self, dir: &Path) -> io::Result<Vec<OsString>> {
        self.listed.borrow_mut().push(dir.to_path_buf());
        self.visited.borrow_mut().push(dir.to_path_buf());
        RealFs.child_dirs(dir)
    }

    fn file_type(&self, path: &Path) -> io::Result<FileType> {
        self.visited.borrow_mut().push(path.to_path_buf());
        RealFs.file_type(path)
    }

    fn try_lock(&self, lock: &Path) -> io::Result<bool> {
        self.visited.borrow_mut().push(lock.to_path_buf());
        RealFs.try_lock(lock)
    }
}

/// [`slot`] plus a free `debug/.cargo-lock` and empty `deps`, `build` and
/// `incremental` directories, the shape of a real profile directory.
fn built_slot() -> tempfile::TempDir {
    let slot = slot();
    for dir in [
        "debug/deps",
        "debug/build/x-0123456789abcdef",
        "debug/incremental/x-1",
    ] {
        std::fs::create_dir_all(slot.path().join(dir)).expect("mkdir");
    }
    std::fs::write(slot.path().join("debug/.cargo-lock"), "").expect("lock file");
    slot
}

/// Fill `dir` with [`LARGE_DEPS`] empty files.
fn fill_deps(dir: &Path) {
    for i in 0..LARGE_DEPS {
        std::fs::write(dir.join(format!("libunit{i}-0123456789abcdef.rlib")), "").expect("file");
    }
}

/// Whether `path` lies inside a `deps`, `build` or `incremental` directory.
fn in_unbounded_dir(slot: &Path, path: &Path) -> bool {
    path.strip_prefix(slot).is_ok_and(|rel| {
        rel.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some("deps" | "build" | "incremental")
            )
        })
    })
}

/// Hold `flock(LOCK_EX)` on `lock`; the returned file keeps it.
fn hold(lock: &Path) -> std::fs::File {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(lock).expect("open");
    // SAFETY: `file` owns a valid descriptor.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
    file
}

/// Release a lock taken by [`hold`] on the description itself (#8850).
fn release(file: std::fs::File) {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` owns a valid descriptor.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) }, 0);
}

/// Set `path`'s permission bits.
fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// #9045: the guard lists only the slot root, and the paths it touches are
/// the same whether `deps` is empty or holds [`LARGE_DEPS`] files.
#[test]
#[serial_test::serial(build_slot_fds)] // #8736: probes lock files; see `slots::tests`.
fn the_lock_guard_lists_only_the_slot_root() {
    let slot = built_slot();
    let empty = CountingFs::default();
    assert!(!cargo_lock_held_in(&empty, slot.path()), "a free slot");
    fill_deps(&slot.path().join("debug/deps"));
    let large = CountingFs::default();
    assert!(!cargo_lock_held_in(&large, slot.path()), "a free slot");
    assert_eq!(*large.listed.borrow(), vec![slot.path().to_path_buf()]);
    assert_eq!(
        *large.visited.borrow(),
        *empty.visited.borrow(),
        "the guard's work does not grow with deps"
    );
    let visited = large.visited.borrow();
    let unbounded: Vec<_> = visited
        .iter()
        .filter(|p| in_unbounded_dir(slot.path(), p))
        .collect();
    assert!(unbounded.is_empty(), "{unbounded:?}");
}

/// #9045: the production guard, no stand-in. A lock under `debug/deps` is not
/// a cargo lock, and the pre-fix walk probed it; the fixed guard never looks.
#[test]
#[serial_test::serial(build_slot_fds)] // #8736: holds a flock; see `slots::tests`.
fn a_lock_inside_deps_is_never_probed() {
    let slot = built_slot();
    let deps = slot.path().join("debug/deps");
    fill_deps(&deps);
    let stray = deps.join(".cargo-lock");
    std::fs::write(&stray, "").expect("lock file");
    let held = hold(&stray);
    let busy = cargo_lock_held(slot.path());
    release(held);
    assert!(!busy, "nothing inside deps is probed");
    let held = hold(&slot.path().join("debug/.cargo-lock"));
    let busy = cargo_lock_held(slot.path());
    release(held);
    assert!(
        busy,
        "the profile lock still reads held beside a large deps"
    );
}

/// #9045: a `--target` build's `<triple>/<profile>/.cargo-lock` is probed by
/// direct path.
#[test]
#[serial_test::serial(build_slot_fds)] // #8736: holds a flock; see `slots::tests`.
fn a_held_cross_target_lock_marks_the_directory_busy() {
    let slot = built_slot();
    let lock = slot.path().join("aarch64-apple-darwin/debug/.cargo-lock");
    std::fs::write(&lock, "").expect("lock file");
    assert!(!cargo_lock_held(slot.path()), "unheld");
    let held = hold(&lock);
    let busy = cargo_lock_held(slot.path());
    release(held);
    assert!(busy, "a held triple-layout lock");
}

/// #9045: a slot never built in is free.
#[test]
fn a_missing_slot_reads_free() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(!cargo_lock_held(&dir.path().join("slot-9")));
}

/// #9045 fail-closed: a slot root that cannot be listed is busy, not free.
#[test]
#[serial_test::serial(build_slot_fds)] // #8736: probes lock files; see `slots::tests`.
fn an_unlistable_slot_counts_as_held() {
    let slot = built_slot();
    chmod(slot.path(), 0o300);
    let busy = cargo_lock_held(slot.path());
    chmod(slot.path(), 0o755);
    assert!(busy, "an unreadable slot root is busy");
}

/// #9045 fail-closed: a `.cargo-lock` that cannot be stat'ed (EACCES on its
/// profile directory) is busy, not absent.
#[test]
#[serial_test::serial(build_slot_fds)] // #8736: probes lock files; see `slots::tests`.
fn an_unstatable_cargo_lock_counts_as_held() {
    let slot = built_slot();
    let profile = slot.path().join("debug");
    chmod(&profile, 0o000);
    let busy = cargo_lock_held(slot.path());
    chmod(&profile, 0o755);
    assert!(busy, "an unstatable lock is busy");
}

/// #9045 fail-closed: a `.cargo-lock` that cannot be opened is busy.
#[test]
#[serial_test::serial(build_slot_fds)] // #8736: probes lock files; see `slots::tests`.
fn an_unopenable_cargo_lock_counts_as_held() {
    let slot = built_slot();
    let lock = slot.path().join("debug/.cargo-lock");
    chmod(&lock, 0o000);
    let busy = cargo_lock_held(slot.path());
    chmod(&lock, 0o644);
    assert!(busy, "an unopenable lock is busy");
}

/// #9045: the fingerprint search lists only the slot root and still finds the
/// host and `--target` fingerprint directories.
#[test]
fn fingerprint_search_lists_only_the_slot_root() {
    let slot = built_slot();
    fill_deps(&slot.path().join("debug/deps"));
    let fs = CountingFs::default();
    let mut dirs = fingerprint_dirs(&fs, slot.path()).expect("listed");
    dirs.sort();
    assert_eq!(
        dirs,
        vec![
            slot.path().join("aarch64-apple-darwin/debug/.fingerprint"),
            slot.path().join("debug/.fingerprint"),
        ]
    );
    assert_eq!(*fs.listed.borrow(), vec![slot.path().to_path_buf()]);
    let visited = fs.visited.borrow();
    assert!(
        !visited.iter().any(|p| in_unbounded_dir(slot.path(), p)),
        "{visited:?}"
    );
}

/// #9045 fail-closed: an error on one listing entry is an error, never
/// `NotFound`, so it cannot read as an absent (free) slot; an entry that
/// vanished is skipped.
#[test]
fn a_per_entry_listing_error_is_never_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir_type = || std::fs::metadata(tmp.path()).expect("meta").file_type();
    let denied = || io::Error::from(io::ErrorKind::PermissionDenied);
    let gone = || io::Error::from(io::ErrorKind::NotFound);
    let name = |n: &str| OsString::from(n);

    let got = dirs_in(vec![Err(gone())].into_iter()).expect_err("entry error fails");
    assert!(!is_absent(&got), "{got:?}");
    let got = dirs_in(vec![Ok((name("a"), Err(denied())))].into_iter()).expect_err("type error");
    assert!(!is_absent(&got), "{got:?}");
    let got = dirs_in(
        vec![Ok((
            name("a"),
            Err(io::Error::from(io::ErrorKind::NotADirectory)),
        ))]
        .into_iter(),
    )
    .expect_err("type error");
    assert!(!is_absent(&got), "{got:?}");
    let kept = dirs_in(
        vec![
            Ok((name("gone"), Err(gone()))),
            Ok((name("debug"), Ok(dir_type()))),
        ]
        .into_iter(),
    )
    .expect("a vanished entry is skipped");
    assert_eq!(kept, vec![name("debug")]);
}

/// #9045 fail-closed: when fingerprint enumeration fails (EACCES on a profile
/// directory), the invalidation errors and the marker is not advanced.
#[test]
fn a_fingerprint_enumeration_error_keeps_the_marker() {
    let slot = slot();
    std::fs::write(slot.path().join(LAST_CHECKOUT_MARKER), "/wt/a").expect("marker");
    let profile = slot.path().join("debug");
    chmod(&profile, 0o000);
    let got = invalidate_if_checkout_changed(slot.path(), Path::new("/wt/b"), || {
        Ok(vec!["trusty-mpm".into()])
    });
    chmod(&profile, 0o755);
    let err = got.expect_err("an unenumerable fingerprint dir must fail the call");
    assert!(err.contains("could not list fingerprints"), "{err}");
    let marker = std::fs::read_to_string(slot.path().join(LAST_CHECKOUT_MARKER)).expect("m");
    assert_eq!(marker, "/wt/a", "the marker is unchanged");
}
