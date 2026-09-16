//! Unit tests for the pinned write handle (#7779).
//!
//! Why: the handle exists to make one property true — a component swapped after
//! validation cannot redirect the write — and that property is only believable
//! against a real filesystem with a real swap performed between the two steps.
//! What: the happy path (so the fix is not proven by refusing everything), the
//! post-validation swap, a symlink present at open time, the nested-child
//! descent, `create_new`'s refusal, and the ledger lock.
//! Test: this file IS the test module.

use super::*;

use std::os::unix::fs::symlink;

/// A project root with an empty `.trusty-code/` already in place.
fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(native_config_dir(tmp.path())).expect("mkdir .trusty-code");
    tmp
}

/// Replace `dir` with a symlink at `victim`, the way a racing writer would.
fn swap_for_symlink(dir: &Path, victim: &Path) {
    std::fs::remove_dir_all(dir).expect("remove the real directory");
    symlink(victim, dir).expect("symlink the victim in its place");
}

/// A pinned handle writes, and reads back, at the path it validated.
///
/// Why: the guard must still let the ordinary deploy through; a fix that
/// refuses everything would pass the swap test below and break the product.
/// What: opens `.trusty-code/agents`, writes a file, and asserts both the
/// on-disk path and the handle's own read return the bytes.
/// Test: this function IS the test.
#[test]
fn pinned_write_lands_at_the_validated_path() {
    let tmp = project();
    let dir = NativeWriteDir::open(tmp.path(), Path::new("agents")).expect("pin the agents dir");

    dir.atomic_write("pm.md", b"hello").expect("write");

    assert_eq!(dir.path(), native_config_dir(tmp.path()).join("agents"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("pm.md")).expect("read back"),
        "hello"
    );
    assert_eq!(
        dir.read("pm.md").expect("read").as_deref(),
        Some(&b"hello"[..])
    );
    // Rewriting is atomic replacement, not an append or a refusal.
    dir.atomic_write("pm.md", b"second").expect("rewrite");
    assert_eq!(
        dir.read("pm.md").expect("read").as_deref(),
        Some(&b"second"[..])
    );
}

/// A component swapped for a symlink AFTER validation never reaches the victim.
///
/// Why: the #7779 race itself. On the pre-fix code the sequence below —
/// validate, swap, write — put the file in the victim directory; a 4 ms swap
/// loop reproduced it in 6 of 40 real `tcode` deploys.
/// What: pins `.trusty-code/skill-refs`, then replaces that name with a symlink
/// to a victim directory, then writes. Asserts the victim stays empty AND that
/// the write reports [`WriteTargetError::Unpinned`], so a swap is loud rather
/// than a deploy into an inode nothing will read.
/// Test: this function IS the test.
#[test]
fn component_swapped_after_open_never_reaches_victim() {
    let tmp = project();
    let victim = tempfile::tempdir().expect("victim tempdir");
    let dir =
        NativeWriteDir::open(tmp.path(), Path::new("skill-refs")).expect("pin the skill-refs dir");

    swap_for_symlink(dir.path(), victim.path());

    let err = dir
        .atomic_write("SKILL.md", b"secret")
        .expect_err("a swapped component must be reported");

    assert!(
        matches!(err, WriteTargetError::Unpinned { .. }),
        "expected Unpinned, got {err:?}"
    );
    let leaked: Vec<_> = std::fs::read_dir(victim.path())
        .expect("read the victim")
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert!(
        leaked.is_empty(),
        "the victim directory must receive nothing, found {leaked:?}"
    );
}

/// A symlinked component present at open time is refused before any write.
///
/// Why: the committed-symlink case #7727 already covered, now enforced by the
/// same `O_NOFOLLOW` descent rather than by a separate resolve-and-compare.
/// What: symlinks `.trusty-code/skill-refs` at a sibling directory inside the
/// project (so the ADR-0044 containment check passes and the descent is what
/// refuses it) and asserts `open` fails.
/// Test: this function IS the test.
#[test]
fn symlinked_component_is_refused_at_open() {
    let tmp = project();
    let inside = native_config_dir(tmp.path()).join("elsewhere");
    std::fs::create_dir_all(&inside).expect("mkdir");
    symlink(&inside, native_config_dir(tmp.path()).join("skill-refs")).expect("symlink");

    let err = NativeWriteDir::open(tmp.path(), Path::new("skill-refs"))
        .expect_err("a symlinked component must be refused");

    assert!(
        matches!(err, WriteTargetError::Unpinned { .. }),
        "expected Unpinned, got {err:?}"
    );
}

