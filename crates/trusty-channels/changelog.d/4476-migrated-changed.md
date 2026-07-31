Changed

- `slack_send_message_draft` was investigated but not implemented: Slack has no public API to create an editable message draft (`chat.postMessage` sends immediately, `chat.scheduleMessage` schedules a send — neither creates a draft). Deliberately excluded from `TOOL_NAMES` rather than stubbed (#3616)
- `slack::handlers` split from a single file into a `handlers/` module tree (`args`, `clean`, `messaging`, `read`, `lookup`, `search`) to stay under the workspace's 500-SLOC production-file cap; the public `dispatch` entry point is unchanged
