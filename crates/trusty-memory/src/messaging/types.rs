//! Message types, tag constants, and the `Message` decoding/formatting helpers.
//!
//! Why: Isolating the data types and tag-building from the send/receive async
//! logic keeps each file under the 500-SLOC cap and allows unit tests to
//! target decoding independently from I/O.
//! What: `Message`, tag-prefix constants, `build_message_tags`,
//! `extract_tag`, and the slug helpers (`slugify_string`, `slugify_for_palace`).
//! Test: `build_message_tags_includes_all_fields`,
//! `decode_message_from_drawer_round_trips`, `slug_derivation_cases`.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use trusty_common::memory_core::palace::Drawer;
use uuid::Uuid;

/// Tag namespace prefix marking a drawer as a v1 inter-project message.
///
/// Why: A single static marker tag lets `inbox-check` filter drawers by tag
/// without having to scan every `msg:*` namespaced tag — and gives the UI a
/// cheap "is this a message?" check without parsing the other tags.
/// What: The literal `"msg:v1"`. Bump the suffix if the message envelope
/// schema ever needs a breaking change.
/// Test: Indirectly via `round_trip_send_and_inbox`.
pub const MSG_MARKER_TAG: &str = "msg:v1";

/// Tag prefix carrying the sender's palace id (e.g. `msg:from=trusty-tools`).
pub const TAG_FROM_PREFIX: &str = "msg:from=";

/// Tag prefix carrying the recipient palace id (e.g. `msg:to=claude-mpm`).
pub const TAG_TO_PREFIX: &str = "msg:to=";

/// Tag prefix carrying the sender-defined purpose (e.g. `msg:purpose=task`).
pub const TAG_PURPOSE_PREFIX: &str = "msg:purpose=";

/// Tag prefix carrying the RFC3339 send timestamp (e.g.
/// `msg:sent_at=2026-05-25T12:34:56+00:00`).
pub const TAG_SENT_AT_PREFIX: &str = "msg:sent_at=";

/// Tag prefix carrying the read flag (`msg:read=false` or `msg:read=true`).
pub const TAG_READ_PREFIX: &str = "msg:read=";

/// Decoded view of a message drawer.
///
/// Why: `inbox-check` and the HTTP `GET /api/v1/messages` endpoint both want
/// a typed view of every message field, not the raw `Vec<String>` of tags.
/// What: Owned strings plus the drawer id and content, populated by
/// [`Message::from_drawer`].
/// Test: `decode_message_from_drawer_round_trips`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub from_palace: String,
    pub to_palace: String,
    pub purpose: String,
    pub sent_at: DateTime<Utc>,
    pub read: bool,
    pub content: String,
}

impl Message {
    /// Decode a drawer that carries the message tag namespace.
    ///
    /// Why: drawers are stored verbatim and the message envelope lives in
    /// `tags`; centralising the parse keeps the inbox handler clean and
    /// surfaces any malformed-tag failures with a uniform error.
    /// What: returns `Some(Message)` when the drawer carries the
    /// [`MSG_MARKER_TAG`] and every required field is present and parseable;
    /// returns `None` (with a debug log) on any missing-field or parse error
    /// so a single corrupt drawer can't poison the whole inbox. Unknown
    /// `read` values default to `false` — better to re-deliver a message
    /// than to silently swallow it.
    /// Test: `decode_message_from_drawer_round_trips`,
    /// `decode_skips_non_message_drawer`.
    pub fn from_drawer(drawer: &Drawer) -> Option<Self> {
        if !drawer.tags.iter().any(|t| t == MSG_MARKER_TAG) {
            return None;
        }
        let from_palace = extract_tag(drawer, TAG_FROM_PREFIX)?.to_string();
        let to_palace = extract_tag(drawer, TAG_TO_PREFIX)?.to_string();
        let purpose = extract_tag(drawer, TAG_PURPOSE_PREFIX)?.to_string();
        let sent_at_raw = extract_tag(drawer, TAG_SENT_AT_PREFIX)?;
        let sent_at = DateTime::parse_from_rfc3339(sent_at_raw)
            .ok()?
            .with_timezone(&Utc);
        let read = extract_tag(drawer, TAG_READ_PREFIX)
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Some(Message {
            id: drawer.id,
            from_palace,
            to_palace,
            purpose,
            sent_at,
            read,
            content: drawer.content.clone(),
        })
    }

