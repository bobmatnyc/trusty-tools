//! Gmail `history.list` polling + profile bootstrap for eventstream listeners.
//!
//! Why: trusty-agents' listener polling engine (#3820, DOC-54
//! SPEC-AGENTS-06 §7.1.2) needs to detect new inbound mail without a
//! Pub/Sub subscription. Gmail's documented zero-setup path is
//! `users.getProfile` (to bootstrap a starting `historyId` cursor) followed
//! by repeated `users.history.list(startHistoryId=...)` calls, each of which
//! returns only what changed since the last cursor.
//! What: `get_gmail_profile` returns the account's current `historyId` (used
//! once, to baseline a brand-new listener with no persisted cursor).
//! `list_gmail_history` fetches changes since a cursor, optionally scoped to
//! `messageAdded` history records and a single label (mirrors the listener's
//! stage-one `label_ids` filter, DOC-54 §5.3). Both are called as direct
//! library functions by trusty-agents' polling engine — never through the
//! MCP JSON-RPC path (`server.rs`) that `trusty-gworkspace-mcp`'s tool
//! handlers use.
//! Test: `history_list_url_includes_label_and_cursor` covers URL
//! construction; live API behavior (410 GONE on an expired cursor, real
//! history records) is exercised manually per the listener's own tests.

use anyhow::Result;
use serde_json::Value;

use crate::api::client::BaseClient;
use crate::api::constants::GMAIL_API_BASE;

/// Fetch the authenticated user's Gmail profile, primarily for its current
/// `historyId` (used to baseline a new listener's cursor).
///
/// Why: A listener with no persisted cursor must not replay the entire
/// mailbox history on first run — it should start watching from "now".
/// What: `GET users/me/profile`; the response's `historyId` field is a
/// `history.list`-compatible starting cursor.
/// Test: Live API only (an authenticated GET with no request body).
pub async fn get_gmail_profile(client: &BaseClient, account: Option<&str>) -> Result<Value> {
    let url = format!("{GMAIL_API_BASE}/users/me/profile");
    client.get(&url, account).await
}

/// List Gmail history records since `start_history_id`.
///
/// Why: The incremental-sync primitive behind the `history-poll` transport
/// (DOC-54 §7.1.2) — cheaper than re-searching the mailbox on every poll,
/// and matches what a Pub/Sub-pull listener would do with the `historyId`
/// carried in a push notification.
/// What: `GET users/me/history?startHistoryId=...`, restricted to
/// `messageAdded` records (new mail — the only history type this listener
/// currently reacts to) and optionally further scoped to one Gmail label
/// (mirrors a listener's `filter.label_ids`, applied here as the single
/// `labelId` query param Gmail's API accepts). Returns the raw response —
/// callers read `history[].messagesAdded[].message.id` for new message ids
/// and `historyId` for the next cursor. On a `410 GONE` (cursor expired) the
/// caller must drop the cursor and re-baseline via `get_gmail_profile`.
/// Test: `history_list_url_includes_label_and_cursor`.
pub async fn list_gmail_history(
    client: &BaseClient,
    account: Option<&str>,
    start_history_id: &str,
    label_id: Option<&str>,
) -> Result<Value> {
    let url = history_list_url(start_history_id, label_id);
    client.get(&url, account).await
}

/// Pure URL builder for [`list_gmail_history`] — split out so query-string
/// construction is unit-testable without a live client.
fn history_list_url(start_history_id: &str, label_id: Option<&str>) -> String {
    let mut url = format!(
        "{GMAIL_API_BASE}/users/me/history?startHistoryId={start_history_id}&historyTypes=messageAdded"
    );
    if let Some(label) = label_id {
        url.push_str("&labelId=");
        url.push_str(label);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_list_url_includes_label_and_cursor() {
        let url = history_list_url("12345", Some("INBOX"));
        assert!(url.contains("startHistoryId=12345"));
        assert!(url.contains("historyTypes=messageAdded"));
        assert!(url.contains("labelId=INBOX"));
    }

    #[test]
    fn history_list_url_omits_label_when_absent() {
        let url = history_list_url("999", None);
        assert!(!url.contains("labelId"));
        assert!(url.contains("startHistoryId=999"));
    }
}
