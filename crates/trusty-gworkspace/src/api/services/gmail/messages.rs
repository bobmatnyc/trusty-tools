//! Gmail message search, retrieval, attachments, compose, modify.
//!
//! Why: The bread-and-butter Gmail surface for an MCP server: search inbox,
//! read messages, download attachments, compose drafts/sends, label.
//! What: Helpers for RFC 2822 MIME composition + base64url encoding so we
//! can POST to `/users/me/messages/send` with the wire-format Google
//! expects.
//! Test: Live only.

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use super::compose;
use crate::api::client::BaseClient;
use crate::api::constants::GMAIL_API_BASE;
use crate::api::services::{account_of, opt_str, require_str};

/// Why: Search is the canonical entry to Gmail for any agent flow.
/// What: Forwards the Gmail-DSL `query` string to `users/me/messages` and returns hits.
/// Test: Live API.
pub async fn search_gmail_messages(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let query = opt_str(&args, "query").unwrap_or("");
    let max = args
        .get("max_results")
        .and_then(|v| v.as_i64())
        .unwrap_or(10);
    let url = format!(
        "{GMAIL_API_BASE}/users/me/messages?q={}&maxResults={max}",
        urlencode(query)
    );
    client.get(&url, account).await
}

/// Why: After search, fetching the parsed body is the next step.
/// What: GETs `messages/{id}?format=full`, decodes parts, returns headers + plain/HTML body.
/// Test: Live API.
pub async fn get_gmail_message_content(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "message_id")?;
    let url = format!("{GMAIL_API_BASE}/users/me/messages/{id}?format=full");
    client.get(&url, account).await
}

/// Why: Attachments live behind a separate Gmail endpoint and need base64 handling.
/// What: GETs `messages/{id}/attachments/{aid}`, returns decoded bytes (b64 in JSON).
/// Test: Live API.
pub async fn download_gmail_attachment(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let msg = require_str(&args, "message_id")?;
    let att = require_str(&args, "attachment_id")?;
    let url = format!("{GMAIL_API_BASE}/users/me/messages/{msg}/attachments/{att}");
    let resp = client.get(&url, account).await?;

    let return_content = args
        .get("return_content")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let save_path = opt_str(&args, "save_path");

    if let Some(path) = save_path
        && let Some(b64) = resp.get("data").and_then(|v| v.as_str())
    {
        let bytes = URL_SAFE_NO_PAD
            .decode(b64.trim_end_matches('='))
            .map_err(|e| anyhow!("base64 decode attachment: {e}"))?;
        std::fs::write(path, bytes)?;
        return Ok(json!({ "saved": path, "size": resp.get("size") }));
    }
    if return_content {
        return Ok(resp);
    }
    Ok(json!({
        "size": resp.get("size"),
        "attachmentId": att,
    }))
}

/// Why: Discovery: enumerate filename/MIME for every attachment before downloading.
/// What: Walks the message payload tree and emits `{filename, mime_type, attachment_id}`.
/// Test: Live API.
pub async fn list_message_attachments(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "message_id")?;
    let url = format!("{GMAIL_API_BASE}/users/me/messages/{id}?format=full");
    let msg = client.get(&url, account).await?;

    let mut atts = Vec::<Value>::new();
    if let Some(payload) = msg.get("payload") {
        collect_attachments(payload, &mut atts);
    }
    Ok(json!({ "attachments": atts }))
}

fn collect_attachments(part: &Value, out: &mut Vec<Value>) {
    if let Some(body) = part.get("body")
        && let Some(att_id) = body.get("attachmentId").and_then(|v| v.as_str())
    {
        out.push(json!({
            "attachmentId": att_id,
            "filename": part.get("filename"),
            "mimeType": part.get("mimeType"),
            "size": body.get("size"),
        }));
    }
    if let Some(parts) = part.get("parts").and_then(|v| v.as_array()) {
        for p in parts {
            collect_attachments(p, out);
        }
    }
}

