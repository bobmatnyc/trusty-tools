//! Gmail MIME message composition: multipart bodies + attachment resolution.
//!
//! Why: Sending anything beyond a bare plain-text email — HTML alternatives,
//! attachments, replies — requires building a correct RFC 2822 / MIME
//! (multipart/alternative, multipart/mixed) envelope and base64url-encoding it
//! for the Gmail `raw` field. Keeping that construction here keeps
//! `messages.rs` focused on dispatch and under the SLOC cap.
//! What: Pure builders (`build_mime`, `encode_raw`) plus async attachment
//! resolution (`resolve_attachments`) that reads local files, decodes inline
//! base64, or fetches Drive files by id.
//! Test: The pure builders are unit-tested below; attachment fetch is live.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde_json::Value;
use uuid::Uuid;

use crate::api::client::BaseClient;
use crate::api::constants::DRIVE_API_BASE;
use crate::api::services::guess_mime_from_path;

/// A resolved attachment ready for MIME encoding.
///
/// Why: Attachment sources differ (disk / inline / Drive) but the MIME builder
/// only needs the concrete name, type, and bytes.
/// What: The normalised triple every source resolves to.
/// Test: Constructed in `build_mime_*` tests.
pub(super) struct Attachment {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// The logical fields of an outgoing message, pre-MIME.
///
/// Why: `build_mime` takes many optional inputs; a struct keeps the call site
/// readable and the argument order safe.
/// What: Recipients, subject, optional plain/HTML bodies, and extra headers
/// (e.g. `In-Reply-To`/`References` for replies).
/// Test: Exercised by every `build_mime_*` test.
pub(super) struct MessageParts<'a> {
    pub to: &'a str,
    pub cc: Option<&'a str>,
    pub bcc: Option<&'a str>,
    pub subject: &'a str,
    pub plain: Option<&'a str>,
    pub html: Option<&'a str>,
    pub extra_headers: Vec<(String, String)>,
}

/// Build the full RFC 2822 / MIME message string.
///
/// Why: Gmail's `raw` field needs a complete, correctly-framed MIME message;
/// mis-nesting alternative/mixed parts or mis-encoding bodies yields broken or
/// rejected mail.
/// What: Chooses text/plain, text/html, or multipart/alternative for the body,
/// wraps it in multipart/mixed when attachments are present, and prepends the
/// address/subject/extra headers. All leaf bodies use base64 transfer-encoding
/// so UTF-8 and boundary-colliding content are always safe.
/// Test: `build_mime_plain_only`, `build_mime_alternative`,
/// `build_mime_with_attachment`, `build_mime_reply_headers`.
pub(super) fn build_mime(parts: &MessageParts, attachments: &[Attachment]) -> String {
    let body_node = match (parts.plain, parts.html) {
        (Some(p), Some(h)) => {
            let boundary = make_boundary();
            multipart(
                "alternative",
                &boundary,
                &[text_part("plain", p), text_part("html", h)],
            )
        }
        (None, Some(h)) => text_part("html", h),
        (Some(p), None) => text_part("plain", p),
        (None, None) => text_part("plain", ""),
    };

    let content = if attachments.is_empty() {
        body_node
    } else {
        let boundary = make_boundary();
        let mut sub = Vec::with_capacity(attachments.len() + 1);
        sub.push(body_node);
        for a in attachments {
            sub.push(attachment_part(a));
        }
        multipart("mixed", &boundary, &sub)
    };

    let mut headers = vec![
        format!("To: {}", parts.to),
        format!("Subject: {}", parts.subject),
    ];
    if let Some(c) = parts.cc {
        headers.push(format!("Cc: {c}"));
    }
    if let Some(b) = parts.bcc {
        headers.push(format!("Bcc: {b}"));
    }
    for (k, v) in &parts.extra_headers {
        headers.push(format!("{k}: {v}"));
    }
    headers.push("MIME-Version: 1.0".to_string());

    // `content` begins with its own Content-Type header, so it continues the
    // header block (no blank line before it) until its own blank separator.
    format!("{}\r\n{}", headers.join("\r\n"), content)
}

/// base64url-encode a MIME string for the Gmail `raw` field.
///
/// Why: Gmail's send/draft endpoints require the whole message base64url
/// (unpadded) encoded.
/// What: Thin wrapper over the URL-safe base64 engine.
/// Test: `encode_raw_round_trips`.
pub(super) fn encode_raw(mime: &str) -> String {
    URL_SAFE_NO_PAD.encode(mime.as_bytes())
}