/// A `.trusty-code` that is itself a symlink out of the project is still refused.
///
/// Why: the PR #6980 code-critic BLOCK must survive the move to a pinned handle
/// — the anchoring check runs first, so the descent never even starts.
/// What: symlinks `<project>/.trusty-code` at an outside directory and asserts
/// `open` returns the original `SymlinkEscape`.
/// Test: this function IS the test.
#[test]
fn symlinked_native_root_escape_is_refused_at_open() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    symlink(outside.path(), tmp.path().join(TRUSTY_CODE_DIRNAME)).expect("symlink");

    let err = NativeWriteDir::open(tmp.path(), Path::new("agents"))
        .expect_err("an escaping config root must be refused");

    assert!(
        matches!(err, WriteTargetError::SymlinkEscape { .. }),
        "expected SymlinkEscape, got {err:?}"
    );
}

/// A nested child directory is created and pinned like its parent.
///
/// Why: skill-ref entries are `<skill>/SKILL.md`, so the descent has to work one
/// level down without re-walking the path from the project root.
/// What: opens `skill-refs`, takes a `child`, writes through it, and asserts a
/// symlink swapped in at the child is refused too.
/// Test: this function IS the test.
#[test]
fn child_dir_is_pinned_like_its_parent() {
    let tmp = project();
    let victim = tempfile::tempdir().expect("victim tempdir");
    let refs = NativeWriteDir::open(tmp.path(), Path::new("skill-refs")).expect("pin");
    let child = refs
        .child(Path::new("git-workflow"))
        .expect("pin the child");

    child.atomic_write("SKILL.md", b"body").expect("write");
    assert_eq!(
        std::fs::read_to_string(child.path().join("SKILL.md")).expect("read back"),
        "body"
    );

    swap_for_symlink(child.path(), victim.path());
    let err = refs
        .child(Path::new("git-workflow"))
        .expect_err("a swapped child must be refused");
    assert!(
        matches!(err, WriteTargetError::Unpinned { .. }),
        "expected Unpinned, got {err:?}"
    );
}

/// `create_new` refuses to replace a file that already exists.
///
/// Why: the legacy import's "never overwrite a user-authored file" rule, decided
/// by the kernel at the moment of the write rather than by an `exists()` test
/// the apply step could be arbitrarily long behind.
/// What: writes once, then asserts the second attempt errors and the original
/// bytes survive.
/// Test: this function IS the test.
#[test]
fn create_new_refuses_an_existing_file() {
    let tmp = project();
    let dir = NativeWriteDir::open(tmp.path(), Path::new("agents")).expect("pin");

    dir.create_new("pm.md", b"first").expect("first write");
    let err = dir
        .create_new("pm.md", b"second")
        .expect_err("an existing target must be refused");

    assert!(
        matches!(err, WriteTargetError::Unpinnable { .. }),
        "expected Unpinnable, got {err:?}"
    );
    assert_eq!(
        dir.read("pm.md").expect("read").as_deref(),
        Some(&b"first"[..])
    );
}

/// The ledger lock is exclusive between two handles on the same directory.
///
/// Why: deploying through a scratch directory would otherwise move the shared
/// deployer's cross-process lock off the project directory, silently dropping
/// the serialisation two concurrent `tcode` daemons rely on.
/// What: takes the lock, then asserts a second thread cannot acquire it until
/// the first handle is dropped.
/// Test: this function IS the test.
#[test]
fn ledger_lock_is_exclusive_across_handles() {
    let tmp = project();
    let root = tmp.path().to_path_buf();
    let dir = NativeWriteDir::open(&root, Path::new("agents")).expect("pin");
    let held = dir.lock_exclusive(".ledger.lock").expect("take the lock");

    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let other = NativeWriteDir::open(&root, Path::new("agents")).expect("pin");
        let lock = other.lock_exclusive(".ledger.lock").expect("take the lock");
        tx.send(()).expect("report acquisition");
        drop(lock);
    });

    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "a second handle must block while the lock is held"
    );
    drop(held);
    rx.recv_timeout(std::time::Duration::from_secs(5))
        .expect("the lock must be acquired once released");
    worker.join().expect("worker thread");
}
