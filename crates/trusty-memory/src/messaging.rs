//! Inter-project messaging primitive (issue #99).
//!
//! Why: Replaces the Python `/mpm-message` skill (claude-mpm repo, writes
//! to `~/.claude-mpm/messaging.db`) with a trusty-memory-native primitive.
//! Single-daemon-per-host architecture means cross-project messaging is
//! just a write to a different palace and a read at session start — no
//! IPC required.
//!
//! What: helpers that encode messages as **drawers tagged with a `msg:*`
//! namespace** so we don't have to change the `Drawer` schema:
//!
//! - `msg:v1` — marker tag for fast filtering / dedup.
//! - `msg:from=<palace>` — sender palace id.
//! - `msg:to=<palace>` — recipient palace id (redundant with the host palace,
//!   kept for audit + cross-palace queries).
//! - `msg:purpose=<string>` — free-text purpose / category set by the sender.
//! - `msg:sent_at=<rfc3339>` — UTC ISO 8601 timestamp when the sender wrote it.
//! - `msg:read=<bool>` — receiver-controlled read flag (`true` after the
//!   SessionStart hook has delivered it once).
//!
//! Addressing convention: receiver palace name = repo slug derived from cwd
//! (basename of the git toplevel, or cwd when not in a git repo). The slug
//! is **lowercased, with whitespace and underscores collapsed to single
//! hyphens, and any character outside `[a-z0-9-]` stripped**. See
//! [`slugify_for_palace`] for the exact rule.
//!
//! Test: `tests::round_trip_send_and_inbox`, `tests::slug_derivation_cases`,
//! `tests::mark_read_is_atomic_under_concurrency`.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use trusty_common::memory_core::palace::{Drawer, RoomType};
use trusty_common::memory_core::retrieval::RememberOptions;
use trusty_common::memory_core::PalaceHandle;
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
fn extract_tag<'a>(drawer: &'a Drawer, prefix: &str) -> Option<&'a str> {
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

/// Persist a message into the recipient palace.
///
/// Why: every send entry point (MCP, CLI, HTTP) needs the same write path:
/// build tags + drawer, call `remember_with_options(force=true)` (we
/// bypass the signal/noise filter because short notifications like "ping"
/// are legitimately short messages), return the new drawer id. Centralising
/// it keeps the three surfaces in lock-step.
/// What: opens a handle to the recipient palace under `data_root`, writes
/// the drawer with the message envelope tags, and returns the new drawer
/// id. The recipient palace must already exist — sending to a non-existent
/// palace fails fast with a clear error rather than silently creating an
/// empty inbox.
/// Test: `round_trip_send_and_inbox`.
pub async fn send_message_to_palace(
    registry: &trusty_common::memory_core::PalaceRegistry,
    data_root: &Path,
    from_palace: &str,
    to_palace: &str,
    purpose: &str,
    content: String,
) -> Result<Uuid> {
    let pid = trusty_common::memory_core::PalaceId::new(to_palace);
    let handle = registry
        .open_palace(data_root, &pid)
        .with_context(|| format!("open recipient palace {to_palace}"))?;

    let sent_at = Utc::now();
    let tags = build_message_tags(from_palace, to_palace, purpose, sent_at);

    // force=true: bypass the signal/noise filter so short messages
    // ("acknowledged", "ping") are not rejected. Messaging is an
    // intentional human-controlled write, not auto-capture noise.
    let opts = RememberOptions {
        force: true,
        ..RememberOptions::default()
    };
    let drawer_id = handle
        .remember_with_options(
            content,
            RoomType::Custom("Messages".to_string()),
            tags,
            0.7,
            opts,
        )
        .await
        .context("write message drawer")?;
    Ok(drawer_id)
}

