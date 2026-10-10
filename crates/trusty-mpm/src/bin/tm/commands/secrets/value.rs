//! Where `tm secrets set` reads a value: the clipboard, or stdin (#7521).
//!
//! Why: owner ruling 2026-09-17 — the clipboard is the default source and a
//! value is never an argument; DOC-74 §9 keeps `--value -` for scripts. The
//! sources sit behind [`ValueSource`] so tests never read the real clipboard.
//! What: [`SystemClipboard`] runs the platform's paste tool from a fixed
//! system directory, bounded in time and size (`paste.rs`), and captures its
//! stdout; [`StdinSource`] reads stdin to EOF. Neither logs, and no error
//! carries what was read.
//! Test: the `set_*` tests in `tests.rs` inject fixed sources;
//! `paste_tests.rs` runs [`SystemClipboard`] against scripted tools.

use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;

use super::paste::{self, CLIPBOARD_READ_TIMEOUT, ClipboardError, PASTE_DIRS};

/// A place a secret value can be read from.
pub(crate) trait ValueSource {
    /// The raw text, untrimmed. An empty clipboard is `Ok("")`.
    fn read(&self) -> anyhow::Result<String>;
}

/// The paste tools tried in order; the first one present answers.
#[cfg(target_os = "macos")]
pub(crate) const PASTE_TOOLS: &[(&str, &[&str])] = &[("pbpaste", &[])];
#[cfg(not(target_os = "macos"))]
pub(crate) const PASTE_TOOLS: &[(&str, &[&str])] = &[
    ("wl-paste", &["--no-newline"]),
    ("xclip", &["-selection", "clipboard", "-o"]),
    ("xsel", &["--clipboard", "--output"]),
];

/// The desktop clipboard, read through the platform's paste tool.
///
/// Why: #7524 P2-L6 — the tool was found through `PATH` and run with no
/// limit, so a planted tool supplied the secret and a hung one blocked
/// `tm secrets set` forever.
/// What: tries each absolute, executable tool path in order. The first one
/// present answers; a missing one is skipped. Every other outcome — timeout,
/// output over the cap, a failed exit — ends the read with that error.
/// Test: `paste_tests.rs`.
pub(crate) struct SystemClipboard {
    /// The tools to try, in order, with their arguments.
    tools: Vec<(PathBuf, Vec<String>)>,
    /// The directories named when no tool is found.
    searched: Vec<PathBuf>,
    /// How long one tool may run.
    timeout: Duration,
}

impl SystemClipboard {
    /// The production reader: the platform's tools in [`PASTE_DIRS`].
    pub(crate) fn system() -> Self {
        let dirs: Vec<PathBuf> = PASTE_DIRS.iter().map(PathBuf::from).collect();
        Self::in_dirs(&dirs, CLIPBOARD_READ_TIMEOUT)
    }

    /// The platform's tools looked up in `dirs`, tool by tool.
    pub(crate) fn in_dirs(dirs: &[PathBuf], timeout: Duration) -> Self {
        // #7524: P2-L6 a full path in a fixed directory; never a PATH lookup.
        let tools = PASTE_TOOLS
            .iter()
            .flat_map(|(name, args)| {
                let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
                dirs.iter().map(move |dir| (dir.join(name), args.clone()))
            })
            .collect();
        Self {
            tools,
            searched: dirs.to_vec(),
            timeout,
        }
    }

    /// Exactly `tools`, for tests.
    #[cfg(test)]
    pub(crate) fn with_tools(tools: Vec<(PathBuf, Vec<String>)>, timeout: Duration) -> Self {
        let mut searched: Vec<PathBuf> = Vec::new();
        for parent in tools.iter().filter_map(|(path, _)| path.parent()) {
            if !searched.iter().any(|dir| dir == parent) {
                searched.push(parent.to_path_buf());
            }
        }
        Self {
            tools,
            searched,
            timeout,
        }
    }
}

impl ValueSource for SystemClipboard {
    fn read(&self) -> anyhow::Result<String> {
        for (program, args) in &self.tools {
            // #7524: P2-L6 only an absolute, executable file runs.
            if !program.is_absolute() || !paste::is_executable_file(program) {
                continue;
            }
            // #7524: P2-L6 a timeout, an oversize or a failed run ends the
            // read; it never falls through to the next tool.
            let bytes = paste::run_bounded(program, args, self.timeout)?;
            return String::from_utf8(bytes).map_err(|_| ClipboardError::NotUtf8.into());
        }
        let searched: Vec<String> = self
            .searched
            .iter()
            .map(|dir| dir.display().to_string())
            .collect();
        Err(ClipboardError::NoReader {
            searched: searched.join(", "),
        }
        .into())
    }
}

/// This process's stdin, read to EOF.
pub(crate) struct StdinSource;

impl ValueSource for StdinSource {
    fn read(&self) -> anyhow::Result<String> {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| anyhow!("tm secrets set: cannot read stdin: {e}"))?;
        Ok(text)
    }
}
