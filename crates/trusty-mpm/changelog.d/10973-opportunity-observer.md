Added

- `tm hook` appends an opportunity row to `usage/opportunity/<YYYY-MM-DD>.jsonl` for every `PostToolUse` result (any tool, MCP and subagent results included) and every `SubagentStop` final message: `tool_type`, `bytes_returned` and `would_compress_bytes` from a dry run of the existing compressor over at most 256 KiB. The ledger keeps 14 day files of at most 8 MiB each. No tool output changes, and `savings.jsonl` never receives these rows, so the 💸 statusline figure and its read cost are unchanged (#10973).
- `compress` savings rows now carry `tool_type` and `tool_name`, and `compression.jsonl` rows carry the Claude Code `session_id` and `tool_type` instead of `null` (#10973).
