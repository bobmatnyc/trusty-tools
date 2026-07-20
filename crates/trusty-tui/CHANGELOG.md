# Changelog — trusty-tui

All notable changes to trusty-tui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

- Initial crate scaffold (Slice 1, closes #3412, part of epic #3411, DOC-50 §3/§5): the `TuiEngine` trait (`setup`, `handle_input`, `cancel_session`, `subscribe_workstream_events`, `shutdown`) and the `ReplEvent` enum (terminal input, submit/cancel, streamed assistant output, tool invocation, statusline/workstream updates, connection loss, clear-scrollback), generalized from tagent's existing `crates/trusty-agents/src/repl/tui/` REPL event model. Also ships the small shared payload stubs (`StatuslineSegment`, `PickerItem`, `CommandDescriptor`, `WorkstreamSummary`) that Slice 1.5 (#3413) will flesh out. No ratatui/crossterm dependency yet — the terminal layer lands in Slice 2 (#3414).
- `ReplEvent::ToolInvocation` carries an `id: String` correlation field (start/complete pairing for Slice 8 tool cards); `ReplEvent::ClearScrollback` covers `/clear` (DOC-50 §5 Slice 7); `WorkstreamActivationChanged`'s field is named `new_active_id` to match the DOC-48 §5.3 SSE wire event exactly.
