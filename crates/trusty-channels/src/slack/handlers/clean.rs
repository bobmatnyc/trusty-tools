//! Response cleaning for the Slack tool handlers — untrusted text is escaped
//! here.
//!
//! Why: the raw Slack envelope is large and several of its text fields
//! (message bodies, display names, search-result snippets) are authored by
//! arbitrary workspace members; the model only needs the compact,
//! markup-neutralised shape. Centralising the cleaning/escaping here keeps
//! [`super::read`], [`super::lookup`], and [`super::search`] focused on their
//! own request shapes.
//! What: each `clean_*` function maps a raw Slack response fragment into a
//! compact `{..}` shape with untrusted fields passed through
//! [`mrkdwn_escape`]; platform-controlled fields (ids, `permalink`) are
//! forwarded verbatim.
//! Test: unit-tested inline below.

use serde_json::{json, Value};

use trusty_common::slack_format::mrkdwn_escape;

/// Extract a string field of a Slack response object, defaulting to `""`.
pub(super) fn field_str(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Clean a Slack `messages` array into escaped `{user, ts, text}` entries.
///
/// Why: the raw Slack envelope is large and its `text` is untrusted; the model
/// only needs author, timestamp, and markup-neutralised text.
/// What: maps each element through [`clean_message`]; a missing/!array field
/// yields an empty vec.
/// Test: `clean_messages_escapes_and_shapes`.
pub(super) fn clean_messages(resp: &Value) -> Vec<Value> {
    resp.get("messages")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(clean_message).collect())
        .unwrap_or_default()
}

/// Shape one Slack message into `{user, ts, text}` with escaped text.
fn clean_message(m: &Value) -> Value {
    json!({
        "user": field_str(m, "user"),
        "ts": field_str(m, "ts"),
        "text": mrkdwn_escape(m.get("text").and_then(Value::as_str).unwrap_or("")),
    })
}