/// Resolve the `attachments` argument into concrete byte payloads.
///
/// Why: Callers describe attachments abstractly (path / inline base64 / Drive
/// id); the MIME builder needs bytes.
/// What: Maps each array element to an [`Attachment`], reading disk, decoding
/// base64, or fetching Drive bytes as needed.
/// Test: Live (I/O + Drive); shape covered indirectly by `build_mime` tests.
pub(super) async fn resolve_attachments(
    client: &BaseClient,
    account: Option<&str>,
    args: &Value,
) -> Result<Vec<Attachment>> {
    let Some(arr) = args.get("attachments").and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        out.push(resolve_one(client, account, item).await?);
    }
    Ok(out)
}

async fn resolve_one(
    client: &BaseClient,
    account: Option<&str>,
    item: &Value,
) -> Result<Attachment> {
    // Bare string is a local path shorthand.
    if let Some(path) = item.as_str() {
        return read_local(path, None, None);
    }
    let filename_arg = item.get("filename").and_then(|v| v.as_str());
    let mime_arg = item.get("mime_type").and_then(|v| v.as_str());

    if let Some(drive_id) = item
        .get("driveFileId")
        .or_else(|| item.get("drive_file_id"))
        .and_then(|v| v.as_str())
    {
        return fetch_drive_attachment(client, account, drive_id, filename_arg, mime_arg).await;
    }
    if let Some(b64) = item.get("content").and_then(|v| v.as_str()) {
        let filename =
            filename_arg.ok_or_else(|| anyhow!("inline attachment requires 'filename'"))?;
        // Accept both standard and url-safe base64.
        let bytes = STANDARD
            .decode(b64)
            .or_else(|_| URL_SAFE_NO_PAD.decode(b64.trim_end_matches('=')))
            .map_err(|e| anyhow!("decode inline attachment '{filename}': {e}"))?;
        let mime = mime_arg
            .map(str::to_string)
            .unwrap_or_else(|| guess_mime_from_path(filename));
        return Ok(Attachment {
            filename: filename.to_string(),
            mime,
            bytes,
        });
    }
    if let Some(path) = item
        .get("path")
        .or_else(|| item.get("local_path"))
        .and_then(|v| v.as_str())
    {
        return read_local(path, filename_arg, mime_arg);
    }
    Err(anyhow!(
        "attachment must be a path string or an object with 'path', 'content'+'filename', or 'driveFileId'"
    ))
}

fn read_local(path: &str, filename: Option<&str>, mime: Option<&str>) -> Result<Attachment> {
    let bytes = std::fs::read(path).with_context(|| format!("read attachment {path}"))?;
    let filename = filename
        .map(str::to_string)
        .unwrap_or_else(|| basename(path));
    let mime = mime
        .map(str::to_string)
        .unwrap_or_else(|| guess_mime_from_path(path));
    Ok(Attachment {
        filename,
        mime,
        bytes,
    })
}

