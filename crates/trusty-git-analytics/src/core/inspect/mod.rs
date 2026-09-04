//! Database inspection and data-handling attestation (#5218).
//!
//! Why: a counterparty granting repository access asks what `tga` retained
//! before they grant it. Answering from the migration files is not evidence —
//! it describes the schema the binary would create, not the rows the operator's
//! database actually holds. DOC-67 §10 defers to this module for the exact
//! attestation language an AUDIT report may quote.
//! What: [`schema`] reads the live schema through `sqlite_master` and
//! `PRAGMA table_info`; [`attest`] turns that reading plus a runtime scan of
//! the free-text columns into an [`attest::Attestation`]; [`render`] formats
//! either one for a terminal. [`open_read_only`] is the entry point both use,
//! and it never creates, migrates, or writes to the file it opens.
//! Test: `core::inspect::tests`.

pub mod attest;
pub mod render;
pub mod schema;
pub mod text_columns;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::core::config::expand_path;
use crate::core::errors::{Result, TgaError};

/// Open `path` read-only, refusing anything that is not an existing SQLite
/// database.
///
/// Why: #5218 — every other entry point in this crate goes through
/// `Database::open`, which CREATES the file and runs all migrations when it is
/// missing. An inspection command on that path would report a complete, empty,
/// freshly-minted schema and exit 0, telling a reviewer that a database they
/// cannot actually read holds nothing. Each arm below therefore names the
/// cause and fails rather than manufacturing a subject to inspect.
/// What: rejects a missing path, a directory, and a file whose first page is
/// not the SQLite header, then opens with [`OpenFlags::SQLITE_OPEN_READ_ONLY`]
/// so an inspection can never migrate or otherwise alter the operator's file.
/// Test: `tests::open_read_only_names_a_missing_database`,
/// `tests::open_read_only_names_a_directory`,
/// `tests::open_read_only_names_a_non_sqlite_file`,
/// `tests::open_read_only_does_not_migrate`.
///
/// # Errors
///
/// - [`TgaError::NotFound`] when `path` does not exist.
/// - [`TgaError::ValidationError`] when `path` is a directory or is not a
///   SQLite database file.
/// - [`TgaError::IoError`] when the file exists but cannot be read.
/// - [`TgaError::DbError`] when SQLite itself refuses the open.
pub fn open_read_only(path: &Path) -> Result<Connection> {
    let resolved: PathBuf = expand_path(path);

    if !resolved.exists() {
        return Err(TgaError::NotFound(format!(
            "no database at {} — run `tga collect` first, or point --database at an existing tga.db",
            resolved.display()
        )));
    }
    if resolved.is_dir() {
        return Err(TgaError::ValidationError(format!(
            "{} is a directory, not a tga database file",
            resolved.display()
        )));
    }

    // Reading the header also surfaces a permission error as an I/O error
    // naming the path, rather than SQLite's opaque "unable to open database
    // file".
    let header = read_header(&resolved)?;
    if !header.starts_with(SQLITE_MAGIC) {
        return Err(TgaError::ValidationError(format!(
            "{} is not a SQLite database (expected the \"SQLite format 3\" header, \
             found {} byte(s) of other content)",
            resolved.display(),
            header.len()
        )));
    }

    Connection::open_with_flags(
        &resolved,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(TgaError::from)
}

/// The 16-byte magic string every SQLite database file begins with.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// Read up to the first 16 bytes of `path`, propagating any I/O failure.
fn read_header(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(|e| {
        TgaError::IoError(std::io::Error::new(
            e.kind(),
            format!("cannot read {}: {e}", path.display()),
        ))
    })?;
    let mut buf = vec![0_u8; SQLITE_MAGIC.len()];
    let read = file.read(&mut buf).map_err(|e| {
        TgaError::IoError(std::io::Error::new(
            e.kind(),
            format!("cannot read {}: {e}", path.display()),
        ))
    })?;
    buf.truncate(read);
    Ok(buf)
}
