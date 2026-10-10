//! Where `tm secrets set` reads a value: the clipboard, or stdin (#7521).
//!
//! Why: owner ruling 2026-09-17 — the clipboard is the default source and a
//! value is never an argument; DOC-74 §9 keeps `--value -` for scripts. The
//! sources sit behind [`ValueSource`] so tests never read the real clipboard.
//! What: [`SystemClipboard`] runs the platform's paste tool and captures its
//! stdout; [`StdinSource`] reads stdin to EOF. Neither logs, and no error
//! carries what was read.
//! Test: the `set_*` tests in `tests.rs` inject fixed sources; the system
//! readers run only in a real `tm`.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail};

use super::paste::{CLIPBOARD_READ_TIMEOUT, PASTE_DIRS};

/// A place a secret value can be read from.
pub(crate) trait ValueSource {
    /// The raw text, untrimmed. An empty clipboard is `Ok("")`.
    fn read(&self) -> anyhow::Result<String>;
}

/// The paste tools tried in order; the first one present answers.
#[cfg(target_os = "macos")]
const PASTE_TOOLS: &[(&str, &[&str])] = &[("pbpaste", &[])];
#[cfg(not(target_os = "macos"))]
const PASTE_TOOLS: &[(&str, &[&str])] = &[
    ("wl-paste", &["--no-newline"]),
    ("xclip", &["-selection", "clipboard", "-o"]),
    ("xsel", &["--clipboard", "--output"]),
];

/// The desktop clipboard, read through the platform's paste tool.
pub(crate) struct SystemClipboard {
    /// The tools to try, in order, with their arguments.
    tools: Vec<(PathBuf, Vec<String>)>,
    /// How long one tool may run.
    #[allow(dead_code)]
    timeout: Duration,
}

impl SystemClipboard {
    /// The production reader: the platform's tools in [`PASTE_DIRS`].
    pub(crate) fn system() -> Self {
        let dirs: Vec<PathBuf> = PASTE_DIRS.iter().map(PathBuf::from).collect();
        Self::in_dirs(&dirs, CLIPBOARD_READ_TIMEOUT)
    }

    /// The platform's tools looked up in `dirs`.
    pub(crate) fn in_dirs(_dirs: &[PathBuf], timeout: Duration) -> Self {
        let tools = PASTE_TOOLS
            .iter()
            .map(|(name, args)| {
                let args = args.iter().map(|a| (*a).to_string()).collect();
                (PathBuf::from(name), args)
            })
            .collect();
        Self { tools, timeout }
    }

    /// Exactly `tools`, for tests.
    #[cfg(test)]
    pub(crate) fn with_tools(tools: Vec<(PathBuf, Vec<String>)>, timeout: Duration) -> Self {
        Self { tools, timeout }
    }
}

impl ValueSource for SystemClipboard {
    fn read(&self) -> anyhow::Result<String> {
        for (program, args) in &self.tools {
            let program = program.display();
            let output = match Command::new(program.to_string())
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
            {
                Ok(output) => output,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => bail!("tm secrets set: cannot run `{program}`: {e}"),
            };
            if !output.status.success() {
                bail!(
                    "tm secrets set: `{program}` could not read the clipboard ({})",
                    output.status
                );
            }
            return String::from_utf8(output.stdout)
                .map_err(|_| anyhow!("tm secrets set: the clipboard does not hold UTF-8 text"));
        }
        bail!("tm secrets set: no clipboard reader found; use `--value -` to read stdin")
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
