//! Classify each kuzu memory row for review and write the masked export (#8624).
//!
//! Why: a reviewer judges a refused row by its masked text and each span's
//! shape. Rows whose rule is never admissible are counted and dropped here, so
//! no later command can admit them.
//! What: [`classify_row`] decides each row: clean, auto-refused (with its
//! label), or exported as a [`MaskedRecord`]. [`write_run`] writes one JSON
//! line per record into a new run directory through a [`ReviewFs`], with
//! modes set at creation, and removes everything it made on any failure.
//! Test: `review::export_tests`.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::engine::filter::{
    MaskError, SECRET_FILTER_VERSION, SecretRule, SecretSpan, classify_secret, find_secret_spans,
    mask_content,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::super::bridge::KuzuMemoryRow;
use super::super::mapping::{MappedMemory, map_memory};
use super::super::screen::screen;
use super::ReviewError;

/// Version of the [`MaskedRecord`] line format.
pub(crate) const RECORD_SCHEMA: u32 = 1;
/// The export file inside a run directory.
pub(crate) const MASKED_FILE: &str = "masked.jsonl";
/// Where the export is staged before the rename publishes it.
pub(crate) const STAGING_FILE: &str = ".masked.jsonl.partial";
pub(crate) const DIR_MODE: u32 = 0o700;
pub(crate) const FILE_MODE: u32 = 0o600;

/// The only rules whose rows may be exported for review. Every other rule,
/// including any rule trusty-common adds later, is auto-refused.
pub(crate) const EXPORTABLE: [SecretRule; 2] = [SecretRule::Base64Blob, SecretRule::MixedCaseAlnum];
/// Auto-refuse label: the row's `Memory.id` is itself secret-shaped.
pub(crate) const SECRET_SHAPED_ID: &str = "secret_shaped_id";
/// Auto-refuse label: the screen refused the row but located no span.
pub(crate) const UNLOCATED: &str = "unlocated";
/// Auto-refuse label: masking refused the span list.
pub(crate) const MASK_ERROR: &str = "mask_error";

/// One span's rule and shape, never its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SpanShape {
    /// Matches the marker's `S{index}`.
    pub index: usize,
    pub rule: &'static str,
    pub len: usize,
    pub cls: &'static str,
    /// Shannon entropy in bits per char, times ten.
    pub ent_tenths: u16,
}

/// One line of `masked.jsonl`.
///
/// Why (#8624): PR-C keys a verdict on `key` plus `filter_version`, so a
/// verdict never applies to changed content or to a newer detector.
/// What: `key` is [`ledger_key`] over the store, the id and the content the
/// import would write; `masked` is that content trimmed, each span replaced by
/// `mask_content`'s marker. Built here, never deserialized from a marker.
/// Test: `record_carries_key_version_and_shapes_but_no_content`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MaskedRecord {
    pub schema: u32,
    pub filter_version: u32,
    pub store: String,
    pub id: String,
    pub key: String,
    pub masked: String,
    pub spans: Vec<SpanShape>,
}

/// What [`classify_row`] decided for one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RowVerdict {
    /// The screen passes the row, or it holds no memory to import.
    Clean,
    Export(MaskedRecord),
    /// Counted under this label and never written.
    AutoRefuse(&'static str),
}

