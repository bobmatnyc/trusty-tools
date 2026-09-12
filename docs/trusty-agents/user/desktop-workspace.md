# Desktop workspace

The desktop workspace keeps each assistant's conversation separate while sharing
registered project folders and installed skills. See also [quickstart](quickstart.md).

## Chats and projects

Use the assistant picker to change the conversation and composer placeholder.
Chat History / Projects switches the sidebar between conversations and folders
available to the current chat. In the assistant's Settings, **Saved projects**
selects defaults that persist across chats and restarts. Chat attachments are saved
separately. Switching assistants loads that assistant's defaults; it does not
copy another assistant's selections. Unavailable saved folders remain listed so
you can remove them or restore access.

Add a registered project, choose a folder in the desktop app, or enter an absolute
folder path. The path must identify a readable directory on the server running
Trusty Agents; it does not need to contain a Git repository. Browser clients can
register server paths, but cannot use the native desktop folder picker. Equivalent
canonical paths are deduplicated. Open a project to browse files in the desktop
app. Dot files and folders are hidden by default.

Markdown, image, and code previews occupy the main pane. The diff toggle compares
supported text files with Git HEAD. Project configuration provides index status,
index/reindex actions, and import into the selected assistant's bound OKG store.
The Knowledge Graph browser occupies the main pane and closes with its X control.

## Pasted images and tables

