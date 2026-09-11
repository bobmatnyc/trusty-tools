Added
- Chat threads carry text, binary and CSV attachments. A file uploaded through `POST /api/agents/{name}/sessions/{session}/attachments` is stored at `<assistant home>/attachments/<session>/<file>` beside `okg`, recorded in a per-session `manifest.json`, and retrieved by id from the same route.
- An attached turn embeds a `[[attachment:<id>]]` marker in its content, so a reload rebuilds its cards. Text and CSV reach the model as size-capped plain text under a fenced block; binary reaches it as a one-line reference.
- The chat view renders each attachment as a click-expandable card — image thumbnail, CSV preview table, text excerpt, or icon — and the composer gains a file picker and file drag-drop.
- A send naming an attachment that is not in the session manifest is refused before anything is persisted, so a rejected turn leaves chat history unchanged.
