Breaking

- Tool calls render as cards in the scrollback: a `⏺ <tool>(<args>)` header, then the result with its line breaks intact, so `read_file` and `cargo test` output no longer collapses onto one line. A call and its result share one card, matched on `ReplEvent::ToolInvocation`'s `id`, and a delegated sub-agent's card stays inside its block (#4596).
- `ChatLine` gained a `tool: Option<ToolCard>` field, so a `ChatLine { role, text }` literal must now supply it. New public items: `ToolCard`, `ReplApp::tool_cards`, and `widgets::tool_card::tool_card_lines`.
