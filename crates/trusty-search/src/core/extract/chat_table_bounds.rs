//! Sparse worksheet coordinates are allocation bounds, independent of ZIP size (#7370).
use std::{
    collections::BTreeMap,
    io::{Cursor, Read},
};
use trusty_common::chat_attachments::AttachmentError;

/// The package relationship naming the workbook part calamine opens.
const OFFICE_DOCUMENT: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";
/// Total uncompressed XML this validator will read from one package.
const MAX_XML: usize = 16 * 1024 * 1024;

/// Why: a small ZIP can declare a huge sparse worksheet and exhaust memory, and a sheet
/// reached only through a workbook relationship target escapes any ZIP-name prefix scan.
/// What: read every part once into a case-folded map, reject ambiguous duplicate names,
/// follow the package and workbook relationships to each sheet, and bound coordinates,
/// element counts and shared-string allocation hints before calamine allocates a range.
/// Cells must carry an explicit `r` coordinate; no part may bypass these checks through
/// its ZIP name or its relationship target.
///
/// Test: `relationship_targets_and_implicit_cells_are_bounded_before_calamine`,
/// `duplicate_zip_parts_are_rejected_as_ambiguous`,
/// `relationship_documents_reject_extra_roots_and_nested_records`.
pub(super) fn validate(bytes: &[u8]) -> Result<(), AttachmentError> {
    // #7655: a `xl/worksheets/` name scan misses sheets reached by relationship target.
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(invalid)?;
    let mut parts: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut total = 0usize;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(invalid)?;
        // Match calamine's case-insensitive ZIP lookup, including stored backslashes.
        let name = entry.name().replace('\\', "/").to_ascii_lowercase();
        let mut xml = Vec::new();
        entry
            .by_ref()
            .take(MAX_XML as u64 + 1)
            .read_to_end(&mut xml)
            .map_err(invalid)?;
        total = total.saturating_add(xml.len());
        if total > MAX_XML {
            return Err(AttachmentError::TooLarge(
                "Workbook XML exceeds 16 MiB".into(),
            ));
        }
        if parts.insert(name, xml).is_some() {
            return Err(invalid("Ambiguous duplicate workbook part"));
        }
    }
    let package = relationships(part(&parts, "_rels/.rels")?)?;
    let (_, workbook) = package
        .iter()
        .rev()
        .find(|(kind, _)| kind == OFFICE_DOCUMENT)
        .ok_or_else(|| invalid("Missing workbook relationship"))?;
    let target = checked_target(workbook)?;
    let prefix = target.rfind('/').map(|i| &target[..=i]).unwrap_or("");
    let prefix = prefix.strip_prefix('/').unwrap_or(prefix);
    let sheets = relationships(part(&parts, &format!("{prefix}_rels/workbook.xml.rels"))?)?;
    for (kind, target) in &sheets {
        let strings = match kind.rsplit('/').next() {
            Some("worksheet" | "chartsheet" | "dialogsheet") => false,
            Some("sharedStrings") => true,
            _ => continue,
        };
        let target = checked_target(target)?;
        let path = target
            .strip_prefix('/')
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{prefix}{target}"));
        validate_xml(part(&parts, &path)?, strings)?;
    }
    if let Some(strings) = parts.get(&format!("{prefix}sharedStrings.xml").to_ascii_lowercase()) {
        validate_xml(strings, true)?;
    }
    Ok(())
}
fn invalid(error: impl std::fmt::Display) -> AttachmentError {
    AttachmentError::Invalid(error.to_string())
}
fn part<'a>(parts: &'a BTreeMap<String, Vec<u8>>, name: &str) -> Result<&'a [u8], AttachmentError> {
    parts
        .get(&name.to_ascii_lowercase())
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("Missing workbook XML part"))
}
fn checked_target(target: &str) -> Result<&str, AttachmentError> {
    // Reject encodings calamine's relationship readers interpret differently.
    if target.is_empty() || target.contains(['&', '\\']) {
        return Err(invalid("Unsupported workbook relationship target"));
    }
    Ok(target)
}
/// Why: calamine stops at the first container end, so an extra root or a nested record
/// could redirect validation away from the part it actually opens.
/// What: accept exactly one `Relationships` root whose direct children are
/// `Relationship` records, rejecting external targets and any entity-bearing content.
/// Test: `relationship_documents_reject_extra_roots_and_nested_records`.
fn relationships(xml: &[u8]) -> Result<Vec<(String, String)>, AttachmentError> {
    let mut result = Vec::new();
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut root_seen = false;
    let mut depth = 0usize;
    loop {
        use quick_xml::events::Event;
        let event = reader.read_event().map_err(invalid)?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(tag) | Event::Empty(tag) => {
                let local = tag.local_name();
                if depth == 0 {
                    if root_seen || local.as_ref() != b"Relationships" {
                        return Err(invalid("Expected one Relationships document root"));
                    }
                    root_seen = true;
                } else if depth != 1 || local.as_ref() != b"Relationship" {
                    return Err(invalid("Relationship entries must be direct root children"));
                }
                if !empty {
                    depth += 1;
                }
                if local.as_ref() == b"Relationships" {
                    continue;
                }
                let mut kind = String::new();
                let mut target = String::new();
                for attr in tag.attributes() {
                    let attr = attr.map_err(invalid)?;
                    let value = std::str::from_utf8(&attr.value).map_err(invalid)?;
                    match attr.key.as_ref() {
                        b"Type" => kind = value.into(),
                        b"Target" => target = value.into(),
                        b"TargetMode" if value == "External" => {
                            return Err(invalid("External workbook parts are unsupported"))
                        }
                        _ => {}
                    }
                }
                result.push((kind, target));
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("Unexpected relationship end"))?;
            }
            Event::Text(text) if !text.iter().all(u8::is_ascii_whitespace) => {
                return Err(invalid("Unexpected relationship text"))
            }
            Event::CData(_) | Event::DocType(_) | Event::GeneralRef(_) => {
                return Err(invalid("Unsupported relationship XML content"))
            }
            Event::Decl(_) if root_seen => return Err(invalid("Unexpected XML declaration")),
            Event::Eof => {
                if !root_seen || depth != 0 {
                    return Err(invalid("Incomplete relationship document"));
                }
                break;
            }
            _ => {}
        }
    }
    Ok(result)
}
/// Why: calamine sizes its allocations from declared coordinates and counts, so every
/// hint has to be bounded before it reaches the parser.
/// What: bound row/cell/merge/dimension coordinates and element counts in a sheet, or
/// the `uniqueCount` and `si` count in a shared-strings part when `strings` is set.
/// Test: `relationship_targets_and_implicit_cells_are_bounded_before_calamine`.
fn validate_xml(xml: &[u8], strings: bool) -> Result<(), AttachmentError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let (mut rows, mut cells, mut string_count) = (0, 0, 0);
    loop {
        use quick_xml::events::Event;
        match reader.read_event().map_err(invalid)? {
            Event::Start(tag) | Event::Empty(tag) => {
                let local = tag.local_name();
                let name = local.as_ref();
                let attribute = match name {
                    b"row" if !strings => {
                        rows += 1;
                        b"r".as_slice()
                    }
                    b"c" if !strings => {
                        cells += 1;
                        b"r".as_slice()
                    }
                    b"dimension" | b"mergeCell" if !strings => b"ref".as_slice(),
                    b"sst" if strings => b"uniqueCount".as_slice(),
                    b"si" if strings => {
                        string_count += 1;
                        b"".as_slice()
                    }
                    _ => continue,
                };
                if rows > 200 || cells > 6000 || string_count > 18000 {
                    return Err(AttachmentError::TooLarge(
                        "Workbook element count exceeds table limits".into(),
                    ));
                }
                let mut found = false;
                for attr in tag.attributes() {
                    let attr = attr.map_err(invalid)?;
                    if attr.key.as_ref() != attribute {
                        continue;
                    }
                    found = true;
                    let value = std::str::from_utf8(&attr.value).map_err(invalid)?;
                    if name == b"row" || name == b"sst" {
                        let count: usize = value.parse().map_err(invalid)?;
                        if (name == b"row" && (count == 0 || count > 200)) || count > 18000 {
                            return Err(AttachmentError::TooLarge(
                                "Workbook allocation hint exceeds table limits".into(),
                            ));
                        }
                    } else {
                        for coordinate in value.split(':') {
                            coordinate_bound(coordinate)?;
                        }
                    }
                }
                if name == b"c" && !found {
                    return Err(invalid("Workbook cells must have explicit coordinates"));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(())
}
fn coordinate_bound(coordinate: &str) -> Result<(), AttachmentError> {
    let mut column: usize = 0;
    let mut row = String::new();
    for c in coordinate.chars().filter(|c| *c != '$') {
        if c.is_ascii_uppercase() && row.is_empty() {
            column = column
                .saturating_mul(26)
                .saturating_add((c as u8 - b'A' + 1) as usize);
        } else if c.is_ascii_digit() {
            row.push(c);
        } else {
            return Err(AttachmentError::Invalid(
                "Invalid worksheet coordinate".into(),
            ));
        }
    }
    let row: usize = row
        .parse()
        .map_err(|_| AttachmentError::TooLarge("Worksheet coordinate exceeds bounds".into()))?;
    if column == 0 || row == 0 {
        return Err(AttachmentError::Invalid(
            "Invalid worksheet coordinate".into(),
        ));
    }
    if column > 30 || row > 200 {
        return Err(AttachmentError::TooLarge(
            "Worksheet exceeds 200 rows or 30 columns".into(),
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const PACKAGE_RELS: &str = concat!(
        "<Relationships><Relationship Type=\"http://schemas.openxmlformats.org",
        "/officeDocument/2006/relationships/officeDocument\" ",
        "Target=\"xl/workbook.xml\"/></Relationships>"
    );
    const WORKBOOK: &str = concat!(
        "<workbook xmlns:r=\"http://schemas.openxmlformats.org/officeDocument",
        "/2006/relationships\"><sheets><sheet name=\"Sheet1\" sheetId=\"1\" ",
        "r:id=\"r1\"/></sheets></workbook>"
    );
    const SAFE_SHEET: &str = concat!(
        "<worksheet><sheetData><row r=\"1\"><c r=\"A1\"><v>1</v></c>",
        "</row></sheetData></worksheet>"
    );

    fn zip_parts(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    fn workbook_rels(target: &str) -> String {
        format!(
            concat!(
                "<Relationships><Relationship Id=\"r1\" Type=\"http://schemas.",
                "openxmlformats.org/officeDocument/2006/relationships/worksheet\" ",
                "Target=\"/{}\"/></Relationships>"
            ),
            target
        )
    }
    fn package(target: &str, worksheet: &str, strings: &str) -> Vec<u8> {
        let rels = workbook_rels(target);
        zip_parts(&[
            ("_rels/.rels", PACKAGE_RELS),
            ("xl/_rels/workbook.xml.rels", rels.as_str()),
            ("xl/workbook.xml", WORKBOOK),
            (target, worksheet),
            ("xl/sharedStrings.xml", strings),
        ])
    }
    #[test]
    fn relationship_documents_reject_extra_roots_and_nested_records() {
        for xml in [
            "<Relationships><Relationship Type='officeDocument' Target='xl/workbook.xml'/></Relationships><Relationships><Relationship Type='officeDocument' Target='safe/workbook.xml'/></Relationships>",
            "<Relationships><Relationships><Relationship Type='worksheet' Target='safe.xml'/></Relationships></Relationships>",
            "<Relationships/><Relationship Type='worksheet' Target='outside.xml'/>",
            "<Relationships><Relationship Type='worksheet' Target='safe.xml'><Relationship Type='worksheet' Target='nested.xml'/></Relationship></Relationships>",
        ] {
            assert!(relationships(xml.as_bytes()).is_err(), "accepted {xml}");
        }
    }
    /// #7655: two ZIP parts that differ only by case or slash direction resolve to one
    /// part for calamine, so which bytes it opens is ambiguous and must be refused.
    #[test]
    fn duplicate_zip_parts_are_rejected_as_ambiguous() {
        let mut wrong = Vec::new();
        for duplicate in ["xl/Workbook.xml", "xl\\workbook.xml"] {
            let rels = workbook_rels("xl/worksheets/sheet1.xml");
            let bytes = zip_parts(&[
                ("_rels/.rels", PACKAGE_RELS),
                ("xl/_rels/workbook.xml.rels", rels.as_str()),
                ("xl/workbook.xml", WORKBOOK),
                (duplicate, WORKBOOK),
                ("xl/worksheets/sheet1.xml", SAFE_SHEET),
            ]);
            match validate(&bytes) {
                Err(e) if e.to_string().contains("Ambiguous duplicate") => {}
                Err(e) => wrong.push(format!("{duplicate} rejected for the wrong reason: {e}")),
                Ok(()) => wrong.push(format!("{duplicate} accepted")),
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
    #[test]
    fn relationship_targets_and_implicit_cells_are_bounded_before_calamine() {
        // Guard only: never pass hostile sparse dimensions to the allocating parser.
        let mut accepted = Vec::new();
        for (target, xml, strings) in [
            ("xl/worksheets/sheet1.xml", "<worksheet><sheetData><row r=\"100000000\"><c><v>1</v></c></row></sheetData></worksheet>", "<sst/>"),
            ("custom/sheet.data", "<worksheet><sheetData><row><c r=\"A100000000\"><v>1</v></c></row></sheetData></worksheet>", "<sst/>"),
            ("custom/sheet.data", "<worksheet><sheetData><row><c><v>1</v></c></row></sheetData></worksheet>", "<sst/>"),
            ("custom/sheet.data", "<worksheet/>", "<sst uniqueCount=\"100000000\"/>"),
        ] {
            if validate(&package(target, xml, strings)).is_ok() {
                accepted.push(format!("{target}: {xml}, {strings}"));
            }
        }
        assert!(
            accepted.is_empty(),
            "accepted hostile workbooks: {accepted:#?}"
        );
        let valid = package(
            "custom/sheet.data",
            "<worksheet><sheetData><row r=\"200\"><c r=\"AD200\"><v>1</v></c></row></sheetData></worksheet>",
            "<sst uniqueCount=\"1\"><si><t>value</t></si></sst>",
        );
        assert!(validate(&valid).is_ok());
        assert!(crate::core::extract::chat_tables::prepare("fixture.xlsx", "xlsx", &valid).is_ok());
    }
    #[test]
    fn sparse_coordinate_cannot_expand_past_table_bounds() {
        assert!(coordinate_bound("AD200").is_ok());
        for coordinate in ["XFD1048576", "A201", "AE1"] {
            assert!(coordinate_bound(coordinate).is_err());
        }
    }
}