/// Compose an email: send, draft, send an existing draft, or reply.
///
/// Why: One tool covers every Gmail write path, now including replies and
/// attachments at parity with the Python upstream (#2630).
/// What: Resolves plain/HTML bodies and attachments, builds the MIME envelope
/// via `compose::build_mime`, base64url-encodes it, and POSTs to the right
/// endpoint. `reply` fetches the original message to set
/// `In-Reply-To`/`References` headers and the send `threadId`, defaulting the
/// recipient/subject from the original when omitted.
/// Test: MIME construction is unit-tested in `compose`; live send is deferred.
pub async fn compose_email(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let action = opt_str(&args, "action").unwrap_or("send");

    if action == "send_draft" {
        let draft_id = require_str(&args, "draft_id")?;
        let url = format!("{GMAIL_API_BASE}/users/me/drafts/send");
        return client.post(&url, json!({ "id": draft_id }), account).await;
    }

    let plain_body = opt_str(&args, "body");
    let html_body = opt_str(&args, "html_body");
    let html_flag = args.get("html").and_then(|v| v.as_bool()).unwrap_or(false);
    // html_body wins; else the legacy `html: true` flag re-labels `body` as HTML.
    let (plain, html): (Option<&str>, Option<&str>) = if html_body.is_some() {
        (plain_body, html_body)
    } else if html_flag {
        (None, plain_body)
    } else {
        (plain_body, None)
    };

    let attachments = compose::resolve_attachments(client, account, &args).await?;

    // Reply context supplies headers, a default recipient/subject, and threadId.
    let mut extra_headers: Vec<(String, String)> = Vec::new();
    let mut thread_id: Option<String> = None;
    let mut to_default: Option<String> = None;
    let mut subject_default: Option<String> = None;
    if action == "reply" {
        let ctx = fetch_reply_context(client, account, require_str(&args, "message_id")?).await?;
        thread_id = ctx.thread_id.clone();
        if let Some(mid) = &ctx.message_id_header {
            extra_headers.push(("In-Reply-To".to_string(), mid.clone()));
            extra_headers.push(("References".to_string(), ctx.references_chain(mid)));
        }
        if opt_str(&args, "to").is_none() {
            to_default = ctx.from.clone();
        }
        if opt_str(&args, "subject").is_none() {
            subject_default = ctx.subject.as_deref().map(reply_subject);
        }
    }

    let to = match (opt_str(&args, "to"), to_default.as_deref()) {
        (Some(t), _) | (None, Some(t)) => t,
        (None, None) => {
            return Err(anyhow!(
                "'to' is required (or reply to a message that has a sender)"
            ));
        }
    };
    let subject = opt_str(&args, "subject")
        .or(subject_default.as_deref())
        .unwrap_or("");

    let parts = compose::MessageParts {
        to,
        cc: opt_str(&args, "cc"),
        bcc: opt_str(&args, "bcc"),
        subject,
        plain,
        html,
        extra_headers,
    };
    let raw = compose::encode_raw(&compose::build_mime(&parts, &attachments));

    match action {
        "send" => {
            let url = format!("{GMAIL_API_BASE}/users/me/messages/send");
            client.post(&url, json!({ "raw": raw }), account).await
        }
        "reply" => {
            let mut payload = json!({ "raw": raw });
            if let Some(tid) = thread_id {
                payload["threadId"] = json!(tid);
            }
            let url = format!("{GMAIL_API_BASE}/users/me/messages/send");
            client.post(&url, payload, account).await
        }
        "draft" => {
            let url = format!("{GMAIL_API_BASE}/users/me/drafts");
            client
                .post(&url, json!({ "message": { "raw": raw } }), account)
                .await
        }
        other => Err(anyhow!("unknown action for compose_email: {other}")),
    }
}

/// The subset of an original message needed to build a threaded reply.
///
/// Why: A well-formed reply must chain `In-Reply-To`/`References` to the
/// original `Message-ID`, ride the same `threadId`, and default To/Subject.
/// What: Carries the original thread id, `Message-ID`, `References`, subject,
/// and sender.
/// Test: `references_chain`/`reply_subject` are unit-tested below.
struct ReplyContext {
    thread_id: Option<String>,
    message_id_header: Option<String>,
    references: Option<String>,
    subject: Option<String>,
    from: Option<String>,
}

