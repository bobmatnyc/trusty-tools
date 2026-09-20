Added
- A completed `finish_task` now publishes `session.events`' `task_finished` event carrying the report as typed data — status, summary, changed files, test counts — instead of only the rendered prose the `tool_finished` event already carried (#8204).
