//! Per-index file size caps for source files and office/PDF documents (#11655).
//!
//! Why: the 1 MiB source cap and the 10 MiB office/PDF cap were constants, so
//! a project with legitimately large documents could not index them (#11651).
//! #11655 keeps both defaults, makes each settable
//! per index in project config, and keep a hard ceiling so the #3373 DoS bound
//! still holds.
//! What: [`FileSizeCaps`] carries the two resolved caps. Every size gate reads
//! it: the walk and single-file admission (`walker::oversize_len`), pushed
//! writes (`walker::len_exceeds_caps`), the watcher
//! (`walker::should_skip_path_with_caps`) and office extraction
//! (`core::extract::read_content_with_cap`). [`validate_size_cap`] refuses 0
//! and anything over [`MAX_CONFIGURABLE_FILE_BYTES`] at config load and at
//! `POST /indexes`; [`FileSizeCaps::resolve`] falls back to the default, with a
//! warning, for an out-of-range value read back from `indexes.toml`.
//! Test: `service::reindex::size_caps_11655_tests`.

use crate::core::extract::{is_extractable_ext, MAX_OFFICE_FILE_BYTES};
use crate::service::walker::MAX_FILE_BYTES;

/// Hard ceiling (256 MiB) on either configurable cap.
///
/// Why: office extraction and source chunking read the whole file into memory,
/// so an unbounded cap would undo the #3373 DoS bound. 256 MiB matches the
/// memory the spreadsheet extractor already accepts for one package
/// (`xlsx::MAX_WORKBOOK_UNCOMPRESSED_BYTES`).
/// What: a value above this is refused by [`validate_size_cap`].
/// Test: `size_cap_validation_refuses_zero_and_over_ceiling`.
pub const MAX_CONFIGURABLE_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// The two per-index size caps, in bytes, resolved to concrete values.
///
/// Why: one `Copy` value threads through the handle, the walk options and the
/// extractor, so no gate can read a different cap from another.
/// What: `source` caps every non-office file; `office` caps the
/// `core::extract::EXTRACT_EXTS` formats. The tighter `data_file_max_bytes`
/// cap (#1372) still applies on top of `source` for data extensions.
/// Test: `default_caps_are_unchanged`, `raised_source_cap_indexes_a_file_over_the_old_default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSizeCaps {
    /// Cap for source and text files. Default [`MAX_FILE_BYTES`] (1 MiB).
    pub source: u64,
    /// Cap for office/PDF documents. Default [`MAX_OFFICE_FILE_BYTES`] (10 MiB).
    pub office: u64,
}

impl Default for FileSizeCaps {
    /// The pre-#11655 constants: 1 MiB source, 10 MiB office/PDF.
    fn default() -> Self {
        Self {
            source: MAX_FILE_BYTES,
            office: MAX_OFFICE_FILE_BYTES,
        }
    }
}

impl FileSizeCaps {
    /// The cap for a lowercased extension (no leading dot).
    pub fn for_ext(&self, ext: &str) -> u64 {
        if is_extractable_ext(ext) {
            self.office
        } else {
            self.source
        }
    }

    /// Resolve stored `Option` caps to concrete ones.
    ///
    /// Why: `indexes.toml` is hand-editable, so a value there can be out of
    /// range even though every write path validates it.
    /// What: `None` is the default; an out-of-range value is logged at warning
    /// level and replaced by the default.
    /// Test: `resolve_falls_back_to_the_default_for_an_invalid_stored_cap`.
    pub fn resolve(source: Option<u64>, office: Option<u64>) -> Self {
        let defaults = Self::default();
        Self {
            source: resolve_one("source_file_max_bytes", source, defaults.source),
            office: resolve_one("office_file_max_bytes", office, defaults.office),
        }
    }

    /// The caps as stored overrides: `None` for a cap at its default.
    ///
    /// Why: `indexes.toml` stays unchanged for an index that sets nothing.
    pub fn overrides(&self) -> (Option<u64>, Option<u64>) {
        let defaults = Self::default();
        (
            (self.source != defaults.source).then_some(self.source),
            (self.office != defaults.office).then_some(self.office),
        )
    }
}

/// One cap from its stored form; see [`FileSizeCaps::resolve`].
fn resolve_one(field: &str, stored: Option<u64>, default: u64) -> u64 {
    match stored {
        None => default,
        Some(v) => match validate_size_cap(field, Some(v)) {
            Ok(()) => v,
            Err(e) => {
                tracing::warn!("{e}; using the default of {default} bytes");
                default
            }
        },
    }
}

/// Refuse a cap of 0 or one above [`MAX_CONFIGURABLE_FILE_BYTES`].
///
/// Why: 0 would skip every file and is never what a caller means; a cap above
/// the ceiling would lift the DoS bound. Both fail loudly rather than being
/// accepted silently.
/// What: `Ok` for `None` or `1..=MAX_CONFIGURABLE_FILE_BYTES`; otherwise an
/// error naming `field` and the allowed range.
/// Test: `size_cap_validation_refuses_zero_and_over_ceiling`.
pub fn validate_size_cap(field: &str, value: Option<u64>) -> Result<(), String> {
    match value {
        Some(v) if v == 0 || v > MAX_CONFIGURABLE_FILE_BYTES => Err(format!(
            "{field} must be between 1 and {MAX_CONFIGURABLE_FILE_BYTES} bytes, got {v}"
        )),
        _ => Ok(()),
    }
}

/// Validate both configurable caps; the first failure wins.
pub fn validate_size_caps(source: Option<u64>, office: Option<u64>) -> Result<(), String> {
    validate_size_cap("source_file_max_bytes", source)?;
    validate_size_cap("office_file_max_bytes", office)
}
