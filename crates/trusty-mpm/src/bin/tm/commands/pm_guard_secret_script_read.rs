//! The bounded, fail-closed read of a script body for
//! `pm_guard_secret_script` (#8879), split out for the 500-SLOC cap.
//!
//! Why: a body the guard cannot read in full is a body it cannot clear, so
//! every class of read it cannot finish is an [`Unread`] refusal.
//! What: [`read_script`] and the types it answers with.
//! Test: `pm_guard_secret_script::tests::read_script_reports_each_unread_class`.

use std::path::Path;

/// Largest script body the guard reads; a larger one is not inspected.
pub(crate) const MAX_SCRIPT_BYTES: u64 = 256 * 1024;

/// Why a script body was not judged. Every class refuses: the guard fails
/// closed on a body it cannot read in full (#8879).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unread {
    /// An open, stat or read error: permission denied, gone mid-check.
    Unreadable,
    /// Not a regular file: a directory, a FIFO, a device.
    NotRegular,
    /// The path's last component is a symlink to a non-executable file.
    Symlink,
    /// Larger than [`MAX_SCRIPT_BYTES`].
    TooLarge,
    /// Not UTF-8 text, or carrying a NUL byte.
    NotText,
    /// A literal script that does not exist and that nothing else in the
    /// command names, so no earlier stage can have written it.
    Missing,
    /// An interpreter's script named through a variable, a substitution or a
    /// glob: no path to read.
    Unresolvable,
}

/// What [`read_script`] found at a path.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Body {
    /// A script body, judged as text.
    Script(String),
    /// A compiled executable (ELF or Mach-O magic): not a script.
    Executable,
}

/// The first bytes of a compiled executable: ELF, then the Mach-O thin
/// (32/64-bit, both byte orders) and fat magics.
const EXECUTABLE_MAGIC: &[[u8; 4]] = &[
    [0x7f, b'E', b'L', b'F'],
    [0xfe, 0xed, 0xfa, 0xce],
    [0xfe, 0xed, 0xfa, 0xcf],
    [0xce, 0xfa, 0xed, 0xfe],
    [0xcf, 0xfa, 0xed, 0xfe],
    [0xca, 0xfe, 0xba, 0xbe],
    [0xbe, 0xba, 0xfe, 0xca],
];

/// Read a bounded prefix of a script body, failing closed.
///
/// Why: #8879 — a body the guard cannot read in full is a body it cannot
/// clear, so every unread class refuses instead of allowing.
/// What: opens the path `O_NONBLOCK` (a FIFO cannot block the hook) and
/// requires the OPENED handle to be a regular file; reads through
/// `take(MAX_SCRIPT_BYTES + 1)` so no size report is trusted. A compiled
/// executable is [`Body::Executable`], symlinked or not. Otherwise a symlinked
/// last component, an over-bound body, and non-UTF-8 or NUL-bearing bytes are
/// each an [`Unread`] error. Nothing the body names is opened here.
/// Test: `read_script_reports_each_unread_class`,
/// `every_unread_class_refuses_8879`.
pub(crate) fn read_script(path: &Path) -> Result<Body, Unread> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let link = std::fs::symlink_metadata(path).map_err(|_| Unread::Unreadable)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Unread::Unreadable)?;
    let opened = file.metadata().map_err(|_| Unread::Unreadable)?;
    if !opened.file_type().is_file() {
        return Err(Unread::NotRegular);
    }
    let mut bytes = Vec::new();
    file.take(MAX_SCRIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Unread::Unreadable)?;
    // #8879: a compiled program run by path is not a script; the kernel runs it.
    if EXECUTABLE_MAGIC.iter().any(|m| bytes.starts_with(m)) {
        return Ok(Body::Executable);
    }
    if link.file_type().is_symlink() {
        return Err(Unread::Symlink);
    }
    if bytes.len() as u64 > MAX_SCRIPT_BYTES {
        return Err(Unread::TooLarge);
    }
    if bytes.contains(&0) {
        return Err(Unread::NotText);
    }
    String::from_utf8(bytes)
        .map(Body::Script)
        .map_err(|_| Unread::NotText)
}