    /// Format the message as the Markdown block the SessionStart hook
    /// injects via stdout.
    ///
    /// Why: Claude Code's SessionStart hook ingests stdout verbatim, so the
    /// receiver needs a self-contained, model-readable block per message
    /// (who, why, when, and the body) rather than raw JSON.
    /// What: returns a multi-line `## Message from <from>` heading plus a
    /// purpose/sent-at metadata line and the body. The caller concatenates
    /// multiple messages with a blank line between them; the receiver agent
    /// then reads them in order.
    /// Test: `formatted_message_includes_from_purpose_and_body`.
    pub fn to_injection_block(&self) -> String {
        format!(
            "## Message from {from} (purpose: {purpose})\n\
             _sent {sent_at} → {to}_\n\
             \n\
             {content}\n",
            from = self.from_palace,
            purpose = self.purpose,
            sent_at = self.sent_at.to_rfc3339(),
            to = self.to_palace,
            content = self.content
        )
    }
}

/// Extract the value of the first tag matching `prefix`.
///
/// Why: every `msg:*=...` field is encoded as a single tag entry; the
/// receiver needs to recover the value half. Returning `Option<&str>`
/// keeps the caller's error handling uniform (use `?` to bail on any
/// missing required field).
/// What: returns `Some(&str)` pointing at the substring after `prefix` of
/// the first tag whose entire text starts with `prefix`, or `None` if no
/// tag matches.
/// Test: indirectly via `decode_message_from_drawer_round_trips`.
pub(super) fn extract_tag<'a>(drawer: &'a Drawer, prefix: &str) -> Option<&'a str> {
    drawer.tags.iter().find_map(|t| t.strip_prefix(prefix))
}

/// Build the tag vector for a freshly-sent message.
///
/// Why: the send path (MCP tool, CLI, HTTP) all want the exact same tag
/// shape — centralising it here means a future schema bump only touches
/// one function.
/// What: returns `[MSG_MARKER_TAG, msg:from=…, msg:to=…, msg:purpose=…,
/// msg:sent_at=…, msg:read=false]` in that order.
/// Test: `build_message_tags_includes_all_fields`.
pub fn build_message_tags(
    from_palace: &str,
    to_palace: &str,
    purpose: &str,
    sent_at: DateTime<Utc>,
) -> Vec<String> {
    vec![
        MSG_MARKER_TAG.to_string(),
        format!("{TAG_FROM_PREFIX}{from_palace}"),
        format!("{TAG_TO_PREFIX}{to_palace}"),
        format!("{TAG_PURPOSE_PREFIX}{purpose}"),
        format!("{TAG_SENT_AT_PREFIX}{ts}", ts = sent_at.to_rfc3339()),
        format!("{TAG_READ_PREFIX}false"),
    ]
}

/// Derive a palace slug from a filesystem path.
///
/// Why: addressing inter-project messages by repo slug means we need a
/// deterministic, reversible-ish rule that maps a working-tree path to a
/// stable palace name. Git users expect the slug to match their repo name;
/// non-git working trees fall back to the directory basename. We aggressively
/// canonicalise so casing, whitespace, and underscore vs. hyphen don't
/// produce two different palaces for the same project.
/// What: returns `basename(toplevel_or_cwd).lowercase()` with:
///   - every run of whitespace or `_` collapsed to a single `-`,
///   - every character outside `[a-z0-9-]` stripped,
///   - leading / trailing `-` trimmed,
///   - consecutive `-` collapsed to one.
///
/// Examples (all yield `trusty-tools`):
///   - `/Users/bob/Projects/trusty-tools`
///   - `/Users/bob/Projects/Trusty_Tools`
///   - `/Users/bob/Projects/trusty tools/`
///   - `/Users/bob/Projects/.trusty-tools.git` (git-suffix stripped)
///
/// Test: `tests::slug_derivation_cases`.
pub fn slugify_for_palace(path: &Path) -> Result<String> {
    let raw = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("path has no final component: {}", path.display()))?;
    Ok(slugify_string(raw))
}

/// String-level slug helper used by [`slugify_for_palace`].
///
/// Why: exposed separately so the CLI can slugify an arbitrary repo name
/// (e.g. from `--to my_project`) without re-deriving from a path. As of #1348
/// the implementation lives in `trusty-common` as the single source of truth
/// (shared with trusty-controller's `tctl ensure`); this is a re-export shim
/// kept so the many `trusty_memory::messaging::slugify_string` call sites do
/// not churn.
/// What: re-exports [`trusty_common::slugify_string`] verbatim.
/// Test: canonical cases pinned in `trusty-common` (`slug::tests`); parity
/// re-asserted here in `tests::slug_derivation_cases`.
pub use trusty_common::slugify_string;
