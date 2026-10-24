//! Write an executable that this process will exec, with no `ETXTBSY` window.
//!
//! See #10248. Every test in the workspace that writes a stub script or copies
//! a binary and then execs it goes through [`write_executable`].

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Mode every stub is left with: owner rwx, group and other r-x.
pub const STUB_MODE: u32 = 0o755;

/// Write `contents` to `path` as a mode-`0755` executable, without this
/// process ever holding a write fd to `path`.
///
/// Why: `execve` fails with `ETXTBSY` ("Text file busy", os error 26) while any
/// process holds a write fd to the file. `std::fs::write` opens that fd in THIS
/// process. A sibling test thread that forks or spawns in that window gives its
/// child a copy of the fd, and the copy lives until the child execs. The later
/// exec of the stub then fails, and CI goes red on an unrelated PR (#9718,
/// #10176, #10232, #10248). Measured on Linux (#10248): with 8 threads spawning
/// `/bin/true`, 21351 of 147994 execs of in-process-written stubs failed with
/// `ETXTBSY`; 0 of 59728 failed when a child process wrote the file.
///
/// What: writes `contents` to a sibling staging file, has a `/bin/cp` child copy
/// it onto `path`, sets mode [`STUB_MODE`] with `chmod(2)` (which opens no fd),
/// then removes the staging file. The only write fd to `path` ever lives in the
/// `cp` child, which exits before this returns, so no fork of this process can
/// inherit one. The race is removed, not retried. `path` may already exist; it
/// is overwritten. Its parent directory must exist.
///
/// Test: `exec_stub::tests::a_stub_runs_and_is_mode_0755`,
/// `exec_stub::tests::rewriting_a_stub_replaces_it_and_leaves_no_staging_file`,
/// `exec_stub::tests::a_missing_parent_is_an_error`,
/// `exec_stub::tests::stubs_exec_while_siblings_spawn_without_etxtbsy`.
///
/// # Errors
/// Any I/O error from staging the bytes, spawning `/bin/cp`, or the chmod; a
/// non-zero `cp` exit is reported as an [`io::ErrorKind::Other`] error naming
/// `path`. The staging file is removed on every path out.
///
/// # Preconditions
/// - `path` names a file (it has a file name) in an existing directory.
///
/// # Postconditions
/// - On `Ok`, `path` holds exactly `contents` and has mode [`STUB_MODE`].
/// - No staging file is left in `path`'s directory.
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let staged = staging_path(path)?;
    let result = install(&staged, path, contents.as_ref());
    // Best effort: a failed stage may have left nothing to remove.
    let _ = std::fs::remove_file(&staged);
    result
}

/// Stage, copy through a child, then chmod. Split out so the caller can always
/// remove the staging file.
fn install(staged: &Path, path: &Path, contents: &[u8]) -> io::Result<()> {
    // The staging file is opened for writing here, and a sibling fork may
    // inherit that fd. It is never exec'd, so that cannot make anything busy.
    std::fs::write(staged, contents)?;
    let status = Command::new("/bin/cp").arg(staged).arg(path).status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "/bin/cp could not install the executable {}: {status}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(STUB_MODE))
}

/// A sibling of `path` unique to this process and call.
fn staging_path(path: &Path) -> io::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("executable path has no file name: {}", path.display()),
        )
    })?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut staged_name = std::ffi::OsString::from(".");
    staged_name.push(name);
    staged_name.push(format!(".{}-{n}.stage", std::process::id()));
    Ok(path.with_file_name(staged_name))
}

#[cfg(test)]
#[path = "exec_stub_tests.rs"]
mod tests;
