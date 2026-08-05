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
/// Why 4096: a real xlsx package has tens to low hundreds of parts (one per
/// worksheet, plus shared strings, styles, relationships, and any embedded
/// media), so 4096 leaves an order of magnitude of headroom over the largest
/// legitimate workbook while capping the metadata table at a few MiB.
///
/// What: checked by [`preflight_package_bounds`] immediately after the central
/// directory is parsed, before either byte pass.
/// Test: `test_entry_count_cap_rejects_many_empty_entries`.
const MAX_WORKBOOK_ENTRIES: usize = 4096;

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
/// job so error messages do not change.
/// Test: `test_zip_bomb_rejected_with_cap_in_message`,
/// `test_forged_central_directory_size_still_rejected`,
/// `test_entry_count_cap_rejects_many_empty_entries`,
/// `test_non_zip_container_passes_preflight`,
/// `test_large_legitimate_workbook_still_extracts`.
fn preflight_package_bounds<R: Read + Seek>(reader: R, cap: u64) -> Result<(), ExtractError> {
    let Ok(mut archive) = zip::ZipArchive::new(reader) else {
        return Ok(());
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
mod tests {
    use super::*;
    use calamine::{Data, Sheets};
    use std::io::Write;

    /// Build a minimal valid `.xlsx` with the given sheets, each a list of
    /// rows of string cells.
    ///
    /// Why: hand-writing the SpreadsheetML XML parts (workbook.xml,
    /// worksheets/sheetN.xml, relationships, content types) directly keeps
    /// the fixture dependency-free rather than pulling in a spreadsheet
    /// *writer* crate just for tests.
    fn build_minimal_xlsx(sheets: &[(&str, Vec<Vec<&str>>)]) -> Vec<u8> {
        let content_types = {
            let overrides: String = sheets
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    format!(
                        r#"<Override PartName="/xl/worksheets/sheet{}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
                        i + 1
                    )
                })
                .collect();
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{overrides}</Types>"#
            )
        };
        let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let workbook_xml = {
            let sheet_entries: String = sheets
                .iter()
                .enumerate()
                .map(|(i, (name, _))| {
                    format!(
                        r#"<sheet name="{name}" sheetId="{}" r:id="rId{}"/>"#,
                        i + 1,
                        i + 1
                    )
                })
                .collect();
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{sheet_entries}</sheets></workbook>"#
            )
        };
        let workbook_rels = {
            let rel_entries: String = sheets
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    format!(
                        r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{}.xml"/>"#,
                        i + 1,
                        i + 1
                    )
                })
                .collect();
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rel_entries}</Relationships>"#
            )
        };

        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();

            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(root_rels.as_bytes()).unwrap();
            zip.start_file("xl/workbook.xml", opts).unwrap();
            zip.write_all(workbook_xml.as_bytes()).unwrap();
            zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
            zip.write_all(workbook_rels.as_bytes()).unwrap();

            for (i, (_, rows)) in sheets.iter().enumerate() {
                let sheet_data: String = rows
                    .iter()
                    .enumerate()
                    .map(|(r, row)| {
                        let cells: String = row
                            .iter()
                            .enumerate()
                            .map(|(c, val)| {
                                let cell_ref = format!("{}{}", column_letter(c), r + 1);
                                format!(
                                    r#"<c r="{cell_ref}" t="inlineStr"><is><t>{val}</t></is></c>"#
                                )
                            })
                            .collect();
                        format!(r#"<row r="{}">{cells}</row>"#, r + 1)
                    })
                    .collect();
                let sheet_xml = format!(
                    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{sheet_data}</sheetData></worksheet>"#
                );
                zip.start_file(format!("xl/worksheets/sheet{}.xml", i + 1), opts)
                    .unwrap();
                zip.write_all(sheet_xml.as_bytes()).unwrap();
            }

            zip.finish().unwrap();
        }
        buf
    }

    /// 0-indexed column number to spreadsheet column letter (A, B, ..., Z, AA, ...).
    fn column_letter(mut col: usize) -> String {
        let mut s = String::new();
        loop {
            let rem = col % 26;
            s.insert(0, (b'A' + rem as u8) as char);
            if col < 26 {
                break;
            }
            col = col / 26 - 1;
        }
        s
    }

    #[test]
    fn test_extracts_single_sheet() {
        let xlsx =
            build_minimal_xlsx(&[("Sheet1", vec![vec!["Name", "Amount"], vec!["Widget", "42"]])]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("book.xlsx");
        std::fs::write(&path, xlsx).unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        assert!(extracted.text.contains("== Sheet1 =="));
        assert!(extracted.text.contains("Name\tAmount"));
        assert!(extracted.text.contains("Widget\t42"));
        assert!(extracted.warning.is_none());
    }

    #[test]
    fn test_extracts_multiple_sheets_as_sections() {
        let xlsx = build_minimal_xlsx(&[
            ("Revenue", vec![vec!["Q1", "100"]]),
            ("Costs", vec![vec!["Q1", "40"]]),
        ]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("book.xlsx");
        std::fs::write(&path, xlsx).unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        assert!(extracted.text.contains("== Revenue =="));
        assert!(extracted.text.contains("== Costs =="));
        // Revenue section appears before the Costs section.
        let rev_pos = extracted.text.find("== Revenue ==").unwrap();
        let costs_pos = extracted.text.find("== Costs ==").unwrap();
        assert!(rev_pos < costs_pos);
    }

    /// Regression test for RUSTSEC-2026-0194/0195 (issue #3367): calamine
    /// parses sheet XML with quick-xml internally. A cell element carrying a
    /// pathologically large number of attributes used to be quadratic-time
    /// (0194) / unbounded-allocation (0195) in the vulnerable quick-xml
    /// versions. Bounded join turns a reintroduced hang into a deterministic
    /// test failure instead of a stuck CI job; the result is not asserted
    /// Ok/Err since either is an acceptable "handled cleanly" outcome.
    #[test]
    fn test_pathological_cell_attribute_count_does_not_hang() {
        let n = 20_000;
        let attrs: String = (0..n).map(|i| format!(r#" a{i}="v""#)).collect();
        let sheet_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"{attrs}><is><t>x</t></is></c></row></sheetData></worksheet>"#
        );

        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
            let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
            let workbook_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
            let workbook_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;

            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(root_rels.as_bytes()).unwrap();
            zip.start_file("xl/workbook.xml", opts).unwrap();
            zip.write_all(workbook_xml.as_bytes()).unwrap();
            zip.start_file("xl/_rels/workbook.xml.rels", opts).unwrap();
            zip.write_all(workbook_rels.as_bytes()).unwrap();
            zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
            zip.write_all(sheet_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("pathological.xlsx");
        std::fs::write(&path, buf).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = extract(&path);
            let _ = tx.send(result.is_ok());
        });
        let completed = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert!(
            completed.is_ok(),
            "a cell with many attributes must parse (Ok or Err) in bounded time, not hang"
        );
    }

    /// Regression test for the calamine 0.26 → 0.36 bump (issue #3367):
    /// upstream calamine `Changelog.md` documents two behavioral side
    /// effects bundled with the CVE fix, both intentional and not something
    /// this crate should "fix" back to the old behavior:
    /// - 0.36.0 trims leading/trailing ASCII whitespace on shared/inline
    ///   string cell text unless the cell carries `xml:space="preserve"`;
    /// - the 0.31.0 quick-xml upgrade normalizes `\r\n` line endings to `\n`
    ///   within string content.
    ///
    /// This locks in the POST-bump behavior so a future dependency bump that
    /// silently reverts either change is caught here rather than surfacing
    /// as a confusing re-indexing diff downstream. See the CHANGELOG.md
    /// entry for this PR for the user-facing note.
    #[test]
    fn test_cell_whitespace_trimmed_and_eol_normalized() {
        let xlsx = build_minimal_xlsx(&[("Sheet1", vec![vec!["  padded  ", "line1\r\nline2"]])]);
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("book.xlsx");
        std::fs::write(&path, xlsx).unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        // Leading/trailing whitespace is trimmed from the cell text.
        assert!(
            extracted.text.contains("padded\tline1\nline2"),
            "expected trimmed + \\n-normalized cell text, got: {:?}",
            extracted.text
        );
        assert!(
            !extracted.text.contains("  padded  "),
            "whitespace should have been trimmed, got: {:?}",
            extracted.text
        );
        assert!(
            !extracted.text.contains("line1\r\nline2"),
            "\\r\\n should have been normalized to \\n, got: {:?}",
            extracted.text
        );
    }

    /// Uncompressed payload the zip-bomb fixture expands to: 512 MiB.
    ///
    /// Matches the `spike/anydoc-extraction-evaluation` harness fixture that
    /// produced the 529.8 MiB peak-RSS measurement, so the before/after numbers
    /// on this file are directly comparable. A run of a repeating byte
    /// deflates ~1000:1, keeping the container around 500 KiB — well under
    /// `MAX_OFFICE_FILE_BYTES`, which is the whole point: the container cap
    /// never sees this file as large.
    const BOMB_PAYLOAD_BYTES: usize = 512 * 1024 * 1024;

    /// Peak-RSS ceiling asserted for a zip-bomb extraction, in bytes.
    ///
    /// The pre-fix behavior peaked at 529.8 MiB; the bounded path peaks in the
    /// tens of MiB. 256 MiB sits an order of magnitude below the former and
    /// an order of magnitude above the latter, so the assertion separates the
    /// two regimes without being sensitive to allocator or platform noise.
    const BOMB_PEAK_RSS_CEILING: u64 = 256 * 1024 * 1024;

    /// Env var carrying the fixture path to the child-process worker below.
    const BOMB_FIXTURE_ENV: &str = "TRUSTY_XLSX_ZIP_BOMB_FIXTURE";

    /// Build an `.xlsx` package whose `xl/worksheets/sheet1.xml` decompresses
    /// to `payload_bytes`, wrapped in enough real OOXML parts that calamine
    /// commits to reading the sheet rather than bailing on the package.
    ///
    /// Ported from `examples/anydoc_spike/fixtures/hostile.rs` on branch
    /// `spike/anydoc-extraction-evaluation` (`547fdeaa`).
    fn build_xlsx_zip_bomb(payload_bytes: usize) -> Vec<u8> {
        let siblings: [(&str, &str); 4] = [
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S1" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
        ];

        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zipw = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in siblings {
                zipw.start_file(name, opts).unwrap();
                zipw.write_all(body.as_bytes()).unwrap();
            }
            zipw.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
            zipw.write_all(br#"<?xml version="1.0"?><root><t>"#)
                .unwrap();
            // Written in chunks so the generator itself never holds the payload.
            let chunk = vec![b' '; 1024 * 1024];
            let mut written = 0usize;
            while written < payload_bytes {
                let n = chunk.len().min(payload_bytes - written);
                zipw.write_all(&chunk[..n]).unwrap();
                written += n;
            }
            zipw.write_all(b"</t></root>").unwrap();
            zipw.finish().unwrap();
        }
        buf
    }

    /// Peak resident set size of the current process, in bytes.
    fn peak_rss_bytes() -> u64 {
        // SAFETY: `usage` is a locally-owned, fully-zeroed `rusage`; getrusage
        // only writes into it and cannot retain the pointer.
        let usage = unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            assert_eq!(
                libc::getrusage(libc::RUSAGE_SELF, &mut usage),
                0,
                "getrusage(RUSAGE_SELF) failed"
            );
            usage
        };
        // macOS/BSD report ru_maxrss in bytes; Linux reports kilobytes.
        let raw = usage.ru_maxrss as u64;
        if cfg!(target_os = "macos") {
            raw
        } else {
            raw * 1024
        }
    }

    /// Child half of `test_zip_bomb_extraction_stays_within_memory_bound`.
    ///
    /// Peak RSS is a process-lifetime high-water mark, so it can only be read
    /// meaningfully from a process that did nothing else — under `cargo test`
    /// the parent's mark is already polluted by every sibling test. This runs
    /// as an ordinary (no-op) test in the normal suite and does the real work
    /// only when the parent re-invokes the test binary with the fixture path.
    #[test]
    fn zip_bomb_extraction_child_worker() {
        let Ok(fixture) = std::env::var(BOMB_FIXTURE_ENV) else {
            return;
        };
        let result = extract(Path::new(&fixture));
        println!("CHILD_RESULT_IS_ERR={}", result.is_err());
        println!("CHILD_PEAK_RSS_BYTES={}", peak_rss_bytes());
    }

    /// A zip bomb must be stopped by a SIZE bound, and the proof is memory —
    /// not merely that extraction returns an error.
    ///
    /// Why this is not covered by the existing mitigations: the walker's
    /// `MAX_OFFICE_FILE_BYTES` (10 MiB) bounds the CONTAINER, and this
    /// fixture's container is ~0.5 MiB; `EXTRACT_TIMEOUT` (30 s) bounds TIME,
    /// and the pre-fix path completed in 143 ms. It completed *successfully*,
    /// having allocated 529.8 MiB on the way — so an assertion on the return
    /// value alone would have passed against the vulnerable code.
    #[test]
    fn test_zip_bomb_extraction_stays_within_memory_bound() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("zipbomb.xlsx");
        let bomb = build_xlsx_zip_bomb(BOMB_PAYLOAD_BYTES);
        assert!(
            (bomb.len() as u64) < super::super::MAX_OFFICE_FILE_BYTES,
            "fixture must slip past the container cap to exercise the gap, got {} bytes",
            bomb.len()
        );
        std::fs::write(&path, &bomb).unwrap();
        drop(bomb);

        // Re-invoke this test binary to run the extraction in a clean process
        // whose peak-RSS mark reflects the extraction and nothing else.
        let module = module_path!();
        let test_name = module
            .split_once("::")
            .map(|(_crate_name, rest)| rest)
            .unwrap_or(module);
        let output =
            std::process::Command::new(std::env::current_exe().expect("current test binary path"))
                .args([
                    &format!("{test_name}::zip_bomb_extraction_child_worker"),
                    "--exact",
                    "--nocapture",
                ])
                .env(BOMB_FIXTURE_ENV, &path)
                .output()
                .expect("spawn child worker");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "child worker failed: {stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let peak: u64 = stdout
            .lines()
            .find_map(|l| l.strip_prefix("CHILD_PEAK_RSS_BYTES="))
            .unwrap_or_else(|| panic!("child did not report peak RSS; stdout: {stdout}"))
            .trim()
            .parse()
            .expect("peak RSS is a number");
        println!(
            "zip-bomb extraction peak RSS: {peak} bytes ({:.1} MiB)",
            peak as f64 / (1024.0 * 1024.0)
        );
        assert!(
            peak < BOMB_PEAK_RSS_CEILING,
            "extracting a {BOMB_PAYLOAD_BYTES}-byte zip bomb peaked at {peak} bytes RSS, \
             over the {BOMB_PEAK_RSS_CEILING} byte ceiling — decompression is unbounded"
        );
        assert!(
            stdout.contains("CHILD_RESULT_IS_ERR=true"),
            "the bomb must be refused, not silently truncated; stdout: {stdout}"
        );
    }

    /// The refusal must name the cap, so an operator seeing a skipped file in
    /// the logs can tell a size refusal from a parse failure.
    #[test]
    fn test_zip_bomb_rejected_with_cap_in_message() {
        // 1 MiB payload against a 64 KiB cap: same shape as the 512 MiB
        // fixture, small enough to keep this test fast.
        let bomb = build_xlsx_zip_bomb(1024 * 1024);

        let err = preflight_package_bounds(Cursor::new(&bomb[..]), 64 * 1024)
            .expect_err("a package over the cap must be refused");
        assert!(
            err.to_string().contains("over the 65536 byte cap"),
            "expected the cap in the message, got: {err}"
        );
    }

    /// Patch every central-directory and local-header uncompressed-size field
    /// belonging to `target_name` so it reads `to`, and report how many fields
    /// were rewritten.
    ///
    /// Both header copies are patched: rewriting only the central directory
    /// would leave the local header available as a second honest source, and
    /// the guard must hold when neither one can be trusted.
    fn forge_uncompressed_size(bytes: &mut [u8], target_name: &str, to: u32) -> usize {
        const CD_SIG: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
        const LOCAL_SIG: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];
        let mut patched = 0;
        let mut i = 0;
        while i + 46 <= bytes.len() {
            if bytes[i..i + 4] == CD_SIG {
                // Central directory header: name length at +28, uncompressed
                // size at +24, name itself at +46.
                let name_len =
                    u16::from_le_bytes(bytes[i + 28..i + 30].try_into().unwrap()) as usize;
                if i + 46 + name_len <= bytes.len()
                    && &bytes[i + 46..i + 46 + name_len] == target_name.as_bytes()
                {
                    bytes[i + 24..i + 28].copy_from_slice(&to.to_le_bytes());
                    patched += 1;
                }
            } else if bytes[i..i + 4] == LOCAL_SIG {
                // Local file header: name length at +26, uncompressed size at
                // +22, name itself at +30.
                let name_len =
                    u16::from_le_bytes(bytes[i + 26..i + 28].try_into().unwrap()) as usize;
                if i + 30 + name_len <= bytes.len()
                    && &bytes[i + 30..i + 30 + name_len] == target_name.as_bytes()
                {
                    bytes[i + 22..i + 26].copy_from_slice(&to.to_le_bytes());
                    patched += 1;
                }
            }
            i += 1;
        }
        patched
    }

    /// Pass 2 must reject a REAL zip whose size fields lie, not just a
    /// `Cursor` whose length exceeds a budget.
    ///
    /// Why this test and not just `test_drain_entry_bounded_*`: those exercise
    /// `Read::take` arithmetic. What actually makes pass 2 work is that `zip`
    /// bounds an entry reader by COMPRESSED length (`zip-2.4.2/src/read.rs`,
    /// `find_content` → `.take(data.compressed_size)`), so a forged
    /// uncompressed size cannot shorten the stream. Were a future `zip` to
    /// bound by the DECLARED uncompressed size instead, the drain would return
    /// exactly `declared`, the guard would pass, the whole defence would become
    /// a no-op — and every other test here would stay green. This one goes red.
    ///
    /// It also fails if pass 2 is removed outright: pass 1 is deliberately
    /// blinded first, and asserted blind.
    #[test]
    fn test_forged_central_directory_size_still_rejected() {
        const CAP: u64 = 64 * 1024;
        let mut bomb = build_xlsx_zip_bomb(1024 * 1024);
        let patched = forge_uncompressed_size(&mut bomb, "xl/worksheets/sheet1.xml", 1024);
        assert!(
            patched >= 2,
            "expected both the central-directory and local-header size fields to be forged, \
             patched {patched}"
        );

        // Pass 1 must now be blind — otherwise this would not isolate pass 2.
        let declared_total: u64 = {
            let mut archive = zip::ZipArchive::new(Cursor::new(&bomb[..])).unwrap();
            (0..archive.len())
                .map(|i| archive.by_index_raw(i).unwrap().size())
                .sum()
        };
        assert!(
            declared_total <= CAP,
            "the forged package must slip past the declared-size pass to exercise the drain, \
             declares {declared_total} against a {CAP} byte cap"
        );

        let err = preflight_package_bounds(Cursor::new(&bomb[..]), CAP)
            .expect_err("a package whose size fields lie must still be refused");
        assert!(
            err.to_string().contains("decompressed past"),
            "expected the drain to catch it, got: {err}"
        );
    }

    /// A package of many empty entries declares zero uncompressed bytes and
    /// decompresses to zero, so both byte passes wave it through — but
    /// `ZipArchive::new` has already built a metadata record per entry, and
    /// calamine then builds a second archive over the same bytes. Only an
    /// entry-count cap bounds that.
    #[test]
    fn test_entry_count_cap_rejects_many_empty_entries() {
        let mut buf = Vec::new();
        {
            let mut zipw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for i in 0..(MAX_WORKBOOK_ENTRIES + 1) {
                zipw.start_file(format!("xl/media/e{i}"), opts).unwrap();
            }
            zipw.finish().unwrap();
        }

        // The byte caps genuinely do not see this: nothing is declared and
        // nothing decompresses.
        let declared_total: u64 = {
            let mut archive = zip::ZipArchive::new(Cursor::new(&buf[..])).unwrap();
            (0..archive.len())
                .map(|i| archive.by_index_raw(i).unwrap().size())
                .sum()
        };
        assert_eq!(declared_total, 0, "fixture must declare no content at all");

        let err = preflight_package_bounds(Cursor::new(&buf[..]), MAX_WORKBOOK_UNCOMPRESSED_BYTES)
            .expect_err("a package over the entry cap must be refused");
        assert!(
            err.to_string()
                .contains(&format!("over the {MAX_WORKBOOK_ENTRIES} entry cap")),
            "expected the entry cap in the message, got: {err}"
        );
    }

    /// A package at the entry cap is still accepted — the cap must refuse the
    /// pathological case without clipping a legitimately part-heavy workbook.
    #[test]
    fn test_entry_count_cap_accepts_package_at_the_limit() {
        let mut buf = Vec::new();
        {
            let mut zipw = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for i in 0..MAX_WORKBOOK_ENTRIES {
                zipw.start_file(format!("xl/media/e{i}"), opts).unwrap();
            }
            zipw.finish().unwrap();
        }

        preflight_package_bounds(Cursor::new(&buf[..]), MAX_WORKBOOK_UNCOMPRESSED_BYTES)
            .expect("a package exactly at the entry cap must pass");
    }

    /// A document over `MAX_OFFICE_FILE_BYTES` is refused by name and size
    /// rather than truncated into a confusing parse error. `extract` is a
    /// module entry point reachable without going through `extract_text`'s
    /// gate, so it re-checks on the bytes it actually read.
    #[test]
    fn test_oversized_package_reports_too_large() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("huge.xlsx");
        std::fs::write(&path, vec![0u8; (MAX_OFFICE_FILE_BYTES + 1) as usize]).unwrap();

        let err = extract(&path).expect_err("an over-cap document must be refused");
        assert!(
            matches!(err, ExtractError::TooLarge { cap, .. } if cap == MAX_OFFICE_FILE_BYTES),
            "expected TooLarge naming the office-document cap, got: {err}"
        );
    }

    /// A lying central directory (declares under the cap, actually
    /// decompresses past it) must still be stopped by the `Read::take` bound.
    #[test]
    fn test_drain_entry_bounded_rejects_underdeclared_stream() {
        let data = vec![b'x'; 200];
        let err = drain_entry_bounded(std::io::Cursor::new(data), "xl/worksheets/sheet1.xml", 100)
            .expect_err("stream past the budget must error");
        assert!(err.to_string().contains("decompressed past"), "{err}");
    }

    #[test]
    fn test_drain_entry_bounded_accepts_within_budget() {
        let data = b"hello world";
        let drained =
            drain_entry_bounded(std::io::Cursor::new(&data[..]), "xl/workbook.xml", 100).unwrap();
        assert_eq!(drained, data.len() as u64);
    }

    /// `.xls` is CFB, not a zip. The preflight must wave it through rather
    /// than turning "not a zip" into a size error and masking calamine's own
    /// diagnostics — the case `test_not_a_workbook_errors` depends on.
    #[test]
    fn test_non_zip_container_passes_preflight() {
        let cfb = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1 not really a workbook";

        preflight_package_bounds(Cursor::new(&cfb[..]), MAX_WORKBOOK_UNCOMPRESSED_BYTES)
            .expect("a non-zip container has no decompression to bound");
    }

    /// The bound must not break real files. A workbook that decompresses to
    /// ~25 MiB — past the whole 10 MiB container cap, and five times the
    /// 5 MiB extracted-text cap — still extracts under the real production
    /// constant, not a test-only relaxed one.
    #[test]
    fn test_large_legitimate_workbook_still_extracts() {
        let cells: Vec<Vec<String>> = (0..2000)
            .map(|r| (0..200).map(|c| format!("cell-{r}-{c}-value")).collect())
            .collect();
        let rows: Vec<Vec<&str>> = cells
            .iter()
            .map(|row| row.iter().map(|s| s.as_str()).collect())
            .collect();
        let xlsx = build_minimal_xlsx(&[("Sheet1", rows)]);

        let uncompressed: u64 = {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(xlsx.clone())).unwrap();
            (0..archive.len())
                .map(|i| archive.by_index(i).unwrap().size())
                .sum()
        };
        assert!(
            uncompressed > 24 * 1024 * 1024,
            "fixture must be genuinely large to be worth asserting on, got {uncompressed} bytes"
        );

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("big.xlsx");
        std::fs::write(&path, xlsx).unwrap();

        let extracted = extract(&path).expect("a large but well-formed workbook must extract");
        assert!(extracted.text.contains("cell-0-0-value"));
        assert!(extracted.text.contains("cell-1999-199-value"));
    }

    /// Non-workbook input must still fail, and fail as a FORMAT problem rather
    /// than a size refusal — the preflight waves a non-zip container through
    /// precisely so calamine keeps owning that diagnosis.
    ///
    /// The message changed with the move to `open_workbook_auto_from_rs`
    /// (#4894 review): the path-based call picked a reader from the `.xlsx`
    /// extension and surfaced that reader's failure —
    /// `Xlsx error: Zip error: invalid Zip archive: Could not find EOCD` —
    /// while the byte-based call sniffs content across all four formats and
    /// reports that none matched. The new text is the more accurate of the
    /// two for input that is not a workbook in any format, so it is asserted
    /// as-is rather than the assertion being relaxed to accommodate it.
    #[test]
    fn test_not_a_workbook_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("not-a-workbook.xlsx");
        std::fs::write(&path, b"this is not a spreadsheet").unwrap();

        let err = extract(&path).expect_err("non-workbook input must error");
        assert_eq!(
            err.to_string(),
            "spreadsheet extraction failed: Cannot detect file format"
        );
    }

    /// Sanity check on the `Sheets` re-export path used by
    /// `open_workbook_auto_from_rs` stays importable — guards against an
    /// upstream API rename silently breaking this module without a compile
    /// error surfaced elsewhere.
    #[test]
    fn test_sheets_type_is_importable() {
        fn _assert_type<T>() {}
        _assert_type::<Sheets<Cursor<Arc<[u8]>>>>();
        let _ = Data::Empty;
    }
}
