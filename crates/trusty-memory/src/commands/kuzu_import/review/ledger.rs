//! The review ledger file: append, parse, seal, and load a sealed ledger
//! (#8624, PR-C).
//!
//! Why: a ledger line that cannot be read must never read as "admit", and a
//! ledger edited after the review closed must not be applied. The ledger
//! also sits beside the masked export, so it gets the same owner-only mode.
//! What: [`append_entry`] adds one [`LedgerEntry`] line to a file created
//! 0600 by the opening call (never a mode set afterwards; a symlink is
//! refused). [`parse_ledger`] refuses the whole ledger on any bad line,
//! naming the line, because a bad line has no key to confine it to one row.
//! [`seal`] writes `ledger.seal` with the ledger's sha256; [`load_sealed`]
//! refuses an unsealed ledger or one whose bytes no longer match the seal.
//! Test: `review::ledger_tests`.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ReviewError;
use super::export::FILE_MODE;
use super::verdict::{LEDGER_SCHEMA, LedgerEntry, sha256_hex};

/// The ledger file inside a run directory.
pub(crate) const LEDGER_FILE: &str = "ledger.jsonl";
/// The seal file, a sibling of the ledger.
pub(crate) const SEAL_FILE: &str = "ledger.seal";
const SEAL_SCHEMA: u32 = 1;

/// Why a ledger could not be written, read or applied. Carries paths, line
/// numbers and parser messages only, never row content.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LedgerError {
    #[error(transparent)]
    Review(#[from] ReviewError),
    #[error(
        "review ledger {} line {line} is unreadable ({reason}); no row is admitted from it",
        path.display()
    )]
    Corrupt {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error(
        "review ledger {} is not sealed; run `trusty-memory import kuzu review finish --run \
         <run>` first",
        path.display()
    )]
    Unsealed { path: PathBuf },
    #[error("review ledger {} does not match its seal; no row is admitted from it", path.display())]
    SealMismatch { path: PathBuf },
    #[error("review run {} is already sealed; no verdict can be recorded", path.display())]
    Sealed { path: PathBuf },
    #[error("{0:?} is not a review run name")]
    BadRun(String),
    #[error("record refused: {0}")]
    Rejected(String),
}

fn io_err<'a>(step: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> LedgerError + 'a {
    move |source| {
        LedgerError::Review(ReviewError::Io {
            step,
            path: path.to_path_buf(),
            source,
        })
    }
}

fn corrupt(path: &Path, line: usize, reason: impl Into<String>) -> LedgerError {
    LedgerError::Corrupt {
        path: path.to_path_buf(),
        line,
        reason: reason.into(),
    }
}

/// What the ledger holds for one key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    One(LedgerEntry),
    /// More than one line names the key; the row is refused.
    Duplicate,
}

/// A parsed ledger: one slot per key.
#[derive(Debug, Default)]
pub(crate) struct ReviewLedger {
    slots: HashMap<String, Slot>,
    lines: usize,
}

/// What [`ReviewLedger::lookup`] found for a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lookup<'a> {
    Missing,
    Duplicate,
    Entry(&'a LedgerEntry),
}

impl ReviewLedger {
    /// The entry for `key`.
    pub(crate) fn lookup(&self, key: &str) -> Lookup<'_> {
        match self.slots.get(key) {
            None => Lookup::Missing,
            Some(Slot::Duplicate) => Lookup::Duplicate,
            Some(Slot::One(e)) => Lookup::Entry(e),
        }
    }

    /// Number of ledger lines.
    pub(crate) fn lines(&self) -> usize {
        self.lines
    }

    /// Every key the ledger names.
    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.slots.keys().map(String::as_str)
    }
}

