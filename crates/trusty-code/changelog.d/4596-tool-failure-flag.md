Changed

- The TUI event producer reports a failed tool call as data: `ReplEvent::ToolInvocation` now carries `failed`, set from `ToolFinished`'s `success` and always true for a `ToolError`. The `FAILED: ` / `ERROR: ` prefixes in the result text stay, but only as text the operator reads — rewording them no longer decides whether the TUI renders the card as an error (#4596).
