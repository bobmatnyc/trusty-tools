//! Appendix B fixture documents, generated in the repo so their licence is
//! known (SPEC-OKF-01 §2.16 "Fixtures"). #10746 S10b.
//!
//! Why: the worker must be proven on real PDF, DOCX, XLSX and PPTX bytes, and
//! on a zip bomb, without checking in third-party files.
//! What: [`text_pdf`], [`table_pdf`], [`scanned_pdf`] and [`corrupt_pdf`]
//! write PDF 1.4 by hand with a computed cross-reference table; [`docx`],
//! [`xlsx`], [`pptx`] and [`zip_bomb_docx`] write Office Open XML packages
//! with the `zip` crate. Every builder is deterministic.
//! Test: used by `tests/fixtures.rs`.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// A PDF from object bodies (object `n` is `objs[n - 1]`), with object 1 as
/// the catalog.
fn pdf_from(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut s = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    s.extend_from_slice(data);
    s.extend_from_slice(b"\nendstream");
    s
}

/// A PDF with one page per content stream. Objects: 1 catalog, 2 pages,
/// 3 Helvetica, 4 an 8x8 grey image, then a page and its content per page.
fn pdf_pages(contents: &[String]) -> Vec<u8> {
    let n = contents.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 5 + 2 * i)).collect();
    let mut objs = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")).into_bytes(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
        stream(
            "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8",
            &[0x80; 64],
        ),
    ];
    for (i, c) in contents.iter().enumerate() {
        objs.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Resources << /Font << /F1 3 0 R >> /XObject << /Im1 4 0 R >> >> \
                 /Contents {} 0 R >>",
                6 + 2 * i
            )
            .into_bytes(),
        );
        objs.push(stream("", c.as_bytes()));
    }
    pdf_from(&objs)
}

/// One text line at (x, y).
fn line(x: u32, y: u32, text: &str) -> String {
    format!("BT /F1 12 Tf {x} {y} Td ({text}) Tj ET\n")
}

/// The page words of the 5-page text PDF, in page order.
pub const PAGE_WORDS: [&str; 5] = ["Aardvark", "Bluebird", "Cheetah", "Dolphin", "Elephant"];

/// Text PDF, 5 pages; page `i` names `PAGE_WORDS[i]`.
pub fn text_pdf() -> Vec<u8> {
    let pages: Vec<String> = PAGE_WORDS
        .iter()
        .enumerate()
        .map(|(i, w)| {
            line(72, 720, &format!("Page {} of the report", i + 1))
                + &line(72, 700, &format!("{w} is the word for this page."))
        })
        .collect();
    pdf_pages(&pages)
}

/// The cells of the table PDF, row by row.
pub const TABLE: [[&str; 3]; 3] = [
    ["Region", "Units", "Revenue"],
    ["North", "120", "4800"],
    ["South", "95", "3800"],
];

/// PDF with a ruled 3x3 table.
pub fn table_pdf() -> Vec<u8> {
    let mut c = line(72, 740, "Sales by region");
    for (r, row) in TABLE.iter().enumerate() {
        let y = 700 - 24 * r as u32;
        for (k, cell) in row.iter().enumerate() {
            c += &line(80 + 150 * k as u32, y, cell);
        }
    }
    for r in 0..=3 {
        c += &format!("72 {} m 522 {} l S\n", 716 - 24 * r, 716 - 24 * r);
    }
    for k in 0..=3 {
        c += &format!("{} 644 m {} 716 l S\n", 72 + 150 * k, 72 + 150 * k);
    }
    pdf_pages(&[c])
}

/// PDF whose one page is only an image: no text layer.
pub fn scanned_pdf() -> Vec<u8> {
    pdf_pages(&["q 400 0 0 400 100 200 cm /Im1 Do Q\n".to_string()])
}

/// #10746: a well-formed PDF (valid xref) whose page tree has `/Count 0`.
pub fn zero_page_pdf() -> Vec<u8> {
    pdf_from(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [] /Count 0 >>".to_vec(),
    ])
}

/// #10746: a one-page PDF whose only content stream claims `/FlateDecode`
/// but holds bytes that are not a deflate stream.
pub fn broken_deflate_pdf() -> Vec<u8> {
    pdf_from(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
          /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>"
            .to_vec(),
        stream("/Filter /FlateDecode", &[0xde, 0xad, 0xbe, 0xef].repeat(32)),
    ])
}

/// A PDF header followed by no valid object, cross-reference or trailer.
pub fn corrupt_pdf() -> Vec<u8> {
    let mut b = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R".to_vec();
    b.extend_from_slice(&[0xff, 0x00, 0x13, 0x37].repeat(64));
    b
}