/// Parse ledger bytes read from `path`.
///
/// Why (#8624 rule 3): a truncated or garbled line has no trustworthy key, so
/// it cannot be confined to one row; dropping it could also unhide an earlier
/// verdict it would have made a duplicate. The whole ledger is refused.
/// What: empty bytes are an empty ledger. Otherwise the bytes must end in a
/// newline (a missing one means a write was cut short), and every line must
/// be UTF-8, non-blank, and a [`LedgerEntry`] of [`LEDGER_SCHEMA`]. A key on
/// two lines becomes a duplicate slot.
/// Test: `truncated_ledger_line_never_reads_as_admit`,
/// `unterminated_last_line_refuses_the_ledger`.
pub(crate) fn parse_ledger(path: &Path, bytes: &[u8]) -> Result<ReviewLedger, LedgerError> {
    let mut ledger = ReviewLedger::default();
    if bytes.is_empty() {
        return Ok(ledger);
    }
    let Some(body) = bytes.strip_suffix(b"\n") else {
        let last = bytes.split(|b| *b == b'\n').count();
        return Err(corrupt(
            path,
            last,
            "no final newline; a write was cut short",
        ));
    };
    for (i, raw) in body.split(|b| *b == b'\n').enumerate() {
        let line = i + 1;
        let text = std::str::from_utf8(raw).map_err(|_| corrupt(path, line, "not UTF-8"))?;
        if text.trim().is_empty() {
            return Err(corrupt(path, line, "blank line"));
        }
        let entry: LedgerEntry =
            serde_json::from_str(text).map_err(|e| corrupt(path, line, e.to_string()))?;
        if entry.schema != LEDGER_SCHEMA {
            return Err(corrupt(path, line, format!("schema {}", entry.schema)));
        }
        ledger
            .slots
            .entry(entry.key.clone())
            .and_modify(|s| *s = Slot::Duplicate)
            .or_insert(Slot::One(entry));
        ledger.lines = line;
    }
    Ok(ledger)
}

/// Open `path` owner-only, refusing a final symlink: `append` creates or
/// appends, otherwise the file must not exist yet.
#[cfg(unix)]
fn open_owner_only(path: &Path, append: bool) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut o = OpenOptions::new();
    o.mode(FILE_MODE).custom_flags(libc::O_NOFOLLOW);
    if append {
        o.append(true).create(true);
    } else {
        o.write(true).create_new(true);
    }
    o.open(path)
}

#[cfg(not(unix))]
fn open_owner_only(_: &Path, _: bool) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "owner-only modes need unix",
    ))
}

/// Refuse `file` unless it is a regular file of mode [`FILE_MODE`].
fn check_owner_only(file: &File, path: &Path) -> Result<(), LedgerError> {
    let meta = file.metadata().map_err(io_err("read the mode of", path))?;
    #[cfg(unix)]
    let got = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o7777;
    #[cfg(not(unix))]
    let got = 0;
    if !meta.is_file() || got != FILE_MODE {
        return Err(ReviewError::Mode {
            path: path.to_path_buf(),
            want: FILE_MODE,
            got,
        }
        .into());
    }
    Ok(())
}

/// The bytes of an owner-only run file, or `None` when it does not exist.
///
/// What: opens without following a final symlink and refuses a file whose
/// mode is not [`FILE_MODE`].
pub(crate) fn read_owner_only(path: &Path) -> Result<Option<Vec<u8>>, LedgerError> {
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = File::open(path);
    let mut file = match opened {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err("open", path)(e)),
    };
    check_owner_only(&file, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(io_err("read", path))?;
    Ok(Some(bytes))
}

/// Append one entry to the ledger at `path`, creating it 0600.
///
/// Why (#8624): the ledger must be owner-only from the moment it exists; a
/// write path that copies the default mode (`0666 & !umask`) widens it.
/// What: one `open` with mode 0600, append, `O_NOFOLLOW`; the opened file's
/// own mode is checked before the line is written and synced.
/// Test: `umask_000_ledger_and_seal_are_owner_only`.
pub(crate) fn append_entry(path: &Path, entry: &LedgerEntry) -> Result<(), LedgerError> {
    let mut line = serde_json::to_string(entry)
        .map_err(|e| io_err("encode a verdict for", path)(io::Error::other(e)))?;
    line.push('\n');
    let mut file = open_owner_only(path, true).map_err(io_err("open", path))?;
    check_owner_only(&file, path)?;
    file.write_all(line.as_bytes())
        .map_err(io_err("write", path))?;
    file.sync_all().map_err(io_err("sync", path))
}

