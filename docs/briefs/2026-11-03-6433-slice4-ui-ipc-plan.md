# #6433 slice 4 — trusty-agents desktop UI: loopback HTTP to Tauri IPC over UDS (plan)

Task df-T41 / df-G04. Plan only; nothing built. Written 2026-11-03 against `origin/main` c1eed98441.
Owner rule (2026-11-03, #6433 reopen comment): Trusty Agents is a UDS daemon with no local HTTP.

## 0. Findings that change the brief

1. **No UDS API exists to call.** `crates/trusty-agents/src/ipc/` is the PM<->sub-agent NDJSON enum, not a socket. The only agents sockets are the per-project ctrl socket (`ctrl/socket_listener.rs`: `task`/`status`/`shutdown` only) and the search socket (5 `search.*` methods). Nothing serves the 72 routes in `api/server/routes.rs`. Removing the listener (slices 1-3) therefore leaves the UI with no daemon surface unless a UDS API slice also lands.
2. **A parked implementation exists:** local branch `feat/6433-agents-api-uds` (commit 589c74d8c2, `crates/trusty-agents/src/api/uds/{mod,client,dispatch,wire,tests}.rs`, 1159 lines, never PR'd). It serves `agents.request {method,path,headers,body}` (base64 body, 6 MiB cap) and `agents.health` by dispatching into the existing axum `Router` via `tower::oneshot`. Its header says the owner chose the tunnel over ~47 typed methods. It has **no stream method**, so `/api/events` is unserved.
3. **The tcode precedent is not a Tauri-IPC design.** `trusty-code-gui/src/bridge/` runs an axum server on `127.0.0.1:0` inside the shell with an in-memory bearer token; the webview still `fetch()`es it (`commands.rs` only returns URL + token). That is a loopback TCP listener, allowlisted for 1.9.0 and ruled for replacement in #10166. Copying it would breach the 2026-11-03 rule. Copy only its UDS half: `bridge/mod.rs` (unary call, `FramedStream` stream open, JSON-RPC-error to status mapping). (The spec path in the brief is `docs/specs/trusty-code-harness-ui.md`; its text carries no §8 bridge description, the code is the precedent.)
4. **The UI is two transports in one tree.** Tauri path: some calls go through `invoke()` Rust commands that use reqwest (`ui/src-tauri/src/task_commands.rs`), but most calls `fetch(apiBase())` straight from the webview to `http://localhost:8765` (`ui/src/lib/api-config.ts:26`). A browser path (`pnpm dev`, and the SPA the daemon embeds via `api/server/ui.rs`) uses `fetch` same-origin. Slice 1 kills both.

## 1. Inventory — UI to daemon calls

Shell = `ui/src-tauri/src/`; UI = `ui/src/`. "UDS today" is "missing" for every row unless noted: no agents UDS API is on main. "Parked" = covered by `agents.request` on the parked branch.

| Caller (file:line) | Method + route | UDS today |
|---|---|---|
| shell `sidecar.rs:72,290-296` | GET `/api/health` (reqwest, checks `ppid`); spawns `--api --port` (`:150`) | missing (parked `agents.health`; `ppid` not in its body) |
| shell `task_commands.rs:55` | GET `/api/tasks` | missing (parked) |
| shell `task_commands.rs:86` | DELETE `/api/tasks` | missing (parked) |
| shell `task_commands.rs:201` | POST `/api/task` | missing (parked) |
| shell `task_commands.rs:228-254` | GET `/api/task/{id}` (250/500 ms poll) | missing (parked) |
| shell `task_commands.rs:345` | DELETE `/api/task/{id}` | missing (parked) |
| shell `sse_bridge.rs:99` | POST `/api/events/ticket` | missing; ticket unneeded over UDS |
| shell `sse_bridge.rs:150` | GET `/api/events` SSE (`agent_message_delta`, slack kinds) | missing, **no stream method** |
| UI `transport.ts:91` | GET `/api/health` | missing (parked) |
| UI `transport.ts:98,106` | GET/DELETE `/api/tasks` (browser twin) | missing (parked) |
| UI `transport.ts:155,187,249` | POST `/api/task`, GET/DELETE `/api/task/{id}` (browser twin) | missing (parked) |
| UI `transport.ts:395` | POST `/api/events/ticket` | missing |
| UI `transport.ts:466` | `EventSource` GET `/api/events` | missing, no stream method |
| UI `App.svelte:146`, `channel-auth.ts:33` | GET `/api/config` | missing (parked) |
| UI `App.svelte:171,201` | `invoke('ensure_api_server',{port:8765})` | shell command; rewrite to socket |
| UI `buildInfo.ts:129` | GET `/api/health` | missing (parked) |
| UI `stores/app.ts:253`; `AddAgentForm.svelte:41` | GET / POST `/api/agents` | missing (parked) |
| UI `stores/app.ts:323` | GET `/api/models` | missing (parked) |
| UI `stores/app.ts:696`; `workspaceFiles.ts:12,20`; `FileNavigator.svelte:67`; `ProjectsView.svelte:101`; `AssistantProjectSettings.svelte:28` | GET/POST `/api/projects[?all=true]` | missing (parked) |
| UI `stores/app.ts:777`; `ProjectsView.svelte:186,203,215,227` | `/api/tm/sessions` (GET/POST), `/{name}` DELETE, `/pause`, `/resume` | missing (parked) |
| UI `agentConfig.ts:64,74,103,221,485,713` | `/api/agents/{n}` GET/PATCH, `/persona`, `/stores`, `/skills`, `/subagents` | missing (parked) |
| UI `kg.ts:105` (callers 121,134,147,156) | GET `/api/agents/{n}/kg`, `/kg/subjects`, `/kg/all`, `/kg/count` | missing (parked) |
| UI `assistantKnowledge.ts:33` | `/api/agents/{a}/knowledge/pipeline` | missing (parked) |
| UI `assistantMemoryPolicy.ts:13`, `assistantMemory.ts:26`, `assistantMcp.ts:72` | `/api/agents/{a}/memory-policy`, `/api/assistants/{a}/memory`, `/mcp` | missing (parked) |
| UI `channels.ts:49-156` | `/api/agents/{n}/channels[/{id}[/messages]]`, `/api/channels[/{id}]` GET/POST/PUT/PATCH/DELETE | missing (parked) |
| UI `chatHistory.ts:132` | GET `/api/agents/{n}/chat-history` | missing (parked) |
| UI `attachments.ts:87,120` | POST/GET `/api/agents/{n}/sessions/{id}/attachments` | missing (parked) |
| UI `attachments.ts:63` -> `AttachmentCard.svelte:47,78,104,135`; `storedImages.ts:59` | GET attachment bytes / chat-assets by URL, used as `<img src>`, `<a href download>` and `fetch` | missing; **a URL, not a call** — see §2 |
| UI `prepareAttachments.ts:21` | POST `/api/chat-attachments/prepare` | missing (parked) |
| UI `projectFiles.ts:157-204` | `/api/projects/{id}/files/{list,read,write}` | missing (parked) |
| UI `projectTools.ts:18-44` | `/api/project-tools[/index,/import,/skills]` | missing (parked) |
| UI `userSkillSettings.ts:18,30` | GET/PATCH `/api/user-skills` | missing (parked) |
| UI `costs.ts:82` | GET `/api/costs` | missing (parked) |
| UI `workstreams.ts:60,76` | GET `/api/workstreams`, `/{n}/history` | missing (parked) |
| UI `vite.config.ts:30` | dev proxy to `localhost:7654` | dev-only; remove |

Not HTTP, no change: `invoke` of `workspace_*`, `*_personalization_overlay` (direct filesystem in the shell). About 35 `fetch` sites and 15 `tmApi` sites funnel through three helpers (`tmApi` `app.ts:714`, `authHeaders`, `apiBase`) plus `transport.ts`.

## 2. Mechanism

Reject the tcode bridge shape (loopback listener in the shell). Use Tauri IPC only.

- **Shell dials the socket.** New `src-tauri/src/agents_uds.rs`: resolves the socket path the daemon resolves (same function as the daemon; share it, do not restate it), one unary exchange per call using `trusty_common::uds` client helpers, stream open via `stream_client::FramedStream`. Mirror `trusty-code-gui/src/bridge/mod.rs` error mapping.
- **One generic command** `agents_request(method, path, headers?, body?) -> {status, headers, body}` over `agents.request`. UI side: replace the body of `tmApi` and the ~35 `fetch(apiBase()+...)` sites with one `agentsFetch(path, init)` returning a `Response`-shaped object, so call sites change by one identifier, not their logic. Typed per-route commands are not proposed (owner chose the tunnel for the daemon; same call here).
- **Existing commands** (`send_message`, `list_tasks`, `cancel_task`, `clear_recent_tasks`, `check_health`) keep names and events (`task-progress/complete/error`); only their reqwest calls become `agents_uds` calls.
- **Streaming.** SSE becomes a daemon `agents.events` stream method (`RpcRouter::typed_stream`, exists in `trusty-common`), wrapping the same broadcast channel `events_sse.rs` reads. The shell opens it once, and for each frame `app.emit("app-event", payload)`. Tickets and bearer tokens disappear (the socket is the trust boundary). The shell's current filter (delta and slack kinds only) widens to all `AppEvent`s so desktop uses the same `eventBridge.ts` path as browser mode; `transport.ts:connectEventSource` becomes a `listen()` wrapper. Reconnect backoff logic (`sse_bridge.rs` 2 s) carries over.
- **Binary URLs.** `<img src>`/`<a download>` cannot use `invoke`. Register an async custom URI scheme (`agents-asset://`) in the shell that maps the path onto `agents.request` GET; `attachmentUrl()` returns that scheme in desktop mode.
- **Sidecar.** `ensure_api_server` drops its `port` arg and `--port`; liveness is a socket connect plus `agents.health` (must return `ppid`, which the parked health body lacks — carry the orphan-detection check over). `api-config.ts` `DEFAULT_API_PORT`, `apiBase()`, `authHeaders()`, `getCurrentApiToken`, the token form in `App.svelte`, and `channel-auth.ts` loopback probe go.
- **Browser mode** has no UDS. See open question 1.

## 3. Slices

Prerequisite P (not slice 4, owned by the slice 1-3 agent or a new one): land the daemon UDS API — adopt the parked branch (`agents.request`, `agents.health`) and add `agents.events`. Without P, slice 4 has nothing to call.

| Slice | Content | Rung | Red test (fails before) |
|---|---|---|---|
| 4a | Daemon `agents.events` stream method + `ppid` in `agents.health` (extends P) | 5 (cross-crate contract) | `trusty-agents` test: open `agents.events` over a temp socket, publish an `AppEvent::AgentMessageDelta`, assert the frame arrives; fails: method unknown |
| 4b | Shell `agents_uds.rs` + `agents_request` command + `ensure_api_server` rewrite (no port) + reqwest removal in `task_commands.rs`/`sidecar.rs` | 5 | `trusty-agents-ui` test against a stub socket: `list_tasks`, `send_message` poll loop and health use the socket; assert the process opens no TCP connection (reqwest dependency removed so it cannot compile-regress) |
| 4c | Shell event stream (replace `sse_bridge.rs`) + custom URI scheme | 5 | stub stream emits 3 frames, assert 3 `app-event`s and a reconnect after stream close; URI-scheme handler test: asset path returns bytes with the stored MIME |
| 4d | UI: `agentsFetch`, rewrite the helper sites, delete `apiBase`/token/ticket paths, `connectEventSource` over `listen` | 6 | vitest: with `apiBase` removed, `tmApi('/api/projects')` calls `invoke('agents_request', ...)`; fails today (calls `fetch`). Existing `transport.test.ts`, `chatStream.test.ts`, `eventBridge.test.ts` updated |
| 4e | Smoke + gates: `scripts/check_no_tcp_listeners.sh` clean for the shell, Playwright `smoke.spec.ts` updated, one packaged-binary run: launch app, send a message, see streamed tokens, `lsof -iTCP -sTCP:LISTEN` shows no agents/UI process | 6 | `smoke.spec.ts` tauri-mock variant asserting no `fetch` to `http://localhost:*` (network log empty) |

4b-4d can be one PR if slice 1 lands in the same release (see §4); 4a must precede 4c.

## 4. Ordering risk — does slice 1 (listener removal) break the shipped UI unless slice 4 lands first or in the same release?

**Yes.** Evidence:
- `sidecar.rs:150` spawns `trusty-agents --api --port <p>`; once `--port` and the listener are gone, the sidecar either fails to parse its args or starts with no endpoint.
- `sidecar.rs:290` health check and `task_commands.rs:55,86,201,228,345` are reqwest to `http://127.0.0.1:<port>`; `sse_bridge.rs:150` likewise. All fail.
- Beyond the shell, the webview itself `fetch`es `http://localhost:8765` at ~50 sites (`api-config.ts:26`), so agents, projects, channels, costs, history and KG panes all break even if the shell commands were fixed.
- `App.svelte:171,200` waits up to 20 s for `check_health`, then leaves `apiReady` false: the packaged app shows a dead chat.
- Compounding: no UDS API replaces the listener on main (§0.1). Slice 1 merged alone leaves the UI broken *and* the REPL/CLI without a daemon surface.

Recommendation: P + 4a-4d ship in the same release as the listener removal; do not merge slice 1's listener removal to `main` before 4d is reviewable, or gate it on the same release train.

## 5. Open questions for the owner

1. **Browser/dev mode (`pnpm dev`, the SPA embedded in the daemon via `ui.rs`, vite proxy).** Options: (a) drop it; the UI becomes Tauri-only and the embedded SPA and `ui.rs` go (recommended; the owner rule says no local HTTP, and the tcode precedent had the same question answered by trusty-console later); (b) keep browser mode served by trusty-console via the console's gateway to the UDS (more work, belongs to the "console-hosted ui/dist" slice 3 in the #6433 scope comment); (c) a Tauri-mock for Playwright only. Affects 4d and 4e size.
2. **Daemon transport shape: `agents.request` tunnel (parked branch) vs typed methods.** Options: (a) tunnel, as the earlier ruling recorded in the parked branch header (recommended; ~700 lines, route table stays single-sourced); (b) typed per-route methods, ~47 methods, ~3-4.5k lines, ~35 routes have no consumer once the SPA moves. The UI plan holds either way, since `agentsFetch` hides it, but only (a) is already written.
3. **Who owns prerequisite P?** Options: (a) the slice 1-3 agent adopts the parked branch, rebased, before removing the listener (recommended; otherwise the daemon has no surface at all); (b) a separate slice 2.5 issue; (c) slice 4 absorbs it (grows rung 5 work into the UI PR).
4. **Event delivery to the webview: forward everything vs the current filter.** Options: (a) forward all `AppEvent`s over one `app-event` channel and reuse `eventBridge.ts` (recommended; deletes the desktop/browser split); (b) keep today's delta and slack-only filter plus separate Tauri event names (smaller diff, keeps the split).
5. **Attachment/asset bytes.** Options: (a) custom URI scheme `agents-asset://` (recommended; `<img>` and download links keep working unchanged); (b) fetch bytes through `agents_request`, build `blob:` URLs in JS (no new scheme, but every image costs a base64 round trip capped at 6 MiB).
6. **Rollout.** Options: (a) single release with P, slice 1 and 4a-4d (recommended, per §4); (b) a transition release where the daemon serves both HTTP and UDS and the UI prefers UDS, then removal next release (safer for installed apps; keeps a TCP listener one more release, which the 2026-11-03 rule disallows).
