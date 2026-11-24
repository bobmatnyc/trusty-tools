//! Write an executable stub that the test will then exec, with no `ETXTBSY` window.
//!
//! Why: `execve` fails with "Text file busy" while any process holds a write fd
//! to the file. `fs::write` opens that fd in THIS process, and a sibling test
//! thread that forks in that window hands its child a copy (#10248). This
//! crate is a leaf with a fixed dependency list (#9171), so it cannot depend on
//! `trusty_common::exec_stub::write_executable`; this is the same design.
//! What: stages the bytes in a sibling file, has a `/bin/cp` child copy them
//! onto `path`, then `chmod`s it 0755. The only write fd to `path` lives in the
//! `cp` child, which has exited before this returns.
//! Test: `tmux_failure_is_a_fault_then_clears`,
//! `system_runner_reports_exit_codes_and_timeouts`,
//! `live_wake_types_then_enters_and_refuses_a_draft`.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// Write `contents` to `path` as a mode-0755 executable. `path`'s directory
/// must exist.
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable path has no file name",
        )
    })?;
    let mut staged_name = std::ffi::OsString::from(".");
    staged_name.push(name);
    staged_name.push(format!(
        ".{}-{}.stage",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let staged = path.with_file_name(staged_name);
    let result = install(&staged, path, contents.as_ref());
    let _ = std::fs::remove_file(&staged);
    result
}

fn install(staged: &Path, path: &Path, contents: &[u8]) -> io::Result<()> {
    // The staging file is never exec'd, so a forked copy of its fd is harmless.
    std::fs::write(staged, contents)?;
    let status = Command::new("/bin/cp").arg(staged).arg(path).status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "/bin/cp could not install {}: {status}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}
