//! OOXML fixture builders: `.docx` (WordprocessingML) and `.xlsx`
//! (SpreadsheetML) written by hand so every structural feature under test —
//! headings, tables, merges, hidden rows — is explicit in source.
//!
//! Inline strings (`t="inlineStr"`) are used throughout rather than a
//! shared-strings part: calamine and anydoc both read them, and it keeps the
//! generator to one worksheet part.

use std::io::Write;

use zip::write::FileOptions;
use zip::ZipWriter;

const W_NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
const S_NS: &str =
    r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main""#;

/// Zip a set of (path, contents) parts into an OOXML package.
fn package(parts: &[(&str, String)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: FileOptions<'_, ()> = FileOptions::default();
        for (name, body) in parts {
            zip.start_file(*name, opts).expect("start entry");
            zip.write_all(body.as_bytes()).expect("write entry");
        }
        zip.finish().expect("finish zip");
    }
    buf
}

/// `word/styles.xml` defining Heading1..Heading3.
///
/// Required for a fair comparison, not decoration: anydoc resolves a
/// paragraph's heading level by looking `w:pStyle`'s id up in this part and
/// matching its `w:name` against `heading N` (see anydoc's
/// `formats::docx::styles::heading_level`), falling back to a direct
/// `outlineLvl`. A fixture carrying `w:pStyle` with no styles part gives it
/// nothing to resolve, and the resulting "anydoc recovered no headings" would
/// be an artifact of the fixture rather than a property of the crate. Real
/// Word documents always ship this part.
fn styles_xml() -> String {
    let styles: String = (1..=3)
        .map(|lvl| {
            format!(
                r#"<w:style w:type="paragraph" w:styleId="Heading{lvl}"><w:name w:val="heading {lvl}"/><w:pPr><w:outlineLvl w:val="{}"/></w:pPr></w:style>"#,
                lvl - 1
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:styles {W_NS}>{styles}</w:styles>"#
    )
}

fn docx_package(body: String) -> Vec<u8> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W_NS}><w:body>{body}</w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    package(&[
        ("[Content_Types].xml", content_types.to_string()),
        ("_rels/.rels", rels.to_string()),
        ("word/_rels/document.xml.rels", doc_rels.to_string()),
        ("word/styles.xml", styles_xml()),
        ("word/document.xml", document),
    ])
}

fn para(text: &str) -> String {
    format!("<w:p><w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r></w:p>")
}

fn heading(level: u8, text: &str) -> String {
    format!(
        "<w:p><w:pPr><w:pStyle w:val=\"Heading{level}\"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"
    )
}

/// A `<w:tbl>` with `rows` x `cols` cells, first row acting as the header.
fn table(rows: usize, cols: usize, tag: &str) -> String {
    let mut out = String::from("<w:tbl>");
    for r in 0..rows {
        out.push_str("<w:tr>");
        for c in 0..cols {
            let cell = if r == 0 {
                format!("{tag} col {c}")
            } else {
                format!("{tag} r{r}c{c}")
            };
            out.push_str(&format!(
                "<w:tc><w:p><w:r><w:t>{cell}</w:t></w:r></w:p></w:tc>"
            ));
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
    out
}

/// `n` plain paragraphs, no headings and no tables.
pub fn docx_flat(n: usize) -> Vec<u8> {
    let body: String = (0..n)
        .map(|i| para(&format!("Paragraph {i}: the quick brown fox jumps over the lazy dog.")))
        .collect();
    docx_package(body)
}

/// Headings + tables + prose, interleaved the way a real report is. This is
/// the fixture that decides whether anydoc's structure recovery is real.
pub fn docx_structured() -> Vec<u8> {
    let mut body = String::new();
    body.push_str(&heading(1, "Quarterly Operations Review"));
    body.push_str(&para(
        "This review covers the reporting period and its material variances.",
    ));
    for section in 0..4 {
        body.push_str(&heading(2, &format!("Section {section}: Regional Detail")));
        for p in 0..15 {
            body.push_str(&para(&format!(
                "Section {section} paragraph {p}: operating expenditure tracked against plan."
            )));
        }
        body.push_str(&table(5, 4, &format!("S{section}")));
    }
    body.push_str(&heading(2, "Appendix"));
    docx_package(body)
}

/// Throughput fixture: many paragraphs and many tables.
pub fn docx_large() -> Vec<u8> {
    let mut body = String::new();
    for i in 0..2000 {
        body.push_str(&para(&format!(
            "Line item {i}: reconciliation entry with narrative commentary attached."
        )));
        if i % 50 == 0 {
            body.push_str(&table(6, 5, &format!("T{i}")));
        }
    }
    docx_package(body)
}

fn xlsx_package(sheets: Vec<(String, String)>) -> Vec<u8> {
    let sheet_entries: String = sheets
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            format!(
                r#"<sheet name="{name}" sheetId="{id}" r:id="rId{id}"/>"#,
                id = i + 1
            )
        })
        .collect();
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook {S_NS} xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets>{sheet_entries}</sheets></workbook>"#
    );
    let wb_rels_entries: String = (1..=sheets.len())
        .map(|i| {
            format!(
                r#"<Relationship Id="rId{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{i}.xml"/>"#
            )
        })
        .collect();
    let wb_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{wb_rels_entries}</Relationships>"#
    );
    let overrides: String = (1..=sheets.len())
        .map(|i| format!(r#"<Override PartName="/xl/worksheets/sheet{i}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#))
        .collect();
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{overrides}</Types>"#
    );
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

    let mut parts = vec![
        ("[Content_Types].xml".to_string(), content_types),
        ("_rels/.rels".to_string(), rels.to_string()),
        ("xl/workbook.xml".to_string(), workbook),
        ("xl/_rels/workbook.xml.rels".to_string(), wb_rels),
    ];
    for (i, (_, xml)) in sheets.into_iter().enumerate() {
        parts.push((format!("xl/worksheets/sheet{}.xml", i + 1), xml));
    }
    let borrowed: Vec<(&str, String)> = parts
        .iter()
        .map(|(k, v)| (k.as_str(), v.clone()))
        .collect();
    package(&borrowed)
}

