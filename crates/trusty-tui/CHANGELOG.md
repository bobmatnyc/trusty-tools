# Changelog — trusty-tui

All notable changes to trusty-tui are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [Unreleased]

### Added

- Initial crate scaffold (Slice 1, closes #3412, part of epic #3411, DOC-50 §3/§5): the `TuiEngine` trait (`setup`, `handle_input`, `cancel_session`, `subscribe_workstream_events`, `shutdown`) and the `ReplEvent` enum (terminal input, submit/cancel, streamed assistant output, tool invocation, statusline/workstream updates, connection loss), generalized from tagent's existing `crates/trusty-agents/src/repl/tui/` REPL event model. Also ships the small shared payload stubs (`StatuslineSegment`, `PickerItem`, `CommandDescriptor`, `WorkstreamSummary`) that Slice 1.5 (#3413) will flesh out. No ratatui/crossterm dependency yet — the terminal layer lands in Slice 2 (#3414).