async fn fetch_drive_attachment(
    client: &BaseClient,
    account: Option<&str>,
    drive_id: &str,
    filename: Option<&str>,
    mime: Option<&str>,
) -> Result<Attachment> {
    let meta = client
        .get(
            &format!(
                "{DRIVE_API_BASE}/files/{drive_id}?fields=id,name,mimeType&supportsAllDrives=true"
            ),
            account,
        )
        .await?;
    let meta_name = meta.get("name").and_then(|v| v.as_str());
    let meta_mime = meta.get("mimeType").and_then(|v| v.as_str());
    let raw = client
        .get_bytes(
            &format!("{DRIVE_API_BASE}/files/{drive_id}?alt=media&supportsAllDrives=true"),
            account,
        )
        .await?;
    if !raw.status.is_success() {
        return Err(anyhow!(
            "fetch Drive attachment {drive_id} failed: HTTP {}",
            raw.status.as_u16()
        ));
    }
    let filename = filename.or(meta_name).unwrap_or(drive_id).to_string();
    let mime = mime
        .map(str::to_string)
        .or_else(|| {
            raw.content_type
                .as_deref()
                .map(|ct| ct.split(';').next().unwrap_or(ct).trim().to_string())
        })
        .or_else(|| meta_mime.map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "application/octet-stream".to_string());
    Ok(Attachment {
        filename,
        mime,
        bytes: raw.bytes,
    })
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

fn make_boundary() -> String {
    format!("=_gw_{}", Uuid::new_v4().simple())
}

/// Render a base64 leaf text part (headers + encoded body).
fn text_part(subtype: &str, body: &str) -> String {
    let b64 = wrap_base64(&STANDARD.encode(body.as_bytes()));
    format!(
        "Content-Type: text/{subtype}; charset=\"UTF-8\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{b64}"
    )
}

/// Render an attachment leaf part (headers + base64 body).
fn attachment_part(a: &Attachment) -> String {
    let b64 = wrap_base64(&STANDARD.encode(&a.bytes));
    format!(
        "Content-Type: {mime}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n{b64}",
        mime = a.mime,
        name = a.filename,
    )
}

/// Wrap a multipart container around already-rendered sub-parts.
fn multipart(subtype: &str, boundary: &str, parts: &[String]) -> String {
    let mut s = format!("Content-Type: multipart/{subtype}; boundary=\"{boundary}\"\r\n\r\n");
    for p in parts {
        s.push_str(&format!("--{boundary}\r\n"));
        s.push_str(p);
        s.push_str("\r\n");
    }
    s.push_str(&format!("--{boundary}--"));
    s
}

/// Wrap base64 at 76 characters per RFC 2045 line-length limits.
fn wrap_base64(b64: &str) -> String {
    b64.as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts_plain<'a>(to: &'a str, subject: &'a str, body: &'a str) -> MessageParts<'a> {
        MessageParts {
            to,
            cc: None,
            bcc: None,
            subject,
            plain: Some(body),
            html: None,
            extra_headers: Vec::new(),
        }
    }

    #[test]
    fn encode_raw_round_trips() {
        let mime = "To: a@b.com\r\nSubject: hi\r\n\r\nbody";
        let raw = encode_raw(mime);
        let decoded = URL_SAFE_NO_PAD.decode(raw).expect("valid base64url");
        assert_eq!(String::from_utf8(decoded).unwrap(), mime);
    }

    #[test]
    fn build_mime_plain_only() {
        let p = parts_plain("to@x.com", "Hello", "the body");
        let mime = build_mime(&p, &[]);
        assert!(mime.contains("To: to@x.com\r\n"));
        assert!(mime.contains("Subject: Hello\r\n"));
        assert!(mime.contains("MIME-Version: 1.0\r\n"));
        assert!(mime.contains("Content-Type: text/plain; charset=\"UTF-8\""));
        // Body is base64 of "the body".
        assert!(mime.contains(&STANDARD.encode("the body")));
        assert!(!mime.contains("multipart/"));
    }

    #[test]
    fn build_mime_alternative() {
        let p = MessageParts {
            to: "to@x.com",
            cc: Some("cc@x.com"),
            bcc: None,
            subject: "S",
            plain: Some("plain text"),
            html: Some("<b>rich</b>"),
            extra_headers: Vec::new(),
        };
        let mime = build_mime(&p, &[]);
        assert!(mime.contains("Cc: cc@x.com\r\n"));
        assert!(mime.contains("multipart/alternative"));
        assert!(mime.contains("Content-Type: text/plain"));
        assert!(mime.contains("Content-Type: text/html"));
        assert!(mime.contains(&STANDARD.encode("<b>rich</b>")));
    }

    #[test]
    fn build_mime_with_attachment() {
        let p = parts_plain("to@x.com", "S", "body");
        let att = Attachment {
            filename: "note.txt".to_string(),
            mime: "text/plain".to_string(),
            bytes: b"attached bytes".to_vec(),
        };
        let mime = build_mime(&p, std::slice::from_ref(&att));
        assert!(mime.contains("multipart/mixed"));
        assert!(mime.contains("Content-Disposition: attachment; filename=\"note.txt\""));
        assert!(mime.contains("Content-Type: text/plain; name=\"note.txt\""));
        assert!(mime.contains(&STANDARD.encode("attached bytes")));
    }

    #[test]
    fn build_mime_reply_headers() {
        let p = MessageParts {
            to: "orig@x.com",
            cc: None,
            bcc: None,
            subject: "Re: Question",
            plain: Some("answer"),
            html: None,
            extra_headers: vec![
                ("In-Reply-To".to_string(), "<abc@mail>".to_string()),
                ("References".to_string(), "<abc@mail>".to_string()),
            ],
        };
        let mime = build_mime(&p, &[]);
        assert!(mime.contains("In-Reply-To: <abc@mail>\r\n"));
        assert!(mime.contains("References: <abc@mail>\r\n"));
        assert!(mime.contains("Subject: Re: Question\r\n"));
    }

    #[test]
    fn wrap_base64_wraps_at_76() {
        let long = "A".repeat(200);
        let wrapped = wrap_base64(&long);
        for line in wrapped.split("\r\n") {
            assert!(line.len() <= 76, "line too long: {}", line.len());
        }
        assert_eq!(wrapped.replace("\r\n", ""), long);
    }
}
