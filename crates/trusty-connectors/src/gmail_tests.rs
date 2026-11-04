//! Tests for the pure half of [`super`]: the SENT rule, the inbound gate
//! and the message record (#10746 S13).

use serde_json::{Value, json};

use super::*;
use crate::config::InboundGrant;

fn req(operator_initiated: bool, store: &str) -> Request {
    Request {
        request_id: 1,
        op: "list".into(),
        kind: "gmail".into(),
        store: store.into(),
        selection: Map::new(),
        operator_initiated,
        cursor: None,
        item_id: None,
    }
}

fn sel(v: Value) -> Selection {
    selection(v.as_object().unwrap()).unwrap()
}

/// Why: criterion 98 — a run no operator started always lists with
/// `in:sent`, and no query can remove it.
/// What: plain, window, negated, OR and empty queries all start with
/// `in:sent (`; unbalanced grouping is refused; `mailbox = all` keeps the
/// query as given.
/// Test: self-contained.
#[test]
fn sent_only_queries_always_carry_in_sent() {
    for q in [
        "in:inbox",
        "-in:sent",
        "a OR in:inbox",
        "{in:inbox in:spam}",
        "\"x)\" y",
    ] {
        let got = listing_query(q, false).unwrap();
        assert_eq!(got, format!("in:sent ({q})"));
    }
    let s = sel(json!({"account": "a", "query": "from:me", "after": "2026/01/01"}));
    assert_eq!(
        listing_query(&s.query, s.inbound).unwrap(),
        "in:sent (from:me after:2026/01/01)"
    );
    assert_eq!(listing_query("", false).unwrap(), "in:sent");
    for q in ["x) OR (in:inbox", "(a", "a}", "\"open", ") (", "{a)"] {
        assert_eq!(
            listing_query(q, false).unwrap_err().code,
            "selection_invalid",
            "{q}"
        );
    }
    assert_eq!(listing_query("in:inbox", true).unwrap(), "in:inbox");
}

/// Why: criterion 98 — inbound mail needs an operator-initiated run and a
/// passed grant review for exactly that store and account.
/// What: the gate's matrix, including a broken grant file.
/// Test: self-contained.
#[test]
fn inbound_needs_an_operator_run_and_a_grant() {
    let inbound = sel(json!({"account": "work", "query": "x", "mailbox": "all"}));
    let sent = sel(json!({"account": "work", "query": "x"}));
    let granted = Ok(Config {
        inbound_grant: vec![InboundGrant {
            store: "mail".into(),
            account: "work".into(),
            review: "DOC-63 S-4.4 review 1".into(),
        }],
    });
    let none: Result<Config, String> = Ok(Config::default());
    let broken: Result<Config, String> = Err("config x: bad".into());
    assert!(authorise(&req(false, "mail"), &sent, &none).is_ok());
    for (r, c) in [
        (req(false, "mail"), &granted),
        (req(true, "mail"), &none),
        (req(true, "other"), &granted),
        (req(true, "mail"), &broken),
    ] {
        assert_eq!(
            authorise(&r, &inbound, c).unwrap_err().code,
            "policy_refused"
        );
    }
    assert!(authorise(&req(true, "mail"), &inbound, &granted).is_ok());
    let bad = selection(
        json!({"account": "w", "query": "x", "mailbox": "spam"})
            .as_object()
            .unwrap(),
    );
    assert_eq!(bad.unwrap_err().code, "selection_invalid");
}

/// Why: Appendix C — the record keeps both bodies, the dates and the
/// thread, lists attachments, and keeps the snippet apart from the body.
/// What: one multipart message with an attachment.
/// Test: self-contained.
#[test]
fn message_record_maps_a_full_message() {
    use base64::Engine;
    let enc = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
    let msg = json!({
        "id": "m1", "threadId": "t1", "internalDate": "1767225600000", "snippet": "Hi",
        "labelIds": ["SENT"],
        "payload": {
            "headers": [{"name": "Subject", "value": "Plan"}, {"name": "Date", "value": "Thu, 1 Jan 2026"},
                        {"name": "To", "value": "b@example.com"}],
            "mimeType": "multipart/mixed",
            "parts": [
                {"mimeType": "text/plain", "body": {"data": enc("Body.")}},
                {"mimeType": "text/html", "body": {"data": enc("<p>Body.</p>")}},
                {"mimeType": "application/pdf", "filename": "deck.pdf", "body": {"size": 2048}}
            ]
        }
    });
    let r = message_record(&msg);
    assert_eq!((r.item_id.as_str(), r.group.as_deref()), ("m1", Some("t1")));
    assert_eq!(r.revision.as_deref(), Some("m1"));
    assert_eq!(r.title.as_deref(), Some("Plan"));
    assert_eq!(r.dates["internal_date"], "1767225600000");
    assert_eq!(r.dates["date_header"], "Thu, 1 Jan 2026");
    let mimes: Vec<_> = r.parts.iter().map(|p| p.mime.as_str()).collect();
    assert_eq!(mimes, vec!["text/plain", "text/html"]);
    assert_eq!(r.attachments[0].name, "deck.pdf");
    assert_eq!(r.attachments[0].size, 2048);
    assert_eq!(r.snippet.as_deref(), Some("Hi"));
    assert_eq!(r.fields["to"], "b@example.com");
    assert!(has_label(&msg, SENT_LABEL));
    assert!(!has_label(&json!({"labelIds": ["INBOX"]}), SENT_LABEL));
    let bare = message_record(&json!({"id": "m2", "snippet": "only"}));
    assert!(bare.parts.is_empty(), "no body part, snippet only");
}
