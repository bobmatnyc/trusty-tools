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
//! value the caller hands in; [`is_executable_file`] is the bar both a
//! machine `program` pin and a `PATH` candidate pass.
//! Test: `onepassword_path_search_skips_relative_empty_and_dot_entries`,
//! `keeper_program_must_be_an_absolute_executable_machine_pin`,
//! `doctor_detects_unsupported_tools_on_the_start_path_without_running_them`.

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The first regular, executable `name` in an absolute entry of
/// `search_path`.
///
/// What: entries split as `PATH` does. An empty entry, `.`, and every other
/// relative entry resolve against the working directory, so they are
/// skipped. `None` and an empty value find nothing.
/// Test: `onepassword_path_search_skips_relative_empty_and_dot_entries`,
/// `onepassword_path_without_an_absolute_op_is_cli_not_installed`.
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
