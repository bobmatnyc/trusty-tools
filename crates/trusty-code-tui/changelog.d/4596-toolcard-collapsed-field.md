Breaking

- `ReplEvent::ToolInvocation` gained `failed: bool` and `ToolCard` gained `collapsed: bool` and `failed: bool`, so a literal of either must now supply them. The one in-workspace producer (`trusty-code`) is updated in the same change, and 0.2.0 is not yet on crates.io, so no released consumer is affected. New public items: `ToolCard::is_error`, `ReplApp::toggle_last_tool_card` (#4596).
