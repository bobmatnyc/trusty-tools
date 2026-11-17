//! The `gmail` adapter (SPEC-OKF-01 §2.15, Appendix C "Gmail").
//!
//! Why: criterion 98's `in:sent` half lives here. A run no operator started,
//! including every scheduled refresh, reads SENT mail only, and no store or
//! caller query may widen it; an operator-initiated run reads inbound mail
//! only after a passed capability-grant review (3l C). The adapter must
//! also never report the end of a listing it cut at its own ceiling.
//! What: the selection is `account`, `query`, an optional `after`/`before`
//! window, `mailbox` (`sent`, the default, or `all` for inbound) and
//! `max_items` (the adapter's ceiling, default 5,000). [`listing_query`]
//! builds `in:sent (<query and window>)` for SENT-only runs, refusing a
//! query with unbalanced quotes, parentheses or braces. [`authorise`]
//! refuses `mailbox = all` unless the run is operator-initiated and the
//! grant file covers the store and account. A `list` is one
//! `messages.list` page: entries carry the message ID (also the revision)
//! and the thread ID; a page that reaches the ceiling with a page token
//! left returns that token with `complete: false`, and later pages are
//! empty, incomplete and repeat the cursor, so the host stops and reports
//! the listing incomplete. A `fetch` reads one `format=full` message (never
//! raw MIME) and, in a SENT-only run, refuses a message without the `SENT`
//! label; [`message_record`] maps it to a `SourceRecord`: plain and HTML
//! bodies as parts, attachments listed by name, type and size, the snippet,
//! `internalDate` and the `Date` header.
//! Test: `gmail::tests`, `c98_*` and `c87_*` (`tests/conformance.rs`).
//!
//! # Spec References
//! - SPEC-OKF-01 §2.15 "Inbound email", items 1-4, 11, 12; Appendix C.

use std::ffi::OsString;

use serde_json::{Map, Value};
use trusty_gworkspace::api::bulk::{
    PAGE_SIZE, gmail_list_page, gmail_message, gmail_query, message_attachments, message_bodies,
    message_header,
};
use trusty_okf::import::record::{Attachment, Part, SourceRecord};

use crate::config::Config;
use crate::google::{Google, account, ceiling, fault, run_main, text};
use crate::server::{Adapter, Entry, Fault, Page, Request};

/// The label every SENT message carries.
pub const SENT_LABEL: &str = "SENT";

/// The `okf-adapter-gmail` entry point; returns the exit code.
pub fn main(args: &[OsString]) -> i32 {
    run_main(args, Gmail::new)
}

/// What one request selects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The account label.
    pub account: String,
    /// The query with its window, before the SENT rule.
    pub query: String,
    /// `mailbox = all`: inbound mail is asked for.
    pub inbound: bool,
    /// The adapter's listing ceiling.
    pub ceiling: usize,
}

/// Read a request's selection. Errors: `selection_required`,
/// `selection_invalid`.
pub fn selection(sel: &Map<String, Value>) -> Result<Selection, Fault> {
    let inbound = match text(sel, "mailbox")?.as_deref() {
        None | Some("sent") => false,
        Some("all") => true,
        Some(other) => {
            return Err(Fault::new(
                "selection_invalid",
                format!("mailbox {other}: expected sent or all"),
            ));
        }
    };
    Ok(Selection {
        account: account(sel)?,
        query: gmail_query(
            &text(sel, "query")?.unwrap_or_default(),
            text(sel, "after")?.as_deref(),
            text(sel, "before")?.as_deref(),
        ),
        inbound,
        ceiling: ceiling(sel)?,
    })
}

/// Whether quotes close and parentheses and braces nest (outside quotes).
fn balanced(q: &str) -> bool {
    let (mut paren, mut brace, mut quoted) = (0i64, 0i64, false);
    for c in q.chars() {
        match c {
            '"' => quoted = !quoted,
            _ if quoted => {}
            '(' => paren += 1,
            ')' => paren -= 1,
            '{' => brace += 1,
            '}' => brace -= 1,
            _ => {}
        }
        if paren < 0 || brace < 0 {
            return false;
        }
    }
    !quoted && paren == 0 && brace == 0
}

/// The query `messages.list` receives (§2.15 "Inbound email").
///
/// A SENT-only run gets `in:sent` ANDed in front of the whole caller query,
/// grouped, so no term of it can remove the scope. Errors:
/// `selection_invalid` for a query whose grouping does not balance.
pub fn listing_query(query: &str, inbound: bool) -> Result<String, Fault> {
    if inbound {
        return Ok(query.to_string());
    }
    if !balanced(query) {
        return Err(Fault::new(
            "selection_invalid",
            "the query's quotes, parentheses or braces do not balance",
        ));
    }
    Ok(if query.trim().is_empty() {
        "in:sent".to_string()
    } else {
        format!("in:sent ({query})")
    })
}

/// Refuse an inbound request the run may not make (3l C).
pub fn authorise(
    req: &Request,
    sel: &Selection,
    config: &Result<Config, String>,
) -> Result<(), Fault> {
    if !sel.inbound {
        return Ok(());
    }
    let refuse = |why: String| Err(Fault::new("policy_refused", why));
    if !req.operator_initiated {
        return refuse("inbound mail needs an operator-initiated run (SPEC-OKF-01 §2.15)".into());
    }
    match config {
        Err(e) => refuse(format!(
            "inbound mail refused: the grant file is unusable: {e}"
        )),
        Ok(c) if !c.inbound_granted(&req.store, &sel.account) => refuse(format!(
            "inbound mail refused: no passed capability-grant review for store {} account {}",
            req.store, sel.account
        )),
        Ok(_) => Ok(()),
    }
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_string)
}

