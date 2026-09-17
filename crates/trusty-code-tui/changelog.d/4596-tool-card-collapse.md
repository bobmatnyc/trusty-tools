Changed

- A completed tool-call card collapses to a one-line summary — `⏺ read_file(src/lib.rs) · ok · 3 lines` — instead of printing its whole result body into the scrollback. **Ctrl-O** expands or collapses the newest card, and `/help` now lists it under a `Keys:` section. No duration is shown: `ReplEvent::ToolInvocation` carries no timing, and timing the TUI's own event arrival would measure the wrong thing (#4596).
- A card the backend reported as failed renders expanded with a red glyph, so the error text never needs a keystroke to read. That verdict is read from `ReplEvent::ToolInvocation`'s new `failed` flag, never from the result text: the `FAILED: ` / `ERROR: ` prefixes trusty-code writes are display text, and rewording them no longer changes how a card renders (#4596).
