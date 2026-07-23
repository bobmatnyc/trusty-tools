# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- Two new live Slack canvas tools (epic #3744 slice 1, `slack_canvas_*` namespace): `slack_canvas_create` (thin `canvases.create` wrapper — `title`/`channel_id` optional, `markdown` required, sent as `document_content: {type: "markdown", markdown}`; free-tier Slack teams must pass `channel_id` or Slack rejects with `free_teams_cannot_create_non_tabbed_canvases`) and `slack_canvas_lookup_sections` (thin `canvases.sections.lookup` wrapper — `canvas_id` required, optional `section_types`/`contains_text` criteria; Slack has no full-canvas-content-read API, so this returns section ids/anchors only, never document content). No markdown translation, push/pull, or sync state in this slice — that is slices 2-4. **Scope-grant caveat**: the app manifest declares `canvases:write`/`canvases:read`, but an installed app's live token may not carry newly-added scopes until the Slack app is reinstalled/re-authorized — expect `missing_scope` until then. Part of #3744
- Ten new live Slack tools for parity with the claude.ai Slack connector (epic #3611): `slack_create_canvas`, `slack_update_canvas`, `slack_read_canvas` (closes #3612); `slack_create_conversation`, `slack_list_channel_members` (closes #3613); `slack_get_reactions` (closes #3614); `slack_read_file` (closes #3615); `slack_schedule_message` (closes #3616); `slack_search_emojis`, `slack_search_users` (closes #3617) — the adapter now exposes 19 live tools total. `slack_read_canvas`/`slack_create_canvas`/`slack_update_canvas` require newly-added `canvases:read`/`canvases:write` OAuth scopes; see the README for the full per-tool scope table
- `slack_search_messages` gained an optional `scope` argument (`"public"` | `"public_and_private"`, default `"public_and_private"` — unchanged prior behaviour) that client-side-filters matches by channel privacy, in lieu of adding claude.ai's separate `slack_search_public`/`slack_search_public_and_private` tools (#3617)
- Cursor pagination for `slack_read_channel` and `slack_read_thread`: both tools now accept an opaque `cursor` (echo back a prior call's `next_cursor` to fetch the next page) plus `oldest`/`latest` ts time-window bounds, and both return `next_cursor`/`has_more`. Lets a caller walk a channel's or thread's full history across repeated calls instead of being capped at one page (closes #2996)

### Not implemented

- `slack_send_message_draft` was investigated but not implemented: Slack has no public API to create an editable message draft (`chat.postMessage` sends immediately, `chat.scheduleMessage` schedules a send — neither creates a draft). Deliberately excluded from `TOOL_NAMES` rather than stubbed (#3616)

### Fixed

- `src/bin/slack-mcp.rs`'s doc-comment falsely claimed tool calls were "not-yet-implemented" and deferred per ADR-0014; corrected to reflect that all 19 tools are live (closes #3618)
- `slack_read_thread`'s `thread_ts` argument is now validated against Slack's `seconds.microseconds` ts shape before any network call, failing fast with an actionable message (naming the expected format) instead of forwarding a malformed value to Slack and surfacing an opaque `invalid_arguments` (#2996)
- `slack_read_channel`/`slack_read_thread`'s `oldest`/`latest` arguments get the same pre-network ts-shape validation as `thread_ts` — a malformed value (e.g. precision lost via a string→float→string round trip) now fails fast with an actionable error instead of being forwarded to Slack
- `conversations.history` / `conversations.replies` `limit` is now clamped to Slack's documented 999-message ceiling for that method family (previously shared the 1000 ceiling meant for `search.messages`)

### Changed

- `slack::handlers` split from a single file into a `handlers/` module tree (`args`, `clean`, `messaging`, `read`, `lookup`, `search`) to stay under the workspace's 500-SLOC production-file cap; the public `dispatch` entry point is unchanged