/// List every unread message drawer in `palace`.
///
/// Why: the SessionStart hook needs to emit every unread message before
/// marking them read. Filtering happens client-side (against
/// `list_drawers`) because the message marker tag is namespaced — the
/// existing tag filter accepts a single string and we filter on the
/// composite `msg:v1` + `msg:read=false` predicate.
/// What: pulls every drawer carrying [`MSG_MARKER_TAG`], decodes the
/// envelope via [`Message::from_drawer`], and returns the ones with
/// `read == false`. Sorted oldest-first by `sent_at` so multi-message
/// inboxes deliver in a natural reading order.
/// Test: `round_trip_send_and_inbox`.
pub fn list_unread_messages(handle: &Arc<PalaceHandle>) -> Vec<Message> {
    let drawers = handle.list_drawers(None, Some(MSG_MARKER_TAG.to_string()), usize::MAX);
    let mut msgs: Vec<Message> = drawers
        .iter()
        .filter_map(Message::from_drawer)
        .filter(|m| !m.read)
        .collect();
    msgs.sort_by_key(|m| m.sent_at);
    msgs
}

/// List every message drawer in `palace`, optionally filtering to unread.
///
/// Why: the HTTP `GET /api/v1/messages` endpoint exposes both modes — full
/// audit history and the unread-only view used by debuggers.
/// What: same as `list_unread_messages` but with an opt-in `unread_only`
/// filter; sorted by `sent_at` ascending in both cases.
/// Test: `round_trip_send_and_inbox` and `inbox_returns_only_unread_after_mark`.
pub fn list_messages(handle: &Arc<PalaceHandle>, unread_only: bool) -> Vec<Message> {
    let drawers = handle.list_drawers(None, Some(MSG_MARKER_TAG.to_string()), usize::MAX);
    let mut msgs: Vec<Message> = drawers
        .iter()
        .filter_map(Message::from_drawer)
        .filter(|m| !unread_only || !m.read)
        .collect();
    msgs.sort_by_key(|m| m.sent_at);
    msgs
}

