Added
- PM sessions now register `tm hook --turn-task` on `UserPromptSubmit`. Each turn gets `Task: <id>@<MMDD>` as hook context when datg's session record names an open task in its `current` field; the hook is silent otherwise and never writes datg state.
