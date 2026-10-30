//! One copy of this test binary per judged name per test process (#10212).
//!
//! Why: a live test runs a copy of its own binary under a name the server
//! judges (`claude`, `notclaude`, `trusty-console`). On macOS every new
//! executable file costs a syspolicyd assessment on its first launch, and a
//! fresh copy per test is one more assessment per test.
//! What: [`self_copy`] returns `<tmp>/<package>-copies-<pid>/<name>`, copied
//! on the first call for that name and reused by every later call in the
//! process. Each name is a real copy with its own inode, never a hard link:
//! macOS `proc_pidpath` reports one path for two links of one inode, and the
//! server judges a peer by that path. An `atexit` hook removes the directory
//! when the process exits; a run killed before `exit` leaves its directory,
//! and the next process's first call removes every sibling directory (a real
//! directory this euid owns, not a symlink) named for a dead canonical pid.
//! Test: `one_name_is_one_copy_per_process`, `two_names_are_two_inodes`,
//! `racing_first_calls_make_one_complete_copy`,
//! `copies_are_removed_at_process_exit`, `a_dead_runs_copies_are_swept`,
//! `live_signed_padded_and_symlinked_entries_survive_the_sweep`.

use std::collections::HashMap;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// The process's copy directory and the copies made in it so far.
static COPIES: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// The directory prefix; the pid follows it.
fn prefix() -> String {
    format!("{}-copies-", env!("CARGO_PKG_NAME"))
}

fn base() -> &'static Path {
    Path::new(if cfg!(target_os = "macos") {
        "/private/tmp"
    } else {
        "/tmp"
    })
}