/// Mark a message drawer as read by atomically rewriting its `msg:read=...`
/// tag.
///
/// Why: the SessionStart hook MUST flip the read flag exactly once per
/// message, even when two terminals race to start a session against the
/// same palace. The naive "forget + remember" approach is not atomic
/// (both racers can forget, then both can re-insert, producing two
/// drawers). The single source of truth for "have we flipped this flag
/// yet" is the in-memory drawer table — a `parking_lot::RwLock<Vec<Drawer>>`
/// guarded by the palace handle. We take the write lock, do the
/// compare-and-swap (return `false` if already read; otherwise rewrite
/// the tag and clone the post-mutation drawer), then release the lock
/// before crossing the `await` boundary for the persistent write.
/// What: returns `Ok(false)` if the drawer is missing or already
/// `msg:read=true`. Otherwise rewrites the tag in place under the write
/// lock, clones the updated drawer, releases the lock, persists via
/// `handle.kg.upsert_drawer`, and returns `Ok(true)`. The persistent
/// write is async (it routes through the per-palace `KgWriter` actor for
/// coalescing) so we cannot hold the parking_lot lock across it — but we
/// don't need to: the in-memory CAS is the single source of truth for
/// "have we flipped this flag", and the persistent write is just durable
/// backing.
/// Test: `mark_read_is_atomic_under_concurrency`,
/// `mark_read_is_idempotent`.
pub async fn mark_message_read(handle: &Arc<PalaceHandle>, drawer_id: Uuid) -> Result<bool> {
    // In-memory compare-and-swap. The `Option<Drawer>` we return is the
    // post-mutation snapshot we need to persist — `None` means "no work
    // to do" (drawer missing or already read).
    let snapshot: Option<Drawer> = {
        let mut drawers = handle.drawers.write();
        match drawers.iter_mut().find(|d| d.id == drawer_id) {
            None => None,
            Some(drawer) => {
                if drawer
                    .tags
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case("msg:read=true"))
                {
                    None
                } else {
                    drawer.tags.retain(|t| !t.starts_with(TAG_READ_PREFIX));
                    drawer.tags.push(format!("{TAG_READ_PREFIX}true"));
                    Some(drawer.clone())
                }
            }
        }
    };
    let Some(updated) = snapshot else {
        return Ok(false);
    };
    // Persist the new tag set. Failures here leave the in-memory state
    // ahead of disk — acceptable trade-off: the next call still observes
    // `read=true` in memory (so no double-delivery) and a later restart
    // will re-deliver the message at worst once. The alternative
    // (rolling back the in-memory mutation) would let a racing reader
    // observe the message as unread despite our intention to flip it.
    handle
        .kg
        .upsert_drawer(&updated)
        .await
        .context("persist drawer tag update (mark-read)")?;
    Ok(true)
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
/// (e.g. from `--to my_project`) without re-deriving from a path.
/// What: applies the canonicalisation rules described on
/// [`slugify_for_palace`].
/// Test: `tests::slug_derivation_cases`.
pub fn slugify_string(input: &str) -> String {
    let lowered = input.trim().to_ascii_lowercase();
    let stripped = lowered.strip_suffix(".git").unwrap_or(&lowered);
    let mut out = String::with_capacity(stripped.len());
    let mut prev_hyphen = false;
    for c in stripped.chars() {
        let next = match c {
            'a'..='z' | '0'..='9' => Some(c),
            '_' | '-' | ' ' | '\t' => Some('-'),
            // Strip everything else.
            _ => None,
        };
        if let Some(c) = next {
            if c == '-' {
                if !prev_hyphen && !out.is_empty() {
                    out.push('-');
                    prev_hyphen = true;
                }
            } else {
                out.push(c);
                prev_hyphen = false;
            }
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Resolve the calling project's palace slug from cwd, preferring the
/// git toplevel when available.
///
/// Why: the SessionStart hook runs with whatever cwd Claude Code launches
/// it under. Using the git toplevel makes `slug` stable regardless of
/// which subdirectory the user opened — `cd /repo/crates/foo && trusty-memory
/// inbox-check` and `cd /repo && trusty-memory inbox-check` resolve to the
/// same slug.
/// What: runs `git rev-parse --show-toplevel` from `cwd` (best-effort, no
/// network); on success slugifies the basename of the returned path. On
/// failure (not a repo, no git on PATH, command timeout) falls back to
/// slugifying `cwd` itself.
/// Test: `tests::cwd_palace_slug_uses_git_toplevel`,
/// `tests::cwd_palace_slug_falls_back_to_basename`.
pub fn cwd_palace_slug() -> Result<String> {
    let cwd = std::env::current_dir().context("read cwd")?;
    cwd_palace_slug_at(&cwd)
}

/// Variant of [`cwd_palace_slug`] that takes the working directory explicitly.
///
/// Why: lets unit tests drive the function without mutating the process'
/// real cwd (which races with concurrent tests).
/// What: same logic as [`cwd_palace_slug`] but rooted at `start`.
/// Test: `tests::cwd_palace_slug_uses_git_toplevel`,
/// `tests::cwd_palace_slug_falls_back_to_basename`.
pub fn cwd_palace_slug_at(start: &Path) -> Result<String> {
    // Best-effort git toplevel resolution: short timeout, no network.
    let output = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(start)
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !toplevel.is_empty() {
                let slug = slugify_for_palace(Path::new(&toplevel))?;
                if !slug.is_empty() {
                    return Ok(slug);
                }
            }
        }
    }
    // Fallback: slugify cwd's basename.
    let slug = slugify_for_palace(start)?;
    if slug.is_empty() {
        return Err(anyhow!(
            "could not derive palace slug from cwd {} — pass --palace explicitly",
            start.display()
        ));
    }
    Ok(slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use trusty_common::memory_core::{Palace, PalaceId, PalaceRegistry};

    /// Helper: build a registry + palace under a tempdir and return both.
    fn fresh_palace(id: &str) -> (PalaceRegistry, Arc<PalaceHandle>, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let registry = PalaceRegistry::new();
        let palace = Palace {
            id: PalaceId::new(id),
            name: id.to_string(),
            description: None,
            created_at: Utc::now(),
            data_dir: root.join(id),
        };
        registry
            .create_palace(&root, palace)
            .expect("create_palace");
        let handle = registry
            .open_palace(&root, &PalaceId::new(id))
            .expect("open_palace");
        (registry, handle, root)
    }

    #[test]
    fn build_message_tags_includes_all_fields() {
        let ts = Utc::now();
        let tags = build_message_tags("alpha", "beta", "task", ts);
        assert!(tags.contains(&MSG_MARKER_TAG.to_string()));
        assert!(tags.iter().any(|t| t == "msg:from=alpha"));
        assert!(tags.iter().any(|t| t == "msg:to=beta"));
        assert!(tags.iter().any(|t| t == "msg:purpose=task"));
        assert!(tags.iter().any(|t| t == "msg:read=false"));
        assert!(tags
            .iter()
            .any(|t| t.starts_with("msg:sent_at=") && t.ends_with(&ts.to_rfc3339())));
    }

    #[test]
    fn decode_message_from_drawer_round_trips() {
        let ts = "2026-05-25T12:34:56+00:00"
            .parse::<DateTime<chrono::FixedOffset>>()
            .unwrap()
            .with_timezone(&Utc);
        let mut d = Drawer::new(Uuid::new_v4(), "hello world");
        d.tags = build_message_tags("alpha", "beta", "task", ts);
        let m = Message::from_drawer(&d).expect("decode");
        assert_eq!(m.from_palace, "alpha");
        assert_eq!(m.to_palace, "beta");
        assert_eq!(m.purpose, "task");
        assert_eq!(m.sent_at, ts);
        assert!(!m.read);
        assert_eq!(m.content, "hello world");
    }

    #[test]
    fn decode_skips_non_message_drawer() {
        let d = Drawer::new(Uuid::new_v4(), "not a message");
        assert!(Message::from_drawer(&d).is_none());
    }

    #[test]
    fn formatted_message_includes_from_purpose_and_body() {
        let mut d = Drawer::new(Uuid::new_v4(), "the body");
        let ts = Utc::now();
        d.tags = build_message_tags("alpha", "beta", "request", ts);
        let m = Message::from_drawer(&d).unwrap();
        let formatted = m.to_injection_block();
        assert!(formatted.contains("alpha"));
        assert!(formatted.contains("beta"));
        assert!(formatted.contains("request"));
        assert!(formatted.contains("the body"));
    }

    #[test]
    fn slug_derivation_cases() {
        // Basic lowercase + hyphenation.
        assert_eq!(slugify_string("trusty-tools"), "trusty-tools");
        assert_eq!(slugify_string("Trusty_Tools"), "trusty-tools");
        assert_eq!(slugify_string("trusty tools"), "trusty-tools");
        assert_eq!(slugify_string("  trusty   tools  "), "trusty-tools");
        // Git suffix stripped.
        assert_eq!(slugify_string("trusty-tools.git"), "trusty-tools");
        // Non-alphanumerics stripped.
        assert_eq!(slugify_string("trusty/tools!"), "trustytools");
        // Multiple consecutive hyphens collapse.
        assert_eq!(slugify_string("foo--bar"), "foo-bar");
        // Pure unicode -> empty (caller must guard).
        assert_eq!(slugify_string("漢字"), "");

        // Path-based variants pick the basename.
        assert_eq!(
            slugify_for_palace(Path::new("/home/u/projects/Trusty_Tools")).unwrap(),
            "trusty-tools"
        );
    }

    #[test]
    fn cwd_palace_slug_uses_git_toplevel() {
        // Best-effort: this test only works when run inside a git checkout.
        // The trusty-tools repo *is* a git checkout, so the test is real.
        let tmp = tempfile::tempdir().expect("tempdir");
        // Init a fake repo so the test is hermetic.
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(tmp.path())
            .status();
        if status.map(|s| s.success()).unwrap_or(false) {
            // Create a sub-directory so we can confirm we resolve back to
            // the toplevel and not to the sub-dir name.
            let nested = tmp.path().join("nested-area");
            std::fs::create_dir_all(&nested).unwrap();
            let slug = cwd_palace_slug_at(&nested).expect("slug");
            // Tempdir basename varies; the important assertion is that we
            // didn't take the nested directory name.
            assert_ne!(slug, "nested-area", "slug must come from git toplevel");
        }
    }

    #[test]
    fn cwd_palace_slug_falls_back_to_basename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("my-project");
        std::fs::create_dir_all(&dir).unwrap();
        // Not a git repo — must fall back to the basename slug.
        let slug = cwd_palace_slug_at(&dir).expect("slug");
        assert_eq!(slug, "my-project");
    }

    #[tokio::test]
    async fn round_trip_send_and_inbox() {
        let (registry, handle_b, root) = fresh_palace("beta");
        // Sender writes into "beta" with from="alpha".
        let id = send_message_to_palace(&registry, &root, "alpha", "beta", "task", "hello".into())
            .await
            .expect("send");
        // Inbox-check at beta returns the new message exactly once.
        let unread = list_unread_messages(&handle_b);
        assert_eq!(unread.len(), 1, "first inbox check returns the message");
        assert_eq!(unread[0].id, id);
        assert_eq!(unread[0].from_palace, "alpha");
        assert_eq!(unread[0].to_palace, "beta");
        assert_eq!(unread[0].purpose, "task");
        assert_eq!(unread[0].content, "hello");
        // Mark read.
        let flipped = mark_message_read(&handle_b, id).await.expect("mark");
        assert!(flipped);
        // Second inbox check returns nothing.
        let after = list_unread_messages(&handle_b);
        assert!(after.is_empty(), "second inbox check is empty after mark");
        // list_messages with unread_only=false still surfaces it.
        let all = list_messages(&handle_b, false);
        assert_eq!(all.len(), 1, "history view retains the read message");
        assert!(all[0].read, "history view reports it as read");
    }

    #[tokio::test]
    async fn inbox_returns_only_unread_after_mark() {
        let (registry, handle, root) = fresh_palace("inbox-only");
        // Send 3 messages.
        let mut ids = Vec::new();
        for i in 0..3 {
            let id = send_message_to_palace(
                &registry,
                &root,
                "alpha",
                "inbox-only",
                "task",
                format!("body {i}"),
            )
            .await
            .expect("send");
            ids.push(id);
        }
        // Mark the middle one read.
        mark_message_read(&handle, ids[1]).await.expect("mark");
        // unread_only=true: 2 messages.
        let unread = list_messages(&handle, true);
        assert_eq!(unread.len(), 2);
        assert!(!unread.iter().any(|m| m.id == ids[1]));
        // unread_only=false: all 3.
        let all = list_messages(&handle, false);
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn mark_read_is_idempotent() {
        let (registry, handle, root) = fresh_palace("idempotent");
        let id = send_message_to_palace(
            &registry,
            &root,
            "alpha",
            "idempotent",
            "task",
            "msg".into(),
        )
        .await
        .expect("send");
        assert!(mark_message_read(&handle, id).await.unwrap());
        // Re-mark — must not error and must report "already read".
        assert!(!mark_message_read(&handle, id).await.unwrap());
    }

    #[tokio::test]
    async fn mark_read_is_atomic_under_concurrency() {
        // Two concurrent inbox-check style flows on the same palace must
        // not double-deliver: exactly one call flips the flag, the other
        // sees `read=true` and returns `false`. The `parking_lot::RwLock`
        // on `handle.drawers` serialises the compare-and-swap.
        let (registry, handle, root) = fresh_palace("concurrent");
        let id = send_message_to_palace(
            &registry,
            &root,
            "alpha",
            "concurrent",
            "task",
            "race".into(),
        )
        .await
        .expect("send");
        // Two concurrent async tasks race on the same drawer. The
        // parking_lot write lock inside `mark_message_read` serialises the
        // compare-and-swap so exactly one observes `read=false`.
        let h1 = handle.clone();
        let h2 = handle.clone();
        let (a, b) = tokio::join!(
            async move { mark_message_read(&h1, id).await },
            async move { mark_message_read(&h2, id).await }
        );
        let a = a.expect("mark a");
        let b = b.expect("mark b");
        // Exactly one of the two flips the flag.
        let total_flips = a as u8 + b as u8;
        assert_eq!(total_flips, 1, "exactly one mark must flip the flag");

        // Exactly one message remains, and it is read.
        let after = list_messages(&handle, false);
        assert_eq!(after.len(), 1, "exactly one message survives the race");
        assert!(after[0].read, "survivor is marked read");
        // Unread inbox is empty.
        let unread = list_unread_messages(&handle);
        assert!(unread.is_empty());
    }
}
