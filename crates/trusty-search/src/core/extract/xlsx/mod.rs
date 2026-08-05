//! Spreadsheet (xls/xlsx/xlsm) text extraction (issue #2923).
//!
//! Why: a flat prose dump of every cell loses the row/column structure that
//! gives spreadsheet content its meaning, and spreadsheets chunk poorly as
//! prose in general. This module renders each sheet as its own labelled
//! section — a `== <sheet name> ==` header followed by one tab-joined line
//! per row — so downstream chunking (the existing sliding-window chunker;
//! see `core::extract` module docs) keeps rows grouped under their sheet.
//!
//! What: [`extract`] reads the file into memory once, then opens the workbook
//! with `calamine::open_workbook_auto_from_rs` over those same bytes and walks
//! `Reader::worksheets()`. The dispatcher routes only `xls`/`xlsx`/`xlsm` here
//! (`core::extract::EXTRACT_EXTS`), so `.ods` never reaches this module even
//! though calamine itself can read it.
//!
//! Zip-bomb defence: calamine still exposes no size bound of its own, so
//! [`preflight_package_bounds`] validates the package with the `zip` crate
//! BEFORE calamine parses it, capping total decompressed bytes at
//! [`MAX_WORKBOOK_UNCOMPRESSED_BYTES`] and entry count at
//! [`MAX_WORKBOOK_ENTRIES`]. See that function for why the check had to move
//! outside calamine, and the constants for how the caps were sized.
//!
//! #4894 review: the guard and the parser must see the SAME bytes. Validating
//! by path and then reopening by path lets an attacker with write access to a
//! watched directory swap a benign workbook for a bomb between the two opens —
//! and the watcher re-triggers extraction on every write, so one winning
//! interleaving out of unlimited attempts is enough. Reading once and handing
//! calamine the in-memory buffer removes the second open entirely.
//!
//! RESIDUAL RISK: the bound covers zip-based packages (xlsx/xlsm) only.
//! `.xls` is a CFB/BIFF container with no stream compression, so its
//! in-memory size is bounded directly by the 10 MiB
//! [`MAX_OFFICE_FILE_BYTES`](super::MAX_OFFICE_FILE_BYTES) container cap and
//! there is nothing to expand. Beyond size, calamine's own cell model for a
//! package that passes the cap is still built in full before
//! [`MAX_EXTRACTED_TEXT_BYTES`](super::MAX_EXTRACTED_TEXT_BYTES) truncates
//! the text — bounded, but proportional to the cap rather than to the 5 MiB
//! of text actually kept.
//!
//! Test: `test_extracts_single_sheet`, `test_extracts_multiple_sheets_as_sections`,
//! `test_not_a_workbook_errors`, `test_zip_bomb_extraction_stays_within_memory_bound`.

use std::fmt::Write as _;
use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::sync::Arc;

use calamine::{open_workbook_auto_from_rs, Reader};

use super::{ExtractError, Extracted, MAX_OFFICE_FILE_BYTES};

/// Cap on the TOTAL uncompressed size of every entry in a spreadsheet
/// package (bytes).
///
/// Why: `MAX_OFFICE_FILE_BYTES` caps only the compressed container on disk,
/// and `EXTRACT_TIMEOUT` caps only wall-clock time — a zip bomb defeats both,
/// because it is small and it is fast. A 511 KiB fixture whose sheet XML
/// expands to 512 MiB extracted *successfully* in 143 ms while peaking at
/// 529.8 MiB RSS (555_499_520 bytes); one such file in a watched directory
/// would OOM the daemon.
///
/// Why 256 MiB, and why one package-wide cap rather than docx's per-part
/// cap: a dense inline-string workbook was measured compressing at 18.5:1,
/// so a legitimate file at the 10 MiB container cap decompresses to ~190 MiB
/// — and effectively all of it lands in the single `sheet1.xml` entry. A
/// per-entry cap at docx's 50 MiB would therefore reject real workbooks, and
/// a per-entry cap large enough not to would be redundant with the total.
/// 256 MiB admits the densest realistic package with ~35% headroom while
/// refusing the ~1000:1 ratios a DEFLATE bomb needs. Unlike docx, which
/// reads exactly one part, calamine reads the whole package, so the total is
/// the quantity that bounds memory.
///
/// What: enforced by [`preflight_package_bounds`] before calamine opens the
/// file.
/// Test: `test_zip_bomb_extraction_stays_within_memory_bound`,
/// `test_zip_bomb_rejected_with_cap_in_message`,
/// `test_large_legitimate_workbook_still_extracts`.
const MAX_WORKBOOK_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