impl ReplyContext {
    /// Append this message id to any existing References chain.
    fn references_chain(&self, msg_id: &str) -> String {
        match self.references.as_deref().filter(|r| !r.is_empty()) {
            Some(prev) => format!("{prev} {msg_id}"),
            None => msg_id.to_string(),
        }
    }
}

/// Fetch the reply context (threadId + relevant headers) for a message.
///
/// Why: Replies need the original's headers to thread correctly.
/// What: GETs the message with `format=metadata` limited to the four headers
/// we consume, then extracts them case-insensitively.
/// Test: Live (network); pure header parsing is exercised via the message JSON.
async fn fetch_reply_context(
    client: &BaseClient,
    account: Option<&str>,
    message_id: &str,
) -> Result<ReplyContext> {
    let url = format!(
        "{GMAIL_API_BASE}/users/me/messages/{message_id}?format=metadata&metadataHeaders=Message-ID&metadataHeaders=References&metadataHeaders=Subject&metadataHeaders=From"
    );
    let msg = client.get(&url, account).await?;
    let thread_id = msg
        .get("threadId")
        .and_then(|v| v.as_str())
        .map(String::from);
    let headers = msg
        .get("payload")
        .and_then(|p| p.get("headers"))
        .and_then(|h| h.as_array())
        .cloned()
        .unwrap_or_default();
    let header = |name: &str| -> Option<String> {
        headers
            .iter()
            .find(|h| {
                h.get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .and_then(|h| h.get("value").and_then(|v| v.as_str()))
            .map(String::from)
    };
    Ok(ReplyContext {
        thread_id,
        message_id_header: header("Message-ID"),
        references: header("References"),
        subject: header("Subject"),
        from: header("From"),
    })
}

/// Prefix a subject with `Re: ` unless it already has one.
fn reply_subject(original: &str) -> String {
    if original
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("re:")
    {
        original.to_string()
    } else {
        format!("Re: {original}")
    }
}

/// Why: Bulk label add/remove (incl. archive/trash) is one Gmail batchModify call.
/// What: POSTs `add_label_ids` and `remove_label_ids` against `messages/batchModify`.
/// Test: Live API.
pub async fn modify_gmail_messages(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let ids: Vec<String> = args
        .get("message_ids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(anyhow!("message_ids must be a non-empty array"));
    }
    let add: Vec<String> = args
        .get("add_label_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let remove: Vec<String> = args
        .get("remove_label_ids")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let body = json!({
        "ids": ids,
        "addLabelIds": add,
        "removeLabelIds": remove,
    });
    let url = format!("{GMAIL_API_BASE}/users/me/messages/batchModify");
    client.post(&url, body, account).await
}

fn urlencode(s: &str) -> String {
    // Minimal URL encoder: replace spaces and non-alphanumerics.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_subject_prefixes_once() {
        assert_eq!(reply_subject("Question"), "Re: Question");
        // Already a reply -> unchanged, case-insensitive.
        assert_eq!(reply_subject("Re: Question"), "Re: Question");
        assert_eq!(reply_subject("RE: Loud"), "RE: Loud");
    }

    #[test]
    fn references_chain_appends_to_existing() {
        let with_prev = ReplyContext {
            thread_id: None,
            message_id_header: Some("<b@m>".to_string()),
            references: Some("<a@m>".to_string()),
            subject: None,
            from: None,
        };
        assert_eq!(with_prev.references_chain("<b@m>"), "<a@m> <b@m>");

        let no_prev = ReplyContext {
            thread_id: None,
            message_id_header: Some("<b@m>".to_string()),
            references: None,
            subject: None,
            from: None,
        };
        assert_eq!(no_prev.references_chain("<b@m>"), "<b@m>");
    }
}
