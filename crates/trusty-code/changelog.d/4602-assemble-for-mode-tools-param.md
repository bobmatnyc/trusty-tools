Changed
- `prompt::assemble_system_prompt_for_mode` takes a new required sixth parameter, `tools: Option<&ToolRegistry>` — the registry the assembled prompt is scoped to; `None` emits no tool-instructing section (#4602).