/// The seal: the ledger's digest and line count when review closed.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    schema: u32,
    ledger_sha256: String,
    entries: usize,
    sealed_at: String,
}

/// Whether `run_dir` holds a seal (or anything at the seal's path).
pub(crate) fn is_sealed(run_dir: &Path) -> bool {
    run_dir.join(SEAL_FILE).symlink_metadata().is_ok()
}

/// Seal `run_dir`'s ledger, creating an empty one first when it has none.
///
/// Why (#8624): after `finish` no verdict can be recorded, and the import
/// applies only the ledger as it stood at the seal.
/// What: refuses when already sealed or when the ledger does not parse;
/// writes `ledger.seal` (0600, exclusive create) with the ledger's sha256 and
/// line count, and returns the parsed ledger.
/// Test: `record_status_finish_flow`, `unsealed_or_edited_ledger_is_refused`.
pub(crate) fn seal(run_dir: &Path) -> Result<ReviewLedger, LedgerError> {
    let seal_path = run_dir.join(SEAL_FILE);
    if is_sealed(run_dir) {
        return Err(LedgerError::Sealed {
            path: run_dir.to_path_buf(),
        });
    }
    let ledger_path = run_dir.join(LEDGER_FILE);
    let created = open_owner_only(&ledger_path, true).map_err(io_err("open", &ledger_path))?;
    check_owner_only(&created, &ledger_path)?;
    drop(created);
    let bytes = read_owner_only(&ledger_path)?.unwrap_or_default();
    let ledger = parse_ledger(&ledger_path, &bytes)?;
    let body = Seal {
        schema: SEAL_SCHEMA,
        ledger_sha256: sha256_hex(&bytes),
        entries: ledger.lines(),
        sealed_at: chrono::Utc::now().to_rfc3339(),
    };
    let mut text = serde_json::to_string(&body)
        .map_err(|e| io_err("encode", &seal_path)(io::Error::other(e)))?;
    text.push('\n');
    let mut file = open_owner_only(&seal_path, false).map_err(io_err("create", &seal_path))?;
    check_owner_only(&file, &seal_path)?;
    file.write_all(text.as_bytes())
        .map_err(io_err("write", &seal_path))?;
    file.sync_all().map_err(io_err("sync", &seal_path))?;
    Ok(ledger)
}

/// Load the sealed ledger at `path` for `import kuzu --review-ledger`.
///
/// Why (#8624 rules 3, 5): the import applies only a review that was closed,
/// and only the bytes that were sealed. Any defect refuses every row.
/// What: reads `ledger.seal` beside `path` (absent is [`LedgerError::Unsealed`]),
/// then the ledger; a sha256 or line-count mismatch is
/// [`LedgerError::SealMismatch`]; then [`parse_ledger`]. Read-only.
/// Test: `unsealed_or_edited_ledger_is_refused`,
/// `corrupt_ledger_aborts_the_import_before_any_store_is_read`.
pub(crate) fn load_sealed(path: &Path) -> Result<ReviewLedger, LedgerError> {
    let seal_path = path.with_file_name(SEAL_FILE);
    let seal_bytes = match std::fs::read(&seal_path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(LedgerError::Unsealed {
                path: path.to_path_buf(),
            });
        }
        Err(e) => return Err(io_err("read", &seal_path)(e)),
    };
    let seal: Seal =
        serde_json::from_slice(&seal_bytes).map_err(|e| corrupt(&seal_path, 1, e.to_string()))?;
    if seal.schema != SEAL_SCHEMA {
        return Err(corrupt(&seal_path, 1, format!("schema {}", seal.schema)));
    }
    let bytes = std::fs::read(path).map_err(io_err("read", path))?;
    let mismatch = || LedgerError::SealMismatch {
        path: path.to_path_buf(),
    };
    if sha256_hex(&bytes) != seal.ledger_sha256 {
        return Err(mismatch());
    }
    let ledger = parse_ledger(path, &bytes)?;
    if ledger.lines() != seal.entries {
        return Err(mismatch());
    }
    Ok(ledger)
}
