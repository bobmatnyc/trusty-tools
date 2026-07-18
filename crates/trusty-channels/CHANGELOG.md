# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- Cursor pagination for `slack_read_channel` and `slack_read_thread`: both tools now accept an opaque `cursor` (echo back a prior call's `next_cursor` to fetch the next page) and both return `next_cursor`/`has_more`; `slack_read_channel` additionally accepts `oldest`/`latest` ts time-window bounds. Lets a caller walk a channel's or thread's full history across repeated calls instead of being capped at one page (closes #2996)

### Fixed

- `slack_read_thread`'s `thread_ts` argument is now validated against Slack's `seconds.microseconds` ts shape before any network call, failing fast with an actionable message (naming the expected format) instead of forwarding a malformed value to Slack and surfacing an opaque `invalid_arguments` (#2996)
- `conversations.history` / `conversations.replies` `limit` is now clamped to Slack's documented 999-message ceiling for that method family (previously shared the 1000 ceiling meant for `search.messages`)

### Changed

- `slack::handlers` split from a single file into a `handlers/` module tree (`args`, `clean`, `messaging`, `read`, `lookup`, `search`) to stay under the workspace's 500-SLOC production-file cap; the public `dispatch` entry point is unchanged