The attachment contract below is under implementation; installed-build verification
is still pending ([#7370](https://github.com/bobmatnyc/trusty-tools/issues/7370)).

Paste PNG, JPEG, or WebP images into the composer, attach CSV or XLSX files, or
paste table cells as HTML or tab-separated text. Review the image or table preview
and remove unwanted attachments before sending. Ordinary pasted text remains in
the message. Table input becomes cell values; it is not executed as HTML or a
spreadsheet program.

Preparation errors retain the editable draft. Unsupported media, corrupt input,
or exceeded limits must produce a visible error. Oversize tables are rejected
without silent truncation. Sending an image also requires a provider that accepts
image input; a text-only provider must report that limitation.

| Limit | Maximum |
| --- | --- |
| Attachments per message | 4 |
| Original input size | 5 MiB each; 10 MiB combined |
| Preparation request body | 15 MiB, including base64 encoding |
| Image dimensions | 8192 pixels per side; 24 million pixels total |
| XLSX archive | 16 MiB expanded; 256 entries |
| Tables | 3 sheets; 200 rows per sheet; 30 columns per row |
| Table text | 50,000 characters combined |

History stores table metadata and opaque image references through `trusty-memory`.
Image bytes belong to the memory service, rather than browser localStorage.
Retrieval checks the assistant and session ownership. A memory daemon that cannot
persist attachments must fail visibly before inference, rather than discard them.

## Memory and settings

Ask the assistant to remember a fact, or enable its **Remember a Fact** capability.
`memory_remember` and the `memory_write` alias save facts through `trusty-memory`.
Explicit memory operations and automatic fact retention use that service. There
is no separate session memory tool or local memory fallback. Conversation context
remains transient. If storage fails, the assistant must report the failure rather
than claim the fact was saved.

Settings shows the assistant's memory namespace as read-only. Reads and writes
use that namespace by default. **Allow queries across memory palaces** is off by
default. Enabling it permits an explicit cross-palace read; writes still go to
this assistant's namespace. Existing exclusive palace bindings are retained.
Ambiguous ownership needs resolution before memory access can proceed.

Ask the assistant to change its settings or check platform health. Native
assistants call `ask_concierge`, which uses the same Settings service as the UI.
This covers model, provider, personality, permissions, memory, projects,
knowledge, skills, subagents, listeners, and channels. Changes require an explicit
user request and the applicable permissions. Background events and delegated
turns cannot change platform settings. A stale revision reloads current settings;
repeat the intended edit to save it.

## Automatic entity extraction

The knowledge pipeline inspects selected project sources and admitted incoming
event excerpts automatically. It uses the assistant's configured model without
tools to extract entities and relationships supported by source quotations.
Validated results enter the assistant's protected knowledge store and search
index. This document index is separate from fact memory in `trusty-memory`.

The pipeline exposes `queued`, `completed`, `blocked_on_dependency`, `retryable`,
and `cancelled` job states, with reasons for indexing, extraction, cleanup, and
publication. A queued item is not evidence that extraction completed. Failed
work retries with bounded backoff; saved extraction output can be reused when
index publication needs another attempt. Pausing or withdrawing a source stops
its eligibility for further work.

Extraction requires working credentials for a tool-free inference provider and
an available `trusty-search` service. ClaudeCode runner extraction is unavailable
and reports a reason. Incoming event excerpts provide only the content actually
received; historical connector backfill can remain blocked on its dependencies.
A custom search daemon may also require approval for the protected store root.

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

### Attachment preparation

`POST /api/chat-attachments/prepare` is authenticated and accepts `{items:[...]}`
with one to four raw inputs. Each input has exactly one of these shapes:

```json
{"kind":"file","name":"table.csv","mime_type":"text/csv","data_base64":"..."}
{"kind":"clipboard","name":"Pasted cells","format":"tsv","text":"Name\tRole\nMaya\tLead"}
```

Clipboard `format` is `html` or `tsv`. File MIME types are `image/png`,
`image/jpeg`, `image/webp`, `text/csv`, or
`application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`.
CSV and XLSX filename extensions must match their types.

A successful response is `{attachments:[...]}`. Images contain `kind`, `name`,
`mime_type`, and `data_base64`. Tables contain `kind`, `name`, `source_format`,
and `sheets:[{name,rows}]`. Submit this normalized attachment array with the
message to `/api/task`; task submission validates it independently. Preparation
performs neither persistence nor inference.

Malformed requests and unknown fields return 400; exceeded limits return 413;
unsupported or corrupt media and non-table HTML return 422. Errors use
`{"error":"..."}`. Do not submit attachments after preparation fails.

In history, an image's `asset_id` replaces `data_base64`; table fields retain
the normalized shape. Authenticated
`GET /api/agents/{assistant}/chat-assets/{asset_id}` returns image bytes after
checking ownership through `trusty-memory`. An asset ID is opaque, never a path
or a remote URL.

### Direct memory and Concierge

These routes invoke the same bound operations as native assistant tools, without
requiring a model turn. Their implementation is awaiting runtime verification.
All require the server's authenticated access; the assistant comes from `{name}`
in the route. Request bodies cannot override its namespace or target identity.

| Route | JSON request |
| --- | --- |
| `POST /api/agents/{name}/memory/remember` | `{text, tags?, context?}`; `tags` defaults to `[]`. |
| `POST /api/agents/{name}/memory/write` | Same request and durable save behavior as `remember`; requires the `memory_write` tool grant. |
| `POST /api/agents/{name}/memory/recall` | `{query, across_palaces?, top_k?}`; defaults are `false` and `6`. |
| `POST /api/agents/{name}/concierge` | `{action, section?, patch?}`; no target-assistant field. |

Remember/write require `memory.write`; recall requires `memory.read`. Each route
also checks its corresponding tool grant: `memory_remember`, `memory_write`, or
`memory_recall`. Allowing `memory_remember` alone does not authorize `/memory/write`.
Later operator changes to effective tool/skill grants remain authoritative. If the
scope is present but a request returns 403, inspect both the tool allowlist and
enabled skills in Settings or through Concierge; a scope alone does not grant
the tool. Successful memory calls return the parsed `trusty-memory` result. Treat its acknowledgement
as the save result. Cross-palace recall additionally requires the saved opt-in;
it never changes the write destination.

Fact text must be nonblank and at most 64 KiB. A request may contain at most
32 tags, each at most 128 bytes, and 16 KiB of context. Recall queries must be
nonblank and at most 16 KiB; `top_k` is between 1 and 50. Invalid values return
400, an unknown assistant returns 404, and missing memory scope returns 403.
Bound-operation failures return 422 with `{"error":"..."}`; malformed service
responses return 502. Unknown request fields are rejected.

Concierge accepts `settings.get`, `settings.patch`, or `platform.health`.
Settings sections are `config`, `model`, `provider`, `personality`, `permissions`,
`memory`, `projects`, `knowledge`, `skills`, `subagents`, `listeners`, and
`channels`. Health needs only `action`. A settings patch carries the target
section's normal request fields and revision, obtained by reading that section.
For manifest-backed changes, obtain the revision from `config`. Each patch
changes one persistence domain through the same handlers used by Settings;
Concierge is not an arbitrary HTTP or filesystem proxy.

A successful Concierge response is `{assistant, action, section, result}`.
This route currently wraps service failures as HTTP 422; the error text contains
the underlying Settings status, including a stale-revision 409. Reload before
retrying a conflicting edit. Direct Settings routes retain their native statuses.

### Persisted conversation reads

`GET /api/agents/{name}/chat-history?limit=100&until=...` reads persisted messages
from `trusty-memory`. `limit` defaults to 100 and is capped at 500. Omit `until`
for the newest page; pass the returned `start` as the next exclusive `until` to
read older messages. Check `available` and its reason before treating an empty
result as empty history. Image attachments use the authenticated asset route
described above; tables retain their normalized values.

### Settings and sources

These routes share the server's existing authentication and write-origin guards.
Clients should read configuration before editing and send the returned revision;
stale revisions return HTTP 409 without applying the stale update.

| Route | Contract |
| --- | --- |
| `GET/PATCH /api/agents/{name}/memory-policy` | Read the bound namespace and revision; patch only `{revision, cross_palace_query}`. |
| `GET /api/agents/{name}/knowledge/pipeline` | Saved assistant projects, per-chat selections, source dependencies, and job states. |
| `PUT /api/agents/{name}/knowledge/pipeline/projects` | Save `{revision, scope:"assistant", projects}` or `{revision, scope:"chat", chat_id, projects}`. For first-time project settings use the returned revision, or `""` when no pipeline exists. |
| `POST /api/projects` | Register `{path, adapter?, name?}` for a readable absolute directory; Git is optional. |
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
