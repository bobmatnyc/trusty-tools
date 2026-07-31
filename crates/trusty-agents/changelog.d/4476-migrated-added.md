Added

- **Cross-project git/search scoping granted to the L0 orchestration tier
  ONLY** (#4172, epic #4167). New `agents::cross_project::CrossProjectScope` is
  the single resolution point for "which repo roots and which search indexes
  may THIS agent touch". An L0 agent's allow-set is its own home project root
  plus every `~/.trusty-agents/projects.json` entry that is *simultaneously*
  `Active`, a real project (no `/tmp`, `/var/folders`, `.tmp*`), existing, and
  a git repository root. An L1 agent gets its home root and nothing else —
  byte-identical to previous behaviour. **DOC-54 §5.1's one-OKG-store-per-agent
  ceiling is untouched for either tier**: nothing here reads, writes, unions or
  widens `[[stores]]`, and `extends`' replace-not-union rule is unchanged;
  cross-project reach is resolved as *paths and tier-2 index ids*
  (`[tools].search_indexes`, #3232), which is where multiplicity already
  belongs. Containment is component-wise `Path::starts_with` against
  *canonicalized* roots, so `..` traversal, symlink escape, and `/a/bc` vs
  `/a/b` prefix-lookalike siblings are all refused; a relative candidate is
  refused outright rather than joined to the cwd. Because libgit2's
  `Repository::discover` walks *upward*, `open_scoped_repo` re-validates the
  discovered work tree after discovery — a permitted seed cannot land on an
  ancestor outside the allow-set. Registry failure denies reach: no `$HOME`, a
  missing file, or malformed JSON yields an empty snapshot, collapsing even an
  L0 agent to its home root. The per-call `repo` argument is advertised in the
  JSON schema only when the scope is actually cross-project, so an L1 persona
  is never shown a knob it cannot turn. Every refusal is logged at `warn` with
  the same `tier=… cross_project=… roots=[…] indexes=[…]` audit line emitted at
  construction. No per-project credential is read, copied, or forwarded.
- **A read-only GitHub PR/CI inspection surface granted to the L0
  orchestration tier ONLY** (#4170, epic #4167). Five tools in a new
  `tools::gh_tools` — `gh_pr_list`, `gh_pr_view`, `gh_pr_checks`,
  `gh_run_list`, `gh_run_view` — returning `gh`'s own output verbatim rather
  than re-shaped JSON. **No mutating capability is granted**: there is no
  `pr create`/`merge`/`edit`/`comment`/`close`, no `run rerun`, no
  `workflow run`, and a test fails the build if a mutating verb ever appears
  in a tool's name or description. The tier gate lives INSIDE the factory
  (which returns an empty vector for any tier but `L0Orchestration`), so a
  future call site that forgets to guard its loop still registers nothing,
  and an L1 persona declaring `gh_pr_view`, `gh_*` or `*` matches nothing and
  is granted nothing. Registered on BOTH assistant dispatch paths — the
  `--direct`/`--agent` subprocess path and the persona-chat path — since
  registering in only one is the divergence #3745 item C had to fix.
  Every LLM-supplied operand is passed as argv, never through a shell, and
  must be non-empty, ≤200 chars, free of a leading `-`, and drawn from
  `[A-Za-z0-9._/#:@-]`; `repo` must be exactly `owner/repo`, `state`/`status`
  are closed enums, and `limit` clamps to 1–100. `gh pr checks` reports check
  state through its exit code, so a non-zero exit is returned as normal
  output rather than an error — otherwise the most useful CI primitive would
  report failure on exactly the state an orchestrator calls it to observe.
- **`l0_shell_exec` — a shell/build/test executor granted to the L0
  orchestration tier ONLY** (#4173, epic #4167). `tools::l0_exec` returns an
  empty tool vector for `AgentTier::L1Standard`, so an L1 persona never has the
  tool *registered* — it cannot be reached by declaring it (or `l0_*`, or `*`)
  in `[tools].allow`, cannot be reached through a delegation taint (which only
  ever intersects with what the child registry already holds), and cannot be
  reached transitively (the #4200 tier gate refuses every L1 → L0 hop). The
  grant follows the spawned agent's OWN resolved tier — never a delegator's —
  and `AgentInfo::tier()`'s fail-closed resolver means an absent, blank or
  unrecognized `tier =` resolves to L1 and therefore to nothing. RBAC still
  applies on top: the tool is refused for the `ReadOnly` and `Analytics`
  service tiers. Per-call `working_dir` is contained to the project root
  (absolute escapes, `..` traversal and symlink escapes are rejected); the
  catastrophic-pattern predicate is reused from `tools::run_bash` rather than
  re-invented, and is documented as defense-in-depth, not the boundary. The
  tier gate is the boundary.
- **`GET /api/agents/:name/kg`, `/kg/subjects`, `/kg/all` and `/kg/count` — a
  read-only proxy onto the Knowledge Graph of the memory palace the agent binds
  via `[[stores]].palace`** (#4290). trusty-memory already owns every one of
  these queries; this crate only maps agent → palace and passes the upstream
  JSON through verbatim under `{palace, connected, data}`. An agent that binds
  no palace, or a trusty-memory daemon that is down, returns `200` with
  `connected: false` and a machine-readable `reason` — the same never-fail
  posture `GET /api/agents/:name/stores` already has — so the Knowledge Graph
  browser renders an empty state instead of an error. Assert and delete are
  deliberately not proxied; editability is a separate ticket.
- **Knowledge Graph browser — the UI leg of #4290.** A "Knowledge Graph"
  button beside the Assistant selector in `ChatHeader` opens a dedicated
  slide-over (`KnowledgeGraphBrowser.svelte`) over the active agent's one
  bound palace: a filterable/sortable subject list with count badges,
  drill-down into a subject's triples, and a paginated "all triples" mode —
  porting `trusty-memory`'s `KG.svelte` interaction pattern into this app's
  Foundry/Tailwind idiom (single palace, no picker). Read-only v1, hidden
  entirely for Concierge (no `agent.toml`/`[[stores]]` binding). Renders six
  explicit states rather than a generic spinner or empty list: loading,
  connected with data, connected with a genuinely empty graph, `connected:
  false` with its `reason`, a `config_error` surfaced alongside that, and the
  fetch-helper's 404/network-error path. New `lib/kg.ts` client following
  `fetchAgentStores`'s null-on-404/throw idiom; the SPA still never calls
  trusty-memory directly.
- **`PATCH /api/agents/:name` accepts `subagents_delegate_allowed`, with a
  server-side floor.** Unlike the existing `tools_allow` field, this one
  validates: a request naming anything outside the reachable floor is rejected
  `400` with the offending names echoed back, and nothing is written. A widened
  list that reaches disk some other way is still refused at dispatch.
- The Sub-agents pane reports the whitelist, whether the agent declares one, and
  the floor a config edit is bounded by; an unreachable target now carries the
  reachable-set reason alongside the existing kind and tier reasons.
- **`[[plugins.python]]` tools declared by an agent are finally registered and
  callable (#446, epic #3052).** The `PythonToolPlugin` subprocess primitive
  (spawn `python3 <script>`, one NDJSON `tool_call` in / one `tool_result`
  out, per-call timeout, fail-closed RBAC tier guard) has shipped fully
  unit-tested since #446, and `AgentConfig` has parsed `[plugins]` for just as
  long — but nothing in the workspace ever called
  `PythonToolPlugin::from_config`, so an agent or skill package declaring a
  `[[plugins.python]]` tool had its declaration parsed and then silently
  dropped: never registered, never advertised, never callable, no warning.
  `run_pm_task_with_persona` now reads `persona_cfg.plugins.python` and
  registers each entry via a new `persona_plugins::register_python_plugins`
  helper, resolving a package-relative `script` / `schema_file` against the
  agent/skill package dir (the same `config_dir` the delegation runner is
  handed), so `script = "../skills/foo/foo.py"` reaches a co-located skill
  package. Registration is not a grant: the `[tools].allow` glob filter, the
  RBAC tier filter and the scope gate all still decide whether the tool
  reaches the model. Two non-fatal skip paths keep one bad line in one agent
  TOML from taking down a chat turn — an entry `from_config` rejects (e.g. an
  unknown `restricted_tiers` string, fail-closed since #3236) is logged and
  skipped, and an entry whose name is already taken by a native or MCP tool is
  refused rather than registered (`ToolRegistry::register` overwrites silently
  in release and `debug_assert!`s in debug, so an unguarded registration would
  have made a debug-build panic reachable straight from user-authored config).
  SECURITY NOTE, unchanged by this PR and worth restating now that the path is
  live: the Python subprocess has NO OS-level sandbox — RBAC gates WHO may call
  the tool, not WHAT the script does once spawned. KNOWN GAP: this wires the
  in-process persona-chat dispatch path only; the subprocess `--agent` path
  (`runtime::tool_registry::build_assistant_tier_registry`, which takes no
  `AgentConfig`) still ignores `[[plugins.python]]` and needs its own change.
- **The app header shows the running daemon's version.** A quiet provenance
  line sits beside the brand lockup, read from `GET /api/health` — the version
  of the daemon the UI is actually talking to, not what the frontend was
  compiled against, since surfacing that mismatch is the point. It is one
  follow-up read fired after the existing readiness probe succeeds, not a
  second poller. Before the probe answers it shows `v…` and on failure `v—`,
  never a guessed number and never an empty slot that shifts the lockup. The
  line is shaped so #4260's commit SHA appends to it (`v0.38.6 · abc1234`)
  without a redesign; the meaningless `build #N` counter from
  `tagent --version` is deliberately not shown.
- **A `Cost` tab showing spend by agent, model, and day (#4098).** The new
  `CostsView` renders `GET /api/costs`, which folds
  `.trusty-agents/state/usage.jsonl` on request into a grand total plus three
  breakdowns. It refuses to render a confident `$0.00` over a usage log that
  does not exist: a missing log is reported as "no usage has been recorded",
  naming the file it looked for, and is visibly distinct from an empty-but-
  present log, from a read failure, and from still-loading. Lines that fail to
  parse are counted and surfaced as a warning that the totals are incomplete,
  rather than silently dropped. Charts are plain CSS bars — no charting
  dependency was added.
- **`GET /api/costs` (#4098).** Optional `?days=<n>` narrows the window,
  anchored on the newest recorded row. Returns `200` with `available: false`
  when no log exists, `500` when a log exists but cannot be read, and always
  reports `records` / `malformed_lines` so a partial total is identifiable as
  one. Aggregation is read-time; the durable rollup tables of #4104/#4105 are
  NOT implemented.
- **The Skills pane renders function skills as groups (#4024).** A `kind:
  "function"` bundle is no longer filtered out of the pane — it renders as a
  collapsed group header over its member skill cards, with the tri-state grant
  display DOC-57 S-16 specifies: "granted" only when every member is granted,
  "partial — k of n" when some are, "not granted" when none are. Members render
  inside their group instead of the flat Actions/Knowledge/System buckets (one
  card, one place); a skill belonging to no bundle renders exactly as before.
  Membership and grant state are read from the route's `groups[]`, never
  re-derived client-side, and a bundle still does not move the "N of M known
  skills granted" count — it is not a capability of its own.
- **A `Google Workspace` function skill (#4024)** bundling the Gmail and Google
  Tasks leaf skills (search, read, compose, draft, format, labels, attachments;
  task lists, create, update, complete) as one grantable unit. Gmail account
  settings and filters, Google account administration, and the Drive/Docs/
  Sheets/Slides/Calendar families are deliberately NOT members.
- **A `CTO Tools` function skill (#4024)** bundling the four CTO operations
  database queries (headcount, budget, risks, work classification).
- **`groups[].providers` on `GET /api/agents/:name/skills` (#4024).** A bundle
  carries no credential of its own, so the group now reports the DISTINCT
  credential requirements across its members, each with the same tri-state
  `configured` a leaf card carries. Members needing different credentials
  produce one entry each — a divergence is rendered, never collapsed into a
  single verdict — and an OAuth requirement still reports `configured: null`
  ("not verified"), never a check nobody ran. Additive: existing fields are
  unchanged.
- **The token-streaming chat path now streams Bedrock models via
  `ConverseStream` instead of silently falling back to the blocking
  `Converse` call (#3767).** `streaming_supported()` no longer excludes
  `Provider::Bedrock`; `stream_reply` routes `bedrock/*` models to a new
  `trusty_common::chat::BedrockProvider` (real event-stream) branch, with the
  `bedrock` feature now enabled on the `trusty-common` dependency (zero added
  weight — this crate already links the AWS Bedrock SDK unconditionally).
  `StreamAssembly` gained a `usage: Option<ChatUsage>` field so per-call token
  usage — which Bedrock reports exactly once, in a terminal event — survives
  the streaming path instead of being dropped.
- **`GET /api/agents/:name/permissions` — the Permissions pane's structured
  backend (#3936, DOC-57 §7).** A new `[permissions]` `agent.toml` section
  (`scopes`, `user_authority`, `default_tier`/`unauthenticated_tier`,
  `autonomy`, `[[grants]]`) describes the four scattered enforcement
  mechanisms (`[tools].allow`, `[tools].scopes`, RBAC tiers, the endpoint
  scope filter) as one coherent model rather than inventing a new one. Every
  response field carries an `enforced` boolean — `true` only for `scopes[]`,
  which is now resolved by `agents::permissions::effective_scopes` and fed
  into the SAME persona-chat dispatch gate the route reports on (so the
  claim is never aspirational); every other field (`user_authority`, tiers,
  `autonomy`, `grants`) is honestly `enforced: false`, since none has a live
  enforcement site yet. `enforced: true` is **qualified**, not universal:
  each `scopes[]` element also carries `enforced_on: ["persona_chat"]`,
  because `effective_scopes` gates the persona-chat dispatch path only — the
  `--direct`/subprocess path (`runtime::subagent_mode` →
  `scope_assistant_allowed_tools`) filters by `[tools].allow` name globs and
  never consults a scope pattern, so an agent invoked via `tagent --direct`
  is not scope-restricted by anything this pane reports (#3987/#4093 track
  unifying the two paths). `[permissions].scopes` supersedes legacy
  `[tools].scopes` within one file (CC-9 — the union is deliberately not
  taken) but still unions base-first across `extends` (§2.3), so a
  partially-migrated inheritance chain never silently drops a base's
  un-migrated grant. `source` distinguishes `declared` from
  `inherited:<base>` so the class of defect DOC-57 §7.3 records (an agent
  granting tools whose scopes it never actually declared) is visible in the
  pane. `user_authority` (DOC-41 §5.5's reserved singleton) is now a real,
  parsed `AgentConfig` field and is never inherited across `extends`
  (PM-3) — the previously `#[ignore]`d
  `extends_does_not_inherit_user_authority` regression test is un-ignored.
  Read-only in every phase (PM-4); grants or widens nothing (C-06.4).
- **`GET /api/agents/:name/knowledge` — the Knowledge pane's unified
  "what does this agent know" backend (#3935, DOC-57 §4).** One response
  carries all three sub-surfaces so the pane never has to correlate a store
  card, a tool glob and a harness endpoint table across three routes: `[[stores]]`
  bindings resolved live (`/stores`' existing, now-fixed logic — see Fixed
  below), the agent's knowledge-classified tools (`kind = Knowledge` in
  `#3933`'s skill catalog, `vector_search` rendered under its bound store),
  the tier-2 `[tools].search_indexes` attachments (`search_indexes` +
  `enforce_search_indexes`, same field names `/stores` already uses), and the
  operator-configured MCP knowledge connections
  (`[[tool_registry.endpoints]]` + `[[mcp.services]]`, read-only, no live
  probe — a disabled/unenabled service is always reported truthfully, never
  as connected, and every card carries `granted_for_agent` so an operator-
  enabled connection isn't conflated with one this specific agent can
  actually reach). Read-only in this slice; store editing remains #3890's
  contract.
- **cto-assistant's four CTO DB tools (`query_headcount`, `query_budget`,
  `query_risks`, `query_work_classification`) are invokable again, via a new
  Python skill instead of hardcoded Rust (#3656, #3700, #3732).** These tool
  names have been in cto-assistant's `[tools].allow` list with no live
  implementation since the Rust-native `crates/cto-assistant` plugin's only
  `install_plugins(...)` call site was removed by PR #3310. Per the owner's
  directive, the fix bundles the query code and its data source together as
  a skill (`.trusty-agents/skills/cto-db/`, a Python package + a fixture
  SQLite database) rather than resurrecting the old Rust wiring, which
  #3656 objects to as a "coded agent" violating DOC-41 §2.0
  declarative-only.
  - New generic bridge `tools::python_skill` (CTO-agnostic — no per-skill
    Rust code): reads a skill's `manifest.json`, builds one `ToolExecutor`
    per declared tool that spawns the manifest's Python command, and wires
    the whole batch through the pre-existing `AgentPlugin`/`install_plugins`
    mechanism. `runtime::startup` now calls
    `python_skill::install_discovered_skill_plugins` once at boot,
    best-effort (a missing skills directory or a malformed manifest is
    logged and skipped, never fatal).
  - **The bundled `cto_fixture.db` is FIXTURE data, not the real Duetto CTO
    ops database** (`~/Duetto/cto/data/cto.db`, unreachable from this
    environment) — set `CTO_DB_PATH` to the real file once it's reachable.
    See `.trusty-agents/skills/cto-db/python/README.md` for the full
    disclosure.
  - The old Rust implementation (`crates/cto-assistant`,
    `crates/tc-services::cto_db`, `crates/trusty-cto-db`) is untouched;
    dissolving it is separate, larger scope (#3732).

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
  into a skill source can widen one grant into N. Nor can one *displace* a
  built-in bundle by id: authored-beats-built-in (DOC-57 S-9) is a **renaming**
  rule, and applied to a bundle id it would be a hijack — a `ticketing.md`
  carrying `tools: ["execute_shell_command"]` would turn
  `[skills].allow = ["ticketing"]` into a shell grant with nothing in
  `agent.toml` for a reviewer to see and no `unresolved` entry. A bundle id is
  the one name whose meaning cannot be checked by reading the config, so it is
  the one name a skill source may not redefine; the attempt is now refused and
  logged. A bundle's *members* stay ordinary displaceable leaves — that path
  narrows the bundle and reports the lost member. A bundle naming a member no
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
- `PATCH /api/agents/:name` — persists a per-agent `model_id`/`provider_id` override to that agent's `.trusty-agents/agents/<name>.toml`, editing the `[agent]` table in place via `toml_edit` so every other key, comment, and prose block (e.g. a long system-prompt) survives untouched. `provider_id` is validated against the `trusty_common::inference::registry` catalog surfaced by `GET /api/models` (#3243); a `runner = "claude-code"` agent rejects any model/provider that doesn't resolve to Anthropic, since the local `claude` CLI only talks to Anthropic. Returns the updated agent in the same shape `GET /api/agents` uses, so a client can round-trip through either route. Pairs with the agent create/edit UI merged in #3279 (closes [#3246](https://github.com/bobmatnyc/trusty-tools/issues/3246), part of epic #3052)
- new bundled base `assistant` agent (`.trusty-agents/agents/assistant/`) — a reusable, **nameless** personal-productivity template derived from the "Izzie" prototype: functional `role = "assistant"`, the curated Google Workspace tool surface, `memory.read`/`memory.write`/`search.read` scopes plus **read-only** `google.read` (§5.5 — a generic template does not mutate a user's Google data by default), and generic helpful/productivity instructions, with NO persona name, user-identity binding, or user-specific skills. Persona identity is contributed by a user's `extends = "assistant"` personalization overlay (Eve §2.5.1). Izzie is now shipped as the reference overlay example (`.trusty-agents/agents/izzie/`): `extends = "assistant"` + personal deltas (display name, personal skills, Masa-bound persona) and an explicit opt-in to Google **write** (`google.*`). The `extends` inheritance resolver lands in [#3055](https://github.com/bobmatnyc/trusty-tools/issues/3055); until then the overlay carries the curated `[tools]` allowlist/scopes and the safety-critical persona guardrails (approval-framing, anti-hallucination) redundantly so it is safe standalone, and the working Izzie the REPL/Telegram persona paths load remains the standalone `izzie.toml` (closes [#3054](https://github.com/bobmatnyc/trusty-tools/issues/3054), refs #3052, #3055)

- `extends:`-based agent personalization (DOC-41 §2.5 / §2.5.1, epic #3052): a user can now personalize a stock/bundled base agent — name it, add tools, override tone — without forking it, by authoring a child agent (e.g. `~/.trusty-agents/agents/my-assistant.md`) that declares `extends: <base>` and layers personal deltas on top. The previously-inert `extends:` frontmatter key is now live. Resolution mirrors trusty-mpm's proven `compose_agent` exactly (same `MAX_DEPTH = 8`, case-insensitive base lookup, base-first prose concatenation) and runs once at load time — reachable from BOTH the informational `AgentRegistry::load` AND the real dispatch loaders `AgentConfig::by_name`/`by_name_async` (a flat `<name>.md` overlay tier was added to both so a personalization overlay actually dispatches, not just shows in the roster) — never per dispatch. Merge follows the §2.5 table: scalars (`model`, `display_name`, `role`, `description`, plus `runner` and opt-in `persistent_session`) child-overrides-parent; list fields (`tools.allowed`, `tools.scopes`, `system_prompt.skills`, `capabilities`) union (dedup, base-first); persona/instruction prose concatenates base-first; `llm`/`compress`/`session`/`plugins`/`rbac` inherit from the base wholesale. Cycles, missing parents, and over-deep chains fail with a clear `AgentExtendsError`; in the registry a failed resolution keeps the unresolved agent but surfaces the breakage in the roster / `tagent agents list` (new `AgentSummary::extends_error`). A directory package whose `extends` can't be resolved no longer silently shadows a complete flat `<name>.toml` — the loader falls back to the flat file with a warn. Case-only duplicate agent names are rejected at load (they broke case-insensitive resolution). `.md` agent frontmatter now also parses `tools:` and `display_name:` so a personalization overlay's declared tools actually union with the base's (closes [#3055](https://github.com/bobmatnyc/trusty-tools/issues/3055))
