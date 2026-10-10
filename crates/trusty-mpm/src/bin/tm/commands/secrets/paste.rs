//! The bounded clipboard read behind `tm secrets set` (#7524 P2-L6).
//!
//! Why: the paste tool is an outside program. Resolved through `PATH`, a
//! planted copy answers instead of the real one; run with no time limit, a
//! hung one blocks `tm secrets set` forever and is never killed.
//! What: the fixed directories the tool is looked up in, the time and size
//! limits, and the typed errors a read can end in.
//! Test: `paste_tests.rs` beside this file.

use std::time::Duration;

/// How long a paste tool may run before it is killed (#7524 P2-L6).
pub(crate) const CLIPBOARD_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The most clipboard bytes `tm secrets set` accepts (#7524 P2-L6).
pub(crate) const CLIPBOARD_MAX_BYTES: usize = 1024 * 1024;

/// The directories searched for a paste tool, in order; never `PATH`.
#[cfg(target_os = "macos")]
pub(crate) const PASTE_DIRS: &[&str] = &["/usr/bin"];
/// See the macOS list above (#7524 P2-L6).
#[cfg(not(target_os = "macos"))]
pub(crate) const PASTE_DIRS: &[&str] = &[
    "/usr/bin",
    "/usr/local/bin",
    "/run/current-system/sw/bin",
    "/nix/var/nix/profiles/default/bin",
    "/snap/bin",
];

/// How a clipboard read failed. No variant carries clipboard bytes.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClipboardError {
    /// The tool did not finish in time; it and its process group were killed.
    #[error(
        "tm secrets set: `{program}` did not answer within {limit:?}; it was stopped and nothing was stored"
    )]
    Timeout { program: String, limit: Duration },
    /// The tool printed more than [`CLIPBOARD_MAX_BYTES`].
    #[error("tm secrets set: the clipboard holds more than {limit} bytes; nothing was stored")]
    TooLarge { limit: usize },
    /// The tool exited with a failure status.
    #[error("tm secrets set: `{program}` could not read the clipboard ({status})")]
    Failed {
        program: String,
        status: std::process::ExitStatus,
    },
    /// The tool could not be started or its output could not be collected.
    #[error("tm secrets set: cannot run `{program}`: {source}")]
    Run {
        program: String,
        source: std::io::Error,
    },
    /// The clipboard is not UTF-8 text.
    #[error("tm secrets set: the clipboard does not hold UTF-8 text")]
    NotUtf8,
    /// No paste tool exists in any searched directory.
    #[error(
        "tm secrets set: no clipboard reader found in {searched}; use `--value -` to read stdin"
    )]
    NoReader { searched: String },
}