/// sha256 over `store`, `id` and `content`, each prefixed by its byte length
/// as a big-endian u64, as lowercase hex.
///
/// Why (#8624): the review ledger key. The length prefixes keep `("ab", "c")`
/// and `("a", "bc")` apart; the content is hashed, never stored.
/// Test: `ledger_key_separates_its_fields`.
pub(crate) fn ledger_key(store: &str, id: &str, content: &str) -> String {
    let mut h = Sha256::new();
    for part in [store, id, content] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Decide one row with the live detector and masker.
///
/// Test: `exportable_rows_are_masked_and_auto_refuse_rows_are_not`.
pub(crate) fn classify_row(row: &KuzuMemoryRow, store: &str) -> RowVerdict {
    classify_row_with(row, store, find_secret_spans, mask_content)
}

/// [`classify_row`] with the span finder and masker passed in.
///
/// Why (#8624): the unlocated and mask-error arms cannot be reached with the
/// live detector, so their tests pass a finder or masker that fails.
/// What: mirrors the import's own refusal order: a secret-shaped id refuses
/// the row; a row with no id or blank content is clean; then the content the
/// import would screen (trimmed) is screened. A refused row is auto-refused
/// when no span is located, when any span's rule is outside [`EXPORTABLE`]
/// (labelled by the first such span), or when masking or slicing fails.
/// Spans, the mask and every shape are read from one `&str` binding.
/// Test: `exportable_rows_are_masked_and_auto_refuse_rows_are_not`,
/// `unlocated_refusal_is_auto_refused_never_exported`,
/// `mask_error_refuses_the_row`.
pub(crate) fn classify_row_with(
    row: &KuzuMemoryRow,
    store: &str,
    find: fn(&str) -> Vec<SecretSpan>,
    mask: fn(&str, &[SecretSpan]) -> Result<String, MaskError>,
) -> RowVerdict {
    if row.id.as_deref().and_then(screen).is_some() {
        return RowVerdict::AutoRefuse(SECRET_SHAPED_ID);
    }
    let Some(m) = map_memory(row, store) else {
        return RowVerdict::Clean;
    };
    classify_mapped_with(&m, find, mask)
}

/// Decide one mapped memory with the live detector and masker.
///
/// Why (#8624, PR-C): the import re-derives a row's verdict and key from the
/// row itself before it reads the ledger, so a ledger line can never admit a
/// row this function auto-refuses.
/// Test: `auto_refuse_row_is_never_admitted_even_with_a_forged_verdict`.
pub(crate) fn classify_mapped(m: &MappedMemory) -> RowVerdict {
    classify_mapped_with(m, find_secret_spans, mask_content)
}

/// [`classify_row_with`] after mapping: the id is screened again, then the
/// trimmed content.
fn classify_mapped_with(
    m: &MappedMemory,
    find: fn(&str) -> Vec<SecretSpan>,
    mask: fn(&str, &[SecretSpan]) -> Result<String, MaskError>,
) -> RowVerdict {
    if screen(&m.memory_id).is_some() {
        return RowVerdict::AutoRefuse(SECRET_SHAPED_ID);
    }
    // #8624 (PR-A critic): one binding for the verdict, the spans and the mask.
    let text = m.content.trim();
    if screen(text).is_none() {
        return RowVerdict::Clean;
    }
    let spans = find(text);
    if spans.is_empty() {
        return RowVerdict::AutoRefuse(UNLOCATED);
    }
    if let Some(s) = spans.iter().find(|s| !EXPORTABLE.contains(&s.rule)) {
        return RowVerdict::AutoRefuse(s.rule.label());
    }
    let Ok(masked) = mask(text, &spans) else {
        return RowVerdict::AutoRefuse(MASK_ERROR);
    };
    let mut shapes = Vec::with_capacity(spans.len());
    for (index, s) in spans.iter().enumerate() {
        let Some(secret) = text.get(s.range.clone()) else {
            return RowVerdict::AutoRefuse(MASK_ERROR);
        };
        let shape = classify_secret(secret);
        shapes.push(SpanShape {
            index,
            rule: s.rule.label(),
            len: shape.len,
            cls: shape.cls,
            ent_tenths: shape.ent_tenths,
        });
    }
    RowVerdict::Export(MaskedRecord {
        schema: RECORD_SCHEMA,
        filter_version: SECRET_FILTER_VERSION,
        store: m.store.clone(),
        key: ledger_key(&m.store, &m.memory_id, &m.content),
        id: m.memory_id.clone(),
        masked,
        spans: shapes,
    })
}

/// The filesystem steps of [`write_run`], so a test can fail any one.
///
/// What: each create is exclusive and sets its mode in the creating call; a
/// mode is never set afterwards.
pub(crate) trait ReviewFs {
    /// Create one directory with mode [`DIR_MODE`]; fails when it exists.
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Create a new file with mode [`FILE_MODE`]; fails when it exists.
    fn create_file(&self, path: &Path) -> io::Result<File>;
    /// The permission bits of `path`, not following a final symlink.
    fn mode(&self, path: &Path) -> io::Result<u32>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
}

/// [`ReviewFs`] over the real filesystem. Unix only: elsewhere every create
/// fails, so the export is refused rather than written with default modes.
pub(crate) struct OsFs;

#[cfg(unix)]
impl ReviewFs for OsFs {
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(DIR_MODE).create(path)
    }

    fn create_file(&self, path: &Path) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(path)
    }

    fn mode(&self, path: &Path) -> io::Result<u32> {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::symlink_metadata(path)?.permissions().mode() & 0o7777)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
}