/// A zip package from (name, bytes) entries, deflated.
fn package(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in entries {
        z.start_file(*name, opts).unwrap();
        z.write_all(data).unwrap();
    }
    z.finish().unwrap().into_inner()
}

const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const ODR: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const WML: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml";

fn types(overrides: &[(&str, String)]) -> Vec<u8> {
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"{CT}\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>"
    );
    for (part, ty) in overrides {
        s += &format!("<Override PartName=\"{part}\" ContentType=\"{ty}\"/>");
    }
    (s + "</Types>").into_bytes()
}

fn rels(targets: &[(&str, &str)]) -> Vec<u8> {
    let mut s =
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Relationships xmlns=\"{REL}\">");
    for (i, (kind, target)) in targets.iter().enumerate() {
        s += &format!(
            "<Relationship Id=\"rId{}\" Type=\"{ODR}/{kind}\" Target=\"{target}\"/>",
            i + 1
        );
    }
    (s + "</Relationships>").into_bytes()
}

fn para(style: Option<&str>, num: bool, text: &str) -> String {
    let mut ppr = String::new();
    if let Some(s) = style {
        ppr += &format!("<w:pStyle w:val=\"{s}\"/>");
    }
    if num {
        ppr += "<w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr>";
    }
    format!("<w:p><w:pPr>{ppr}</w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>")
}

fn docx_package(body: &[u8]) -> Vec<u8> {
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:styles xmlns:w=\"{W}\">\
         <w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/>\
         <w:pPr><w:outlineLvl w:val=\"0\"/></w:pPr></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/>\
         <w:pPr><w:outlineLvl w:val=\"1\"/></w:pPr></w:style></w:styles>"
    );
    let numbering = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:numbering xmlns:w=\"{W}\">\
         <w:abstractNum w:abstractNumId=\"0\"><w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/>\
         <w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"-\"/></w:lvl></w:abstractNum>\
         <w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num></w:numbering>"
    );
    package(&[
        (
            "[Content_Types].xml",
            types(&[
                ("/word/document.xml", format!("{WML}.document.main+xml")),
                ("/word/styles.xml", format!("{WML}.styles+xml")),
                ("/word/numbering.xml", format!("{WML}.numbering+xml")),
            ]),
        ),
        (
            "_rels/.rels",
            rels(&[("officeDocument", "word/document.xml")]),
        ),
        (
            "word/_rels/document.xml.rels",
            rels(&[("styles", "styles.xml"), ("numbering", "numbering.xml")]),
        ),
        ("word/styles.xml", styles.into_bytes()),
        ("word/numbering.xml", numbering.into_bytes()),
        ("word/document.xml", body.to_vec()),
    ])
}

/// DOCX with two headings, a two-item bulleted list and a 2x2 table.
pub fn docx() -> Vec<u8> {
    let cell = |t: &str| format!("<w:tc><w:p><w:r><w:t>{t}</w:t></w:r></w:p></w:tc>");
    let row = |a: &str, b: &str| format!("<w:tr>{}{}</w:tr>", cell(a), cell(b));
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"{W}\"><w:body>{}{}{}{}{}\
         <w:tbl><w:tblGrid><w:gridCol/><w:gridCol/></w:tblGrid>{}{}</w:tbl></w:body></w:document>",
        para(Some("Heading1"), false, "Quarterly Report"),
        para(None, false, "Opening paragraph."),
        para(Some("Heading2"), false, "Highlights"),
        para(None, true, "First point"),
        para(None, true, "Second point"),
        row("Region", "Revenue"),
        row("North", "4800"),
    );
    docx_package(body.as_bytes())
}

/// The sheet names of [`xlsx`], in workbook order.
pub const SHEETS: [&str; 2] = ["Sales", "Costs"];