/// Cap on how many entries a spreadsheet package may contain.
///
/// Why: the byte caps say nothing about entry COUNT, and `ZipArchive::new`
/// materialises every entry's metadata before either byte pass runs. A package
/// of empty entries declares zero uncompressed bytes, so both passes wave it
/// through: 200_000 stored empty entries in a 16.6 MiB container returned
/// `Ok(())` while adding 142.8 MiB RSS. At the 10 MiB
/// [`MAX_OFFICE_FILE_BYTES`] container ceiling that is ~115k entries ≈ 82 MiB,
/// and calamine then builds its own `ZipArchive` over the same bytes and pays
/// it a second time.
///
/// Why 16384: a typical xlsx package has tens to low hundreds of parts, but a
/// chart-dense dashboard emits roughly 4-5 parts per chart, so a 4096 cap
/// would refuse a real (if extreme) ~850-chart workbook — and the failure mode
/// of a too-tight cap is a legitimate file logged as an extraction failure.
/// The bound that actually matters is metadata cost at the container ceiling
/// (~115k entries ≈ 82 MiB), which 16384 stays an order of magnitude inside.
///
/// What: checked by [`preflight_package_bounds`] immediately after the central
/// directory is parsed, before either byte pass.
/// Test: `test_entry_count_cap_rejects_many_empty_entries`,
/// `test_entry_count_cap_accepts_package_at_the_limit`.
const MAX_WORKBOOK_ENTRIES: usize = 16384;

/// Extract text from an xls/xlsx/xlsm workbook.
///
/// Why/What: see module docs. The file is read once and both the guard and
/// calamine work from that single buffer, so there is no window in which the
/// bytes can change between validation and parsing. Decompression is bounded
/// by [`MAX_WORKBOOK_UNCOMPRESSED_BYTES`] and entry count by
/// [`MAX_WORKBOOK_ENTRIES`] (zip-bomb defence; see
/// [`preflight_package_bounds`]).
/// Test: `test_extracts_single_sheet`,
/// `test_zip_bomb_extraction_stays_within_memory_bound`,
/// `test_forged_central_directory_size_still_rejected`.
pub fn extract(path: &Path) -> Result<Extracted, ExtractError> {
    // #4894: read ONCE, then validate and parse the same bytes. Validating a
    // path and reopening it hands calamine a file the guard never saw.
    let bytes = read_package_bytes(path)?;

    preflight_package_bounds(Cursor::new(&bytes[..]), MAX_WORKBOOK_UNCOMPRESSED_BYTES)?;

    // `Arc<[u8]>` because `open_workbook_auto_from_rs` clones the reader once
    // per format it sniffs; cloning a `Vec` would copy the package each time.
    let mut workbook = open_workbook_auto_from_rs(Cursor::new(Arc::<[u8]>::from(bytes)))
        .map_err(|e| ExtractError::Xlsx(e.to_string()))?;

    let mut text = String::new();
    for (name, range) in workbook.worksheets() {
        let _ = writeln!(text, "== {name} ==");
        for row in range.rows() {
            let cells: Vec<String> = row.iter().map(|cell| cell.to_string()).collect();
            let _ = writeln!(text, "{}", cells.join("\t"));
        }
        text.push('\n');
    }

    Ok(Extracted {
        text,
        warning: None,
    })
}

