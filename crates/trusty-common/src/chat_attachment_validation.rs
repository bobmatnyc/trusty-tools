//! Bounded validation at every attachment trust boundary (#7370).
use super::*;
use base64::Engine;

/// Why: callers must distinguish malformed input, resource limits and unsupported content.
/// What: validation errors preserve these categories for structured transport responses.
/// Test: `attachment_limits_and_signatures_are_enforced`; HTTP mapping is covered by trusty-agents attachment_errors_are_structured.
#[derive(Debug, thiserror::Error)]
pub enum AttachmentError {
    #[error("Malformed attachment: {0}")]
    Malformed(String),
    #[error("Attachment exceeds limits: {0}")]
    TooLarge(String),
    #[error("Invalid attachment: {0}")]
    Invalid(String),
}
fn invalid(message: &str) -> AttachmentError {
    AttachmentError::Invalid(message.into())
}
fn large(message: &str) -> AttachmentError {
    AttachmentError::TooLarge(message.into())
}

/// Why: declared MIME/size cannot authorize arbitrary uploaded bytes.
/// What: decode bounded base64, verify codec, dimensions, pixel budget and full image decoding.
/// Test: `attachment_limits_and_signatures_are_enforced`.
pub fn decode_image(mime: &str, encoded: &str) -> Result<Vec<u8>, AttachmentError> {
    if encoded.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
        return Err(large("image is over 5 MiB"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| AttachmentError::Malformed("Invalid base64".into()))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(large("image is over 5 MiB"));
    }
    let format = match mime {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        _ => return Err(invalid("only PNG, JPEG and WebP images are supported")),
    };
    if image::guess_format(&bytes).ok() != Some(format) {
        return Err(invalid("image signature does not match MIME"));
    }
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|e| invalid(&e.to_string()))?;
    if width == 0
        || height == 0
        || width > 8192
        || height > 8192
        || u64::from(width) * u64::from(height) > 24_000_000
    {
        return Err(large(
            "image dimensions exceed 8192 per side or 24 million pixels",
        ));
    }
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().map_err(|e| invalid(&e.to_string()))?;
    Ok(bytes)
}
/// Why: both incoming bytes and persisted references share bounded table metadata.
/// What: reject invalid names, formats, item counts, dimensions and aggregate cell characters.
/// Test: `attachment_limits_and_signatures_are_enforced`.
pub fn validate_metadata<I>(attachments: &[Attachment<I>]) -> Result<(), AttachmentError> {
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(large("at most four attachments"));
    }
    let mut chars = 0usize;
    for item in attachments {
        if item.name().trim().is_empty()
            || item.name().chars().count() > 255
            || item.name().contains('\0')
        {
            return Err(invalid("invalid filename"));
        }
        if let Attachment::Table {
            source_format,
            sheets,
            ..
        } = item
        {
            if !["clipboard-html", "clipboard-tsv", "csv", "xlsx"].contains(&source_format.as_str())
            {
                return Err(invalid("unsupported table format"));
            }
            if sheets.is_empty() || sheets.len() > 3 {
                return Err(large("one to three sheets required"));
            }
            for sheet in sheets {
                if sheet.name.chars().count() > 255 || sheet.rows.len() > 200 {
                    return Err(large("sheet name or rows exceed limit"));
                }
                for row in &sheet.rows {
                    if row.len() > 30 {
                        return Err(large("at most 30 columns"));
                    }
                    for cell in row {
                        chars = chars.saturating_add(cell.chars().count());
                    }
                }
            }
        }
    }
    if chars > MAX_TABLE_CHARS {
        return Err(large("table text exceeds 50000 characters"));
    }
    Ok(())
}
/// Why: API and memory boundaries must enforce identical image limits.
/// What: validate metadata and complete image decoding, then enforce the aggregate byte budget.
/// Test: `attachment_limits_and_signatures_are_enforced`.
pub fn validate_inputs(attachments: &[InputAttachment]) -> Result<(), AttachmentError> {
    validate_metadata(attachments)?;
    let mut total = 0;
    for item in attachments {
        if let Attachment::Image {
            mime_type, image, ..
        } = item
        {
            total += decode_image(mime_type, &image.data_base64)?.len();
        }
    }
    if total > MAX_TOTAL_BYTES {
        return Err(large("images exceed 10 MiB total"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attachment_wire_roundtrips_image_and_table() {
        let image = serde_json::json!({"kind":"image","name":"a.png","mime_type":"image/png","data_base64":"YWJj"});
        let typed: InputAttachment = serde_json::from_value(image.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), image);
        let table = serde_json::json!({"kind":"table","name":"a.csv","source_format":"csv","sheets":[{"name":"Sheet1","rows":[["Name","Role"],["Maya",""]]}]});
        let typed: StoredAttachment = serde_json::from_value(table.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), table);
    }
    #[test]
    fn attachment_limits_and_signatures_are_enforced() {
        assert!(decode_image("image/png", "bm90LWFuLWltYWdl").is_err());
        assert!(decode_image("text/plain", "bm90LWFuLWltYWdl").is_err());
        let items = vec![Attachment::<ImageBytes>::Table {
            name: "test.csv".into(),
            source_format: "csv".into(),
            sheets: vec![Sheet {
                name: "Sheet".into(),
                rows: vec![vec!["".into(); 31]],
            }],
        }];
        assert!(validate_inputs(&items).is_err());
    }
}
