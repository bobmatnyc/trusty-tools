//! Bounded structured table extraction for chat transports (#7370).
use std::io::Cursor;
use trusty_common::chat_attachments::{
    validate_metadata, Attachment, AttachmentError, ImageBytes, InputAttachment, Sheet,
};

/// Why: API callers need the same safe table preparation as desktop callers.
/// What: parse supported formats with standard parsers; reject bounds instead of truncating cells.
/// Test: `structured_tables_preserve_empty_and_quoted_cells`.
pub fn prepare(name: &str, format: &str, bytes: &[u8]) -> Result<InputAttachment, AttachmentError> {
    use calamine::Reader;
    let invalid = |message: String| AttachmentError::Invalid(message);
    let sheets = match format {
        "csv" | "clipboard-tsv" => {
            let delimiter = if format == "csv" { b',' } else { b'\t' };
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .delimiter(delimiter)
                .from_reader(bytes);
            let mut rows = Vec::new();
            for record in reader.records() {
                let record = record.map_err(|e| invalid(e.to_string()))?;
                rows.push(record.iter().map(str::to_owned).collect());
                if rows.len() > 200 {
                    return Err(AttachmentError::TooLarge("Table exceeds 200 rows".into()));
                }
            }
            vec![Sheet {
                name: "Sheet1".into(),
                rows,
            }]
        }
        "xlsx" => {
            if !bytes.starts_with(b"PK") {
                return Err(invalid("Invalid XLSX package".into()));
            }
            super::xlsx::preflight_bounded(Cursor::new(bytes), 16 * 1024 * 1024, 256).map_err(
                |e| {
                    if e.to_string().contains("cap") {
                        AttachmentError::TooLarge(e.to_string())
                    } else {
                        invalid(e.to_string())
                    }
                },
            )?;
            super::chat_table_bounds::validate(bytes)?;
            let mut workbook: calamine::Xlsx<_> = calamine::Reader::new(Cursor::new(bytes))
                .map_err(|e: calamine::XlsxError| invalid(e.to_string()))?;
            if workbook.sheet_names().len() > 3 {
                return Err(AttachmentError::TooLarge(
                    "Workbook exceeds three sheets".into(),
                ));
            }
            let mut sheets = Vec::new();
            for name in workbook.sheet_names().to_vec() {
                let range = workbook
                    .worksheet_range(&name)
                    .map_err(|e| invalid(e.to_string()))?;
                if range.height() > 200 || range.width() > 30 {
                    return Err(AttachmentError::TooLarge(
                        "Worksheet exceeds 200 rows or 30 columns".into(),
                    ));
                }
                let rows = range
                    .rows()
                    .map(|row| row.iter().map(ToString::to_string).collect())
                    .collect();
                sheets.push(Sheet { name, rows });
            }
            sheets
        }
        "clipboard-html" => html(bytes)?,
        _ => return Err(invalid("Unsupported table format".into())),
    };
    let attachment = Attachment::<ImageBytes>::Table {
        name: name.into(),
        source_format: format.into(),
        sheets,
    };
    validate_metadata(std::slice::from_ref(&attachment))?;
    Ok(attachment)
}

fn html(bytes: &[u8]) -> Result<Vec<Sheet>, AttachmentError> {
    use scraper::{Html, Selector};
    let text = std::str::from_utf8(bytes)
        .map_err(|_| AttachmentError::Invalid("Clipboard HTML must be UTF-8".into()))?;
    let document = Html::parse_fragment(text);
    let selector = |s: &str| Selector::parse(s).expect("constant CSS selector");
    let tables = selector("table");
    let trs = selector("tr");
    let cells = selector("th,td");
    let mut sheets = Vec::new();
    for table in document.select(&tables) {
        if sheets.len() == 3 {
            return Err(AttachmentError::TooLarge(
                "Clipboard exceeds three tables".into(),
            ));
        }
        if table.select(&tables).next().is_some() {
            return Err(AttachmentError::Invalid(
                "Nested tables are unsupported".into(),
            ));
        }
        let mut rows = Vec::new();
        for tr in table.select(&trs) {
            let mut row = Vec::new();
            for cell in tr.select(&cells) {
                if ["rowspan", "colspan"]
                    .iter()
                    .any(|attr| cell.value().attr(attr).is_some_and(|n| n != "1"))
                {
                    return Err(AttachmentError::Invalid(
                        "Merged clipboard cells are unsupported; paste TSV instead".into(),
                    ));
                }
                let value = cell
                    .descendants()
                    .filter_map(|node| {
                        if node.ancestors().any(|n| {
                            n.value().as_element().is_some_and(|e| {
                                matches!(e.name(), "script" | "style" | "template")
                            })
                        }) {
                            return None;
                        }
                        node.value().as_text().map(|t| t.to_string())
                    })
                    .collect::<String>();
                row.push(value);
                if row.len() > 30 {
                    return Err(AttachmentError::TooLarge("Table exceeds 30 columns".into()));
                }
            }
            rows.push(row);
            if rows.len() > 200 {
                return Err(AttachmentError::TooLarge("Table exceeds 200 rows".into()));
            }
        }
        sheets.push(Sheet {
            name: format!("Sheet{}", sheets.len() + 1),
            rows,
        });
    }
    if sheets.is_empty() {
        return Err(AttachmentError::Invalid(
            "Clipboard HTML contains no table".into(),
        ));
    }
    Ok(sheets)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_tables_preserve_empty_and_quoted_cells() {
        let Attachment::Table { sheets, .. } =
            prepare("a.csv", "csv", b"name,value,empty\nMaya,\"1,200\",\n").unwrap()
        else {
            panic!()
        };
        assert_eq!(sheets[0].rows[1], ["Maya", "1,200", ""]);
        let Attachment::Table { sheets, .. } = prepare(
            "paste",
            "clipboard-html",
            b"<table><tr><td>A&amp;B<script>bad()</script></td><td></td></tr></table>",
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(sheets[0].rows[0], ["A&B", ""]);
        assert!(prepare("paste", "clipboard-html", b"<p>No table</p>").is_err());
    }
}
