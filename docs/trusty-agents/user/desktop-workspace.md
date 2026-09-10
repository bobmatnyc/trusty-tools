# Desktop workspace

The desktop workspace keeps each assistant's conversation separate while sharing
registered project folders and installed skills. See also [quickstart](quickstart.md).

## Chats and projects

Use the assistant picker to change the conversation and composer placeholder.
Chat History / Projects switches the sidebar between conversations and folders
attached to the current chat. Add folders from registered projects; unavailable
directories are omitted and canonical paths are deduplicated. Open a project to
browse files. Dot files and folders are hidden by default.

Markdown, image, and code previews occupy the main pane. The diff toggle compares
supported text files with Git HEAD. Project configuration provides index status,
index/reindex actions, and import into the selected assistant's bound OKG store.
The Knowledge Graph browser occupies the main pane and closes with its X control.

## Skills

Project configuration lists installed project skill sources. User skill settings
manage user sources. Discovery recognizes `.claude/skills`, `.agents/skills`,
`.codex/skills`, and `.trusty-agents/skills` under each scope, including flat
Markdown and package `SKILL.md` files. Source switches control Trusty discovery;
they do not prevent an external CLI from independently reading files.

The native assistant can inspect its bound skill catalog with `project_skill`.
Concierge can use `manage_skills` to move installed packages between user and
project scope. Moves validate source identity and revisions and refuse an existing
destination rather than replacing it. Skill content does not grant tool access.
Authorized built-in capabilities also appear in the assistant skill catalog.

## Channels and listeners

Channels belong to an assistant. Configure the provider, destination, enabled
state, send/receive permissions, filters, and per-binding instructions. Slack
supports reads and sends; receiving updates requires the authenticated Slack
listener to be running. Telegram currently supports sending only.

Receiving an update does not automatically send a reply to the external channel.
Use the explicit send control, or an authorized native `channel` tool call.
Listener filters combine fields with AND and alternatives within a field with OR;
exclusions win. Sender matching supports case-insensitive leading/trailing `*`,
and subject/snippet matching uses case-insensitive literal text.

## HTTP integration

These routes share the server's existing authentication and write-origin guards.
Clients should read configuration before editing and send the returned revision;
stale revisions return HTTP 409 without applying the stale update.

| Route | Contract |
| --- | --- |
| `GET /api/agents/{name}/channels` | Bindings, provider capabilities, revision, and listener configuration. |
| `PUT /api/agents/{name}/channels` | Replace bindings with `{revision, bindings}`. |
| `POST /api/agents/{name}/channels/{id}/send` | Explicitly send `{text, revision}` to the saved binding. |
| `GET /api/agents/{name}/channels/{id}/messages` | Availability, messages, and provider limitation reason. |
| `GET/PUT /api/agents/{name}/listeners` | Read or revision-check listener configuration. |
| `GET /api/project-tools/skills?path=...` | Registered project's skill sources and revision. |
| `PATCH /api/project-tools/skills` | Save `{path, revision, sources:[{id, enabled}]}`. |
| `GET /api/user-skills` | User skill sources and revision. |
| `PATCH /api/user-skills` | Save `{revision, sources:[{id, enabled}]}`. |

Tool activity and persisted incoming-event history updates flow through the
existing SSE transport and native desktop bridge. The native `channel` and
`listener_config` tools bind their identity to the assistant; event-triggered
turns do not receive these mutation tools. CLI-backed assistants receive feature
context but cannot invoke the native-only tools.