/// `atexit` hook: remove this process's copy directory.
extern "C" fn remove_copies() {
    if let Some(dir) = DIR.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Whether `pid` names a live process (`EPERM` counts as alive).
fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the pid exists; nothing is sent.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// A pid written the way [`copy_dir`] writes one: positive decimal digits,
/// no sign, no leading zero. `kill` reads 0 and negatives as process groups.
fn canonical_pid(text: &str) -> Option<libc::pid_t> {
    let digits = !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits || text.starts_with('0') {
        return None;
    }
    text.parse().ok()
}

/// Remove the copy directories of dead runs of this package: real
/// directories this euid owns, named for a canonical pid that is not alive.
fn sweep_dead_runs() {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(entries) = std::fs::read_dir(base()) else {
        return;
    };
    let prefix = prefix();
    // SAFETY: `geteuid` takes no arguments and cannot fail.
    let euid = unsafe { libc::geteuid() };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix(&prefix))
            .and_then(canonical_pid)
        else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.file_type().is_dir() || meta.uid() != euid {
            continue;
        }
        if pid != std::process::id() as libc::pid_t && !alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// This process's copy directory, made (0700) on first use.
fn copy_dir() -> &'static Path {
    DIR.get_or_init(|| {
        sweep_dead_runs();
        let dir = base().join(format!("{}{}", prefix(), std::process::id()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .expect("create the copy dir");
        // SAFETY: registers a plain `extern "C" fn` with no captured state.
        let rc = unsafe { libc::atexit(remove_copies) };
        assert_eq!(rc, 0, "atexit");
        dir
    })
}

/// The copy of this test binary named `name`, made once per process.
///
/// The copy is written under a staging name and renamed into place while the
/// lock is held, so a caller only ever gets a complete executable.
pub fn self_copy(name: &str) -> PathBuf {
    let copies = COPIES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut copies = copies.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = copies.get(name) {
        return path.clone();
    }
    let dir = copy_dir();
    let path = dir.join(name);
    let staging = dir.join(format!(".{name}.tmp"));
    let exe = std::env::current_exe().expect("current exe");
    std::fs::copy(&exe, &staging).expect("copy test binary");
    std::fs::rename(&staging, &path).expect("publish the copy");
    copies.insert(name.to_owned(), path.clone());
    path
}

mod tests {
    use super::self_copy;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    fn inode(path: &std::path::Path) -> u64 {
        std::fs::metadata(path).expect("stat copy").ino()
    }

    #[test]
    fn one_name_is_one_copy_per_process() {
        // Stat between the calls: a re-copy renamed over the path gets a new inode.
        let first = self_copy("claude");
        let first_inode = inode(&first);
        let second = self_copy("claude");
        assert_eq!(first, second);
        assert_eq!(first_inode, inode(&second), "a second copy was made");
    }

    #[test]
    fn two_names_are_two_inodes() {
        let claude = self_copy("claude");
        let control = self_copy("notclaude");
        assert_ne!(claude, control);
        assert_ne!(inode(&claude), inode(&control), "one inode for two names");
    }

    #[test]
    fn racing_first_calls_make_one_complete_copy() {
        let len = std::fs::metadata(std::env::current_exe().unwrap())
            .unwrap()
            .len();
        let barrier = std::sync::Barrier::new(8);
        let got: Vec<(std::path::PathBuf, u64)> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        barrier.wait();
                        let path = self_copy("racer");
                        let meta = std::fs::metadata(&path).unwrap();
                        assert_eq!(meta.len(), len, "an incomplete copy");
                        assert_ne!(meta.permissions().mode() & 0o111, 0, "not executable");
                        (path, meta.ino())
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(got.windows(2).all(|w| w[0] == w[1]), "{got:?}");
        let dir = got[0].0.parent().unwrap();
        let racers = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("racer"))
            .count();
        assert_eq!(racers, 1, "one file for the name, no staging left");
    }

    /// Set in the child run of [`copies_are_removed_at_process_exit`].
    const PROBE: &str = "SELF_COPY_PROBE_OUT";

    #[test]
    fn copies_are_removed_at_process_exit() {
        if let Some(out) = std::env::var_os(PROBE) {
            let copy = self_copy("probe");
            std::fs::write(out, copy.parent().unwrap().as_os_str().as_encoded_bytes()).unwrap();
            return;
        }
        let out = tempfile::NamedTempFile::new().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "self_copy::tests::copies_are_removed_at_process_exit",
                "--exact",
            ])
            .env(PROBE, out.path())
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "probe run");
        let dir = std::path::PathBuf::from(std::fs::read_to_string(out.path()).unwrap());
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&super::prefix()), "{dir:?}");
        assert!(!dir.exists(), "{dir:?} outlived its process");
    }

    #[test]
    fn a_dead_runs_copies_are_swept() {
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        let stale = super::base().join(format!("{}{pid}", super::prefix()));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("claude"), b"leftover").unwrap();
        super::sweep_dead_runs();
        assert!(!stale.exists(), "{stale:?} survived the sweep");
    }

    #[test]
    fn live_signed_padded_and_symlinked_entries_survive_the_sweep() {
        let mut live = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let dead = gone.id();
        gone.wait().unwrap();
        let (base, prefix) = (super::base(), super::prefix());
        let dirs = [
            base.join(format!("{prefix}{}", live.id())),
            base.join(format!("{prefix}+{dead}")),
            base.join(format!("{prefix}0{dead}")),
        ];
        for dir in &dirs {
            std::fs::create_dir_all(dir).unwrap();
        }
        let target = tempfile::tempdir().unwrap();
        let link = base.join(format!("{prefix}{dead}"));
        std::os::unix::fs::symlink(target.path(), &link).unwrap();
        super::sweep_dead_runs();
        let mut survived: Vec<_> = dirs.iter().map(|d| (d.clone(), d.exists())).collect();
        survived.push((link.clone(), link.symlink_metadata().is_ok()));
        for dir in &dirs {
            let _ = std::fs::remove_dir_all(dir);
        }
        let _ = std::fs::remove_file(&link);
        let _ = live.kill();
        let _ = live.wait();
        let removed: Vec<_> = survived.iter().filter(|(_, kept)| !kept).collect();
        assert!(removed.is_empty(), "swept: {removed:?}");
    }
}