/// Read the whole document into memory, refusing anything over
/// [`MAX_OFFICE_FILE_BYTES`].
///
/// Why: every later step — the preflight guard and calamine — works from this
/// one buffer, so the file is opened exactly once and cannot change underneath
/// the guard (#4894 review). The size check is re-run on the bytes actually
/// read rather than trusted from `metadata`, so a file that grows between the
/// two is reported rather than silently truncated into a parse error.
/// What: returns the file's contents, or `TooLarge` when it exceeds the cap.
/// Test: `test_oversized_package_reports_too_large`,
/// `test_extracts_single_sheet`.
fn read_package_bytes(path: &Path) -> Result<Vec<u8>, ExtractError> {
    let to_io_err = |source| ExtractError::Io {
        path: path.display().to_string(),
        source,
    };
    let too_large = |size| ExtractError::TooLarge {
        path: path.display().to_string(),
        size,
        cap: MAX_OFFICE_FILE_BYTES,
    };

    let file = std::fs::File::open(path).map_err(to_io_err)?;
    let declared = file.metadata().map_err(to_io_err)?.len();
    if declared > MAX_OFFICE_FILE_BYTES {
        return Err(too_large(declared));
    }

    // One byte past the cap: enough to detect an over-cap file, never enough
    // to let one in.
    let mut bytes = Vec::with_capacity(declared as usize);
    file.take(MAX_OFFICE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(to_io_err)?;
    if bytes.len() as u64 > MAX_OFFICE_FILE_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    Ok(bytes)
}

/// Refuse a spreadsheet package whose entries decompress past `cap` bytes in
/// total, before calamine reads any of it.
///
/// Why the check lives here and not inside the calamine call: calamine owns
/// its zip reading and offers no size limit, no bounded-reader seam, and no
/// hook — `open_workbook_auto` takes a path and returns a parsed workbook.
/// Nor does the zip layer bound it implicitly: `zip` limits an entry read by
/// its COMPRESSED length and lets the decompressor emit as much as that
/// inflates to, and calamine streams sheet XML through quick-xml without ever
/// consulting the declared uncompressed size. So the only place a bound can
/// be enforced is outside calamine, on the same file, using the `zip` crate
/// this crate already depends on.
///
/// What: an entry-count check plus two byte passes, the latter mirroring
/// `docx::read_entry_bounded`'s declared-size + `Read::take` pair, widened
/// from one part to the whole package.
/// 0. Entry count against [`MAX_WORKBOOK_ENTRIES`]. Neither byte pass sees a
///    package of empty entries — it declares and decompresses to nothing —
///    yet `ZipArchive::new` has already built a metadata record for each one.
/// 1. Central-directory metadata only. Summing the declared uncompressed
///    sizes refuses an honestly-declared bomb having decompressed nothing at
///    all — the cheapest possible rejection, and the one the measured
///    fixture takes.
/// 2. A bounded drain. Declared sizes are attacker-controlled, so each entry
///    is also streamed through `Read::take` into `io::sink()` and counted
///    against the remaining budget. Discarding rather than buffering keeps
///    this pass O(1) in memory no matter how large `cap` is, so the cap
///    governs only whether calamine runs — never how much the guard itself
///    allocates.
///
/// Takes a reader rather than a path so the caller can validate the exact
/// bytes it goes on to parse (#4894 review) — see [`extract`].
///
/// A non-zip container (`.xls` is CFB) is passed through untouched: there is
/// no compression to bound, and reporting on malformed input stays calamine's
/// job so error messages do not change. A container that IS a zip but that this
/// crate's zip cannot open is refused instead — see
/// [`starts_with_zip_signature`] for why the two cases cannot share a branch.
/// Test: `test_zip_bomb_rejected_with_cap_in_message`,
/// `test_forged_central_directory_size_still_rejected`,
/// `test_entry_count_cap_rejects_many_empty_entries`,
/// `test_non_zip_container_passes_preflight`,
/// `test_unparseable_zip_is_refused_rather_than_handed_to_calamine`,
/// `test_large_legitimate_workbook_still_extracts`.
fn preflight_package_bounds<R: Read + Seek>(mut reader: R, cap: u64) -> Result<(), ExtractError> {
    // #4894: fail CLOSED on a zip this crate cannot open — waving it through
    // hands calamine's own, newer zip an unbounded package the guard never saw.
    let container_is_zip = starts_with_zip_signature(&mut reader)?;
    let mut archive = match zip::ZipArchive::new(reader) {
        Ok(archive) => archive,
        Err(_) if !container_is_zip => return Ok(()),
        Err(e) => {
            return Err(ExtractError::Xlsx(format!(
                "zip package could not be read for size validation, \
                 refusing to parse it unbounded: {e}"
            )))
        }
    };

    if archive.len() > MAX_WORKBOOK_ENTRIES {
        return Err(ExtractError::Xlsx(format!(
            "workbook package holds {} entries, over the {MAX_WORKBOOK_ENTRIES} entry cap",
            archive.len()
        )));
    }

    let mut declared_total: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive
            .by_index_raw(i)
            .map_err(|e| ExtractError::Xlsx(e.to_string()))?;
        declared_total = declared_total.saturating_add(entry.size());
        if declared_total > cap {
            return Err(ExtractError::Xlsx(format!(
                "workbook package declares at least {declared_total} uncompressed bytes, \
                 over the {cap} byte cap"
            )));
        }
    }

    let mut remaining = cap;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| ExtractError::Xlsx(e.to_string()))?;
        let name = entry.name().to_string();
        remaining -= drain_entry_bounded(entry, &name, remaining)?;
    }
    Ok(())
}

