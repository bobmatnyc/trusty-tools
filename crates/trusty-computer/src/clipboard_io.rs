//! The pasteboard seam of the clipboard tools (#11696 Phase C).
//!
//! Why: the bounded cua-driver daemon cannot expose the clipboard, so the
//! broker reads and writes it itself; tests must not touch the user's real
//! clipboard, so the access sits behind [`Pasteboard`].
//! What: [`SystemPasteboard`] runs `/usr/bin/pbpaste` and `/usr/bin/pbcopy`
//! with a cleared environment (`LANG=en_US.UTF-8` only, so non-ASCII text
//! round-trips). A read stops after `limit` bytes and kills `pbpaste`, so an
//! oversized clipboard is never held in full.
//! Test: `live_clipboard_write_then_redacted_read` (`tests/live_clipboard.rs`).

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Plain-text clipboard access.
pub trait Pasteboard: Send + Sync {
    /// The clipboard's plain text as bytes, at most `limit` of them.
    fn read(&self, limit: usize) -> Result<Vec<u8>, String>;
    /// Replace the clipboard with `text`.
    fn write(&self, text: &str) -> Result<(), String>;
}

/// The user's clipboard through `pbpaste` / `pbcopy`.
pub struct SystemPasteboard;

fn command(path: &str) -> Command {
    let mut cmd = Command::new(path);
    cmd.env_clear()
        .env("LANG", "en_US.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd
}

impl Pasteboard for SystemPasteboard {
    fn read(&self, limit: usize) -> Result<Vec<u8>, String> {
        let mut child = command("/usr/bin/pbpaste")
            .args(["-Prefer", "txt"])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("pbpaste: {e}"))?;
        let mut out = Vec::new();
        let read = child
            .stdout
            .take()
            .ok_or("pbpaste: no stdout")?
            .take(limit as u64)
            .read_to_end(&mut out);
        if out.len() >= limit {
            // #11696 Phase C: never hold more than the cap; the rest is refused anyway.
            let _ = child.kill();
            let _ = child.wait();
            return Ok(out);
        }
        read.map_err(|e| format!("pbpaste: {e}"))?;
        let status = child.wait().map_err(|e| format!("pbpaste: {e}"))?;
        if !status.success() {
            return Err(format!("pbpaste: {status}"));
        }
        Ok(out)
    }

    fn write(&self, text: &str) -> Result<(), String> {
        let mut child = command("/usr/bin/pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("pbcopy: {e}"))?;
        let wrote = child
            .stdin
            .take()
            .ok_or("pbcopy: no stdin")?
            .write_all(text.as_bytes());
        let status = child.wait().map_err(|e| format!("pbcopy: {e}"))?;
        wrote.map_err(|e| format!("pbcopy: {e}"))?;
        if !status.success() {
            return Err(format!("pbcopy: {status}"));
        }
        Ok(())
    }
}
