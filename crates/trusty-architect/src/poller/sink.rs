//! The poller's two append-only outputs: `alerts.jsonl` and `poll.log`.
//!
//! Why: the Architect reads `alerts.jsonl` line by line; a line must be whole
//! JSON in the bytes the prototype writes (`json.dumps(a, ensure_ascii=False)`)
//! (#9171 A3a).
//! What: [`append_alerts`] opens the file in append mode and writes each alert
//! as one line with one `write_all`. When the file ends in a partial line (a
//! writer killed mid-append), it first ends that line, so the new alerts are
//! never glued onto it; that is the one place the bytes differ from the
//! prototype, which would merge the two. [`append_log`] writes
//! `<iso time> <msg>`.
//! Test: `alerts_jsonl_lines_match_python_bytes`,
//! `a_truncated_last_alert_line_does_not_corrupt_the_next_append`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::classify::Alert;
use crate::classify::alerts::iso;
use crate::pyjson::to_python_json;

/// True when `f` is non-empty and its last byte is not a newline.
fn ends_mid_line(f: &mut File) -> io::Result<bool> {
    if f.metadata()?.len() == 0 {
        return Ok(false);
    }
    f.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    f.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// Appends `alerts` to `path`, one JSON line each. Nothing is written for an
/// empty slice (the prototype opens the file only when there is a new alert).
///
/// # Errors
/// The file cannot be opened, read or written.
pub fn append_alerts(path: &Path, alerts: &[Alert]) -> io::Result<()> {
    if alerts.is_empty() {
        return Ok(());
    }
    let mut f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(path)?;
    if ends_mid_line(&mut f)? {
        // #9171 A3a: end a partial line left by a killed writer.
        f.write_all(b"\n")?;
    }
    for a in alerts {
        let mut line = to_python_json(a, false).map_err(io::Error::other)?;
        line.push('\n');
        f.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// Appends `<YYYY-MM-DDTHH:MM:SSZ> <msg>` to the poll log.
///
/// # Errors
/// The file cannot be opened or written.
pub fn append_log(path: &Path, now: f64, msg: &str) -> io::Result<()> {
    let mut f = OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(format!("{} {msg}\n", iso(now)).as_bytes())
}
