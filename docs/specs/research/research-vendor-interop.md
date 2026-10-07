# Vendor interoperability research: engineering-project memory sync (trusty-memory)

Date read for every citation: 2026-10-04. Scope: engineering/code-development project memory only (brief Addendum C). No whole-profile ingestion. Every imported item must map to an authorized project.

Method note. Pages fetched through a summarising fetch tool; quotes below come from tool output. Pages that returned HTTP 403 are marked UNVERIFIED and are not relied on:
- help.openai.com/en/articles/8590148-memory-faq (403)
- help.openai.com/en/articles/11146739-how-does-reference-saved-memories-work (403)
- help.openai.com/en/articles/10169521-projects-in-chatgpt (403)
- A raw GitHub fetch of codex-rs/app-server/README.md returned content that did not start as expected; "no memory methods" there is a weak negative (see 5.7).

Label key: GA / beta / experimental / undocumented. "Direction" is always from trusty-memory's point of view: import = vendor to trusty, export = trusty to vendor.

Common adapter target (project-scoped memory record):
`memory_id, revision_id, project_id, originator, author_vs_importer, scope, content, created_at/observed_at/valid_from/valid_to, supersedes, tombstone`.

---

## 1. Claude app memory (consumer / Team / Enterprise app)

### 1.1 What it is
Memory inside the Claude apps (web, Desktop, mobile). Claude saves "a set of individual topics as you chat" and keeps a separate memory space and project summary per Claude Project.
- https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context (read 2026-10-04)

### 1.2 Access path
- UI only: Settings > Memory > Topics (view, edit, delete individual memories); chat commands ("remember / forget").
- Import/export: Settings > Memory (new) or Settings > Capabilities > Memory (legacy). Export = ask Claude "Write out your memories of me verbatim" and copy text; import = paste text, "Add to memory", Claude extracts individual entries.
  - https://support.claude.com/en/articles/12123587-import-and-export-your-memory-from-claude (read 2026-10-04)
- Team/Enterprise: memory entries are "included in standard data exports" and follow org retention; owners enable memory in Organization settings > Capabilities; disabling org-wide "permanently deletes all memory data for everyone".
  - 11817273 article above; blog https://claude.com/blog/memory appeared in search results only (not fetched; UNVERIFIED beyond the search snippet).
- API: none documented. The import/export article documents no API; the 11817273 article says API memory docs are separate. Absence of an API is inferred from these pages, not stated by the vendor in so many words.

### 1.3 Schema
No documented schema. Export is free text; import is free text parsed by Claude. No field names, ids, timestamps or author fields are published.

### 1.4 Operations
| Op | Support | Evidence |
|---|---|---|
| read | UI view; verbatim-text export via chat | 12123587 |
| write | chat instruction or paste-import | 12123587, 11817273 |
| delete | UI per topic; org-wide disable wipes all | 11817273 |
| list | UI topic list only | 11817273 |
| change feed | none documented | absence |

### 1.5 Scope unit
User (personal) or user-within-organization, with a per-Claude-Project memory partition. The Claude Project is a chat-app construct, not a repository or trusty project id.

### 1.6 Versioning / deletion
No versions documented. Deletion is by topic (user) or org-wide wipe (owner). No tombstone, no retention contract for individual entries stated.

### 1.7 Stability
Import/export: "experimental and still in active development" (12123587). Memory itself: presented as a shipped feature; plan-gated (Free/Pro/Max default on; Team/Enterprise owner-controlled; HIPAA, public-sector and custom-retention plans excluded) per 11817273.

### 1.8 Adapter mapping
| Field | Mapping |
|---|---|
| direction | import: manual, human-mediated text paste out of the app into a reviewed trusty draft. export: manual paste into the app. Both are one-shot migration, NOT sync. |
| memory_id / revision_id | none from vendor; trusty must mint ids |
| project_id | not present; human must assign per import batch |
| originator / author | lost: text is Claude's synthesis about "me"; no per-entry author |
| scope | Claude Project partition may hint, but needs explicit binding to a trusty project |
| content | verbatim text block |
| dates | none; importer time only |
| supersedes / tombstone | none |

Authorship and scope: both lost. Claude may also drop non-work content on import ("may not retain imported personal details unrelated to work").

### 1.9 Limitations
1. No API; UI/chat only. Automation would mean driving a consumer UI or prompting for text, which is unsanctioned.
2. Export is a model-generated rendering ("verbatim" request), not a stored-record dump; fidelity is unverifiable.
3. No ids, timestamps, versions or authors, so idempotent re-import and dedup are impossible without trusty-side hashing.
4. Import is "experimental"; "Claude may not always successfully incorporate imported memories".
5. Content is about the user, not about a repository; a whole-profile export would violate the project-only rule. Importing requires manual filtering per project.
6. Claude Code memory is not covered by this article (the article does not mention it).
7. No continuous replication; do not describe as sync.

