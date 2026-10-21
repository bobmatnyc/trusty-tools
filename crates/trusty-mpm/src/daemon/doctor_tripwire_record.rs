//! `tm doctor` CTO tripwire record probe (#8454 G3, slice S6 prep).
//!
//! Why: until the go-live alert lands, the tripwire's JSONL record is the only
//! sign that a channel-tainted session reached a configured project's files
//! (Architect 2026-10-21, 11:13Z). A record nobody reads hides a trip, and a
//! record the hook cannot write hides every later one.
//! What: [`check_tripwire_record`] stats the record directory and reads
//! `trips.jsonl` ([`crate::core::channels_tripwire`]). `Ok` when the file is
//! absent or empty. `Warn` when it parses with one or more trip lines,
//! naming the count and the newest timestamp. `Fail` when a line does not
//! parse, or when the directory exists but is not a writable directory.
//! `Unknown` when the directory or file cannot be stat'ed or read. A `Fail`
//! outranks a `Warn`. Read-only: it never creates, repairs or truncates.
//! Test: `doctor_tripwire_record_tests.rs`.

use std::path::Path;

use crate::core::channels_tripwire::{parse, record_dir, record_path};
use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name.
const NAME: &str = "tripwire_record";

/// Report the tripwire record's state under `home`. See the module doc.
pub(super) fn check_tripwire_record(home: &Path) -> DoctorCheck {
    let dir = record_dir(home);
    let unwritable = match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return unknown(format!("{} cannot be stat'ed ({e})", dir.display()));
        }
        Ok(meta) if !meta.is_dir() => Some(format!(
            "{} is not a directory, so the egress guard cannot record a trip",
            dir.display()
        )),
        Ok(_) if !writable(&dir) => Some(format!(
            "{} is not writable, so the egress guard cannot record a trip",
            dir.display()
        )),
        Ok(_) => None,
    };
    let file = record_path(home);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        // A blocked directory leaves the file unreachable; the Fail says why.
        Err(_) if unwritable.is_some() => None,
        Err(e) => return unknown(format!("{} cannot be read ({e})", file.display())),
    };
    let remedy = "Restore a writable `~/.trusty-tools/trusty-mpm/channels/tripwire/` directory.";
    if let Some(why) = unwritable {
        return DoctorCheck::new(NAME, CheckStatus::Fail, format!("{why}. {remedy}"));
    }
    let lines = match text.as_deref().map(parse) {
        None => Vec::new(),
        Some(Ok(lines)) => lines,
        Some(Err(n)) => {
            return DoctorCheck::new(
                NAME,
                CheckStatus::Fail,
                format!(
                    "{} line {n} is not a tripwire record line, so the trip count cannot be \
                     trusted. Inspect the file; never truncate it before the operator has read it.",
                    file.display()
                ),
            );
        }
    };
    let Some(newest) = lines.iter().map(|l| l.at).max() else {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Ok,
            "no CTO tripwire trip recorded (#8454)".to_owned(),
        );
    };
    DoctorCheck::new(
        NAME,
        CheckStatus::Warn,
        format!(
            "{} CTO tripwire trip(s) recorded, the newest at {}: a channel-tainted session's tool \
             call reached a configured project (#8454). Read {} and ask the operator; until the \
             go-live alert lands this row is the only trip signal.",
            lines.len(),
            newest.to_rfc3339(),
            file.display()
        ),
    )
}

/// An `Unknown` row naming `why`.
fn unknown(why: String) -> DoctorCheck {
    DoctorCheck::new(
        NAME,
        CheckStatus::Unknown,
        format!("cannot read the CTO tripwire record: {why}"),
    )
}

/// Whether this process may write in `dir` (`access(2)` with `W_OK`).
#[cfg(unix)]
fn writable(dir: &Path) -> bool {
    nix::unistd::access(dir, nix::unistd::AccessFlags::W_OK).is_ok()
}

/// Whether `dir` is not marked read-only.
#[cfg(not(unix))]
fn writable(dir: &Path) -> bool {
    std::fs::metadata(dir).is_ok_and(|m| !m.permissions().readonly())
}

#[cfg(test)]
#[path = "doctor_tripwire_record_tests.rs"]
mod tests;
