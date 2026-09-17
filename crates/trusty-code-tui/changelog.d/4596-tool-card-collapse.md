Breaking

- A completed tool-call card collapses to a one-line summary — `⏺ read_file(src/lib.rs) · ok · 3 lines` — instead of printing its whole result body into the scrollback. **Ctrl-O** expands or collapses the newest card, and `/help` now lists it under a `Keys:` section. A card whose result failed renders expanded, so the error text never needs a keystroke to read, and its glyph is red. No duration is shown: `ReplEvent::ToolInvocation` carries no timing, and timing the TUI's own event arrival would measure the wrong thing (#4596).
- Failure is read from the result's leading `ERROR:` / `FAILED:` marker, which is what the producers already write (`crates/trusty-code/src/tui_client/session_events.rs`); no event field changed.
- `ToolCard` gained a `collapsed: bool` field, so a `ToolCard { .. }` literal must now supply it. New public items: `ToolCard::is_error`, `ReplApp::toggle_last_tool_card`.
