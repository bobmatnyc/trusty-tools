//! #10746 S9: the S8 fetch check/read race (`DocTreeSource::fetch`).
//!
//! Why: the S8 fetch checked a file's type and size by path and then read
//! it by path again, so a racing writer could swap a link, a FIFO or a
//! larger file in between. The fix opens once and checks and reads the one
//! handle.
//! What: each swap arm installs a read-seam hook that replaces the file
//! after the path is resolved and before its bytes are read. The fetch must
//! return the bytes of the file it checked, never the swapped-in content,
//! and must never block. The plain arms check that a link or a FIFO already
//! at the path is refused without blocking.
//! Test: `cargo test -p trusty-okf --lib import::source::tests`.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::bundle::ops::fail;

/// How long a fetch may take before the test calls it blocked.
const BLOCKED: Duration = Duration::from_secs(5);

struct Tree {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

/// A source root holding `a.txt` ("original"), and a file outside it.
fn tree() -> Tree {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("src");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), "original").unwrap();
    let outside = base.join("secret.txt");
    std::fs::write(&outside, "SECRET").unwrap();
    Tree {
        _tmp: tmp,
        root,
        outside,
    }
}

/// Replace `path` atomically with what `make` creates at a sibling name
/// (a rename, as a racing writer would; no file is deleted).
fn swap_in(path: &Path, make: impl FnOnce(&Path)) {
    let tmp = path.with_extension("swap");
    make(&tmp);
    std::fs::rename(&tmp, path).unwrap();
}

fn mkfifo(path: &Path) {
    let st = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success(), "mkfifo {path:?}");
}

/// Unblock a reader stuck on the FIFO at `path`: a non-blocking write open
/// succeeds while a reader waits, and closing it gives the reader EOF.
fn release_fifo(path: &Path) {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path);
}

/// Fetch `a.txt` on a worker thread, with `swap` installed at the read
/// seam. Panics when the fetch blocks.
fn fetch_with_swap(
    t: &Tree,
    swap: impl FnMut(&Path) + Send + 'static,
) -> Result<Vec<u8>, ExtractError> {
    let root = t.root.clone();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        fail::set_before_read(swap);
        let mut src = DocTreeSource { root };
        let got = src.fetch("a.txt");
        fail::clear();
        let _ = tx.send(got);
    });
    match rx.recv_timeout(BLOCKED) {
        Ok(got) => got,
        Err(_) => {
            release_fifo(&t.root.join("a.txt"));
            panic!("fetch blocked on the swapped-in file");
        }
    }
}

/// Why: a symbolic link swapped in between the check and the read must
/// never be followed out of the root.
/// What: the hook replaces `a.txt` with a link to a file outside the root;
/// the fetch returns the checked file's bytes, not the link target's.
/// Test: self-contained.
#[test]
fn fetch_never_reads_a_symlink_swapped_in_before_the_read() {
    let t = tree();
    let outside = t.outside.clone();
    let got = fetch_with_swap(&t, move |p| {
        swap_in(p, |tmp| std::os::unix::fs::symlink(&outside, tmp).unwrap());
    });
    let got = got.expect("the checked file is read");
    assert_ne!(got, b"SECRET", "followed a swapped-in link");
    assert_eq!(got, b"original");
}

/// Why: a FIFO swapped in after the check would block the read under the
/// bundle lock.
/// What: the hook replaces `a.txt` with a FIFO no one writes; the fetch
/// returns within the bound with the checked file's bytes.
/// Test: self-contained.
#[test]
fn fetch_never_blocks_on_a_fifo_swapped_in_before_the_read() {
    let t = tree();
    let got = fetch_with_swap(&t, |p| swap_in(p, mkfifo));
    assert_eq!(got.expect("the checked file is read"), b"original");
}

/// Why: the size cap must bind the bytes actually read, not a file the
/// path pointed at earlier.
/// What: the hook renames a file over the cap onto `a.txt`; the fetch reads
/// the checked inode, so no more than the cap comes back.
/// Test: self-contained.
#[test]
fn fetch_size_check_and_read_act_on_one_inode() {
    let t = tree();
    let big = t.root.join("big.tmp");
    let size = usize::try_from(MAX_ITEM_BYTES).unwrap() + 1024;
    std::fs::write(&big, vec![b'x'; size]).unwrap();
    let got = fetch_with_swap(&t, move |p| std::fs::rename(&big, p).unwrap());
    match got {
        Ok(bytes) => {
            assert!(bytes.len() as u64 <= MAX_ITEM_BYTES, "read past the cap");
            assert_eq!(bytes, b"original");
        }
        Err(e) => assert_eq!(e.code, "too_large"),
    }
}

/// Why: a link or a FIFO already at the path is refused, and opening the
/// FIFO does not block.
/// What: `a.txt` is a link to an outside file, then a FIFO; each fetch
/// fails with `symlink` or `unsupported_format` within the bound.
/// Test: self-contained.
#[test]
fn fetch_refuses_a_link_or_fifo_at_the_path_without_blocking() {
    let t = tree();
    let a = t.root.join("a.txt");
    swap_in(&a, |tmp| {
        std::os::unix::fs::symlink(&t.outside, tmp).unwrap()
    });
    let got = fetch_with_swap(&t, |_| {});
    assert_eq!(got.unwrap_err().code, "symlink");
    swap_in(&a, mkfifo);
    let got = fetch_with_swap(&t, |_| {});
    assert_eq!(got.unwrap_err().code, "unsupported_format");
}
