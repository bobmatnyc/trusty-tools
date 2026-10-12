Added
- `tm hook --egress-guard` denies the path tool calls (`Read`, `Edit`, `Write`, `NotebookEdit`, `Glob`, `Grep`, `Bash`) of a channel-tainted session, and of a call that names no `session_id` (#8433).
- At `SessionStart` the daemon re-applies a tainted managed session's taint to the new Claude session id before it answers. When it cannot, the `SessionStart` hook call fails, and the hook taints the new id itself so the egress guard still denies; a double failure is recorded for `tm doctor` (#8433).
- A deliberate stop or a decommission of a managed session retires its taint files; nothing else removes them. A new `tm doctor` row, `channel_taint`, lists orphaned taint files, failed re-applies and double failures, and deletes nothing (#8433).
