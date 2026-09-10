//! Sparse worksheet coordinates are allocation bounds, independent of ZIP size (#7370).
use std::io::{Cursor, Read};
use trusty_common::chat_attachments::AttachmentError;

/// Why: a small ZIP can declare a huge sparse worksheet and exhaust memory.
/// What: reject out-of-bounds coordinates before calamine materializes cell ranges.
/// Test: `sparse_coordinate_cannot_expand_past_table_bounds`.
pub(super) fn validate(bytes: &[u8]) -> Result<(), AttachmentError> {
    let invalid = |e: String| AttachmentError::Invalid(e);
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| invalid(e.to_string()))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| invalid(e.to_string()))?;
        if !entry.name().starts_with("xl/worksheets/") || !entry.name().ends_with(".xml") {
            continue;
        }
        let mut xml = Vec::new();
        entry
            .by_ref()
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut xml)
            .map_err(|e| invalid(e.to_string()))?;
        if xml.len() > 16 * 1024 * 1024 {
            return Err(AttachmentError::TooLarge(
                "Worksheet XML exceeds 16 MiB".into(),
            ));
        }
        let mut reader = quick_xml::Reader::from_reader(xml.as_slice());
        loop {
            use quick_xml::events::Event;
            match reader.read_event().map_err(|e| invalid(e.to_string()))? {
                Event::Start(tag) | Event::Empty(tag) => {
                    let local = tag.local_name();
                    let attribute = match local.as_ref() {
                        b"c" => b"r".as_slice(),
                        b"dimension" | b"mergeCell" => b"ref".as_slice(),
                        _ => continue,
                    };
                    for attr in tag.attributes() {
                        let attr = attr.map_err(|e| invalid(e.to_string()))?;
                        if attr.key.as_ref() == attribute {
                            let value = attr
                                .decoded_and_normalized_value(
                                    quick_xml::XmlVersion::Implicit1_0,
                                    reader.decoder(),
                                )
                                .map_err(|e| invalid(e.to_string()))?;
                            for coordinate in value.split(':') {
                                coordinate_bound(coordinate)?;
                            }
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            }
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
    #[test]
    fn sparse_coordinate_cannot_expand_past_table_bounds() {
        assert!(coordinate_bound("AD200").is_ok());
        for coordinate in ["XFD1048576", "A201", "AE1"] {
            assert!(coordinate_bound(coordinate).is_err());
        }
    }
}
