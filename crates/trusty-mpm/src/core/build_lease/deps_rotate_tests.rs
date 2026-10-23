//! #10212: the rotation, its refusals, its failure arms, and the reclaim.
//!
//! Every test works in a temp dir; none touches a real cargo target.

use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;

/// A pid no process can hold: it does not fit a positive `pid_t`.
const DEAD_PID: u32 = 4_294_967_294;

/// A target whose `debug/deps` holds `n` empty files.
fn target_with(root: &Path, n: usize) -> PathBuf {
    let deps = root.join(PROFILE).join("deps");
    std::fs::create_dir_all(&deps).expect("deps");
    for i in 0..n {
        std::fs::write(deps.join(format!("f{i}")), b"").expect("file");
    }
    root.to_path_buf()
}

/// Hold `<target>/debug/.cargo-lock` as a running cargo does.
fn hold_cargo_lock(target: &Path) -> File {
    let lock = File::create(target.join(PROFILE).join(".cargo-lock")).expect("lock file");
    lock.try_lock().expect("hold cargo's lock");
    lock
}

fn deps_len(target: &Path) -> usize {
    std::fs::read_dir(target.join(PROFILE).join("deps"))
        .map(Iterator::count)
        .unwrap_or(0)
}

/// Wait, bounded, until `path` is gone.
fn gone_within(path: &Path, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

#[test]
fn a_target_over_the_line_is_rotated_aside() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = target_with(tmp.path(), 6);
    let rotation = rotate_if_over(&target, 5, true);
    let Rotation::Rotated { aside } = rotation else {
        panic!("expected a rotation, got {rotation:?}");
    };
    assert!(!target.join(PROFILE).exists(), "debug/ was moved aside");
    assert_eq!(aside.parent(), Some(target.as_path()), "renamed in place");
    let name = aside.file_name().and_then(|n| n.to_str()).unwrap_or("");
    assert!(name.contains(EVICTING_INFIX), "{name}");
    assert!(
        gone_within(&aside, Duration::from_secs(10)),
        "the aside tree is deleted in the background: {aside:?}"
    );
}

/// The criterion: a dir whose cargo lock is held is NOT rotated.
#[test]
fn a_held_cargo_lock_spares_the_rotation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = target_with(tmp.path(), 6);
    let _held = hold_cargo_lock(&target);
    let rotation = rotate_if_over(&target, 5, true);
    assert!(matches!(rotation, Rotation::Busy(_)), "{rotation:?}");
    assert_eq!(deps_len(&target), 6, "the live deps dir is untouched");
}

#[test]
fn a_dir_under_the_line_is_left_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = target_with(tmp.path(), 3);
    assert_eq!(
        rotate_if_over(&target, 5, true),
        Rotation::NotNeeded(DepsCount::Under(3))
    );
    assert_eq!(deps_len(&target), 3);
}

/// Fail-Open Check: a failed rename is a failure, never a rotation, and the
/// live dir stays where it was.
#[test]
fn a_failed_rename_is_reported_and_leaves_the_live_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = target_with(tmp.path(), 6);
    let rotation = rotate_with(&target, 5, true, |_| Err(std::io::Error::other("EXDEV")));
    assert_eq!(rotation, Rotation::Failed("EXDEV".into()));
    assert!(rotation.is_failure());
    assert_eq!(deps_len(&target), 6, "the live deps dir is untouched");
}

/// Fail-Open Check: a deps dir that cannot be counted is not rotated and not Ok.
#[test]
fn an_uncountable_deps_dir_is_not_rotated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = target_with(tmp.path(), 6);
    let deps = target.join(PROFILE).join("deps");
    std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let rotation = rotate_if_over(&target, 5, true);
    std::fs::set_permissions(&deps, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(matches!(rotation, Rotation::Unknown(_)), "{rotation:?}");
    assert!(rotation.is_failure());
    assert_eq!(deps_len(&target), 6);
}

#[test]
fn a_dry_run_moves_nothing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let target = target_with(tmp.path(), 6);
    assert_eq!(rotate_if_over(&target, 5, false), Rotation::WouldRotate);
    assert_eq!(deps_len(&target), 6);
}

#[test]
fn a_dead_deleters_aside_tree_is_reclaimed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tree = tmp
        .path()
        .join(format!(".slot-x{EVICTING_INFIX}{DEAD_PID}.1"));
    std::fs::create_dir_all(tree.join("deps")).expect("aside");
    std::fs::write(tree.join("deps/f"), b"").expect("file");
    let reclaim = reclaim_aside(tmp.path());
    assert_eq!(reclaim.removed, 1, "{reclaim:?}");
    assert!(!tree.exists());
}

