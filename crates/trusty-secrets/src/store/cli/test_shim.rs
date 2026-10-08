//! Install helpers for the fake CLIs the 1Password and Keeper tests run
//! (#7519 P3, moved from the 1Password shim).
//!
//! What: [`install_script`] writes an executable `/bin/sh` script under a
//! given name and waits out `ETXTBSY`; [`relative_to_cwd`] turns an
//! absolute directory into a path relative to the working directory, for a
//! test that proves a relative program is never run.
//! Test: the tests that build a shim.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The argument an [`install_script`] script exits 0 on before its body runs.
const PROBE: &str = "__probe7519__";

/// Write `dir/name`, a 0755 `/bin/sh` script running `body`, and return its
/// path.
///
/// Why: a thread that forks while the file is open for writing keeps that
/// descriptor until it execs, and an `exec` of the file meanwhile fails with
/// `ETXTBSY`.
/// What: after the write, runs the script with [`PROBE`] until `exec` no
/// longer fails busy. The descriptor is closed by then, so no later fork
/// can inherit it, and no later spawn of the script fails busy.
pub(crate) fn install_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let script = format!("#!/bin/sh\n[ \"$1\" = {PROBE} ] && exit 0\n{body}\n");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    for _ in 0..500 {
        match Command::new(&path).arg(PROBE).status() {
            Ok(status) => {
                assert!(status.success(), "{}: {status}", path.display());
                return path;
            }
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{}: {e}", path.display()),
        }
    }
    panic!("{} stayed busy", path.display());
}

/// `dir`, absolute, as a path relative to this process's working directory.
///
/// What: one `..` per component of the canonical working directory, then
/// `dir` without its leading `/`. Tests read the working directory and
/// never change it. Asserts that the result reaches `dir`.
pub(crate) fn relative_to_cwd(dir: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap().canonicalize().unwrap();
    let mut relative = PathBuf::new();
    for _ in cwd.components().skip(1) {
        relative.push("..");
    }
    relative.push(dir.strip_prefix("/").unwrap());
    assert!(
        relative.is_relative() && cwd.join(&relative).is_dir(),
        "{relative:?}"
    );
    relative
}
