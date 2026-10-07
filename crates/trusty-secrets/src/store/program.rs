//! Where a CLI-backed backend, and doctor's tool detection, find a program
//! (#7519).
//!
//! Why: a bare program name resolves through the `PATH` of whichever
//! process spawned the server, and a relative or empty `PATH` entry names
//! the working directory, where a planted CLI would receive values. Both
//! CLI backends judge a candidate program by the same bar, so the bar lives
//! here once (moved from the 1Password backend in P3, and out of `cli` in
//! P4 so doctor's DOC-74 §7 detection uses it on every Unix build).
//! What: [`find_on_path`] searches only the absolute entries of a `PATH`
//! value the caller hands in, for doctor's tool detection;
//! [`is_executable_file`] is the bar a machine `program` pin and every
//! candidate pass. #7524 P2-M2: the 1Password backend no longer searches a
//! `PATH` at all; [`ONEPASSWORD_DIRS`] is the fixed list it searches.
//! Test: `onepassword_resolves_op_from_the_system_dirs_in_order`,
//! `keeper_program_must_be_an_absolute_executable_machine_pin`,
//! `doctor_detects_unsupported_tools_on_the_start_path_without_running_them`.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The directories the 1Password backend searches for `op` when the
/// machine config pins no `program`, in order (DOC-74 §6.2).
///
/// Why: #7524 P2-M2, Architect Decision A — the spawner's `PATH` names
/// directories the caller chose, so a planted `op` ahead of the real one
/// received item templates, values inside. A fixed list of system
/// directories leaves that choice to the machine's administrator.
/// What: macOS — Homebrew on Apple silicon, then Intel, then the system
/// directory. Every other Unix — `/usr/local/bin`, then `/usr/bin`.
/// Test: `onepassword_system_dirs_are_the_documented_list`.
#[cfg(target_os = "macos")]
pub(crate) const ONEPASSWORD_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// See the macOS list above (#7524 P2-M2).
#[cfg(not(target_os = "macos"))]
pub(crate) const ONEPASSWORD_DIRS: &[&str] = &["/usr/local/bin", "/usr/bin"];

/// [`ONEPASSWORD_DIRS`] as paths, for the production backend factory.
pub(crate) fn onepassword_dirs() -> Vec<PathBuf> {
    ONEPASSWORD_DIRS.iter().map(PathBuf::from).collect()
}

/// The first regular, executable `name` in an absolute directory of
/// `dirs`, in order; a relative entry is skipped (#7524 P2-M2).
#[cfg(feature = "cli-backends")]
pub(crate) fn find_in_dirs(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// The first regular, executable `name` in an absolute entry of
/// `search_path`.
///
/// What: entries split as `PATH` does. An empty entry, `.`, and every other
/// relative entry resolve against the working directory, so they are
/// skipped. `None` and an empty value find nothing.
/// Test: `doctor_detects_unsupported_tools_on_the_start_path_without_running_them`.
pub(crate) fn find_on_path(name: &str, search_path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(search_path?)
        // #7519: only an absolute entry; never the working directory.
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Whether `path` is a regular file with an execute bit, following a symlink.
pub(crate) fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
}