fn col_letter(c: usize) -> String {
    let mut n = c + 1;
    let mut s = String::new();
    while n > 0 {
        let rem = (n - 1) % 26;
        s.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    s
}

fn inline_cell(r: usize, c: usize, value: &str) -> String {
    format!(
        r#"<c r="{col}{row}" t="inlineStr"><is><t>{value}</t></is></c>"#,
        col = col_letter(c),
        row = r + 1
    )
}

fn sheet_xml(inner: String) -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet {S_NS}>{inner}</worksheet>"#)
}

/// `sheets` worksheets, each a dense `rows` x `cols` grid of inline strings.
pub fn xlsx_grid(sheets: usize, rows: usize, cols: usize) -> Vec<u8> {
    let built: Vec<(String, String)> = (0..sheets)
        .map(|s| {
            let mut data = String::from("<sheetData>");
            for r in 0..rows {
                data.push_str(&format!(r#"<row r="{}">"#, r + 1));
                for c in 0..cols {
                    let v = if r == 0 {
                        format!("Header {c}")
                    } else {
                        format!("S{s} r{r} c{c} value")
                    };
                    data.push_str(&inline_cell(r, c, &v));
                }
                data.push_str("</row>");
            }
            data.push_str("</sheetData>");
            (format!("Sheet{}", s + 1), sheet_xml(data))
        })
        .collect();
    xlsx_package(built)
}

/// anydoc issue #8: an `F1:O3` merge whose only populated cell is the anchor.
pub fn xlsx_merged_range() -> Vec<u8> {
    let mut data = String::from("<sheetData>");
    data.push_str(r#"<row r="1">"#);
    data.push_str(&inline_cell(0, 5, "Merged heading"));
    data.push_str("</row>");
    data.push_str("</sheetData>");
    data.push_str(r#"<mergeCells count="1"><mergeCell ref="F1:O3"/></mergeCells>"#);
    xlsx_package(vec![("Merge Example".to_string(), sheet_xml(data))])
}

/// anydoc issue #9: a hidden row and a hidden column beside visible cells.
pub fn xlsx_hidden() -> Vec<u8> {
    let mut inner = String::new();
    inner.push_str(r#"<cols><col min="2" max="2" width="10" hidden="1" customWidth="1"/></cols>"#);
    inner.push_str("<sheetData>");
    inner.push_str(r#"<row r="1">"#);
    inner.push_str(&inline_cell(0, 0, "Visible row"));
    inner.push_str(&inline_cell(0, 1, "Hidden column"));
    inner.push_str("</row>");
    inner.push_str(r#"<row r="2" hidden="1">"#);
    inner.push_str(&inline_cell(1, 0, "Hidden row"));
    inner.push_str("</row>");
    inner.push_str("</sheetData>");
    xlsx_package(vec![("Visibility Example".to_string(), sheet_xml(inner))])
}
