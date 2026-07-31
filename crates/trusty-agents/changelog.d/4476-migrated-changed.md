Changed

- **`cto-assistant` now declares its OKG docstore grant in the bundled
  persona.** `okg_ingest_docstore` and `okg_sources` are named in the bundled
  package's `[tools].allow` (owner decision, 2026-07-31) instead of living as
  a hand edit in the deployed copy, where a bundled-agent reprovision
  overwrote the file and destroyed the grant with no `.stale.bak` (#4461).
  There is still no API to grant a tool (#3890), so bundling is what makes it
  durable — the next reprovision converges on it rather than eating it.
  Resolved behaviour is unchanged: the base `assistant` has granted the OKG
  tools since #3883 and `extends` unions `[tools].allow` base-first, so this
  is an independence guarantee against a future narrowing of the base, not a
  new capability. Narrow on purpose — the overlay does not name
  `okg_ingest_gmail`/`okg_ingest_drive`; widening it is a deliberate decision
  about what the persona may ingest. Pinned by
  `bundled_cto_assistant_pins_okg_docstore_reach`, which asserts both the
  resolved reach and the overlay's own declaration, since a resolution-only
  check would stay green through a full revert.
- **`discover_one_returns_none_on_timeout` now runs on Tokio's paused clock
  instead of sleeping out the real 15s `DISCOVERY_TIMEOUT`** — 15.015s to
  0.00s, removing what was the single slowest test in the workspace (of
  20,317). `#[tokio::test(start_paused = true)]` still spawns the real
  unresponsive `sleep 3600` server and still fires the real, unmodified
  15s constant; the runtime just auto-advances to the deadline rather than
  waiting for it. Coverage is verified preserved, not assumed: with the
  `tokio::time::timeout` removed from `discover_one` the test fails, and a
  new elapsed-time assertion additionally fails if `None` ever comes back
  from an early spawn failure rather than from the deadline expiring — a
  paused-clock test that no longer exercises the timeout would otherwise
  pass green.
- **An assistant's reachable sub-agent set is now an editable configuration
  whitelist, and for the bundled assistants it is exactly `{research-agent,
  ticketing-agent}` — no coding agents** (ADR-0024 decision 4, ratified by the
  owner 2026-07-29). Reachability was previously an unfiltered scan of every
  role-eligible agent on the host, with no per-agent narrowing possible. It is
  now `[subagents].delegate_allowed` in each agent's `agent.toml`, intersected
  with a server-owned floor. **No agent definitions were removed** — every
  engineer, QA, docs, planning and ops agent is still in the catalog and still
  dispatchable by `pm`; only what an ASSISTANT can reach changed.
- **An absent whitelist reaches NOTHING** (fail-closed), and every bundled
  assistant persona now ships a seeded `delegate_allowed` so this is not a
  silent capability loss. Personalizing overlays inherit and can add to their
  base's list; a custom persona that declares none reaches nothing until it
  does, which is deliberate.
- **Assistant persona prose rewritten to match.** `assistant`, `izzie`,
  `cto-assistant`, `ctrl` and `personal-assistant` previously instructed the
  model to delegate by name to `engineer`, `python-engineer`, `qa-agent`,
  `docs-agent`, `local-ops-agent` and `plan-agent` — every one of which the new
  gate refuses. Each persona now describes what it can actually reach, and what
  to do when asked for coding work: say plainly that it is not something it can
  take on, do the part it can (discuss it, or gather background via the research
  specialist), and offer to open a ticket via the ticketing specialist.
- **The role allow-list gained `ticketing`** so `ticketing-agent` is
  role-eligible at all. This widens a COARSE pre-filter, not the reachable set —
  the whitelist is now the binding gate.
- **Assistants are tier L0 (ADR-0024 decision 3).** An agent whose `[agent].role`
  is `assistant` now resolves `AgentTier::L0Orchestration` instead of
  `L1Standard`; sub-agent roles (`engineer`, `qa`, `researcher`, `documentation`,
  `ops`, `planner`, …) stay `L1Standard`. The Sub-agents pane consequently
  reports `tier l0` for the viewing assistant and for peer-assistant target
  cards, and `tier l1` for sub-agents — it previously labelled every agent
  `l1`, including assistant-kind ones. The tier is DERIVED from the agent's kind
  by `AgentTier::for_kind`, so no agent TOML changed and none needs a
  `tier = "l0"` line; an explicit `[agent].tier` declaration still wins in both
  directions and is still fail-closed (an unrecognized value narrows to L1 and
  can never elevate). No capability grant changes: delegation refusal keys on
  agent KIND rather than tier order, and the one tier-conditioned surface
  (#4171's read-only session-state tools) stays unreachable for every bundled
  assistant because none names those tools in its `[tools].allow`.
- **The chat's "waiting on the agent" indicator now lives inside the reply
  bubble.** The spinner used to render as a separate element below the message
  list, left-aligned under the bubble it belonged to and visually detached from
  it. It now renders in the bubble's own body — standing in for the `…`
  placeholder while the reply is empty, then trailing the streamed text like a
  caret — and the in-flight bubble carries a pulsing green ring (Foundry's
  `--trusty-success`, so it themes correctly in light and dark). Both the
  spinner and the ring are driven by the same `isRunning` + `activeTaskId`
  pair, so they cannot disagree, and both clear on completion, error, and
  cancellation. Users with `prefers-reduced-motion: reduce` get the same green
  ring, static rather than pulsing.

- **The `ticketing` function skill now also bundles the two read-only git
  inspection skills (#4024)** — `git_search_commits` and `git_branches` — so
  git and ticketing are granted as one unit. Git *mutation* skills (commit,
  push, branch creation, checkout, stash) remain excluded, and a test pins
  that exclusion.

- `trusty-memory` requirement raised to `^0.22` (was `^0.21.1`). trusty-memory
  goes 0.21.3 -> 0.22.0 because it publicly re-exports `trusty_common::palace_id`
  items and its `trusty-common` requirement moved to `^0.27`; `^0.21.1` resolves
  to `>=0.21.1, <0.22.0` and would not have admitted the new version. Requirement
  edit only — this crate's own version is unchanged.
- **`PmBridgeBackend::run` gained a `target: Option<&str>` parameter (#4026).**
  `None` preserves today's exact command line. Only the tcode leg honours a
  target; the tm leg has no per-agent selector and deliberately ignores one
  rather than mis-routing (the tool forces the tcode leg whenever a specialist
  is named, so this is unreachable in practice).
- **`AgentConfig` gained an optional `[subagents]` section (#4026).**
  `SubagentsConfig { allowed: Option<Vec<String>> }` — a struct rather than a
  bare list so #4029's Sub-agents configuration section can add fields without
  a breaking config change, exactly as `SkillsConfig` was shaped. Absent =
  EMPTY, never "all". Exact names only, no glob dialect.
- **Two smaller behaviour changes in `SkillCatalog::expand` (#4022).** Member
  resolution is **memoised**, so expansion is linear in the catalog rather than
  once per distinct path through the bundle graph — a depth-only cycle guard
  would have re-walked a diamond-shaped graph exponentially, and `expand` runs
  on every persona turn and subagent launch. As a consequence `unresolved[]`
  now reports each dangling id **once** even when `[skills].allow` names it
  more than once; previously a duplicate entry produced a duplicate report. The
  Skills pane's own counts now exclude bundles, matching `granted_count` —
  counting a group header as an eleventh capability printed "11 of N" over ten
  rendered cards. DOC-57 gains §5.7b (S-13…S-16) specifying the tier, an
  extended `kind` enum in §5.3, the new response fields in §5.7, and epic
  #4021 OQ-1's resolution recorded alongside §12 OQ-2.

- **A `[tools].scopes` grant that can match nothing is now reported instead of
  silently denying everything** (issue #3987, option C): `ScopePattern`
  matching fails closed AND silent, so an agent declaring a pattern no tool can
  ever advertise — the base `assistant`'s `google.read`, which no gworkspace
  tool advertises because discovery only ever emits
  `google.<family>.<access>` — got no error, no log line, just a persona whose
  entire scoped tool surface vanished before the model saw it. That failure
  mode is indistinguishable from "this agent has no tools", which is how both
  #3938 and #3987 stayed invisible. The new
  `tools::registry::dead_scope` module names the condition: at persona
  registry-build time (`ctrl::pm_task::dispatch::persona`, the #3208
  enforcement site, and the only place holding both the agent's resolved
  patterns and the registry's scope vocabulary) a dead pattern now produces a
  prominent `WARN` naming the agent, the pattern, and the nearest reachable
  scopes; `GET /api/agents/{name}/skills` gained a `dead_scope_patterns`
  array; and the Skills pane renders those as a "Dead scope grants" warning
  block, so the GUI says so outright rather than showing `granted: true` cards
  for tools dispatch will drop.

  Reachability is the union of three sources — the scopes the registry kept,
  every scope the endpoints *advertised* before the operator's
  `[[endpoints]].scopes` policy filtered them, and the closed compile-time
  Google vocabulary — and a pattern is only judged when its namespace appears
  in that set. So neither a `gworkspace`/`trusty-memory` daemon being *down*
  nor an operator deliberately narrowing an endpoint can make a working grant
  look broken: denied-by-policy and cannot-possibly-exist stay distinct
  conditions.

  This ships as a warning rather than a load error. Failing closed is the
  right end state, but it cannot precede every bundled agent being clean —
  which is what #3987's option B (the base `assistant`'s explicit Google
  family grants) delivers. Promoting it to a hard error is deliberately left
  as its own change even after that: it would also stop *user-authored* agents
  from loading, which deserves its own decision and release note.
- **The base `assistant` template now declares explicit Google family scope
  grants instead of the dead `google.read`** (issue #3987, option B — owner
  decision, see the issue's PM-recommendation comment). All ~60 Google
  Workspace tools the base allowlists were scope-denied at dispatch on the
  persona-chat path, and had been since the line was written: no gworkspace
  tool ever advertises `google.read` (discovery only emits
  `google.<family>.<access>`) and `google.read` is a no-wildcard pattern
  requiring string equality. The base now grants
  `google.gmail.*`, `google.calendar.*`, `google.drive.*`, `google.docs.*`,
  `google.sheets.*`, `google.slides.*`, `google.tasks.*` and
  `google.accounts.*` — audited tool-by-tool against the real `rpc.discover`
  document, one family per surface the allowlist actually names, with no
  family granted that has no allowlisted tool behind it (a new test fails in
  both directions).

  **Posture note, stated plainly**: a base assistant granted these tools holds
  WRITE-CAPABLE Google credentials. There was no read-only enforcement to
  preserve — `google.read` granted nothing, so the read-only posture was
  fictional — but the honest description of what the base can now do is
  "read and mutate the user's Gmail, Calendar, Drive, Docs, Sheets, Slides and
  Tasks", because every OAuth scope `trusty-gworkspace` requests is the
  full-access variant of its API. A genuinely read-only tier (#3987 option A)
  is deferred pending a product requirement; it needs `.readonly` OAuth
  variants and user re-consent. The `[tools].allow` list remains the binding
  constraint — scope and allow-list intersect at dispatch (verified in
  #3985) — so this exposes no tool the curated allowlist does not already
  name. Overlays are unaffected: `cto-assistant`'s narrow `google.gmail.*` /
  `google.tasks.*` (#3985) collapse into the base's by the extends union's
  dedup, and `izzie`'s blanket `google.*` remains a superset.

  **Known gap, tracked in #4054**: the base persona ingests
  attacker-controllable text (`okg_ingest_gmail`, `okg_ingest_drive`,
  `get_gmail_message_content`, `web_search`) in the same turn that now holds
  send/share credentials, and five allowlisted tools are exfiltration-capable
  (`compose_email`, `manage_file_permissions`, `manage_gmail_settings`,
  `manage_gmail_filters`, `modify_gmail_messages`). The allow-list bounds
  which tools exist, not what an injected model does with them. A
  confirmation gate for those five needs either #3074/#3075's
  `user_authority` field or an interim read site for
  `[[permissions.grants]] mode = "ask"` — neither exists yet, so it is filed
  rather than shipped here.

  Consequently the base assistant, and every `extends = "assistant"` overlay,
  is now clean under the #3987 option-C dead-grant diagnostic, which
  discharges the sequencing constraint that forced that check to ship as a
  warning rather than a load error.
- **Skills pane renders resolved capability instead of glob prefixes (#3933):**
  #3932's placeholder grouped `[tools].allow` by the substring before the first
  `_` and badged every card `synthetic`. It was honest about being a guess, but
  it showed the user prefix fragments and could not say whether anything
  resolved. The pane now reads `GET …/skills`: human-named cards grouped by kind,
  each showing the single tool it wraps and its credential state. A credential
  reads CONFIGURED only when an environment variable actually resolved; an OAuth
  grant reads "not verified" rather than a green chip nobody checked. The
  client-side `synthesizeSkills` derivation is removed with its tests.

- **OKG ingestion feeds the bound search index (#3892, remediation (a)):** an
  `okg_ingest_*` call used to write markdown into the KB tree and stop there,
  while `vector_search` queried an index built over an unrelated root — so an
  ingest could report "N entities ingested" truthfully and the assistant would
  still find nothing, with no error on either side. Every ingest tool now ends
  by draining that source's index backlog into the agent's `[[stores]]`-bound
  trusty-search index (`stores::index_feed`, over the daemon's
  `POST /indexes/{id}/index-file` / `remove-file` routes). The contract: the
  tree write is the durable record and is never gated on a reachable daemon;
  the journal line is appended only AFTER the daemon acknowledges the push, so
  a crash costs one redundant re-push rather than a lost entity; every run
  reconciles the whole backlog, so a failed push is self-healing; a changed
  entity is withdrawn before it is re-pushed and a tombstoned one is withdrawn
  outright. Fail-open, never silent — each result reports `index.indexed` /
  `removed` / `pending` plus a `reason` when nothing could be fed. The
  operator's configured index roots are untouched (remediation (b) was not
  taken). One constraint the live check surfaced and the feed now enforces:
  trusty-search post-filters any search result whose path escapes the index
  root (issues #64 / #541), so pushing a tree into a foreign-rooted index would
  store the chunks and hide them from every query. The feed reads the index's
  root first and REFUSES with an actionable reason when it does not contain the
  tree, rather than reporting `indexed: N, pending: 0` over a corpus nothing can
  return.
- **`okg://<agent>` finally resolves (`stores::binding`, #3892):** the URI was an
  opaque label nothing ever mapped to a path, so a binding's tree tail matching
  its KB directory was a naming coincidence rather than an invariant. It now
  resolves to `<knowledge_dir>/<slug(agent)>`, and a push is REFUSED when the
  binding names a different tree than the one that was written — trading a
  silent-empty failure for a loud one instead of a silent-wrong one.
- **Store status surfaces the unsearchable backlog (#3892):** `okg_sources`
  reports per-source `index.synced` / `index.pending` and names the bound index;
  `GET /api/agents/:name/stores` adds the resolved `tree_path` plus
  `pending_index` / `synced_index`, so a store can no longer read as CONNECTED
  while holding none of the content its tree holds.
- **Agent configuration is five sections: Personality / Knowledge / Skills /
  Listeners / Permissions (#3932, GUI):** per the owner's directive — *"Instead
  of tools, let's show skills"* — the config takeover's tab strip is reshaped to
  DOC-57's normative order and re-backed, not merely relabelled. **Knowledge**
  widens the old OKG Stores tab from store bindings alone to one pane over three
  sub-surfaces: the live bindings, the `vector_search`→store linkage resolved
  from the agent's own allow-list, and the declared MCP knowledge endpoints
  (which report `trusty-memory` and `trusty-search` as *disabled* — their
  shipped default — rather than quietly omitting them). **Skills** replaces the
  Tools tab's single monospace textarea of raw globs with capability cards
  grouped by tool-name prefix, each badged `synthetic` because no skill manifest
  wraps its tools yet — so the unwrapped surface stays visible and wrapping
  progress becomes measurable. **Permissions** moves last and stops being a chip
  list: each mechanism now carries an explicit *enforced* / *not enforced*
  marker, states the deny-on-absent polarity of an empty allow-list, and names
  what it cannot see (values inherited through `extends`, the reserved
  `user_authority` field, and the tier/autonomy model with no read path).
  Front-end only: no new HTTP route, no config-schema change, independently
  revertable. The panel is split into one component per section with the persona
  edit buffer and dirty aggregation kept in the shell, so an in-progress prompt
  survives a section switch and every exit path still routes through the
  unsaved-changes guard.
- **GUI OKG Stores pane is real:** the gear panel's OKG Stores tab now fetches
  `GET /api/agents/:name/stores` and renders observed state — CONNECTED with
  the index name, chunk count, readiness and indexed root, or NOT CONNECTED
  with the backend's reason (plus a separate line for palace health). The
  "Scaffolding — `agent.toml` has no OKG store binding field yet" placeholder
  and the client-side `scaffoldOkgStores()` fabricator are deleted; an agent
  that binds nothing now says so explicitly instead of showing a fake row.
- **First real eventstream listener: Gmail history-poll + agent wake (#3820,
  DOC-54 SPEC-AGENTS-04/06):** the third leg of the agent-config triple
  (stores / tools / **listeners**) lands with a working Gmail connector.
  Declarative config: harness-level `[[listeners]]` in `~/.trusty-agents/
  config.toml` (stage-one ingestion filter, `enabled = false` by default)
  and per-agent `[[listeners]]` bindings in `agent.toml` (stage-two wake
  filter — event types, sender globs). New `crate::listeners` module: a
  Gmail `history.list` polling engine (cursor persistence, dedup, exponential
  backoff, 410-GONE re-baseline — Pub/Sub-pull is deferred), an append-only
  JSONL event store under `~/.trusty-agents/events/` with a per-event-type
  include/exclude filter, and the stage-two wake dispatcher that reuses
  `run_pm_task_with_persona` (the same entry point `/agent` uses) so a wake
  reaction lands in the agent's chat history like an ordinary turn —
  ask-first framing, rate-limited to one wake per poll cycle (enforced by a
  pure cycle-gate, `listeners::wake::gate_wake`, threaded through
  `poll_once`'s per-event loop — every event is still durably stored
  regardless of whether its wake was rate-limited), every decision logged.
  `poll_interval_secs` is floored at 15s on config load (values below the
  floor are clamped up with a warning, not silently accepted) since this
  polls a real personal Gmail account. Cursor persistence
  (`~/.trusty-agents/events/cursor-<listener>.json`) writes atomically
  (temp-then-rename) and logs a warning on a malformed cursor read instead
  of silently re-baselining. New API: `GET /api/listener-events`, `POST
  /api/listener-events/filter` (named apart from the pre-existing SSE
  `/api/events` telemetry stream). Listener events also publish onto the
  existing harness event bus (`Event::ListenerEventReceived`) so the Events
  pane (#3818) updates live via the same SSE/Tauri bridge; the pane's
  `gmail` source bucket is now real data, not a concept placeholder. Per-
  connector event instructions load from `agents/<name>/events/<connector>.md`
  (#3817) when a listener wakes an agent. Izzie ships with a demo
  `gmail-personal` binding + `events/gmail.md`; enabling the live listener
  requires a `~/.trusty-agents/config.toml` stanza (never written by this
  code) — see the PR body. Deferred: Pub/Sub-pull transport, the Calendar
  connector, multi-agent fanout beyond the one-wake-per-cycle limit.

- **GUI reshape: WORKSTREAMS-backed "Tasks" sidebar, CHAT/EVENTS nav, and an
  in-pane agent config gear panel (#3819, epic #3052):** replaces the
  `PROJECTS` sidebar section and the `Chat/Projects/Personality` top nav.
  Sidebar now shows a pinned Concierge (`ctrl`) entry plus a "Tasks" list
  (user-facing label for trusty-memory's `ws:<name>`-tagged workstreams,
  DOC-53) grouped/collapsible by owning agent, each row resumable (injects
  recent tagged history as a context banner). Top nav is `Chat | Events`;
  Events is a deterministic, listener-tagged event list with per-source
  include/exclude toggles (excluded rows stay visible, muted) and inference-
  cost guidance copy. The chat pane gets its own header: the active agent's
  display name as the title, an inline selector (moved out of the top
  toolbar), and a gear icon opening an in-pane Personality/OKG
  Stores/Tools/Permissions/Listeners config form scoped to that agent — a
  new add-agent flow creates an agent from the `assistant` template.
  New backend surface: `GET /api/workstreams[/:name/history]`, `GET/PATCH
  /api/agents/:name` (extended with `personality`/`tools_allow`), `GET
  /api/agents/:name/persona`, `POST /api/agents` (template create). Also
  fixes a pre-existing bug where `PATCH /api/agents/:name` wrote a
  lower-priority flat TOML shadow instead of a directory-package agent's
  real `agent.toml` for every bundled demo agent (`assistant`/`izzie`/
  `cto-assistant`/`ctrl`). Adds `AgentInfo::hidden` (default `false`) — a
  listing-only visibility flag, independent of `role` (which also gates
  tool-registry security posture), honored by both the REPL's
  `list_assistant_agents_into` and the API roster (`GET /api/agents`).
  `ProjectsView`/`PersonalityPanel` are left in the tree but no longer
  routed — their dropped/relocated capabilities are documented in the PR
  body. Deferred to follow-ups: the eventstream connector backend (#3798
  family), tagging new outgoing memory writes with the resumed workstream,
  real per-listener wake plumbing, and reconciling "+ New Task" to Bob's
  later-refined "continuous per-agent chat, tasks are an inferred filter/
  view, not a context wall" model (this slice still clears context on "+ New
  Task", matching the pre-existing architecture).

- **Filterable-chat context assembly: in-band task classification, focused
  mode, and per-workstream summaries (DOC-54 §9.6 / SPEC-AGENTS-08,
  demo-day 2026-07-24):** `run_pm_task_with_persona` now appends a compact
  classification block to every turn's system prompt (the agent's closed
  label vocabulary + a `[[task: <label>]]` / `[[task: new: <label>]]`
  trailing-marker instruction), parses that marker out of the response
  before display, and persists the turn as a `ws:<label>`-tagged
  trusty-memory drawer — the same convention `GET /api/workstreams`
  already reads, so classified chat turns show up in the Tasks sidebar for
  free. New `/focus [<label>|off]` REPL command and `TaskRequest.focused_workstream`
  API field (chat-side parameter, in place of a separate focus-session
  endpoint) set the active workstream; when set, context assembly follows
  the spec's stable order — global summary (stub) → cached per-workstream
  summary → last `recent_window` raw turns — and the persisted response
  notes which task it's focused on. A cached per-workstream summary
  regenerates every `summarize_every` turns (one extra LLM call on the
  agent's own model/credentials). New per-agent `[workstreams]` TOML
  section (`enabled`, `summarize_every` default 5, `recent_window` default
  12). Task-bleed handling never restricts tools/memory or forces a
  mismatched classification — a turn that classifies outside the focused
  workstream is tagged honestly, logged, and gets a gentle inline nudge.
  New module `ctrl::pm_task::dispatch::classification`; shared read/write
  drawer primitives (`list_workstream_labels_at`, `drawers_by_tag_at`,
  `create_tagged_drawer_at`) added to `api::server::workstreams` (now
  `pub(crate)`, split into `workstreams/{mod,tests}.rs` to stay under the
  500-SLOC cap). Deferred: lazy summary invalidation on manual re-tag, a
  real global prompt-history summary source, periodic near-duplicate label
  consolidation, a persistent `POST /api/workstreams/:name/focus`
  session-state endpoint, and classification for the claude-cli subprocess
  persona path (separate execution model, out of scope for this slice).
  Verified end-to-end live against a real trusty-memory daemon (REPL +
  `/api/task`), routed through OpenRouter/Sonnet per the current provider
  policy: closed-vocab classification (new labels + correct reuse),
  `ws:`-tagged persistence, `/api/workstreams` rollup, focused-mode
  assembly in spec order, the task-bleed nudge, and the `summarize_every`
  cadence — including a real generated `ws-summary:<label>` drawer at the
  turn-5 boundary — all confirmed working. Minor noted gap (not fixed
  here, moot under the current no-Ollama operating policy): the
  summary-refresh one-shot call (`llm::chat_adapter_aware`) doesn't route
  an `ollama/`-prefixed persona model correctly (only Bedrock is
  special-cased) — caught and logged, the turn's own response/persistence
  is unaffected.

- **Filterable-context: critic-review fixes — marker hardening, a real
  `enabled` gate, and off-response-path persistence (PR #3840 review
  round, demo-day 2026-07-24):** three fixes to the classification slice
  above, pre-merge.
  1. `parse_marker` is now anchored to the response's own LAST LINE (not a
     bare "last occurrence anywhere" scan) and validates the parsed label
     against a `[a-z0-9-]{1,64}` charset allow-list before ever treating it
     as real. Closes two holes: a persona emitting the marker twice no
     longer leaves the first occurrence visible in the displayed text (both
     trailing marker lines are stripped; the last one is authoritative —
     documented in `parse_marker`'s doc comment), and a persona echoing the
     marker SYNTAX itself (e.g. explaining the convention, literally ending
     with `[[task: <label>]]`) can no longer have that placeholder text
     persisted as a real `ws:<label>` tag — charset validation rejects it
     (logged, turn left unclassified).
  2. `[workstreams].enabled` is now a REAL master switch: `false` skips the
     vocabulary fetch and classification-block rendering in
     `build_turn_context` entirely (focused-mode context assembly is
     skipped too — a stale user focus setting is inert on a disabled
     agent), AND gates `finish_turn`'s persistence — previously only the
     summarizer checked it.
  3. Turn persistence (`create_tagged_drawer_at`) and the cadence summary
     LLM call now run in a detached `tokio::spawn`ed task AFTER the
     response is parsed/nudged — the user sees their answer immediately
     instead of waiting through a second full LLM round trip on a cadence
     turn. Errors from the background task are logged exactly like the
     previous inline failures.
  New tests: `parse_marker_double_marker_strips_both_last_wins`,
  `parse_marker_trailing_syntax_echo_is_stripped_but_unclassified`,
  `parse_marker_ignores_trailing_syntax_echo_mid_sentence`,
  `is_valid_label_*`, `build_turn_context_disabled_is_a_real_no_op`,
  `finish_turn_disabled_does_not_persist`,
  `finish_turn_persists_via_detached_task` (the last two against a new
  mock-daemon harness proving both the `enabled` gate and the detached
  write land correctly).
  Folded in from the same review round (filed as issue #3843, not fixed
  here — both are pre-existing design gaps, not regressions, and don't
  affect the demo's short-lived workstreams): the in-band classification
  vocabulary has no label-count/character cap (unbounded up to trusty-memory's
  own 500-drawer scan limit); and `should_refresh_summary`'s turn count
  derives from a 200-capped fetch, so a workstream past 200 turns sticks at
  that count and re-fires the summary refresh EVERY turn instead of on
  cadence (needs a real stored turn counter, not a capped re-scan).
  Integration note for #3838 (Gmail eventstream listener + agent-wake):
  classification applies uniformly to every `run_pm_task_with_persona`
  turn, including event-reaction/wake turns — this is per-design (DOC-54
  §9.2: event reactions get classified into the continuous conversation
  like any other turn), not an oversight. Flagging for #3838's own
  integration smoke test to confirm one real wake turn parses/classifies
  cleanly (no marker leakage into the surfaced event-reaction text).

- **`tagent mcp-serve` — stdio MCP server exposing trusty-agents to external MCP
  clients (Claude Code, etc.) (#3633 core):** a line-delimited JSON-RPC / MCP
  server over stdio, built on the shared `trusty_common::mcp` framework
  (`run_stdio_loop` + `initialize_response`, protocol `2024-11-05`) with zero new
  dependencies. Ships two tools — `list_agents` (read-only; returns the same
  roster the `GET /api/agents` route serves, via a newly-extracted shared
  `agent_roster` fn) and `dispatch_task` (wraps the existing opaque, RBAC-gated
  `PmBridgeTool` rooted at the server's cwd). Dispatched before `run_startup_init`
  so the JSON-RPC stdout stream is never polluted by startup output. Deferred to
  epic #3633: HTTP/SSE transport, `rpc.discover`, tool namespacing, auth, and
  RBAC tier-mapping of external callers.
- **No interim status text in the chat response bubble — the spinner is the
  sole waiting indicator:** the assistant bubble no longer fills with
  "Running…"/"submitted…"-style `task-progress` status strings. It stays empty
  (with a bare animated spinner shown beneath the thread — no "Running…" text
  label; `role="status"` + `aria-label` preserve the accessible busy-state
  name) until the first streamed `task-delta` or, for non-streaming providers,
  the final `task-complete` narrative arrives. Removed the two progress-text
  writes into message content (`ChatView`'s `task-progress` listener, dropped
  entirely; `InputArea`'s poll-reconcile write) and the spinner-row text label;
  the pending→real id reconcile itself is retained (it's load-bearing for
  streamed-delta attribution).
- **Base `assistant` persona defaults to Sonnet instead of Opus (#3688):**
  `claude-opus-4-6` → `claude-sonnet-4-6` on `assistant/agent.toml`, trading a
  small quality margin for materially lower per-turn latency (a trivial turn
  measured ~2.1s on Opus via OpenRouter). `pm` is unaffected and stays on Opus
  per the prior owner ruling (PR #3360).
- **Generic "Assistant" is the default persona; Izzie / CTO Bot are selectable
  (owner decision, #3738, epic #3052):** the base `assistant/` package is now
  the concrete GENERIC default persona with a professional display name
  `"Assistant"` (reversing the prior "bases are nameless" decision) — it stays
  the un-personalized `extends` base for the named personas, which override
  `display_name` via child-Some-wins per-key merge. `cto-assistant`'s display
  name is renamed `"CTO Assistant"` → `"CTO Bot"` (a new `"cto bot"` `/switch`
  alias is added; existing aliases still work). Izzie keeps display name
  `"Izzie"` and her flavor in her own overlay. The deprecated `ctrl` coordinator
  persona stays in place and reachable via `/switch ctrl`, but nothing defaults
  to it (the plain-CLI startup already auto-switches to `assistant`; the
  `/switch` help and picker now list Assistant first and mark ctrl deprecated).
  A single canonical accessor `AgentInfo::display_label()` (`display_name` →
  `name` fallback) now drives the speaker label, and `GET /api/agents` surfaces
  `display_name` so the GUI's per-message speaker attribution reads
  "Assistant" / "Izzie" / "CTO Bot" from the backend without re-deriving the
  mapping client-side. Tool registrations (weather / Metro-North on Izzie) are
  unchanged — no tool scopes moved.
- **Assistant delegates to specialists + peers; black-boxes internal tooling
  (PR A, epic #3052):** the base `assistant` agent (and every persona built
  on it — `izzie`, `cto-assistant`, `personal-assistant`) can now actually
  get hands-on engineering/QA/research work done. `assistant/agent.toml`
  gains `delegate_to_agent` in `[tools].allow` (native, in-process
  tagent→tagent delegation via `src/tools/delegate.rs`, already wired into
  persona dispatch by `filter_persona_tool_names` — only the grant was
  missing). `izzie` and `cto-assistant` both declare `extends = "assistant"`
  and inherit the grant automatically (`[tools].allow` is unioned base-first
  by `crate::agents::extends::merge_extends`, #3055/#3106).
  `assistant/persona.md` softens the old "redirect coding to engineering
  agents" framing to "bring in the right specialist and relay the outcome",
  adds a peer consult-and-relay capability (assistant instances may consult
  each other and summarize the input, staying in control of the
  conversation), and adds a BLACK-BOX rule: never say "tm", "tcode",
  "trusty-mpm", "trusty-code", "PM session", "subprocess", "sub-agent", or
  "subagent" to the user — describe outcomes via "a specialist" / "my team",
  never internal mechanics or system/daemon names. `cto-assistant/agent.toml`
  is refactored from a hand-forked full copy (`role = "assistant"`, no
  `extends`) to `extends = "assistant"`, keeping only its Duetto-specific
  deltas (CTO ops DB tools, ticketing, extra git tools, org/APEX/voice
  skills, and its tuned `[llm]` sampling — see below) — it now also inherits
  the base's black-box posture and full gworkspace surface.

- **Per-key `[llm]` `extends` inheritance for `temperature`/`max_tokens`:**
  `crate::agents::extends::merge_extends` previously inherited the entire
  `[llm]` block wholesale from the base, discarding a child's declared
  `temperature`/`max_tokens` even when explicitly set — a live regression
  for `cto-assistant`'s conversion above (its #469 max_tokens bump, 1024 →
  4096, for untruncated GFA reports/org tables, and its lower temperature,
  0.3, for factual precision, would otherwise have silently reverted to the
  base's conversational defaults). `temperature`/`max_tokens` are now the
  only two `[llm]` fields resolved per-key: a child that declares one
  overrides the base's, one it omits inherits the base's. This is enabled by
  a new UNSET-sentinel serde default (`LlmParams::temperature`/`max_tokens`
  — `NaN` / `u32::MAX`, never a realistic real-world value) plus a new
  `AgentConfig::validate_llm_required_for_root` parse-time guard: a root
  (non-`extends`) agent TOML must still declare real values (restoring the
  fail-fast guarantee the old serde-mandatory-field requirement gave), while
  an `extends` child may now legitimately omit either or both to inherit the
  base's. `.md`-format `extends` children (which have no frontmatter path to
  declare `temperature`/`max_tokens` at all) now correctly inherit the
  base's values too, instead of always resolving to the `parse_md_agent`
  stub defaults (0.2 / 8192) regardless of the base. Every other `[llm]`
  field (`use_anthropic_direct`, `stop_sequences`, `max_turns`, ...) keeps
  the pre-existing inherited-from-base-wholesale behavior — this is a
  narrow, deliberate schema addition scoped to the two fields with no serde
  default, not a general per-key `[llm]` merge. New tests:
  `extends_llm_child_overrides_temperature_only`,
  `extends_llm_child_overrides_max_tokens_only`,
  `extends_llm_child_inherits_when_omitted`
  (`crates/trusty-agents/src/agents/extends/tests.rs`),
  `llm_required_fields_missing_on_root_agent_is_rejected`,
  `llm_omitted_fields_allowed_on_extends_child`, and
  `assistant_tier_llm_sampling_params_pin_per_key_extends_inheritance`
  (`crates/trusty-agents/src/agents/tests/loading.rs`) — the last one pins
  `cto-assistant` resolving to `(0.3, 4096)` and `assistant`/`izzie`
  resolving to their unchanged `(0.7, 1024)`.
- The REPL startup splash (`repl::tui::banner::banner_lines`, and its dormant widget-mode counterpart `draw_banner`) now renders the shared trusty splash art (`trusty_common::banner::TRUSTY_SPLASH_ART`, per-glyph shaded via `shade_bucket`) instead of a bespoke robot-glyph design, so `tagent`'s REPL presents the same trusty branding as `tm`'s launch banner (closes [#3326](https://github.com/bobmatnyc/trusty-tools/issues/3326)). The left column is now sized from the shared art's actual width instead of a fixed 18-col floor. tagent's own contextual text (identity line, recent activity, commands) is unchanged.
- `gworkspace` `[[tool_registry.endpoints]]` in the bundled default config now
  ships `enabled = true` (was `false`, "pending auth"). `rpc.discover` on the
  `trusty-gworkspace-mcp` server is a pure static function that never touches
  the token store, so eager discovery at harness startup always succeeds
  regardless of auth state; unauthenticated tool calls fail with a clear
  "run setup" operational error instead of a startup crash. Authenticate with
  `trusty-gworkspace-mcp setup` to actually light up Gmail/Calendar/Drive
  access for the Assistant (Phase 1 of the Assistant epic #3052, part of
  [#3056](https://github.com/bobmatnyc/trusty-tools/issues/3056))
