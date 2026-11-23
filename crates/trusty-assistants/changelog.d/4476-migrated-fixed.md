Fixed

- **The daemon now writes a real, best-effort log file, scoped per project
  and port** (#4111). `--serve`'s detached child previously had
  stdin/stdout/stderr all redirected to `/dev/null`, so a daemon crash was
  completely undiagnosable — no `~/Library/Logs/trusty-agents/` directory
  ever existed, unlike every other trusty-* daemon.
  `service::start_service` now opens (and rotates, once past 10 MiB)
  `~/Library/Logs/trusty-agents/daemon-<project-hash>-<port>.log` and
  redirects the child's stderr to it instead of discarding it; stdin/stdout
  are unchanged (`/dev/null` / `/dev/null`), preserving the existing "stdout
  stays clean for MCP JSON-RPC framing" contract. Log setup is best-effort —
  an unwritable log directory, a full disk, or a permissions problem falls
  back to `Stdio::null()` rather than aborting the daemon spawn, so a
  logging failure can never take down Concierge/Telegram/Slack with it. The
  log path is keyed by BOTH a hash of the project root (mirroring
  `pid_file_path()`'s own per-project identity — port alone didn't actually
  prevent clobbering, since every project shares the same hardcoded default
  port) and port, so two concurrently-running daemons don't interleave into
  (or rotate-clobber) the same file. The daemon's own tracing setup
  (`runtime::startup`) is unchanged — it already wrote to stderr in
  non-interactive mode; the bug was purely the spawn call discarding that
  stream.
- **A failed task no longer renders a raw Rust error as if the assistant said
  it, in either Tauri or browser/`pnpm dev` mode** (#4320). Both
  `send_message` (Tauri `ui/src-tauri/src/task_commands.rs`) and its browser
  fallback (`ui/src/lib/transport.ts`'s `fetchFallback` poll loop — the sole
  narrative-delivery path in that mode) emitted `task-complete` for every
  terminal poll response regardless of status, so a `status: "error"`
  response's narrative — a raw failure string such as `subprocess exited
  with status Some(1)`, or a Debug-formatted `anyhow::Error` — landed in
  `ChatView.svelte`'s `task-complete` handler, which renders `narrative`
  verbatim with no error framing. Both now emit `task-error` (the same
  event, with the same `"Error: "`-prefixing handler, that transport
  failures already use) for `status: "error"` responses; `partial`/
  `cancelled` keep the existing `task-complete` framing since their
  narratives are already user-facing text, not raw errors.
- **Knowledge Graph browser: a mid-session daemon/palace failure now explains
  itself instead of rendering as an empty graph** (#4290 code-review
  finding). Only `bootstrap()`'s initial `/kg/subjects` call checked
  `env.connected`; `loadAll`, `loadCount`, and `loadSubject` assigned
  `env.data` unconditionally, so a `connected: false` response arriving AFTER
  a successful bootstrap — trusty-memory restarts, the palace goes
  unreachable — rendered as "0 active triples" / "No triples.", visually
  identical to a genuinely empty, connected palace. All three now check
  `env.connected` first and route into the same disconnected state
  `bootstrap()` already shows, carrying the backend's `reason`.
- **The Sub-agents pane no longer advertises delegation targets the gate
  refuses.** Its copy said targets were *"fixed by the role allow-list
  (engineer, qa, researcher, documentation, ops, planner, assistant)"* — true
  of the constant, misleading as presented: `assistant` is in that list, but
  ADR-0024's delegation KIND rule independently refuses every assistant →
  assistant edge, so a reader took the list as a promise of reachability the
  code denies. The pane now states the EFFECTIVE rule — role-eligible MINUS
  kind-refused — quoting the allow-list as an eligibility filter, then saying
  plainly that eligibility is not reachability and that a peer assistant is
  never a delegation target (assistants communicate with each other rather
  than delegating to one another). The refusal is attributed to the KIND rule
  rather than the tier gate, which is what post-#4296 is actually true: with
  assistants at L0 both endpoints of that edge are L0, so tier is vacuous for
  it. Copy-only — no gate, payload or reachability logic changed, and
  `ASSISTANT_ALLOWED_DELEGATE_ROLES` deliberately still carries `assistant`
  (its doc comment records why the entry is kept: the constant also feeds the
  `/subagents` route's `allowed_roles` report, and the kind rule must be
  enforced by code, not by a curated list of names). The stale doc comments
  that described the rule as "role allowlist + tier gate" were corrected to
  name the kind rule too.
- **Cost figures were priced at Haiku rates for every model (#4098).**
  `usage::daily` carried a second, hardcoded two-constant rate table that the
  REPL statusline and the persisted daily total both used, so a Sonnet session
  — the default — displayed roughly a twelfth of its real cost. The duplicate
  table is deleted, not corrected; `perf::pricing::cost_usd` is now the single
  pricing entry point for the statusline, the daily total, the perf collector,
  and the new Costs tab. Its token counts widened from `u32` to `u64` so the
  aggregate callers cannot saturate a total.
- **The Sub-agents pane no longer offers `hidden` agents as delegation targets
  (#4235).** `GET /api/agents/:name/subagents` filtered its in-product target
  list by role and by the kind/tier gate but never by `hidden`, so
  `assistant`, `personal-assistant` and `researcher` — all `hidden = true`
  since #3819 — leaked back in as target cards, rendering a SECOND row
  display-labelled "Izzie" beside `izzie`. The pane now applies the same
  `!hidden` filter the picker/roster applies, through the same
  `projects::is_hidden` predicate (promoted from private to `pub(super)`
  rather than reimplemented, so the two surfaces cannot drift). A hidden agent
  is OMITTED rather than shown with a reason — `hidden` is a listing-surface
  flag, not a gate, and a labelled row would still draw the duplicate and
  would name an id the pane's no-roster-dump posture keeps unnamed. It is
  counted instead, in a new `in_product.hidden_excluded_count`, disjoint from
  `role_excluded_count`, which keeps its existing meaning unchanged. The pane
  renders that count on its own line ("N further agent(s) … are hidden"), so
  the omission is stated rather than silent, and the two exclusion reasons stay
  separate — they have different remedies. This weakens no gate: `hidden` has
  only ever governed visibility, and delegating to a hidden agent by name is
  unaffected.
- **`GET /api/agents/:name/stores` (and the new `/knowledge` route above) no
  longer report `connected: true` for an index whose corpus failed to open
  (#4115).** `resolve_store_statuses` previously derived `connected` from
  HTTP reachability alone — a warm-booted index with `corpus_open_failed`
  answers the status probe with 2xx, `status: "ready"` and `chunk_count: 0`
  while every staged-pipeline lane is `Failed`, and the store card showed a
  healthy green connection for a corpus that answers zero queries.
  `StoreStatus` now derives `connected` from the response's `stages` object
  (the same ground truth trusty-search's own `/health` handler uses,
  `IndexStages::any_failed`) and carries a new `failed_stages` field naming
  which lane(s) failed, instead of collapsing everything to a boolean.
- **Streamed replies no longer flash blank or "(no narrative)" the moment a
  task finishes** (issue #3759): in browser mode the GUI's SSE→web-bus bridge
  emitted a `task-complete` with a hardcoded empty narrative on every
  `session_done`, racing the poll loop's `task-complete` that carries the real
  text. Last writer won, and with token streaming (#3753) `session_done`
  normally landed first — up to a poll interval ahead — wiping freshly streamed
  prose mid-read. `session_done` now stays off the web bus entirely (its status
  bookkeeping already flowed through the workflow store and the Events log), so
  a task's narrative has exactly one emitter per transport. Two consequences,
  both deliberate: a foreign session finishing mid-turn — the `/api/events`
  stream is a process-wide broadcast — no longer clears this tab's spinner; and
  the spinner now stops on the poll-driven completion rather than on the SSE
  event, so it runs up to one poll interval longer (250ms for the first ~5s of
  a turn, 500ms after). That bound is the price of a single narrative emitter
  and errs the safe way — a spinner that over-runs reads as "still working",
  where stopping early reads as "done" over text that has not arrived.

- **Relay token compared in constant time; reply identity validated
  server-side** (issue #3761): `POST /api/internal/relay-event` compared its
  shared secret with `==`, which short-circuits on the first differing byte and
  leaks the matching-prefix length to a caller able to time its own requests —
  now a constant-time byte comparison via `subtle::ConstantTimeEq` (length is
  still checked first; a token's length is not secret). The same treatment is
  applied to the API bearer token in `auth_middleware`, which is the
  higher-value of the two secrets since it gates every `/api/*` route. The
  `SlackReplySent` arm also published `identity` completely unchecked, so any
  holder of the relay token could stamp a reply with a fabricated speaker such
  as "CTO Bot (as Bob)" and the mirror pane would render it as truth. The
  server knows the one honest value (`slack::BOT_IDENTITY`) and now rejects
  anything else with `422`, mirroring the existing closed-set RBAC tier check
  on the inbound half.

- **`cto-assistant`'s Gmail and Google Tasks tools are no longer scope-denied at
  dispatch** (issue #3938): the bundled `cto-assistant` DIRECTORY PACKAGE
  (`.trusty-agents/agents/cto-assistant/agent.toml`), which wins the loader's
  resolution order over the flat `cto-assistant.toml`, declared no
  `[tools].scopes` — so it inherited only the base assistant's `google.read` by
  union. No gworkspace tool ever advertises that scope: discovery maps every
  `x-google-scopes` OAuth URL to `google.<family>.<access>` (`google.gmail.write`,
  `google.tasks.write`), and `ScopePattern::matches` compares on segment
  boundaries, so `google.read` matched nothing and the per-agent scope gate in
  `filter_persona_tool_names` (#3208) dropped the persona's entire Gmail/Tasks
  surface before the model saw it. The package now declares
  `scopes = ["google.gmail.*", "google.tasks.*"]` explicitly — narrowly, the two
  families its allowlisted tools actually need, not a blanket `google.*` —
  restoring the line the flat file has always carried and that the package
  conversion dropped. A new test resolves the SHIPPED package through the real
  loader, derives each tool's dotted scope from the real `trusty-gworkspace`
  `rpc.discover` document, and runs the real dispatch-time filter, so
  re-dropping the declaration fails in CI instead of at a user's dispatch.

- **Streamed chat turns now send the same sampling parameters as the blocking
  path** (issue #3758): `llm::stream::stream_reply` built its
  `OpenRouterProvider` with no temperature, token ceiling, or stop sequences, so
  a streamed conversational/persona reply ran on provider defaults while the
  blocking fallback for the SAME turn sent `llm.temperature`, `llm.max_tokens`,
  and `llm.stop_sequences` — the streamed and fallback answers could differ in
  style, verbosity, and where they stopped. Both call sites (the persona turn
  and the conversational fast path) now forward their turn's exact values,
  including the conversational path's `effective_max_tokens`, which the
  local-route branch may override.

- **A truncated stream no longer renders as a complete answer** (issue #3757):
  `stream_with_provider`'s empty-stream guard only caught a stream that produced
  NOTHING, so a partial-then-truncated stream returned its cut-off text as a
  success and the blocking fallback never engaged. The upstream pump now emits
  `ChatEvent::Error` for a mid-stream error frame, a truncated EOF, and an
  unusable non-SSE 200, so those surface through `assembly.error` and the caller
  falls back to the blocking chat path.
- **`GET /api/agents/:name/skills` read a narrower catalog than dispatch
  (#3933):** the route built a built-in-only catalog while both dispatch paths
  built built-in + authored, so an authored-only skill id was reported to the
  user as a dangling `unresolved[]` reference even though it resolved and
  granted at runtime, and authored overrides (`web-search.md`'s name) never
  reached the pane. The route now builds the catalog exactly as dispatch does.
- **Skill-source precedence was silently inverted (#3933):**
  `skills/sources/mod.rs` documents "higher-priority sources scanned first,
  first writer wins", but the authored-overlay loop let a later (lower-priority)
  manifest evict an earlier (higher-priority) one. Conflicts now displace only
  built-in rows, so among authored manifests the first source wins while
  authored still beats built-in.
- **`listeners::store::EventStore`'s `$HOME`/`HOME_LOCK` cross-test race
  closed via dependency injection (issue #3922).**
  `dedup_seed_loads_recent_ids` failed once in CI (run 30165303605) with
  `assertion failed: ids.contains("id2")`, then passed on rerun. Root cause
  (confirmed by local reproduction, not just inferred): `llm::http::tests`'s
  five credential-resolution tests sandbox `$HOME` under
  `#[serial_test::serial]` ALONE — a `std::sync::Mutex` (`HOME_LOCK`) can't
  be held across `.await`, so they never take it — while `listeners::store`'s
  own tests sandboxed `$HOME` under `HOME_LOCK` alone. The two groups never
  actually excluded each other. Looping just these two test groups together
  under maximised parallelism reproduced the failure at 2/5 runs, with a
  captured panic (`ids.contains("id1")` / a spurious ENOENT opening another
  test's already-removed tempdir) matching the CI failure's shape. New
  `EventStore::append_at`/`read_events_at`/`recent_ids_at`/`load_filters_at`/
  `set_filter_at`/`is_event_type_included_at` let a caller inject its target
  directory directly instead of relying on the shared `$HOME`; every test in
  `listeners::store::tests` now uses this seam and no longer touches the
  process environment at all, so it can never again be a party to this race
  regardless of what any other test in the crate does to `$HOME`. Production
  behaviour is unchanged (the plain, non-`_at` methods still resolve
  `events_dir()` from `$HOME` exactly as before). A `#[cfg(test)]`-only guard
  in `events_dir()` now panics on the first run of any FUTURE test that
  reaches it without the CALLING thread holding `HOME_LOCK`, turning the
  next instance of this omission into a deterministic failure instead of a
  flake. A new standing regression guard,
  `concurrent_event_store_scenarios_survive_home_env_hammering` (issue
  #3925), runs four `EventStore` scenarios concurrently against a
  background task that hammers `$HOME` with zero synchronization — the exact
  #3922 attack shape — and asserts none lose data; validated to fail
  deterministically (not probabilistically) when the DI seam is reverted.
  **Code-critic follow-up (HIGH, fixed):** the guard's first version checked
  `HOME_LOCK.try_lock().is_ok()` — global contention, not per-caller
  ownership — so a background thread holding the lock for an unrelated
  reason masked a genuinely unguarded caller on a different thread (the
  critic reproduced this directly). New `crate::test_env::lock_home()` /
  `home_lock_held_by_this_thread()` track ownership via a thread-local flag
  instead (sound because every test in this crate uses the default
  current-thread `#[tokio::test]` runtime), and `listeners::poll`'s cursor
  tests plus the `api::server`/`slack` handler-level tests that still
  sandbox `$HOME` directly were migrated to the new helper. A new test,
  `events_dir_panics_when_a_different_thread_holds_home_lock`, pins the
  exact masking scenario the critic found. Stress-testing this fix (60+
  loop iterations of the full affected test set under `--test-threads=16`)
  also surfaced an unrelated, pre-existing bug: `EventStore::append_at`
  never called `.flush()` after `write_all`, so `tokio::fs::File`'s
  background write could still be in flight when a separate `read_events_at`
  call opened a fresh handle to the same path — observed as an intermittent
  "0 events immediately after appending 1" under heavy concurrent test load.
  Fixed with an explicit `.flush().await`.
- **Instructions editor is a first-class editing surface (#3894, GUI).**
  #3862's `55vh`/`70vh` clamp fixed a 2-line strip by trading it for a field
  capped independently of the space available — dead area below it on a tall
  window. In the full-pane takeover the persona editor now grows into every
  remaining pixel (`flex-1`/`min-h-0`, no `rows`, no viewport clamp) and is the
  only scroll container in its tab, so there are no nested double scrollbars.
  Measured in a real browser: 26 visible lines at a 900px viewport, 52 at
  1400px. The Tools allowlist textarea gets the same treatment, and a
  Playwright spec (`ui/tests/config-takeover.spec.ts`) now measures the
  rendered height at two viewports so a third regression cannot ship quietly.
- **Chat no longer yanks you back to the newest message.** `ChatView` forced
  `scrollTop = scrollHeight` after every render, so a streaming delta pulled a
  reader who had scrolled up back to the bottom — and reset the scroll offset
  of the chat sitting behind the config takeover, which is supposed to be left
  exactly as it was. It now re-anchors only while the reader is already at the
  bottom (`lib/chatScroll.ts`).
- **Bulk backfills are bounded and resumable.** `max_messages`/`max_files` were
  taken from the model with no ceiling, and every fetched item accumulated in
  memory with the ledger committed only after the whole batch — so a large pull
  could exhaust memory, and a crash mid-fetch lost all progress. Both are now
  hard-clamped regardless of the caller's value, and items are fetched and
  committed in chunks, making each chunk a durable commit point. Drive's
  deletion sweep runs once at the end (via `okg_sweep_deleted`) rather than
  per chunk, which would have tombstoned everything outside the current page.
- **Drive files with no revision signal are re-ingested rather than frozen.**
  Such a file received a constant fingerprint, which the ledger read as
  "unchanged" forever. They are now marked volatile and always re-fetched.

- **`vector_search` had no `index_id` parameter and silently searched the
  wrong corpus (#3864):** personas documented `vector_search(index_id=…)`
  while the tool advertised no such field and was hardwired to the
  CWD-relative `.trusty-agents/state/code/` index, so index routing was a
  no-op (the live workaround was to grant `grep` instead). The tool now takes
  an optional `index_id`, defaulting to the agent's bound OKG store index, and
  routes to the shared trusty-search daemon (`POST /indexes/{id}/search`) when
  one resolves — an unqualified search hits the agent's OWN knowledge, an
  explicit id still targets another corpus. Every failure (undiscoverable
  daemon, missing index, transport error) degrades to the embedded index and
  then to the regex fallback, never an errored turn. `vector_search` is also
  now registered on the persona-chat dispatch path, where it previously was
  not — the grant in `cto-assistant`'s allowlist had nothing behind it.
- **Agent config gear panel's Personality/Tools textareas were unusably
  cramped:** the `persona.md` editor on the Personality tab (and the tools
  allowlist editor on the Tools tab) rendered as a ~2-line strip regardless
  of content length, because their `flex-1` sizing had no definite height
  floor. Both textareas now get a `min-h-[55vh]`/`max-h-[70vh]` range with
  vertical resize (`resize-y`) and internal scroll; the Personality Save
  button is now sticky so it stays visible below the taller field.
- **Agent pickers list only assistant-role agents, not the 16 bundled
  coding/engineer/support agents:** the GUI/Tauri picker and MCP
  `list_agents` tool (both backed by `agent_roster`/`scan_agent_catalog`)
  and the REPL's `/agents` slash command (previously a flat, non-role-aware
  `discover_agent_names` glob that disagreed with `/agent`) now filter to
  `role == "assistant"` — a missing/unparseable role is treated as
  NOT-assistant (fail-closed), so a malformed manifest can never leak a
  coding agent back into a picker. `/agents` now delegates to the same
  filtered listing `/agent` (no arg) already used, so the two can't drift
  again. `*.stale.bak` directory-package backups (e.g. `izzie.stale.bak/`)
  are additionally excluded from the REPL listing (the GUI/API catalog
  already excluded them). Direct by-name resolution — `/agent <name>`,
  `/switch`, and `delegate_to_agent` (which resolves targets directly via
  `AgentConfig::by_name_in`, never through the roster) — is unaffected, so
  ctrl/pm/coding agents remain fully dispatchable though no longer listed.
- **Agent picker no longer loses Izzie / CTO Bot on a cold start:** the
  `AgentSwitcher` (and `ModelSwitcher`) fetched their catalog exactly once in
  `onMount`, but `<Header>` — and therefore both pickers — renders before
  `apiReady`. On a packaged-app cold start the sidecar isn't listening yet, so
  that first `GET /api/agents` fetch failed, `catalogAgents` stayed empty with
  no retry, and the roster showed only the built-in "Assistant" for the whole
  session (Izzie / CTO Bot never selectable). `App.svelte` now re-drives the
  agent + model catalog loads (and the overlay refresh) whenever `apiReady`
  flips true, backfilling the already-mounted pickers via their reactive stores
  (and refetching on any later sidecar reconnect). The `GET /api/agents` roster
  itself was already correct — it returns all bundled personas including the
  `izzie`/`cto-assistant` directory packages.
- **Recent Tasks no longer lists chat replies as junk "(empty)" rows:** every
  chat turn (a Conversational/Research message) is finalized as a
  `type: "agent_response"` `PmResponse` with an empty request field, so
  `GET /api/tasks` returned them and the workflow-oriented Recent Tasks panel
  rendered them as "(empty)" success rows. The panel now filters on a shared
  pure predicate (`lib/taskHistory.ts::isDisplayableTask`) that excludes
  `agent_response` (chat) envelopes — chat is not a workflow run — while keeping
  the pre-existing rule that hides the contentless running placeholder; the
  empty-state and Clear affordance now key off the visible count.
- **Telegram `/switch assistant` now resolves the canonical package persona:**
  the `/switch` pre-check validated FLAT persona files only
  (`.trusty-agents/agents/<name>.toml` and the `$HOME` equivalent), so the
  canonical `assistant` persona — which ships ONLY as a directory package
  (`agents/assistant/agent.toml` + `persona.md`, no flat `assistant.toml`) —
  was rejected with "Unknown persona: assistant" even though the real dispatch
  resolver (`AgentConfig::by_name_async`) loads directory packages fine. The
  pre-check (both the project-local tier and the `$HOME` fallback) now also
  accepts a directory package (`agents/<name>/agent.toml`), mirroring the
  resolver's order. No alias-table or migration changes.
- **MCP tool results no longer collapse the live conversation to the system
  prompt:** an MCP tool result (e.g. `grep` via trusty-search) is shrunk by no
  `compress_tool_output` filter and can be multiple megabytes; the send-time
  context trimmer's pure oldest-first, whole-message eviction then evicted the
  user's question AND the assistant tool-call turn AND the result itself,
  leaving only the protected system message — so the follow-up completion
  answered with zero context (`input_tokens` flatlined at the system-prompt
  size and the model ignored the tool output). `ContextManager::trim_to_budget`
  now splits the history into three regions — the protected system header, an
  OLD history region, and a RECENCY window (the "live turn": from the last user
  message to the end) — and applies strategies in increasing order of damage:
  (1) water-fill-truncate only the OLD region, keeping the header AND the newest
  turn at FULL fidelity; (2) evict oldest OLD messages (never the header, never
  the recency window); (3) as a last resort — when the oversized message is
  itself inside the live turn (the MCP shape) — truncate within the recency
  window so the question and every assistant/tool pairing survive rather than
  being evicted. This preserves THREE guarantees at once: the live conversation
  never collapses; the **newest turn keeps full fidelity** whenever the budget
  allows (restoring what oldest-first eviction implicitly gave — a
  uniformly-sized long conversation no longer shrinks its most recent message);
  and **no assistant `tool_calls` message is ever separated from its `tool`
  results** — the recency boundary is pairing-atomic (it can't split a
  multi-tool-call group even in a tools-only continuation with no user message)
  and Strategy 2 evicts tool-call groups atomically by sweeping the whole
  history regardless of message ordering/adjacency (so even adversarial or
  replayed interleaved groups leave whole), so the request never carries an
  orphaned `tool_call_id` that OpenAI/OpenRouter would reject.
  Truncation is logged distinctly from eviction so it never masquerades as the
  catastrophic path.
- **Chat stream renders inside ONE stable assistant bubble — no mid-stream
  flicker:** as `task-delta` tokens arrived the reply bubble visibly flashed
  because each delta fired two separate `messages` store writes (content, then
  speaker), doubling the keyed-`{#each}` re-render + forced scroll reflow per
  token. Now a single `streamDeltaIntoTask` write funnels content + speaker
  into the ONE bubble whose `taskId` matches the delta's globally-unique
  backend id, rewriting only `content`/`speaker` so the message `id` is
  preserved (the keyed `{#each}` patches, never re-mounts). Attribution is by
  exact backend id, searched across every conversation — never by list position
  and never scoped to the currently-viewed project — so a background stream
  reaches its own bubble after a project switch and one turn's tokens can never
  land in another turn's or project's bubble; an unattributable frame (e.g.
  arriving before the poll loop reconciles the `pending-<ts>` id) is dropped,
  and its text — held by the shared `streamAccumulator` — appears on the first
  matching write. No-op frames skip the store `set` entirely, so they notify no
  subscribers. `InputArea`'s reconcile and `ChatView`'s progress handler both
  gate on the shared `streamAccumulator` so neither overwrites live streamed
  text.
- **Persona chat turns no longer exhaust `max_turns` after a couple of tool
  calls:** `run_pm_task_with_persona` (the `/agent`/`--direct` persona REPL
  turn) hardcoded its `chat_with_tools_gated` ceiling to 2 turns (no tools) or
  4 turns (with tools) since #254 and never consulted config, unlike the
  general subagent tool loop's `[llm].max_turns`. A persona running a couple
  of sequential tool calls (e.g. two `grep`s plus a summary) routinely
  exhausted the 4-turn budget and failed with `chat_with_tools exceeded
  max_turns`. The tool-using default is now 8 (raised from 4) and is
  overridable per agent via the new `[llm].persona_max_turns` TOML field; the
  no-tools case is unchanged (still 2).
- **Izzie's own tools now reach the `--direct`/`--agent` dispatch path (#3745
  item C):** `tagent --direct izzie` loaded a tool registry of only
  `[git_log, git_status, web_search, delegate_to_agent]` and the model answered
  "weather tool unavailable" — despite `izzie/agent.toml` declaring
  `get_weather`, `get_train_schedule`, and `get_train_alerts` in `[tools].allow`.
  Root cause: the assistant-tier subprocess registry
  (`runtime::tool_registry::build_assistant_tier_registry`) never registered the
  izzie platform-hosted executors, so `scope_assistant_allowed_tools` intersected
  the (correct) allow list against a registry lacking those tools and dropped
  them. The persona-chat REPL path (`run_pm_task_with_persona`) already registered
  them via `crate::tools::izzie::izzie_tools()`; the two dispatch paths had
  diverged. Fix registers `izzie_tools()` into the assistant-tier registry so both
  paths behave identically — reachability is still gated per-persona by
  `[tools].allow`, so no other assistant-tier persona is widened. Config
  resolution (package-vs-flat, `extends` merge) was verified correct and is
  unchanged.
- **`tagent --api` self-exits when its parent GUI dies (#3734):** the sidecar
  now accepts `--parent-pid <pid>` and, when given one, arms a parent-death
  watchdog that self-exits the process once that parent is gone (detected by
  reparenting away from it — on macOS to launchd, pid 1 — or the pid vanishing
  outright). This closes the orphan class the Tauri-side quit reap cannot cover:
  a GUI killed by macOS's synchronous Cmd+Q teardown, a crash, or an external
  SIGTERM/SIGKILL previously left `tagent --api` running and holding its fixed
  port, so a later GUI launch could fail to bind or attach to a stale backend.
  Absent the flag (e.g. a hand-run `tagent --api` in a shell) the watchdog is
  simply not armed. Unix-only; a no-op on other targets. `GET /api/health` now
  also reports the sidecar's `pid` and `ppid` so a GUI can verify it is talking
  to the sidecar it spawned rather than a reparented orphan on the same port.

- **A parallel phase could not fail when its merge failed (#3671):**
  `executor::dispatch` folded `ConflictResolver::merge`'s `Err` into a
  `"merge failed: ..."` string appended to the phase's content and returned
  `Ok(AgentOutput)` regardless. `merge()` only errors on a genuine I/O fault
  (`create_dir_all`/`read_dir`/`read`/`write`) — every recoverable conflict
  (a failed `git merge-file`, a failed LLM fallback) is already degraded to
  `Ok` with the reason recorded in the merge report. So a real `Err` meant the
  merged tree on disk was incomplete and nondeterministic (the collection
  loop walks a `HashMap` and can bail partway through), yet downstream phases
  consumed it as if it were whole, with the failure visible only as a buried
  string. `merge`'s `Err` now propagates as `WorkflowError::PhaseFailed`,
  failing the phase/workflow instead. Sibling of #3652/#3653/#3675 (the same
  "silent success" bug class).
- **Parallel-phase conflict resolution no longer silently truncates files to
  zero bytes when `git merge-file` fails (#3652):**
  `ConflictResolver::resolve_conflict` read the merge subprocess's stdout
  while ignoring its exit status. `git merge-file` signals three distinct
  outcomes through one integer — `0` clean, `1..=127` conflict count, and
  anything above that a git *error* — and on error it prints nothing. So any
  condition that broke git's repository discovery (corrupt worktree,
  `safe.directory` "dubious ownership" refusal, a stale `GIT_DIR`) produced
  empty stdout, which the marker scan then read as a clean merge, and the
  resolver wrote a zero-byte file over both sub-agents' work. Exit status is
  now decided explicitly by the pure `decide_merge_bytes`, a git failure
  degrades to first-agent-wins instead of to an empty file, and byte-identical
  versions short-circuit before git is spawned at all — so merging N copies of
  the same file is a true no-op regardless of the ambient git environment.
- **The same zero-byte truncation on the LLM resolution path — the *default*
  production path — is fixed (#3652 follow-up, review of #3653):**
  `llm_resolve` did `resp["choices"][0]["message"]["content"].as_str()
  .unwrap_or("")`. `reqwest`'s `send()` does not error on 4xx/5xx (that needs
  `error_for_status()`), and OpenRouter's error payload is well-formed JSON,
  so an expired key (401), a rate limit (429), an out-of-credits 402, or an
  empty `choices` array all parsed cleanly, extracted to `""`, and were
  written over both agents' work. This branch runs whenever an OpenRouter key
  is configured — which `executor::dispatch` resolves by default — so every
  genuine conflict went through it, and its triggers are routine. It also had
  zero test coverage, because the existing tests passed an empty API key
  specifically to disable it. `llm_resolve` now calls `error_for_status()?`
  and parses through `extract_llm_content`, which requires a non-blank
  `content` string and returns `Err` otherwise, so the caller degrades to a
  real version. Regression tests drive a local HTTP stub through 401/402/429/
  500 and a 200-with-no-choices.
- **The non-empty invariant is now enforced structurally at one seam rather
  than trusted per branch:** `resolve_conflict` routes every branch's result
  through the pure `enforce_non_empty`, which rejects an empty resolution for
  non-empty input and degrades to the first agent's full version. A future
  resolution path therefore cannot truncate a file even if it forgets the
  rule. Genuinely empty agent output is still passed through unchanged.
- **Degraded conflict resolution is no longer reported as a successful merge
  (review of #3653):** `merge()` stamped `[MERGED from a+b]` and counted
  `Conflicts resolved: N` identically whether the bytes came from a real
  structural merge, an LLM merge, or first-agent-wins after git or the LLM
  failed — and that report is the only channel downstream phases read
  (`executor::dispatch` appends it to the phase's `AgentOutput`; the
  `tracing::warn!` goes to a log nobody downstream reads). Resolution mode is
  now recorded per file and rendered distinctly — `[MERGED from a+b]`,
  `[LLM-MERGED from a+b]`, `[DEGRADED from a+b] … kept 'a', discarded the
  rest (reason)` — the summary splits into `conflicts: N (merged: X,
  degraded: Y)`, and any degradation puts a `!! DEGRADED` block at the top of
  the report. Report line ordering is now sorted, so it is reproducible
  across runs despite `HashMap` iteration order.
- **Flaky `HOME`-race unit tests in `skills::sources::tests` fixed (#3607):**
  `remote_git_without_subdir_uses_cache_dir_directly` and its siblings
  `remote_git_cache_path_computed_correctly` /
  `sources_resolved_paths_expands_tilde` each called `dirs::home_dir()`
  twice — once implicitly inside `SkillSourceRegistry::resolved_paths()`,
  once directly to build the expected value — without holding
  `test_env::HOME_LOCK`, so a concurrent test's `$HOME` sandboxing could
  change the value in between the two calls under `cargo test`'s default
  multi-threaded runner. All three now hold `HOME_LOCK` for their full
  body, matching the crate-wide convention documented in `test_env.rs`.
- **`gworkspace` OpenRPC endpoint no longer silently discovers zero tools
  (#3577):** `DirectDriver::discover()` deserialized `rpc.discover`'s raw
  result directly into `EndpointManifest`, which expected a top-level
  `tools` array; every real server (`trusty-memory`, `trusty-search`,
  `trusty-gworkspace`) actually emits standard OpenRPC's `methods` array.
  Because `EndpointManifest.tools` carried `#[serde(default)]`, the shape
  mismatch deserialized successfully with `tools: vec![]` instead of
  erroring — the `gworkspace` endpoint (enabled by default since #3056)
  contributed zero tools in production with no error anywhere. New
  `discovery::parse_manifest` recognizes the real `methods` shape (and the
  legacy `tools` shape), converts each OpenRPC method into a
  `DiscoveredTool` (JSON Schema `input_schema`/`output_schema`
  reconstructed from `params[]`/`result.schema`), and resolves scope from
  `x-scopes` (dotted string, used as-is) or `x-google-scopes` (OAuth URLs,
  mapped to a dotted scope via the new `google_scope` module — classified
  by what the OAuth grant permits, not by today's per-tool behavior). A
  payload with neither `methods` nor `tools` now returns a hard error
  naming the endpoint and the keys actually present, instead of silently
  producing an empty tool list; a manifest that parses successfully but
  advertises zero tools now logs a loud warning naming the endpoint.
  `default-config.toml`'s gworkspace endpoint scopes gained
  `google.slides.*` and `google.accounts.*`, without which the Slides and
  local-account-management tools would parse but then be dropped by scope
  filtering.
- **Bundled agent templates now refresh automatically when the binary's embedded content changes, instead of freezing at whatever was deployed on first run (#3556).** `agents::bundled::deploy_bundled_agents` has always been a strict "write missing files only" deploy — `ensure_bundled_agents_deployed` (run once per process from `runtime::startup::run_startup_init`) called it unconditionally, so a machine that deployed the bundled roster once NEVER picked up a later binary's changes to those same templates. Concretely: PR-A's `delegate_to_agent` grant added to the base `assistant` agent never reached a machine that already had the old `assistant/agent.toml` on disk — the assistant silently declined to delegate, and the only fix was a human manually deleting the stale file. Root cause fixed via a version-stamped reprovision pass:
  - A new `agents::bundled::stamp` module computes a stable SHA-256 content hash over the embedded bundle's `(path, bytes)` set and persists it as `$HOME/.trusty-agents/agents/.bundled-stamp`.
  - `ensure_bundled_agents_deployed` now compares the current binary's stamp to the on-disk one; a missing or mismatched stamp triggers a refresh pass that rewrites exactly the bundled files whose content actually differs from the current embedded template — every other bundled file (already up to date) and every NON-bundled (user-authored) file are left completely untouched, since the refresh only ever iterates the embedded bundle's own relative paths.
  - Before overwriting a bundled file whose on-disk content differs from the incoming template (e.g. a user hand-edited it, or it's genuinely stale), the existing content is archived to a sibling `<file>.stale.bak` first, so it's always recoverable — but ONLY when that backup doesn't already exist, so a later pass can never clobber a genuine prior backup with torn or already-refreshed content.
  - Added an explicit manual escape hatch, `tagent agents repair`, that force-refreshes every bundled file regardless of the stamp (`tagent agents deploy` also gained the same stamp-aware refresh behavior it previously lacked).
  - Concurrency (code-critic follow-up, HIGH): `delegate_to_agent` routinely spawns a fresh `tagent` subprocess per delegation, so multiple processes can enter the stale-stamp refresh path concurrently right after a binary upgrade. Every write in `agents::bundled` (per-file overwrite, backup, and the stamp file) now goes through `state_writer::atomic_write` (tmp-file + rename, reusing the same `fs4`-based primitive the crate already uses for state files) instead of a plain truncate-then-write, and a new `agents::bundled::lock` module takes a SINGLE pass-level advisory lock for the whole `reprovision_bundled_agents` read-backup-write loop, so two concurrent passes over the same target directory are fully serialized rather than interleaving per-file decisions.
  - Concurrency, part 2 (code-critic follow-up, MEDIUM): the first pass acquired that lock only INSIDE the refresh loop, leaving the stamp `read`/is-stale-decide/`write` sequence in `ensure_bundled_agents_deployed_in` and `force_reprovision_bundled_agents` outside it — two concurrent callers could both read the same old stamp, both refresh, then race writing the stamp back (self-healing on the next launch, but not actually the fully-serialized invariant intended). The loop body moved into a new `reprovision_bundled_agents_locked`, which REQUIRES an already-held lock guard as a parameter; `ensure_bundled_agents_deployed_in` and `force_reprovision_bundled_agents` now acquire the lock ONCE, before `stamp::read`, and hold it across the entire read-decide-refresh-write sequence.
  - `crates/trusty-agents/src/agents/bundled.rs` was split into `agents/bundled/{mod.rs, stamp.rs, lock.rs, tests.rs}` to keep the new logic under the crate's 500-SLOC production-file cap.
  - Regression coverage: `deployed_assistant_config_survives_scoping_with_delegate_to_agent` (`runtime/tool_registry_tests.rs`) seeds a genuinely STALE `assistant/agent.toml` (an old allow-list without `delegate_to_agent`) plus a wrong stamp into a tempdir, drives the REAL refresh path (`ensure_bundled_agents_deployed_in`), then resolves the refreshed config via `AgentConfig::by_name` and the real assistant-tier registry, asserting `delegate_to_agent` survives `scope_assistant_allowed_tools` — this sequence fails on pre-#3556 code, closing the exact deployed-config-to-registry-scoping gap that silently lost the grant. `agents::bundled::tests` adds `stale_stamp_triggers_refresh`, `refresh_backs_up_differing_content`, `existing_stale_backup_is_never_clobbered`, `non_bundled_user_file_untouched_by_refresh`, `matching_stamp_is_a_fast_noop`, `missing_stamp_establishes_baseline_without_rewriting_matching_content`, `force_reprovision_overwrites_even_when_stamp_matches`, `concurrent_ensure_calls_over_stale_target_converge_to_one_consistent_refresh` (races 6 threads' full `ensure_bundled_agents_deployed_in` calls over one seeded-stale target, asserting exactly one performs the refresh and every other observes a fully-consistent no-op), and `ensure_deployed_blocks_on_externally_held_pass_lock` (proves `stamp::read` itself is now behind the lock, by holding it externally and asserting a concurrent call cannot proceed until release); `agents::bundled::lock::tests` adds `pass_lock_serializes_concurrent_reprovision_calls`.

- **Test-isolation cluster: env-var and CWD races between concurrently-running tests (issues #3398, #3516).**
  - `ctrl/tests/tools_tests.rs`'s `detect_self_project_finds_via_env_var` / `detect_self_project_returns_none_when_no_marker` used to mutate the process-global `TAGENT_PROJECT_DIR` env var directly, with no `#[serial]` guard — but `workflow::engine::executor::tests::checkpoint_resume` mutates the SAME var under its own `#[serial]` convention, so the unguarded pair could clobber it mid-flight and produce a `CheckpointNotFound` failure in a completely unrelated test (#3398). `ctrl::util::detect_self_project` now delegates to a new `detect_self_project_with_hint(Option<&str>)`; the tests call the hinted version directly, so no env var is touched at all.
  - `telegram::tests::tempdir_for_test` hand-rolled tempdir "uniqueness" from `std::process::id()` (constant across every thread of one test binary) plus a nanosecond timestamp, which can collide under very high `--test-threads` if two threads sample the same clock tick — silently sharing one `telegram.pid` fixture between two of the `telegram_pid_guard_*` tests (#3516). Now uses `tempfile::tempdir()`, which guarantees a collision-free unique directory.
  - `default_bundled_config_dir`'s legacy-migration test (`env_compat.rs`) used to `std::env::set_current_dir` into a scratch tempdir under `#[serial]` — but CWD is process-global exactly like an env var, and `api::server::tests` handler tests resolve their own CWD-relative paths (`.trusty-agents/projects`, `.trusty-agents/agents`) while holding a *different* lock (`HOME_LOCK`), so `#[serial]` alone did not protect them from this CWD swap. `default_bundled_config_dir` now delegates to a new `default_bundled_config_dir_checking(&Path)` that roots its existence checks at an explicit parameter; the test points it at a tempdir directly, so no CWD mutation occurs at all.
  - Verified with the full `trusty-agents` lib suite run repeatedly at `--test-threads=64` and `128` (9 full runs total) with zero failures in `checkpoint_resume`, `telegram_pid_guard_*`, `get_project_config_falls_back_to_registry_when_toml_missing`, or `gather_specs_does_not_spawn_mcp_add_discover_bypass` — not proof the underlying rare flakes (#3514, #3516's second sub-issue) are fully eliminated, but no regression observed either.

- **Assistant can now actually delegate to bundled worker agents (`engineer`,
  `qa-agent`, …) regardless of the invoking CWD (#3555 follow-up):** a live
  run (`tagent --direct assistant --task "have an engineer run cargo check
  and report back"`) proved `delegate_to_agent` always failed with
  `is_error=true` and no `engineer` sub-agent was ever spawned, whenever the
  assistant was launched from a directory with no local
  `.trusty-agents/agents/` of its own (a bare worktree root, `/tmp`, …) — the
  model honestly reported "no engineering agent is wired up right now."
  Root cause: `build_assistant_tier_registry`
  (`src/runtime/tool_registry.rs`) built `delegate_to_agent`'s pre-flight
  validation directory as a single, hand-rolled
  `<cwd>/.trusty-agents/agents` — invisible to the bundled worker roster
  `agents::bundled::ensure_bundled_agents_deployed` deploys to
  `$HOME/.trusty-agents/agents/` at every startup. `AgentConfig::by_name`
  (used by the actual spawn) already resolves through
  `agents_dir_candidates()` — CWD/`TAGENT_CONFIG_DIR` tier, THEN the `$HOME`
  fallback — so validation rejected names the spawn would have happily
  found, killing the delegate call before the runner was ever invoked.
  - `DelegateToAgentTool` (`src/tools/delegate.rs`) now validates against a
    `Vec<PathBuf>` of candidate directories (`with_config_dirs`) instead of a
    single `Option<PathBuf>`, checked via the new `agents::agent_name_resolves`
    predicate (package/flat-toml/flat-md tiers — the same tiers the loader's
    `by_name_unresolved_src_in` uses). `with_config_dir` (single dir) is kept
    unchanged for `pm`/`ctrl`, which intentionally validate against exactly
    one project-scoped roster.
  - `build_assistant_tier_registry` now sources its directories from the
    newly crate-visible `agents::agents_dir_candidates()` — the SAME
    multi-tier list `AgentConfig::by_name` resolves against — so validation
    and the actual spawn can never diverge again.
  - Observability: `delegate_to_agent`'s pre-flight rejection and any
    sub-agent run error are now logged at `debug` with the actual failure
    reason (`src/tools/delegate.rs`); the tool-loop's per-call log
    (`src/llm/tool_loop/mod.rs`) now includes the error content on failure
    instead of only `is_error=true` with no payload — previously a failing
    `delegate_to_agent` call was undebuggable at any log level.
  - New tests: `delegate_resolves_agent_from_secondary_config_dir` and
    `delegate_rejects_agent_absent_from_all_config_dirs`
    (`src/tools/delegate.rs`); `agent_name_resolves_tests::*`
    (`src/agents/tests/loading.rs`) pin the three resolution tiers plus the
    negative/traversal cases.
  - Live-verified end-to-end (`RUST_LOG=debug`, CWD = bare worktree root with
    no local `.trusty-agents/agents/`): `delegate_to_agent` dispatches with
    `is_error=false`, a real `agent=engineer` sub-agent spawns and completes,
    and the assistant relays the engineer's actual `cargo check` findings.
  - **`agent_name_resolves` edge-case fix (code-critic MEDIUM):** the real
    resolver (`AgentConfig::by_name_in`, via `load_agent_package`'s `?`) hard
    -aborts the ENTIRE search the moment `<dir>/<name>/` exists as a
    directory without a readable `agent.toml`/`persona.md` inside it — it
    does NOT fall through to a flat `<name>.toml` in a later directory.
    `agent_name_resolves` previously kept scanning in that case (a
    `.iter().any(...)` over all dirs), so validation could accept a name the
    real spawn then hard-errors on. Rewrote it as an explicit per-`dir` loop
    that mirrors the same short-circuit: on the first `<dir>/<name>/`
    directory hit, it returns immediately based on whether BOTH
    `agent.toml` and `persona.md` are present, exactly like the real
    resolver either resolves or aborts right there. New pinning test
    `agent_name_resolves_tests::short_circuits_on_malformed_directory_package_matching_real_resolver`
    proves both `agent_name_resolves` and `AgentConfig::by_name_in` agree in
    the same scenario. (Known, documented, deliberately-deferred limitation:
    `agent_name_resolves` still doesn't check the legacy
    `~/.claude/agents`/`<cwd>/.claude/agents` claude-mpm-format tier the real
    resolver's final fallback searches — never affects the bundled roster
    `delegate_to_agent` actually targets, since every bundled agent lives
    under the `.trusty-agents/agents` tiers this function does check.)
- **Credential-test env isolation flake (#3464):** several `llm::credentials`,
  `llm::helpers`, `llm::adapter`, `llm::http`, `ctrl::ctrl_turn::dispatch`, and
  `runtime::cli_def` tests that assert "no credential resolves" or "the store
  wins when env is absent" intermittently failed under `cargo test --workspace`
  (and deterministically on any machine/CI runner with a real project or
  `$HOME` `.env.local`). Root cause: `trusty_common`'s `.env.local` loader is a
  process-global `OnceLock` that fires at most once per test binary; whichever
  test's credential-resolution call happened to be first could have it silently
  re-populate an env var a test had just cleared, mid-call. Fixed by forcing
  that loader to have already run (`test_env::force_env_local_loaded`) before
  any test clears credential env vars, and by clearing every registry-mapped
  provider's env var (not just the three `openrouter`/`anthropic`/`claude-code`
  names) in `other_configured_providers()`'s tests
  (`test_env::clear_all_credential_env_vars`). Also sandboxes `$HOME` in
  `ctrl_creds_errors_when_nothing_configured`, a pre-existing gap that made it
  fail deterministically on any machine with a real secure-store credential.
- **Favicon/RobotIcon drift (#3486):** `ui/public/favicon.svg` was a fourth,
  independently hand-drawn robot face (different rect/circle coordinates, a
  `<text>`-rendered `>_` mouth) that had drifted from `RobotIcon.svelte`'s
  actual markup. Regenerated the favicon's geometry from `RobotIcon.svelte`'s
  `full`-variant paths (same head rect, antenna, eye positions, and stroked
  chevron+underscore mouth) so there is a single robot design instead of two.
- Committed `ui/pnpm-lock.yaml` (architecture-review tranche 0): the file was gitignored with a comment claiming the repo-wide detect-secrets pre-commit hook flags npm integrity hashes, but the root `.pre-commit-config.yaml` already excludes `.*pnpm-lock\.yaml` (only the crate-scoped `crates/trusty-agents/.pre-commit-config.yaml`, which governs Python tooling and is unrelated to the JS UI, was missing that exclusion) and no CI workflow runs detect-secrets at all. Every other Tauri UI in the workspace (`trusty-mpm-gui`, `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-code-gui`, `trusty-console`) already commits its own subdir-scoped `pnpm-lock.yaml`; `trusty-agents/ui` was the only outlier. Removed the stale `.gitignore` entry and regenerated the lockfile with `pnpm install --lockfile-only`.
- REPL `/agent <name>` (`handle_agent_command_into`, `crates/trusty-agents/src/repl/agent_commands.rs`) now resolves personas via the same directory-package + `extends`-aware loader the one-shot `--direct` path already used, instead of a hardcoded flat-file check (`agents_dir.join("{name}.toml")`) — so `/agent assistant` now activates the base Assistant persona, which ships only as a directory package (`assistant/agent.toml`, no flat `assistant.toml`). `/agent` with no argument (`list_assistant_agents_into`) now surfaces directory-package agents too, not just flat `*.toml` files, and `/switch assistant` is a recognized alias alongside ctrl/izzie/cto. Resolution is anchored to the REPL's own resolved `agents_dir` via a new `AgentConfig::by_name_in(dirs, name)` (the default-dirs `AgentConfig::by_name` is now a thin wrapper over it) rather than `by_name`'s process-global `TAGENT_CONFIG_DIR`/CWD dirs, so a standalone launch (CWD outside the project) still activates exactly what it just listed. `by_name`/`by_name_in` (and their async twin) also now reject an agent name containing `/`, `\`, or an exact `.`/`..` segment before any `dir.join(name)` join — closing a path-traversal gap where `/agent ../../foo` could read `agent.toml`/`persona.md` outside the intended agents directory (closes [#3303](https://github.com/bobmatnyc/trusty-tools/issues/3303), refs #3052)
- `run_pm_task_with_persona` (the dispatch path a selected named persona/Assistant chat turn takes) now reaches tool parity with the session path (`run_pm_task_with_history`) — delegation (`delegate_to_agent`), CTRL project-management (`add_project`, `list_projects`, `remove_project`, `stop_task`, `set_active_project`), filesystem (`move_file`, `create_dir`), search (`search_code`), shell (`run_bash`), and `tm` tools are now registered alongside the MCP/git/ticketing/live-MCP tools it already wired. Access is still gated per persona: a tool is only advertised to the LLM when its name matches the persona's `[tools].allow` glob list (extracted into the new pure `filter_persona_tool_names` helper, pinned by 4 unit tests) and passes the existing RBAC-tier filter — registering a tool is not the same as granting it, so no persona gains capability beyond what it already declares. Full `[tools].scopes` enforcement remains tracked separately by [#3208](https://github.com/bobmatnyc/trusty-tools/issues/3208) (closes [#3285](https://github.com/bobmatnyc/trusty-tools/issues/3285))
- `llm::credentials::pick_credentials()` now resolves `openrouter`/`anthropic`/`claude-code` via the shared `trusty_common::inference::credentials::resolve_key` 3-tier resolver (env > `.env.local` > secure keyring/file store) instead of a raw `std::env::var` read — a credential configured only via `tagent config keys set` was previously invisible at chat dispatch even though `trusty-channels` and this crate's own `GET /api/models` catalog already consulted the shared store (closes [#3248](https://github.com/bobmatnyc/trusty-tools/issues/3248))
- `tmux::orchestrator::TmuxOrchestrator::create_session` and `debugger::tmux::TmuxAdapter::create_session` now apply generous tmux scrollback (`history-limit=100000`) and mouse-wheel scrolling BEFORE `new-session`, via the new shared `trusty_common::tmux` layer — this crate previously ran its own independent tmux implementation with no scrollback handling at all, so every trusty-agents-hosted tmux session (including the debug REPL) was stuck at tmux's tiny 2000-line default even after trusty-mpm's #2398/#2399 fix landed (closes [#3004](https://github.com/bobmatnyc/trusty-tools/issues/3004), refs #2398, #2399)
- migrate off archived/unsound `serde_yml` (GHSA-hhw4-xg65-fp2x) and its transitive `libyml` (GHSA-gfxp-f68g-8x78) onto the workspace-blessed `serde_yaml` 0.9, closing Dependabot alerts #42/#43 (closes [#2991](https://github.com/bobmatnyc/trusty-tools/issues/2991))
- bump `lru` 0.12 → 0.16, fixing RUSTSEC-2026-0002 (`IterMut` violates Stacked Borrows, GHSA-rhfx-m35p-ff5j); no call-site changes needed ([#2782](https://github.com/bobmatnyc/trusty-tools/pull/2782)) ([`e62b454`](https://github.com/bobmatnyc/trusty-tools/commit/e62b4540d39c5a442d05e849197157932f37e664))
