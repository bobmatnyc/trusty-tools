//! Unit tests for the pinned write handle (#7779).
//!
//! Why: the handle exists to make one property true — a component swapped after
//! validation cannot redirect the write — and that property is only believable
//! against a real filesystem with a real swap performed between the two steps.
//! What: the happy path (so the fix is not proven by refusing everything), the
//! post-validation swap, the config-root swap raced against the pin itself, a
//! symlink present at open time, the nested-child descent, `create_new`'s
//! refusal, the single-component name rule, and the ledger lock.
//! Test: this file IS the test module.

use super::*;

use std::os::unix::fs::symlink;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Every file anywhere beneath `root` — "the victim received nothing" has to
/// mean the whole tree, not just its top level.
fn descendants(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
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

/// A `.trusty-code` swapped between the membership check and the pin never
/// becomes the pinned directory.
///
/// Why: #7779 round 2, and a different window from the test above. The
/// membership rule resolved `.trusty-code` and `pin_native_root` resolved it
/// AGAIN, with the descriptor coming from the second resolution — so a rename
/// landing between the two pinned a directory nothing had approved. A
/// code-critic race model of that exact call sequence put 1419 of 20138
/// completed writes in the victim, 152 of them with `verify_pinned` still
/// reporting healthy.
/// What: a swapper thread parks the real `.trusty-code` and links a victim in
/// its place, over and over, while this thread opens the agents directory and
/// writes through it. Asserts the victim tree receives nothing, and that at
/// least one write DID complete — a run that refused everything must not pass.
/// Test: this function IS the test.
#[test]
fn native_root_swapped_between_check_and_pin_never_reaches_victim() {
    let tmp = project();
    let victim = tempfile::tempdir().expect("victim tempdir");
    let link_at = native_config_dir(tmp.path());
    let parked = tmp.path().join(".trusty-code-parked");

    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let stop = Arc::clone(&stop);
        let victim_path = victim.path().to_path_buf();
        let (link_at, parked) = (link_at.clone(), parked.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = std::fs::rename(&link_at, &parked);
                let _ = symlink(&victim_path, &link_at);
                std::thread::sleep(std::time::Duration::from_micros(50));
                if std::fs::remove_file(&link_at).is_err() {
                    // The writer created a fresh real `.trusty-code` while the
                    // original was parked — legal, and not a symlink, so clear
                    // it the other way round and keep the loop swapping.
                    let _ = std::fs::remove_dir_all(&link_at);
                }
                let _ = std::fs::rename(&parked, &link_at);
            }
            let _ = std::fs::remove_file(&link_at);
            let _ = std::fs::rename(&parked, &link_at);
        })
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (mut attempts, mut written) = (0u32, 0u32);
    while std::time::Instant::now() < deadline && (attempts < 3000 || written == 0) {
        attempts += 1;
        if let Ok(dir) = NativeWriteDir::open(tmp.path(), Path::new("agents"))
            && dir
                .atomic_write(&format!("pm-{attempts}.md"), b"AGENT BODY")
                .is_ok()
        {
            written += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    swapper.join().expect("swapper thread");

    assert!(
        written > 0,
        "the harness proves nothing if every attempt was refused ({attempts} attempts)"
    );
    let leaked = descendants(victim.path());
    assert!(
        leaked.is_empty(),
        "the victim tree must receive nothing across {attempts} attempts, found {leaked:?}"
    );
}

/// A file name carrying a path separator is refused by every entry point.
///
/// Why: `O_NOFOLLOW` guards only an `openat` name's LAST component, so a name
/// like `link/escaped.md` would have `link` resolved with symlinks followed —
/// straight back out of the pinned directory. No caller passes one today, and
/// [`NativeWriteDir`] is `pub`, so the type refuses it rather than trusting that
/// (#7779 round 2).
/// What: plants a symlink to a victim directory INSIDE the pinned directory,
/// then asserts every name-taking method refuses a separator, `..`, `.` and the
/// empty name, and that the victim stays empty.
/// Test: this function IS the test.
#[test]
fn a_name_with_a_path_separator_is_refused() {
    let tmp = project();
    let victim = tempfile::tempdir().expect("victim tempdir");
    let dir = NativeWriteDir::open(tmp.path(), Path::new("agents")).expect("pin");
    symlink(victim.path(), dir.path().join("link")).expect("symlink inside the pinned dir");

    for name in [
        "link/escaped.md",
        "../escaped.md",
        "sub/escaped.md",
        "",
        ".",
        "..",
    ] {
        assert!(
            matches!(
                dir.atomic_write(name, b"secret"),
                Err(WriteTargetError::Unpinnable { .. })
            ),
            "atomic_write must refuse `{name}`"
        );
        assert!(
            matches!(
                dir.create_new(name, b"secret"),
                Err(WriteTargetError::Unpinnable { .. })
            ),
            "create_new must refuse `{name}`"
        );
        assert!(
            matches!(dir.read(name), Err(WriteTargetError::Unpinnable { .. })),
            "read must refuse `{name}`"
        );
        assert!(
            matches!(
                dir.lock_exclusive(name),
                Err(WriteTargetError::Unpinnable { .. })
            ),
            "lock_exclusive must refuse `{name}`"
        );
    }

    let leaked = descendants(victim.path());
    assert!(
        leaked.is_empty(),
        "nothing may reach a symlink's target, found {leaked:?}"
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
