//! Message send/receive/mark-read operations.
//!
//! Why: Isolates the async I/O operations from the type definitions so each
//! file stays under the 500-SLOC cap and the async functions are grouped by
//! concern.
//! What: `send_message_to_palace`, `list_unread_messages`, `list_messages`,
//! `mark_message_read`, and the `cwd_palace_slug` / `cwd_palace_slug_at`
//! helper that resolves the current project's palace slug.
//! Test: `round_trip_send_and_inbox`, `mark_read_is_atomic_under_concurrency`,
//! `mark_read_is_idempotent`.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use trusty_common::memory_core::palace::{Drawer, RoomType};
use trusty_common::memory_core::retrieval::RememberOptions;
use trusty_common::memory_core::PalaceHandle;
use uuid::Uuid;

use super::types::{build_message_tags, Message, MSG_MARKER_TAG, TAG_READ_PREFIX};

/// Persist a message into the recipient palace.
///
/// Why: every send entry point (MCP, CLI, HTTP) needs the same write path:
/// build tags + drawer, call `remember_with_options(force=true)` (we
/// bypass the signal/noise filter because short notifications like "ping"
/// are legitimately short messages), return the new drawer id. Centralising
/// it keeps the three surfaces in lock-step.
/// What: opens a handle to the recipient palace under `data_root`, writes
/// the drawer with the message envelope tags plus the supplied creator
/// attribution tags, and returns the new drawer id. The recipient palace
/// must already exist — sending to a non-existent palace fails fast with
/// a clear error rather than silently creating an empty inbox. `creator`
/// is the writer's identity (HTTP / MCP / CLI / hook) — passed by every
/// caller so noise drawers can be traced back to their origin.
/// Test: `round_trip_send_and_inbox`.
pub async fn send_message_to_palace(
    registry: &trusty_common::memory_core::PalaceRegistry,
    data_root: &Path,
    from_palace: &str,
    to_palace: &str,
    purpose: &str,
    content: String,
    creator: crate::attribution::CreatorInfo,
) -> Result<Uuid> {
    let pid = trusty_common::memory_core::PalaceId::new(to_palace);
    let handle = registry
        .open_palace(data_root, &pid)
        .with_context(|| format!("open recipient palace {to_palace}"))?;

    let sent_at = chrono::Utc::now();
    let mut tags = build_message_tags(from_palace, to_palace, purpose, sent_at);
    creator.merge_into(&mut tags);

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
/// Why: lets unit tests drive the function without mutating the process' real
/// cwd (which races with concurrent tests). Also used by the `prompt-context`
/// hook, which must NOT trigger the lazy pin-file write (a read-only context
/// may not have write permission and the hook's stdout must stay clean for the
/// injection protocol). The pin-file read path therefore always uses the
/// non-writing variant (`project_slug_at_readonly`).
///
/// Resolution order is [`trusty_common::palace_resolve::resolve_palace`]'s and
/// is documented there: env override, committed
/// `.trusty-tools/trusty-memory.yaml` pin, git `owner/repo`, then `parent/dir`
/// of the MAIN worktree root. All git calls are best-effort and read-only; the
/// pin read never writes, keeping the hook path clean.
///
/// A pin file that exists but cannot be parsed is an ERROR here, not a
/// fallthrough (#5811) — deriving past a broken pin hands the caller a
/// plausible name and sends its writes to a palace nobody chose.
/// What: returns `Ok(slug)`, or an error when no level yields one.
/// Test: `tests::cwd_palace_slug_at_prefers_pin_file`,
/// `tests::cwd_palace_slug_at_reads_pin_from_subdir`,
/// `tests::cwd_palace_slug_at_pin_read_does_not_create_pin_file`,
/// `tests::cwd_palace_slug_at_env_override_wins`,
/// `tests::cwd_palace_slug_at_uses_git_owner_repo`, plus
/// `trusty_common::palace_resolve`'s own suite.
pub fn cwd_palace_slug_at(start: &Path) -> Result<String> {
    // #5811: every level now lives in one place. This used to inline the four
    // steps, which made trusty-memory the only crate that read the pin — the
    // other four callers derived past it, and trusty-mpm's pin-blind answer
    // became the `TRUSTY_MEMORY_PALACE` variable that outranks the pin here.
    Ok(trusty_common::palace_resolve::resolve_palace(start)?.id)
}