#[cfg(not(unix))]
impl ReviewFs for OsFs {
    fn create_dir(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owner-only modes need unix",
        ))
    }
    fn create_file(&self, _: &Path) -> io::Result<File> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owner-only modes need unix",
        ))
    }
    fn mode(&self, _: &Path) -> io::Result<u32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owner-only modes need unix",
        ))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
}

fn io_err<'a>(step: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> ReviewError + 'a {
    move |source| ReviewError::Io {
        step,
        path: path.to_path_buf(),
        source,
    }
}

/// Refuse `path` unless its mode is exactly `want`.
fn check_mode(fs: &dyn ReviewFs, path: &Path, want: u32) -> Result<(), ReviewError> {
    let got = fs.mode(path).map_err(io_err("read the mode of", path))?;
    if got != want {
        return Err(ReviewError::Mode {
            path: path.to_path_buf(),
            want,
            got,
        });
    }
    Ok(())
}

/// Write `records` to `run_dir/masked.jsonl`, or leave nothing behind.
///
/// Why (#8624): the export must never exist with a looser mode, half written,
/// or in a directory the guard would refuse.
/// What: creates each missing ancestor and then `run_dir` itself (which must
/// not exist) with [`DIR_MODE`], checking each mode; runs `recheck` on
/// `run_dir`; creates the staging file with [`FILE_MODE`], checks its mode,
/// writes one JSON line per record, syncs, and renames it to [`MASKED_FILE`].
/// On any error every file and directory it created is removed, newest first,
/// and the first error is returned.
/// Test: `each_failed_step_aborts_with_nothing_left_behind`,
/// `a_looser_mode_at_creation_aborts_the_export`,
/// `umask_000_end_to_end_export_is_owner_only_and_leaks_nothing`.
pub(crate) fn write_run(
    fs: &dyn ReviewFs,
    run_dir: &Path,
    records: &[MaskedRecord],
    recheck: &dyn Fn(&Path) -> Result<(), ReviewError>,
) -> Result<PathBuf, ReviewError> {
    let mut created: Vec<PathBuf> = Vec::new();
    let result = write_run_steps(fs, run_dir, records, recheck, &mut created);
    if result.is_err() {
        for path in created.iter().rev() {
            // Best effort: the caller already holds the real error.
            let _ = match std::fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => std::fs::remove_dir(path),
                _ => std::fs::remove_file(path),
            };
        }
    }
    result
}

fn write_run_steps(
    fs: &dyn ReviewFs,
    run_dir: &Path,
    records: &[MaskedRecord],
    recheck: &dyn Fn(&Path) -> Result<(), ReviewError>,
    created: &mut Vec<PathBuf>,
) -> Result<PathBuf, ReviewError> {
    let missing: Vec<&Path> = run_dir
        .ancestors()
        .skip(1)
        .take_while(|a| a.symlink_metadata().is_err())
        .collect();
    for dir in missing.iter().rev().copied().chain([run_dir]) {
        fs.create_dir(dir).map_err(io_err("create", dir))?;
        created.push(dir.to_path_buf());
        check_mode(fs, dir, DIR_MODE)?;
    }
    recheck(run_dir)?;
    let staging = run_dir.join(STAGING_FILE);
    let mut file = fs
        .create_file(&staging)
        .map_err(io_err("create", &staging))?;
    created.push(staging.clone());
    check_mode(fs, &staging, FILE_MODE)?;
    for record in records {
        let mut line = serde_json::to_string(record)
            .map_err(|e| io_err("encode a record for", &staging)(io::Error::other(e)))?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .map_err(io_err("write", &staging))?;
    }
    file.sync_all().map_err(io_err("sync", &staging))?;
    drop(file);
    let target = run_dir.join(MASKED_FILE);
    fs.rename(&staging, &target)
        .map_err(io_err("publish", &target))?;
    Ok(target)
}
