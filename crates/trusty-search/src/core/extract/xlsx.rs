//! Spreadsheet (xls/xlsx/xlsm) text extraction (issue #2923).
//!
//! Why: a flat prose dump of every cell loses the row/column structure that
//! gives spreadsheet content its meaning, and spreadsheets chunk poorly as
//! prose in general. This module renders each sheet as its own labelled
//! section — a `== <sheet name> ==` header followed by one tab-joined line
//! per row — so downstream chunking (the existing sliding-window chunker;
//! see `core::extract` module docs) keeps rows grouped under their sheet.
//!
//! What: [`extract`] opens the workbook with `calamine::open_workbook_auto`,
//! which handles xls/xlsx/xlsm (and ods) uniformly, and walks
//! `Reader::worksheets()`.
//!
//! RESIDUAL RISK (zip-bomb, documented per review of PR #2932 / issue #2923):
//! unlike the docx path (which bounds decompression via
//! `docx::MAX_DOCUMENT_XML_BYTES`), calamine decompresses sheet XML
//! internally and exposes no pre-decompression size check or bounded-reader
//! seam to interpose on — a crafted spreadsheet under the 10 MiB
//! `MAX_OFFICE_FILE_BYTES` container cap could still expand to a large
//! in-memory sheet model before `MAX_EXTRACTED_TEXT_BYTES` truncation runs
//! post-hoc in `extract_text`. Mitigations in place: the 10 MiB container
//! cap, the [`EXTRACT_TIMEOUT`](super::EXTRACT_TIMEOUT) wall-clock bound on
//! the blocking-pool extraction, and the post-hoc extracted-text cap. A
//! calamine-level guard is tracked on issue #2923.
//!
//! Test: `test_extracts_single_sheet`, `test_extracts_multiple_sheets_as_sections`,
//! `test_not_a_workbook_errors`.

use std::fmt::Write as _;
use std::path::Path;

use calamine::{open_workbook_auto, Reader};

use super::{ExtractError, Extracted};

/// Extract text from an xls/xlsx/xlsm workbook.
///
/// Why/What: see module docs.
/// Test: `test_extracts_single_sheet`.
pub fn extract(path: &Path) -> Result<Extracted, ExtractError> {
    let mut workbook = open_workbook_auto(path).map_err(|e| ExtractError::Xlsx(e.to_string()))?;

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

    #[test]
    fn test_not_a_workbook_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("not-a-workbook.xlsx");
        std::fs::write(&path, b"this is not a spreadsheet").unwrap();

        let result = extract(&path);
        assert!(result.is_err(), "non-workbook input must error");
    }

    /// Sanity check on the `Sheets` re-export path used by `open_workbook_auto`
    /// stays importable — guards against an upstream API rename silently
    /// breaking this module without a compile error surfaced elsewhere.
    #[test]
    fn test_sheets_type_is_importable() {
        fn _assert_type<T>() {}
        _assert_type::<Sheets<std::io::BufReader<std::fs::File>>>();
        let _ = Data::Empty;
    }
}