#[test]
fn a_live_deleters_aside_tree_is_left_alone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mine = tmp
        .path()
        .join(format!(".slot-x{EVICTING_INFIX}{}.1", std::process::id()));
    let init = tmp.path().join(format!(".slot-x{EVICTING_INFIX}1.1"));
    for tree in [&mine, &init] {
        std::fs::create_dir_all(tree).expect("aside");
    }
    assert!(stale_aside(tmp.path()).is_empty());
    assert_eq!(reclaim_aside(tmp.path()), Reclaim::default());
    assert!(mine.exists() && init.exists());
}

/// Fail-Open Check: a delete that fails is reported, and the tree is left for
/// the next pass.
#[test]
fn a_failed_reclaim_is_reported_and_left() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tree = tmp
        .path()
        .join(format!(".slot-x{EVICTING_INFIX}{DEAD_PID}.1"));
    std::fs::create_dir_all(&tree).expect("aside");
    let reclaim = reclaim_with(tmp.path(), |_| Err(std::io::Error::other("EBUSY")));
    assert_eq!(reclaim.removed, 0);
    assert_eq!(reclaim.failed, vec![(tree.clone(), "EBUSY".to_string())]);
    assert!(tree.exists());
}

/// A shared root and a pool root: one shared target with `shared_n` deps
/// entries and one slot with `slot_n`.
fn roots(shared_n: usize, slot_n: usize) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let shared = tmp.path().join("cargo-target");
    let pool = tmp.path().join("pool");
    target_with(&shared.join("o/r"), shared_n);
    target_with(&pool.join("o/r/slot-0"), slot_n);
    (tmp, shared, pool)
}

#[test]
fn the_repair_rotates_an_over_line_target() {
    let (_tmp, shared, pool) = roots(6, 2);
    let rows = rotate_all(&shared, &pool, 5, true);
    let shared_target = shared.join("o/r");
    let slot = pool.join("o/r/slot-0");
    assert!(
        rows.iter()
            .any(|(t, r)| *t == shared_target && matches!(r, Rotation::Rotated { .. })),
        "{rows:?}"
    );
    assert!(
        rows.contains(&(slot.clone(), Rotation::NotNeeded(DepsCount::Under(2)))),
        "{rows:?}"
    );
    assert!(!shared_target.join(PROFILE).exists());
    assert_eq!(deps_len(&slot), 2);
}

#[test]
fn the_repair_refuses_a_target_whose_cargo_lock_is_held() {
    let (_tmp, shared, pool) = roots(6, 0);
    let shared_target = shared.join("o/r");
    let _held = hold_cargo_lock(&shared_target);
    let rows = rotate_all(&shared, &pool, 5, true);
    assert!(
        rows.iter()
            .any(|(t, r)| *t == shared_target && matches!(r, Rotation::Busy(_))),
        "{rows:?}"
    );
    assert_eq!(deps_len(&shared_target), 6);
}

/// Fail-Open Check: the repair reports an aside tree it could not delete.
#[test]
fn the_repair_reports_an_aside_tree_it_cannot_delete() {
    let (_tmp, shared, pool) = roots(0, 0);
    let slot = pool.join("o/r/slot-0");
    let tree = slot.join(format!(".slot-x{EVICTING_INFIX}{DEAD_PID}.1"));
    std::fs::create_dir_all(tree.join("deps")).expect("aside");
    std::fs::write(tree.join("deps/f"), b"").expect("file");
    let locked = tree.join("deps");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).expect("chmod");
    let rows = rotate_all(&shared, &pool, 5, true);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        rows.iter().any(
            |(_, r)| matches!(r, Rotation::DeleteFailed { tree: t, .. } if *t == tree)
                && r.is_failure()
        ),
        "{rows:?}"
    );
    assert!(tree.exists(), "left for the next pass");
}

#[test]
fn the_lease_is_quiet_under_the_warn_line() {
    let target = Path::new("/t");
    assert_eq!(
        lease_notice(target, &Rotation::NotNeeded(DepsCount::Under(3))),
        None
    );
    assert_eq!(
        lease_notice(target, &Rotation::NotNeeded(DepsCount::Missing)),
        None
    );
    let warned = lease_notice(
        target,
        &Rotation::NotNeeded(DepsCount::Under(DEPS_WARN_ENTRIES)),
    );
    assert!(warned.is_some_and(|l| l.contains("warn line")));
    let failed = lease_notice(target, &Rotation::Failed("EXDEV".into()));
    assert!(failed.is_some_and(|l| l.contains("FAILED")));
}
