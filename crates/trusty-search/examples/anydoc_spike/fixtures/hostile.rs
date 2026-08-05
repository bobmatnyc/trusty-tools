//! Attack-shaped inputs. Every builder here stays under
//! `MAX_OFFICE_FILE_BYTES` (10 MiB) so the walker's size gate does not reject
//! the file before either parser sees it — the interesting regime is a
//! hostile document small enough to get through.

use std::io::Write;

use zip::write::{FileOptions, SimpleFileOptions};
use zip::ZipWriter;

/// Uncompressed payload each zip bomb expands to: 512 MiB.
///
/// Sized to sit above our `MAX_DOCUMENT_XML_BYTES` (50 MiB) AND above
/// anydoc's `MAX_ENTRY_BYTES` (128 MiB), so both extractors' declared bounds
/// are genuinely crossed rather than merely approached. A run of NUL bytes
/// deflates to roughly 1/10000th of its size, keeping the container tiny.
const BOMB_BYTES: usize = 512 * 1024 * 1024;

/// Build a zip whose single named entry decompresses to 512 MiB of a
/// repeating byte, wrapped so the surrounding package still looks like the
/// target format.
fn bomb_package(entry: &str, siblings: &[(&str, &str)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: SimpleFileOptions =
            FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in siblings {
            zip.start_file(*name, opts).expect("start sibling");
            zip.write_all(body.as_bytes()).expect("write sibling");
        }
        zip.start_file(entry, opts).expect("start bomb entry");
        // Write in chunks so the generator itself never holds 512 MiB.
        let chunk = vec![b' '; 1024 * 1024];
        let mut written = 0usize;
        // A leading XML prologue keeps the entry plausibly parseable so the
        // parser commits to decompressing rather than bailing on byte 1.
        zip.write_all(b"<?xml version=\"1.0\"?><root><t>")
            .expect("write prologue");
        while written < BOMB_BYTES {
            let n = chunk.len().min(BOMB_BYTES - written);
            zip.write_all(&chunk[..n]).expect("write bomb chunk");
            written += n;
        }
        zip.write_all(b"</t></root>").expect("write epilogue");
        zip.finish().expect("finish bomb");
    }
    buf
}

/// `.docx` whose `word/document.xml` expands to 512 MiB.
pub fn docx_zip_bomb() -> Vec<u8> {
    bomb_package(
        "word/document.xml",
        &[
            ("[Content_Types].xml", r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#),
            ("_rels/.rels", r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#),
        ],
    )
}

/// `.xlsx` whose `xl/worksheets/sheet1.xml` expands to 512 MiB. This is the
/// leg issue #2923 documents as a residual risk: calamine exposes no
/// pre-decompression bound, so our own extractor has nothing to enforce here.
pub fn xlsx_zip_bomb() -> Vec<u8> {
    bomb_package(
        "xl/worksheets/sheet1.xml",
        &[
            ("[Content_Types].xml", r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#),
            ("_rels/.rels", r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
            ("xl/workbook.xml", r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="S1" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
        ],
    )
}

/// `.docx` whose `word/document.xml` nests `depth` elements deep. Probes the
/// XML-depth bound: anydoc declares `MAX_XML_DEPTH = 256`; our streaming
/// quick-xml parse declares none (it never builds a tree, so depth costs it
/// nothing — a difference in mechanism, not a missing bound).
pub fn docx_deep_xml(depth: usize) -> Vec<u8> {
    let ns = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    let mut xml = format!(r#"<?xml version="1.0"?><w:document {ns}><w:body>"#);
    xml.push_str(&"<w:p>".repeat(depth));
    xml.push_str("<w:r><w:t>deep</w:t></w:r>");
    xml.push_str(&"</w:p>".repeat(depth));
    xml.push_str("</w:body></w:document>");

    let mut buf = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: SimpleFileOptions =
            FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("[Content_Types].xml", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
        zip.start_file("_rels/.rels", opts).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#).unwrap();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(xml.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Cut a well-formed document off at `keep` of its length.
pub fn truncated(mut bytes: Vec<u8>, keep: f64) -> Vec<u8> {
    let cut = ((bytes.len() as f64) * keep) as usize;
    bytes.truncate(cut);
    bytes
}

/// Zip local-file-header magic followed by noise: the central directory is
/// unreachable.
pub fn malformed_archive() -> Vec<u8> {
    let mut buf = b"PK\x03\x04".to_vec();
    buf.extend((0..8192u32).map(|i| (i % 251) as u8));
    buf
}

/// PDF header followed by noise.
pub fn malformed_pdf() -> Vec<u8> {
    let mut buf = b"%PDF-1.7\n".to_vec();
    buf.extend((0..8192u32).map(|i| (i.wrapping_mul(37) % 253) as u8));
    buf
}