/// XLSX with two sheets of inline-string and number cells.
pub fn xlsx() -> Vec<u8> {
    const S: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    const SML: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml";
    let sheet = |rows: &[(&str, u32)]| {
        let mut s = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"{S}\"><sheetData>"
        );
        for (i, (label, n)) in rows.iter().enumerate() {
            let r = i + 1;
            s += &format!(
                "<row r=\"{r}\"><c r=\"A{r}\" t=\"inlineStr\"><is><t>{label}</t></is></c>\
                 <c r=\"B{r}\"><v>{n}</v></c></row>"
            );
        }
        (s + "</sheetData></worksheet>").into_bytes()
    };
    let workbook = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><workbook xmlns=\"{S}\" xmlns:r=\"{ODR}\"><sheets>\
         <sheet name=\"{}\" sheetId=\"1\" r:id=\"rId1\"/><sheet name=\"{}\" sheetId=\"2\" r:id=\"rId2\"/>\
         </sheets></workbook>",
        SHEETS[0], SHEETS[1]
    );
    package(&[
        (
            "[Content_Types].xml",
            types(&[
                ("/xl/workbook.xml", format!("{SML}.sheet.main+xml")),
                ("/xl/worksheets/sheet1.xml", format!("{SML}.worksheet+xml")),
                ("/xl/worksheets/sheet2.xml", format!("{SML}.worksheet+xml")),
            ]),
        ),
        (
            "_rels/.rels",
            rels(&[("officeDocument", "xl/workbook.xml")]),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            rels(&[
                ("worksheet", "worksheets/sheet1.xml"),
                ("worksheet", "worksheets/sheet2.xml"),
            ]),
        ),
        ("xl/workbook.xml", workbook.into_bytes()),
        (
            "xl/worksheets/sheet1.xml",
            sheet(&[("Widgets", 12), ("Gadgets", 7)]),
        ),
        (
            "xl/worksheets/sheet2.xml",
            sheet(&[("Rent", 900), ("Power", 150)]),
        ),
    ])
}

/// The slide texts of [`pptx`], in slide order.
pub const SLIDES: [&str; 3] = ["Opening slide", "Middle slide", "Closing slide"];

/// PPTX with three slides; the zip stores them out of order (3, 1, 2) so
/// only the presentation's slide list gives the order.
pub fn pptx() -> Vec<u8> {
    const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
    const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    const PML: &str = "application/vnd.openxmlformats-officedocument.presentationml";
    let slide = |t: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><p:sld xmlns:p=\"{P}\" xmlns:a=\"{A}\">\
             <p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:txBody>\
             </p:sp></p:spTree></p:cSld></p:sld>"
        )
        .into_bytes()
    };
    let presentation = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><p:presentation xmlns:p=\"{P}\" xmlns:r=\"{ODR}\">\
         <p:sldIdLst><p:sldId id=\"256\" r:id=\"rId1\"/><p:sldId id=\"257\" r:id=\"rId2\"/>\
         <p:sldId id=\"258\" r:id=\"rId3\"/></p:sldIdLst></p:presentation>"
    );
    package(&[
        (
            "[Content_Types].xml",
            types(&[
                (
                    "/ppt/presentation.xml",
                    format!("{PML}.presentation.main+xml"),
                ),
                ("/ppt/slides/slide1.xml", format!("{PML}.slide+xml")),
                ("/ppt/slides/slide2.xml", format!("{PML}.slide+xml")),
                ("/ppt/slides/slide3.xml", format!("{PML}.slide+xml")),
            ]),
        ),
        (
            "_rels/.rels",
            rels(&[("officeDocument", "ppt/presentation.xml")]),
        ),
        (
            "ppt/_rels/presentation.xml.rels",
            rels(&[
                ("slide", "slides/slide1.xml"),
                ("slide", "slides/slide2.xml"),
                ("slide", "slides/slide3.xml"),
            ]),
        ),
        ("ppt/presentation.xml", presentation.into_bytes()),
        ("ppt/slides/slide3.xml", slide(SLIDES[2])),
        ("ppt/slides/slide1.xml", slide(SLIDES[0])),
        ("ppt/slides/slide2.xml", slide(SLIDES[1])),
    ])
}

/// A DOCX whose `word/document.xml` inflates to `mib` MiB of one paragraph
/// of text: a deflate ratio near 1000:1, far over any sane document.
pub fn zip_bomb_docx(mib: usize) -> Vec<u8> {
    let mut z = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    let small = docx_package(b"");
    let mut src = zip::ZipArchive::new(Cursor::new(small)).unwrap();
    for i in 0..src.len() {
        let f = src.by_index(i).unwrap();
        if f.name().ok().as_deref() != Some("word/document.xml") {
            z.raw_copy_file(f).unwrap();
        }
    }
    z.start_file("word/document.xml", opts).unwrap();
    write!(
        z,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"{W}\"><w:body><w:p><w:r><w:t>"
    )
    .unwrap();
    let chunk = vec![b'a'; 1 << 20];
    for _ in 0..mib {
        z.write_all(&chunk).unwrap();
    }
    z.write_all(b"</w:t></w:r></w:p></w:body></w:document>")
        .unwrap();
    z.finish().unwrap().into_inner()
}