/// Clean a Slack `channels` array into `{id, name, is_private}` with escaped names.
pub(super) fn clean_channels(resp: &Value) -> Vec<Value> {
    resp.get("channels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "id": field_str(c, "id"),
                        "name": mrkdwn_escape(c.get("name").and_then(Value::as_str).unwrap_or("")),
                        "is_private": c.get("is_private").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Clean a `search.messages` response into escaped match entries.
///
/// Why: Slack nests results under `messages.matches`, and each match's `text`,
/// `username`, and channel `name` are authored by arbitrary workspace members —
/// exactly the hostile-text vector `mrkdwn_escape` defends against. Ids and the
/// Slack-generated `permalink` are platform-controlled and forwarded verbatim so
/// the permalink stays a usable URL.
/// What: maps each `messages.matches[]` element to
/// `{channel_id, channel_name, user, ts, text, permalink}` with `channel_name`
/// and `text` escaped; a missing path yields an empty vec.
/// Test: `clean_search_matches_escapes_and_shapes`.
pub(super) fn clean_search_matches(resp: &Value) -> Vec<Value> {
    resp.get("messages")
        .and_then(|m| m.get("matches"))
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(clean_search_match).collect())
        .unwrap_or_default()
}

/// Shape one `search.messages` match with escaped untrusted text.
fn clean_search_match(m: &Value) -> Value {
    let channel = m.get("channel");
    let channel_id = channel
        .and_then(|c| c.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let channel_name = channel
        .and_then(|c| c.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "channel_id": channel_id,
        "channel_name": mrkdwn_escape(channel_name),
        "user": field_str(m, "user"),
        "ts": field_str(m, "ts"),
        "text": mrkdwn_escape(m.get("text").and_then(Value::as_str).unwrap_or("")),
        "permalink": field_str(m, "permalink"),
    })
}

/// Filter a `conversations.list` response to channels matching `query`.
///
/// Why: `slack_search_channels` has no server-side search, so it matches
/// `query` (case-insensitive) against each channel's name, topic, and purpose
/// locally. Names are still escaped for defence in depth.
/// What: keeps channels whose name / `topic.value` / `purpose.value` contains
/// `query`; returns escaped `{id, name, is_private}` entries.
/// Test: `filter_channels_matches_name_and_topic`.
pub(super) fn filter_channels(resp: &Value, query: &str) -> Vec<Value> {
    let needle = query.to_lowercase();
    resp.get("channels")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|c| channel_matches(c, &needle))
                .map(|c| {
                    json!({
                        "id": field_str(c, "id"),
                        "name": mrkdwn_escape(c.get("name").and_then(Value::as_str).unwrap_or("")),
                        "is_private": c.get("is_private").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a channel object matches the lowercased search `needle`.
///
/// Why: a single predicate keeps the name/topic/purpose match rule in one place.
/// What: `true` if the lowercased name, `topic.value`, or `purpose.value`
/// contains `needle`.
/// Test: `filter_channels_matches_name_and_topic`.
fn channel_matches(c: &Value, needle: &str) -> bool {
    let name = c.get("name").and_then(Value::as_str).unwrap_or("");
    let topic = c
        .get("topic")
        .and_then(|t| t.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let purpose = c
        .get("purpose")
        .and_then(|p| p.get("value"))
        .and_then(Value::as_str)
        .unwrap_or("");
    name.to_lowercase().contains(needle)
        || topic.to_lowercase().contains(needle)
        || purpose.to_lowercase().contains(needle)
}

/// Clean a Slack `members` array into escaped `{id, name, real_name}` entries.
pub(super) fn clean_users(resp: &Value) -> Vec<Value> {
    resp.get("members")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().map(clean_user).collect())
        .unwrap_or_default()
}

/// Shape one Slack user object into `{id, name, real_name}` with escaped names.
///
/// Why: `real_name` may live at the top level or under `profile`; both are
/// user-authored and must be escaped.
/// What: prefers top-level `real_name`, falling back to `profile.real_name`.
/// Test: `clean_user_escapes_names`.
pub(super) fn clean_user(u: &Value) -> Value {
    let real_name = u
        .get("real_name")
        .and_then(Value::as_str)
        .or_else(|| {
            u.get("profile")
                .and_then(|p| p.get("real_name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    json!({
        "id": field_str(u, "id"),
        "name": mrkdwn_escape(u.get("name").and_then(Value::as_str).unwrap_or("")),
        "real_name": mrkdwn_escape(real_name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_messages_escapes_and_shapes() {
        let resp = json!({
            "messages": [
                { "user": "U1", "ts": "1.1", "text": "<!channel> hi & bye" },
                { "user": "U2", "ts": "2.2", "text": "plain" },
            ]
        });
        let out = clean_messages(&resp);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["text"], "&lt;!channel&gt; hi &amp; bye");
        assert_eq!(out[0]["user"], "U1");
        assert_eq!(out[1]["text"], "plain");
    }

    #[test]
    fn clean_messages_missing_array_is_empty() {
        assert!(clean_messages(&json!({ "ok": true })).is_empty());
    }

    #[test]
    fn clean_user_escapes_names() {
        let u = json!({ "id": "U1", "name": "<b>", "profile": { "real_name": "A & B" } });
        let out = clean_user(&u);
        assert_eq!(out["id"], "U1");
        assert_eq!(out["name"], "&lt;b&gt;");
        assert_eq!(out["real_name"], "A &amp; B");
    }

    #[test]
    fn clean_search_matches_escapes_and_shapes() {
        let resp = json!({
            "messages": {
                "matches": [
                    {
                        "channel": { "id": "C1", "name": "gen<x>" },
                        "user": "U1",
                        "ts": "1.1",
                        "text": "<!channel> secret & stuff",
                        "permalink": "https://x.slack.com/archives/C1/p1"
                    }
                ]
            }
        });
        let out = clean_search_matches(&resp);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["channel_id"], "C1");
        assert_eq!(out[0]["channel_name"], "gen&lt;x&gt;");
        assert_eq!(out[0]["text"], "&lt;!channel&gt; secret &amp; stuff");
        assert!(!out[0]["text"].as_str().unwrap().contains('<'));
        assert_eq!(out[0]["permalink"], "https://x.slack.com/archives/C1/p1");
    }

    #[test]
    fn clean_search_matches_missing_path_is_empty() {
        assert!(clean_search_matches(&json!({ "ok": true })).is_empty());
    }

    #[test]
    fn filter_channels_matches_name_and_topic() {
        let resp = json!({
            "channels": [
                { "id": "C1", "name": "backend-alerts", "is_private": false },
                { "id": "C2", "name": "random", "topic": { "value": "ALERTS go here" } },
                { "id": "C3", "name": "general", "purpose": { "value": "chatter" } },
            ]
        });
        let out = filter_channels(&resp, "alert");
        // C1 (name) and C2 (topic, case-insensitive) match; C3 does not.
        assert_eq!(out.len(), 2);
        let ids: Vec<&str> = out.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"C1"));
        assert!(ids.contains(&"C2"));
    }

    #[test]
    fn filter_channels_escapes_names() {
        let resp = json!({ "channels": [ { "id": "C1", "name": "al<e>rt" } ] });
        let out = filter_channels(&resp, "al");
        assert_eq!(out[0]["name"], "al&lt;e&gt;rt");
    }
}