/// Whether a `format=full` message carries `label`.
pub fn has_label(msg: &Value, label: &str) -> bool {
    msg.get("labelIds")
        .and_then(Value::as_array)
        .is_some_and(|l| l.iter().any(|x| x.as_str() == Some(label)))
}

/// The record of a `format=full` message (Appendix C "Gmail").
pub fn message_record(msg: &Value) -> SourceRecord {
    let id = str_of(msg, "id").unwrap_or_default();
    let payload = msg.get("payload").unwrap_or(&Value::Null);
    let (plain, html) = message_bodies(payload);
    let parts = [("text/plain", plain), ("text/html", html)]
        .into_iter()
        .filter_map(|(mime, data)| {
            data.filter(|d| !d.trim().is_empty()).map(|data| Part {
                mime: mime.to_string(),
                encoding: None,
                data,
            })
        })
        .collect();
    let mut dates = std::collections::BTreeMap::new();
    let internal = msg.get("internalDate").and_then(|v| {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    });
    if let Some(ms) = internal {
        dates.insert("internal_date".to_string(), ms);
    }
    if let Some(d) = message_header(msg, "Date") {
        dates.insert("date_header".to_string(), d);
    }
    let mut fields = Map::new();
    for (key, header) in [("from", "From"), ("to", "To"), ("cc", "Cc")] {
        if let Some(v) = message_header(msg, header) {
            fields.insert(key.to_string(), Value::String(v));
        }
    }
    SourceRecord {
        item_id: id.clone(),
        group: str_of(msg, "threadId"),
        revision: Some(id),
        title: message_header(msg, "Subject"),
        dates,
        parts,
        snippet: str_of(msg, "snippet"),
        attachments: message_attachments(payload)
            .into_iter()
            .map(|a| Attachment {
                name: a.name,
                mime: a.mime,
                size: a.size,
            })
            .collect(),
        notes: Vec::new(),
        fields,
    }
}

/// The `gmail` adapter of one process.
pub struct Gmail {
    google: Google,
    config: Result<Config, String>,
    /// Entries listed so far in this process (the ceiling's count).
    listed: usize,
}

impl Gmail {
    /// An adapter over `google` with the grant file `config`.
    pub fn new(google: Google, config: Result<Config, String>) -> Gmail {
        Gmail {
            google,
            config,
            listed: 0,
        }
    }
}

impl Adapter for Gmail {
    const KIND: &'static str = "gmail";

    async fn list(&mut self, req: &Request) -> Result<Page, Fault> {
        let sel = selection(&req.selection)?;
        authorise(req, &sel, &self.config)?;
        let q = listing_query(&sel.query, sel.inbound)?;
        // #10746 S13: at the ceiling, never say the listing ended; repeat
        // the cursor so the host stops with it still left.
        if self.listed >= sel.ceiling {
            return Ok(Page {
                entries: Vec::new(),
                cursor: req.cursor.clone(),
                complete: false,
            });
        }
        let room = PAGE_SIZE.min(sel.ceiling - self.listed);
        let page = gmail_list_page(
            &self.google.client,
            Some(&sel.account),
            &q,
            req.cursor.as_deref(),
            room,
        )
        .await
        .map_err(|e| fault(&e))?;
        let over = page.messages.len() > room;
        let entries: Vec<Entry> = page
            .messages
            .into_iter()
            .take(room)
            .map(|(id, thread)| Entry {
                revision: Some(id.clone()),
                item_id: id,
                group: thread,
                ..Entry::default()
            })
            .collect();
        self.listed += entries.len();
        if over {
            // More than asked for: the page is re-listed on resume.
            return Ok(Page {
                entries,
                cursor: req.cursor.clone(),
                complete: false,
            });
        }
        let cut = self.listed >= sel.ceiling && page.next.is_some();
        Ok(Page {
            entries,
            cursor: page.next,
            complete: !cut,
        })
    }

    async fn fetch(&mut self, req: &Request) -> Result<SourceRecord, Fault> {
        let sel = selection(&req.selection)?;
        authorise(req, &sel, &self.config)?;
        let id = req
            .item_id
            .as_deref()
            .ok_or_else(|| Fault::new("protocol_error", "fetch names no item_id"))?;
        let msg = gmail_message(&self.google.client, Some(&sel.account), id)
            .await
            .map_err(|e| fault(&e))?;
        if msg.get("id").and_then(Value::as_str) != Some(id) {
            return Err(Fault::new(
                "protocol_error",
                format!("asked for {id}, got another message"),
            ));
        }
        // #10746 S13: the SENT scope holds per message too, whatever the
        // listing returned.
        if !sel.inbound && !has_label(&msg, SENT_LABEL) {
            return Err(Fault::new(
                "policy_refused",
                format!("message {id} is not SENT mail, and this run reads SENT mail only"),
            ));
        }
        Ok(message_record(&msg))
    }
}

#[cfg(test)]
#[path = "gmail_tests.rs"]
mod tests;