---

## 2. Anthropic client-side memory tool (API tool)

### 2.1 What it is
An Anthropic-defined tool on the Messages API. Claude emits `tool_use` calls with file-style commands; the application executes them against storage the application owns. "Memory lives entirely in your application."
- https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool (read 2026-10-04)

### 2.2 Access path
- `POST https://api.anthropic.com/v1/messages` with `tools: [{"type": "memory_20250818", "name": "memory"}]`. `name` must be `memory`; no input schema is supplied.
- Documented as available on "all Claude 4 and later models". The page says the tool itself "doesn't require a beta header"; SDK helpers sit in beta namespaces (`BetaAbstractMemoryTool` Python/C#, `betaMemoryTool` TS, `BetaMemoryToolHandler` Java; `BetaLocalFilesystemMemoryTool` Python/TS).
- ZDR: eligible (API and data retention page, row "Memory tool": Yes / Yes; "Client-side memory storage where you control data retention").
  - https://platform.claude.com/docs/en/manage-claude/api-and-data-retention (read 2026-10-04)

### 2.3 Schema (commands and exact input fields)
All paths must live under `/memories`.
| command | input fields | notes |
|---|---|---|
| `view` | `path`, optional `view_range: [start, end]` (`-1` = to end) | directory: 2 levels deep, sizes, hidden and `node_modules` excluded; file: 6-wide line numbers; text truncated above 16,000 chars; images .jpg/.jpeg/.png |
| `create` | `path`, `file_text` | tool description says "creates or overwrites"; returning an error on existing file is reference behavior, overwrite is allowed |
| `str_replace` | `path`, `old_str`, optional `new_str` | error on not-found or multiple occurrences |
| `insert` | `path`, `insert_line`, `insert_text` | inserted after line; 0 = start |
| `delete` | `path` | file or directory recursive; root cannot be deleted |
| `rename` | `old_path`, `new_path` | error if destination exists; root cannot be renamed |
Results go back in `tool_result` blocks (`is_error: true` for errors). Return strings are recommended text, not mandated: "you can return different strings".

### 2.4 Operations and who stores what
- Anthropic stores nothing durable of the memory files. The application stores files/keys; `/memories` is "a prefix that your handler maps onto real storage, such as a per-user directory or keys in a database".
- Anthropic injects a system-prompt protocol telling Claude to view `/memories` first and to assume interruption.
- read/write/delete/list: yes, via the six commands, executed by our handler. Change feed: none (there is no remote side; the app is the source).

### 2.5 Scope unit
Whatever the handler maps `/memories` to. Trusty chooses: one handler instance per authorized project. Path = file path within a project view. The vendor defines no user, workspace or project concept.

### 2.6 Versioning / deletion
None from the vendor. Versioning and deletion semantics are entirely ours. The page recommends file-size caps, expiry of stale files, and sanitising sensitive data.

### 2.7 Stability
Date-versioned tool type `memory_20250818`; no beta header required; page carries a feedback-form note. Label: GA-style API tool with a dated type string (vendor does not use the word "GA" on this page; classification is mine). Helpers in SDK beta namespaces.

### 2.8 Adapter mapping
This is not a remote store, so there is nothing to sync with. It is an integration surface for exposing trusty memory to Claude.
| Field | Mapping |
|---|---|
| direction | export (trusty to Claude context) via a read-mostly file view; import (Claude-authored writes to trusty) via `create/str_replace/insert/delete/rename` handlers. Both are local application integration, not vendor sync. |
| memory_id | file path alone is unstable (`rename`); trusty must keep id to path in a side index |
| revision_id | trusty-minted on each handler write |
| project_id | fixed by which handler/session is bound; never derived from path |
| originator | the agent session that issued the call; handler must stamp authenticated human + agent session, not "Claude" |
| author vs importer | handler writes are agent-authored; mark `author=agent session`, `importer=handler` |
| content | file text (free-form; one file may hold many facts, so granularity is a design choice) |
| dates | none supplied; handler stamps |
| supersedes | `str_replace`/`create`-overwrite to new revision with `supersedes` = prior revision |
| tombstone | `delete` to tombstone, not physical delete |

Authorship/scope: preserved only if the handler is bound to an authenticated project session and stamps provenance. The tool carries no identity.

### 2.9 Limitations
1. Application integration only; no access to Claude app memory and no Anthropic-hosted store.
2. No ids, versions, timestamps in the protocol; all must be synthesised.
3. File granularity does not match one-fact-per-record; edits like `str_replace` can change several facts at once.
4. Claude can overwrite, rename or recursively delete; handler must convert these to revisions/tombstones and enforce policy.
5. Prompt-injection risk: stored text is re-read as trusted context; the page warns about path traversal and sensitive data. Shared project memory must not be rendered as instructions (matches brainstorming note).
6. Model-driven writes are non-deterministic; promotion rules must gate what leaves the local store.
7. Return strings are advisory; conformance tests cannot assert vendor text.

---

## 3. Anthropic Managed Agents memory stores (beta)

### 3.1 What it is
"A memory store is a workspace-scoped collection of text documents optimized for Claude." Attached to a Managed Agents session as a directory under `/mnt/memory/`; also readable and writable directly through the API and Console ("tuning, importing, and exporting").
- https://platform.claude.com/docs/en/managed-agents/memory (read 2026-10-04)
- https://platform.claude.com/docs/en/managed-agents/self-hosted-sandboxes-memory (read 2026-10-04)
- https://platform.claude.com/docs/en/api/beta/memory_stores/memories/list (read 2026-10-04)
- https://platform.claude.com/docs/en/api/beta/memory_stores/memory_versions/list (read 2026-10-04)

### 3.2 Access path
Header `anthropic-beta: agent-memory-2026-07-22` on memory-store calls (do not combine with `managed-agents-2026-04-01`: 400). Session calls still use `managed-agents-2026-04-01`. Optional `anthropic-workspace-id` header for multi-workspace credentials. Auth `x-api-key`.
| Op | Endpoint |
|---|---|
| create store | `POST /v1/memory_stores` (`name`, `description`) |
| list/retrieve/update/archive/delete store | `GET /v1/memory_stores`(`include_archived`), `GET .../{id}`, update, `POST .../{id}/archive`, `DELETE .../{id}` |
| create memory | `POST /v1/memory_stores/{id}/memories` (`path`, `content`); does not overwrite |
| list memories | `GET /v1/memory_stores/{id}/memories` |
| read memory | `GET /v1/memory_stores/{id}/memories/{mem_id}` |
| update memory | `POST /v1/memory_stores/{id}/memories/{mem_id}` (`content`, `path`, `precondition`) |
| delete memory | `DELETE /v1/memory_stores/{id}/memories/{mem_id}` |
| list versions | `GET /v1/memory_stores/{id}/memory_versions` |
| read version | `GET /v1/memory_stores/{id}/memory_versions/{memver_id}` |
| redact version | `POST /v1/memory_stores/{id}/memory_versions/{memver_id}/redact` |
SDKs: Python, TS, C#, Go, Java, PHP, Ruby; CLI `ant beta:memory-stores:...`. Self-hosted worker: `ant` CLI 1.33.0+ or `EnvironmentWorker` (Python/TS/Go).

### 3.3 Schema (field names from the API reference)
`memory` object: `type:"memory"`, `id` (`mem_...`, stable across renames), `memory_store_id`, `memory_version_id` (head pointer; no `is_latest` flag), `path` (starts `/`, case-sensitive, unique per store, max 1,024 bytes), `content` (null under `view=basic`; max 102,400 bytes), `content_sha256` (hex SHA-256 of UTF-8 bytes, no normalisation), `content_size_bytes`, `created_at`, `updated_at` (RFC 3339).
`memory_prefix` (list rollup, not stored): `type`, `path`.
`memory_version` object: `type:"memory_version"`, `id` (`memver_...`), `memory_id`, `memory_store_id`, `operation` (`created|modified|deleted`), `content`, `content_sha256`, `content_size_bytes`, `path`, `created_at`, `created_by`, `redacted_at`, `redacted_by`.
`created_by` actor union: `session_actor{session_id}`, `api_actor{api_key_id}`, `user_actor{user_id}`, `service_account_actor{service_account_id}`; null if unrecorded. Quote: "A `session_actor` is an agent writing through the store's mounted filesystem... The API key that created that session is not recorded on agent writes, so attribution names who made the write, not who is ultimately responsible."
Limits: 100 kB per memory; 10,000 memories per store; 8 stores per session; mount `instructions` up to 4,096 chars.

### 3.4 Operations
- Read, write, delete, list: yes (table above).
- Pagination: list memories `limit` 1-100 (default 20; capped at 20 with `view=full`), opaque `page` cursor, response `next_page` (null at end); stable server-defined order. Versions: `limit` default 20, newest first by `created_at` then `id`, `page`/`next_page`.
- Filters: memories `path_prefix` (must end `/`, segment-aligned), `depth` (0 or 1); versions `memory_id`, `operation`, `session_id`, `api_key_id`, `service_account_id`, `created_at[gte]`, `created_at[lte]`.
- Change detection (documented building blocks, no dedicated feed): `view=basic` returns `content_sha256`, `content_size_bytes`, `updated_at`, `memory_version_id` "so sync clients can diff without fetching content"; `view=full` is "the bulk-read path for export and sync". A version listing filtered with `created_at[gte]` gives an incremental change log that includes `deleted` rows. There is no push notification, ETag, or since-cursor on the memories list. A true change feed must be built from polling version history (UNVERIFIED that `created_at[gte]` is gap-free under concurrent writes; the page does not state ordering guarantees for equal timestamps beyond `id` tiebreak).
- Conflict handling: optimistic precondition `{"type":"content_sha256","content_sha256":"..."}` on update; mismatch means re-read and retry. Create does not overwrite (path unique). Self-hosted worker: "Conflicts resolve in favor of the store"; worker keeps store version, overwrites local, logs warning.
- Sync interval for the self-hosted worker: default 15 s, minimum 5 s; deletions mode `enabled|log_only|disabled`.

### 3.5 Scope unit
Workspace (store belongs to a workspace; `GET /v1/memory_stores` lists "stores in the workspace"). Within a store: path. Docs suggest "one store per end user, per team, or per project" as a convention. The vendor defines no project object; project = a store we create and name, or a path prefix.

### 3.6 Versioning / deletion / retention
- Every mutation makes an immutable version. Versions belong to the store and survive memory deletion.
- Retention: "Versions are retained for 30 days after they are written; however, the recent versions of a live memory are always kept regardless of age". "Past memory versions might be deleted after 30 days. To preserve memory history for longer, export versions through the API." Deleted memories' history therefore expires after 30 days.
- No restore endpoint; roll back by writing an old version's content forward.
- Redact scrubs `content/path/sha/size` of a historical version, keeps who/when; head version cannot be redacted.
- Store archive is one-way (read-only, no unarchive); store delete removes memories and versions permanently.
- Retention eligibility: Managed Agents are not ZDR- or HIPAA-eligible (Managed Agents row, API and data retention page: No / No; "transcripts persist until you delete them"). https://platform.claude.com/docs/en/manage-claude/api-and-data-retention (read 2026-10-04)
- Not available for self-hosted environments on Claude Platform on AWS.

### 3.7 Stability
Beta (page `featureMetadata.status: beta`; dated header `agent-memory-2026-07-22`). Expect breaking changes under new dated headers.

### 3.8 Adapter mapping
| Field | Mapping |
|---|---|
| direction | import (store to trusty) and export (trusty to store) are both technically possible with documented endpoints, each as a client-driven polling loop. The vendor documents no bidirectional replication protocol, no change feed and no webhook; any two-way sync is OUR design on top of CRUD+versions. |
| memory_id | `mem_...` as external id; trusty keeps its own global memory id and a mapping row (`mem_id` stable across rename, path is not) |
| revision_id | `memory_version_id` (`memver_...`) for import; on export trusty records the returned head `memory_version_id` |
| project_id | explicit binding: store id (or store id + path prefix) to trusty project id, configured by an authorized human; never inferred from store name |
| originator | `created_by` actor; for `session_actor` this names a session, not a human; mapping to a human needs Sessions API lookup (`GET` session) plus our own identity binding |
| author vs importer | author = `created_by` (or unknown); importer = our connector identity; on export the author is lost (all writes appear as our `api_actor`/`service_account_actor`), so embed original author in `content` front matter or an adjacent path (e.g. `/_meta/`), accepting that Claude reads it |
| scope | store and path prefix to project scope; `access: read_only` mount for consumers |
| content | text up to 100 kB; one memory per file; trusty records may need packing/splitting (content-addressed by sha256) |
| dates | `created_at`, `updated_at` (RFC 3339) to created/observed; no valid_from/valid_to (put in front matter) |
| supersedes | `operation: modified` chain of versions for same `memory_id`; cross-memory supersession not modeled |
| tombstone | `operation: deleted` version with null content; lasts only about 30 days, so a connector offline longer than that cannot learn of a deletion from version history and must reconcile by full list diff |

Authorship/scope: partially preserved on import (actor id, timestamps); lost on export unless embedded in content. Project scope exists only by our convention.

### 3.9 Limitations
1. Beta; header-versioned; may change.
2. Workspace scope is not project scope; a project binding is ours, and workspace credentials can read every store in the workspace (access control is at workspace/API-key level, not per project; per-store ACLs not documented, UNVERIFIED).
3. No change feed, no webhooks, no since-token; sync = poll list (`view=basic`, diff on `content_sha256`/`memory_version_id`) plus version log.
4. Version history deleted after 30 days; deletions invisible to a connector offline longer than that unless it keeps its own inventory.
5. Writes through mount are attributed to a `session_actor`, not a human or API key; originator attribution is weak.
6. Paths are renameable keys; `mem_` id is the only stable key. Case-sensitive paths collide on case-insensitive filesystems.
7. 100 kB / 10,000 memory limits; create fails at the cap.
8. Default `read_write` mounts allow prompt-injected writes that later sessions treat as trusted; the vendor recommends `read_only` for shared references.
9. Precondition is content-hash only (no revision-id precondition documented): a delete has no documented precondition.
10. Not ZDR/HIPAA eligible; content is held by Anthropic until deleted.
11. Self-hosted worker sync is last-store-wins; the agent does not see conflict errors.

---

## 4. ChatGPT memory (saved memories / reference chat history)

### 4.1 What it is
Consumer/Business ChatGPT feature. Per search snippets of OpenAI Help Center: saved memories are details ChatGPT is asked to keep or saves as useful context, "stored separately from chat history"; reference chat history is "a continually updated synthesis of context from your past chats". "Project-only memory" limits memory to one ChatGPT Project: project chats do not reference saved memories or outside conversations, and outside chats cannot reference project conversations.
- Sources: help.openai.com/en/articles/8590148-memory-faq and /11146739-how-does-reference-saved-memories-work, /10169521-projects-in-chatgpt. All three direct fetches returned HTTP 403 on 2026-10-04. The statements above come only from web-search result snippets of those pages. Mark: UNVERIFIED (not read in full).
- Related, via search snippet only: GPTs "do not use saved memory, custom instructions, or previous conversations" (help.openai.com/en/articles/8983148). UNVERIFIED.

### 4.2 Access path
UI settings (Personalization/Memory), chat commands. Export/delete paths, plan matrix, admin controls: UNVERIFIED (pages blocked). The generic ChatGPT account data export exists but I could not confirm whether memory is included; do not rely on it.
Codex docs state ChatGPT Work "uses the memory settings available to your account and workspace" (https://learn.chatgpt.com/docs/customization/memories, read 2026-10-04).

### 4.3 API
No documented API for ChatGPT saved memories or reference chat history was found in any primary page read. Evidence is negative: the Responses/Conversations docs (surface 6) describe developer-owned state only. A search-tool summary asserted "no direct API endpoint to access a user's saved ChatGPT memories"; that is a secondary inference and I treat it as UNVERIFIED rather than a vendor statement. Brainstorming note seed 11 reached the same conclusion ("No general ChatGPT saved-memory synchronization API was established").

### 4.4 Schema / ops / scope / versioning
- Schema: none published. Ops: read/list/delete by UI only (UNVERIFIED detail). Change feed: none. Scope unit: user account; optional ChatGPT Project partition (project-only memory); workspace-level admin settings on Business/Enterprise (UNVERIFIED). Versioning: none documented. Deletion: user-driven; semantics UNVERIFIED.
- Stability: product feature; not an API. Label: undocumented as an integration surface.

### 4.5 Adapter mapping
direction: NONE (no sanctioned import or export path established). At most a human reads their own saved memories and manually authors a project note in trusty; the result is a new human-authored record, not an import of ChatGPT memory.
| Field | Mapping |
|---|---|
| all fields | not applicable; if a human re-authors a note, originator = that human, importer = none, project = chosen by the human, provenance = "manually transcribed from ChatGPT", no vendor ids |

### 4.6 Limitations
1. No documented API or machine-readable export; automation would scrape a consumer UI (unsanctioned).
2. Reference chat history is an opaque synthesis, not a list of items; there is nothing to map one to one.
3. A ChatGPT Project is not a repository or trusty project; project-only memory is the only boundary, and account-wide memory still applies to projects when project-only is off.
4. Key help pages returned 403 to my fetcher; plan/admin/export claims remain UNVERIFIED.
5. Content describes the person, not the repository; unsuitable for project memory without per-item human review.

---

## 5. Local Codex memory (Codex CLI / app / IDE)

### 5.1 What it is
Local recall layer: "carry useful context from earlier work into future work". Stored under `~/.codex/memories/` (Codex home default `~/.codex`) as "summaries, durable entries, recent inputs, and supporting evidence from prior chats". Official warning: treat files as "generated state. You can inspect them when troubleshooting or before sharing your Codex home directory, but don't rely on editing them by hand."
- https://learn.chatgpt.com/docs/customization/memories?surface=app (read 2026-10-04; https://developers.openai.com/codex/memories issues a 308 redirect to it)
- https://github.com/openai/codex/blob/main/codex-rs/memories/README.md (read raw 2026-10-04)

### 5.2 Access path (files and config; no API)
- Files: `~/.codex/memories/` containing (from README and sandbox docs): `raw_memories.md`, `rollout_summaries/`, `MEMORY.md`, `memory_summary.md`, `skills/`, `phase2_workspace_diff.md` (transient), and a git baseline at `~/.codex/memories/.git`. Pipeline state lives in a separate state DB (job claims, retry, watermarks, `usage_count`, `last_usage`, `generated_at`, `selected_for_phase2`). Schema of that DB is not published as a contract.
- Config (`config.toml`): `[features] memories = true` (default false), `memories.generate_memories`, `memories.use_memories`, `memories.disable_on_external_context` (excludes chats using MCP, web search or tool search), `memories.min_rate_limit_remaining_percent`, `memories.extract_model`, `memories.consolidation_model`.
- Surfaces: ChatGPT desktop app (Settings > Personalization), Codex CLI `/memories`, IDE extension (uses connected Codex host's store). Source: learn.chatgpt.com page above.
- App-server: no memory methods found. The raw `app-server/README.md` fetch looked partial/odd, and a summarising fetch of the GitHub page also reported no memory methods; treat as weak negative (UNVERIFIED).
- Reset/delete: docs page "doesn't explicitly detail deletion procedures" in what I read; disable via `memories = false` or UI toggles.

### 5.3 Schema
No public record schema. Generation pipeline output fields (README): per rollout `raw_memory`, `rollout_summary`, optional `rollout_slug`; consolidated artifacts are Markdown written by a consolidation sub-agent (no approvals, no network, local write only). Secrets are redacted in generated fields. Records are keyed by rollout/thread id in the state DB, not by a stable public memory id.

### 5.4 Operations
- read: local files (inspect only). write: the pipeline, not the user (hand edits discouraged). delete: not documented as supported operation. list: filesystem. change feed: none public; a git baseline exists locally (implementation detail, could change).
- Scope unit: Codex home (`CODEX_HOME`), i.e. per machine per OS user. README says Phase 2 does "global consolidation" into "shared memory artifacts"; no per-project partition documented. The scoping-by-project question was "not addressed" in the page content I read.
- Phase 2 ranks by `usage_count`, then recency (`last_usage`/`generated_at`) and drops memories unused beyond `max_unused_days`: memories can be silently pruned.

### 5.5 Sanctioned extension points
- `AGENTS.md`: discovery per https://learn.chatgpt.com/docs/agent-configuration/agents-md (read 2026-10-04; redirect from developers.openai.com/codex/guides/agents-md): global `~/.codex/AGENTS.override.md` else `AGENTS.md` (first non-empty); then from Git root down to cwd, per directory `AGENTS.override.md`, `AGENTS.md`, then `project_doc_fallback_filenames`; one file per directory; concatenated root-down (closer files appear later); stops at `project_doc_max_bytes` (32 KiB default).
- Docs direct required guidance to AGENTS.md or checked-in docs: "Treat memories as a helpful recall layer, not as the only source for rules that must always apply."
- MCP servers are also an official Codex integration path (config), but chats that use MCP are excluded from memory generation when `disable_on_external_context` is set. Using trusty-memory as an MCP server therefore sits beside Codex memory, not inside it.

### 5.6 Stability
Not labelled experimental on the page read; presented as standard. `features.memories` defaults false. Internal layout is implementation-defined (the README says runtime orchestration lives in `codex-core`; template and layout can change). Classification: documented feature, undocumented/unstable file contract.

### 5.7 Adapter mapping
| Field | Mapping |
|---|---|
| direction | export (trusty to Codex): supported only through AGENTS.md generation (checked-in file, a project-scoped, human-reviewable, size-bounded 32 KiB channel) or MCP. import (Codex to trusty): none sanctioned; reading `~/.codex/memories` is inspection of generated state, and the vendor says not to rely on it. NEVER write into `~/.codex/memories`. |
| memory_id | none public (rollout/thread id internal) |
| revision_id | none (git baseline is internal) |
| project_id | absent from the artifact; Codex memory is global per Codex home. Any mapping to a project needs the rollout's cwd/repo, which is in session rollouts, not documented as a memory field |
| originator | the local OS user's Codex home; no author field |
| author vs importer | LLM-synthesised text; neither human author nor agent session is recorded in the consolidated files |
| content | Markdown |
| dates | `generated_at`, `last_usage` exist in the state DB, not a stable public interface |
| supersedes / tombstone | none; prune-by-age instead of tombstone |

### 5.8 Limitations
1. Generated state; vendor discourages hand editing; no write contract.
2. Not project-scoped: one global consolidated store per Codex home (README "global consolidation").
3. No record ids, authors, or stable schema; consolidation rewrites and prunes content so there is no immutable revision.
4. May contain content from other projects and personal context; ingesting it wholesale violates the project-only rule.
5. Secret redaction is best-effort ("Redacts secrets from generated memory fields"); still, it was not designed as a sharing format.
6. Chats using MCP/web/tool search can be excluded; coverage is incomplete by design.
7. Generation depends on rate-limit headroom and idleness; non-deterministic and delayed.
8. AGENTS.md is the only supported, deterministic, repository-scoped channel; it has a 32 KiB combined cap and is read at session start, not queried.
9. App-server and CLI memory management APIs not found; UNVERIFIED.

---

## 6. OpenAI agent memory (Agents SDK sessions, Sandbox Agents memory, Responses, Conversations)

Four distinct things. Only 6.2 is semantic/reusable memory. 6.1, 6.3 and 6.4 are conversation history/state, not semantic memory.

### 6.1 Agents SDK Sessions: CHAT HISTORY, not semantic memory
- Source: https://openai.github.io/openai-agents-python/sessions/ (read 2026-10-04).
- Protocol methods: `get_items`, `add_items`, `pop_item`, `clear_session`. Implementations: `SQLiteSession`, `SQLAlchemySession`, `OpenAIConversationsSession`, `RedisSession`, `DaprSession`, `EncryptedSession`, `OpenAIResponsesCompactionSession`. They store "conversation history for a specific session" (user input, assistant output, tool calls).
- Scope: `session_id`. No author, project, revision or semantic fields. Sandbox docs state it plainly: SDK session memory "preserves message history".
- Adapter: direction none. Replaying transcripts into project memory would be summarisation by us (outside the sync-only connector) and is not vendor-supported sync. Stability: SDK feature (label not stated on page read).

### 6.2 Sandbox Agents persistent memory: reusable memory files
- Sources: https://developers.openai.com/api/docs/guides/agents/sandboxes#persist-memory-across-runs (read 2026-10-04); https://openai.github.io/openai-agents-python/sandbox/memory/ (read 2026-10-04).
- What: a `Memory` capability (Python `Memory()`, TS `memory()`) on a `SandboxAgent`. "Memory enables both reads and generation by default." Reads need `Shell`; live updates need `Filesystem`.
- Layout (workspace-relative): `sessions/<rollout-id>.jsonl`; `memories/memory_summary.md` (injected at run start), `memories/MEMORY.md` (searched when relevant), `memories/raw_memories.md`, `memories/phase_two_selection.json`, `memories/raw_memories/<rollout-id>.md`, `memories/rollout_summaries/<rollout-id>_<slug>.md`, `memories/skills/`.
- Generation: "When the session closes, memory generation first extracts conversation summaries and raw memories, then consolidates those raw memories into `MEMORY.md` and `memory_summary.md`." Two phases (extraction; layout consolidation). `MemoryGenerateConfig`: `max_raw_memories_for_consolidation` (default 256), `extra_prompt`. Modes: default read/write; `Memory(generate=None)` read-only; `Memory(read=None)` generate-only; read config can disable live updates.
- Isolation: `MemoryLayoutConfig` (`memories_dir`, `sessions_dir`). "Agents with the same layout and the same memory conversation ID share one memory conversation and one consolidated memory. Agents with different layouts keep separate rollout files, raw memories, `MEMORY.md`, and `memory_summary.md`." Conversation association order: `conversation_id`, then `session.session_id`, then `RunConfig.group_id`, then a generated per-run id. The sandbox session id is not the memory conversation id.
- Persistence: memory dirs survive only if you keep the live sandbox session, resume session state, start from a snapshot, or mount persistent storage "such as S3". No automatic cross-instance persistence; no retention policy stated.
- Providers listed: Unix-local, Docker, E2B, Modal, Blaxel, Cloudflare, Daytona, Runloop, Vercel. Stability: page does not label it beta/experimental (the fetch summary says so); classify as documented feature, label not stated. Examples: `memory.py`, `memory_s3.py`, `memory_multi_agent_multiturn.py`.
- Operations: read/write/list/delete = file operations in the sandbox workspace or on the S3 mount; no API endpoint, no change feed, no versioning. Scope unit: memory layout (`memories_dir`) and conversation id; a project boundary is whatever workspace/S3 prefix we bind.
- Adapter:
| Field | Mapping |
|---|---|
| direction | export (trusty to sandbox): seed `MEMORY.md` / `memory_summary.md` files from authorized project records into a project-specific mounted directory; import (sandbox to trusty): read the same files as untrusted, LLM-consolidated text and ingest only as a labelled candidate. No sync protocol is documented; two-way flow would be our design. Writing into generated files risks being overwritten by Phase 2 consolidation. |
| memory_id | none; rollout ids exist only for raw files; `MEMORY.md` is one consolidated blob |
| revision_id | none (use storage-layer object versions if S3 versioning is enabled by us) |
| project_id | our binding: S3 prefix or layout dir per project; never inferred |
| originator / author | agent run; no human identity. Our wrapper must stamp it |
| content | Markdown, free form |
| dates | file mtimes / rollout ids only |
| supersedes / tombstone | none |

### 6.3 Responses API state: CHAT HISTORY / run state
- Source: https://developers.openai.com/api/docs/guides/conversation-state (read 2026-10-04).
- `previous_response_id` chains responses; "all previous input tokens for responses in the chain are billed as input tokens". "Response objects are saved for 30 days by default"; `store: false` disables retention. Not a memory API.

### 6.4 Conversations API: CHAT HISTORY object
- Same page. "persist conversation state as a long-running object with its own durable identifier" usable "across sessions, devices, or jobs". Object has `id`, `created_at`, `metadata`; items (messages, tool calls, tool outputs) support list/add/delete. "Conversation objects and items in them are not subject to the 30 day TTL."
- Scope: conversation id (OpenAI project/organization scoping of API keys applies; not read here). Stability: page presents as available; label not stated.
- Adapter: direction none for semantic memory. It can carry an agent's transcript; it has no memory ids, authors beyond roles, or project concept (`metadata` is a free-form key-value we could set, which is our convention not vendor semantics). Do not treat as memory sync.

### 6.5 Limitations (surface 6)
1. Sessions, Responses chaining and Conversations are transcript state; none offers semantic records, dedup, supersession or authorship.
2. Sandbox memory is LLM-generated Markdown, consolidated at session close; content is rewritten, with no immutable revision.
3. Persistence is the operator's job (snapshots, S3); durability, access control and retention are those of the chosen storage, not an OpenAI memory service.
4. No change feed, id scheme or conflict semantics; concurrent agents must use separate layouts or they share one consolidated memory.
5. Shared layout plus shared conversation id means cross-contamination; project isolation needs one layout/workspace per project.
6. A Phase 2 rewrite can erase injected project facts; seed files are not authoritative.
7. Conversation items default retention: responses 30 days, conversations no TTL; sensitive project data persists until deleted.
8. Whether sandbox memory is beta is not stated; I did not find a stability label.

---

## 7. Cross-cutting rules for any adapter

1. Every imported item maps to one authorized project through an explicit, human-configured binding (store id / S3 prefix / repo path to trusty project id). No binding means no import.
2. Authorship: keep `originator` (authenticated human or workload identity bound by trusty) separate from `vendor_actor` (Anthropic `created_by`, agent session) and from `importer` (our connector). Vendor actors are never treated as authenticated humans.
3. Imported text is untrusted data, never instructions (client-side tool and Managed Agents pages both warn that stored memory is later read as trusted context).
4. Connector mints trusty memory ids and keeps a mapping table (external id, external revision, content sha256). Idempotency key = external id + external revision.
5. Do not re-export imported items back to the vendor they came from (loop prevention), and do not auto-reshare across endpoints.
6. Only surface 3 offers a documented remote CRUD + version API suited to a connector; surfaces 2 and 6.2 are local file/handler integrations; surfaces 1, 4 and 5 offer no sanctioned programmatic path.
7. Retention gap: Managed Agents deletion history lasts about 30 days; a connector must keep its own inventory to emit retractions.

## 8. Summary table

| Surface | Vendor label | Direction supported (documented) | Scope unit | Adapter feasibility | Recommendation |
|---|---|---|---|---|---|
| 1. Claude app memory | Import/export "experimental"; memory shipped, plan-gated | Manual text paste both ways; no API; not sync | user; Claude Project partition | Very low (human-in-loop only) | Avoid (at most documented manual one-off, per project, human reviewed) |
| 2. Anthropic client-side memory tool | `memory_20250818`, no beta header | None remote; local handler (we implement view/create/str_replace/insert/delete/rename) | whatever our handler maps `/memories` to | High, but it exposes trusty to Claude rather than syncing with a vendor store | Adapt (file-view over a project-bound handler; writes become revisions/tombstones) |
| 3. Managed Agents memory stores | Beta (`agent-memory-2026-07-22`) | Import and export both possible via CRUD + versions; no documented replication, change feed or webhook | workspace; store; path | Medium (poll `view=basic` diffs plus version log; 30-day history; weak author attribution) | Adapt as optional, opt-in connector backend behind explicit project to store binding; gate on beta stability |
| 4. ChatGPT memory | Product feature; API UNVERIFIED (help pages 403) | None established | user; ChatGPT Project (project-only memory) | None | Avoid |
| 5. Local Codex memory | Documented feature, generated state; not labelled experimental | Export only, via AGENTS.md (or MCP); import none sanctioned | Codex home (global, per machine/user) | Low for import, medium for AGENTS.md export | Adapt export through AGENTS.md; Avoid reading or writing `~/.codex/memories` |
| 6.1 Agents SDK Sessions | SDK feature | None (chat history) | session_id | Not memory | Avoid as memory sync |
| 6.2 Sandbox Agents memory | Documented; no stability label stated | File seed (export) / read-and-quarantine (import); no sync protocol | memory layout dir + conversation id; operator storage | Medium-low (generated Markdown, no ids) | Adapt cautiously (seed from project records; ingest only as candidates) |
| 6.3/6.4 Responses state, Conversations API | Documented API | None for semantic memory (chat history) | response chain / conversation id | Not memory | Avoid as memory sync |

Open items I could not verify: ChatGPT memory export/API/admin (403), Codex memory deletion procedure and any app-server memory methods, per-store ACLs on Managed Agents, ordering guarantees of version listing by `created_at` for gap-free polling, Sandbox Agents beta label.

## Prompt feedback
- Clear: six-surface split and the ban on claiming bidirectional sync.
- Unclear: "date read" for pages that were 403 or redirected; I recorded them as UNVERIFIED rather than substituting search snippets.
- Unnecessary: the long adapter field list repeats for surfaces that have no import path; "none" per field would do.