/// Does the container carry a zip signature (`PK`) in its first two bytes?
///
/// Why the guard needs this at all: it reads the package with this crate's
/// `zip` 2.x while calamine parses the same bytes with its own `zip` 8.x
/// (`cargo tree -p trusty-search -i zip@8.6.0`). Two independent zip readers
/// over one file do not agree at the edges of the format, so "our reader could
/// not open it" is NOT "calamine will not open it either". Treating an open
/// failure as nothing-to-bound therefore fails OPEN: a package crafted to trip
/// the older reader — a zip64 locator layout, an extra field, an EOCD variant
/// it rejects — skips the guard entirely and reaches calamine with no bound at
/// all, which is the exact vector this module exists to close.
///
/// The signature is what separates the two cases the guard must keep apart. An
/// OOXML package always begins `PK\x03\x04`; `.xls` is CFB and begins
/// `\xD0\xCF\x11\xE0`, and anything that is not a workbook in any format begins
/// with neither. So: `PK` plus an open failure is a refusal, and no `PK` is a
/// pass-through that leaves the diagnosis to calamine.
///
/// What: reads two bytes and rewinds, so the caller's reader is untouched.
/// A container too short to hold a signature is not a zip.
/// Test: `test_unparseable_zip_is_refused_rather_than_handed_to_calamine`,
/// `test_non_zip_container_passes_preflight`, `test_not_a_workbook_errors`.
fn starts_with_zip_signature<R: Read + Seek>(reader: &mut R) -> Result<bool, ExtractError> {
    let mut magic = [0u8; 2];
    let is_zip = match reader.read_exact(&mut magic) {
        Ok(()) => &magic == b"PK",
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(e) => {
            return Err(ExtractError::Xlsx(format!(
                "reading the package signature: {e}"
            )))
        }
    };
    reader
        .rewind()
        .map_err(|e| ExtractError::Xlsx(format!("rewinding the package: {e}")))?;
    Ok(is_zip)
}

/// Decompress a zip entry to nowhere, refusing to read past `budget` bytes,
/// and report how many bytes it actually produced.
///
/// Why: the package bound has to hold even when the central directory's size
/// field lies, so the declared-size pass alone is not enough — this is the
/// hard half. Generic over `Read` so the lying-size path is unit-testable
/// without crafting a malicious zip (same reasoning as
/// `docx::read_entry_bounded`).
/// What: reads at most `budget + 1` bytes into `io::sink()`; exceeding
/// `budget` means the declared size was false, which is an error.
/// Test: `test_drain_entry_bounded_rejects_underdeclared_stream`,
/// `test_drain_entry_bounded_accepts_within_budget`, and — over a real zip
/// whose size fields were forged rather than a `Cursor` —
/// `test_forged_central_directory_size_still_rejected`.
fn drain_entry_bounded<R: Read>(entry: R, name: &str, budget: u64) -> Result<u64, ExtractError> {
    let drained = std::io::copy(
        &mut entry.take(budget.saturating_add(1)),
        &mut std::io::sink(),
    )
    .map_err(|e| ExtractError::Xlsx(format!("{name}: {e}")))?;
    if drained > budget {
        return Err(ExtractError::Xlsx(format!(
            "{name} decompressed past the {budget} bytes remaining of the package cap"
        )));
    }
    Ok(drained)
}

#[cfg(test)]
mod tests;
