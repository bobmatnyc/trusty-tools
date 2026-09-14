# CTO Assistant base case: gap analysis (2026-09-14)

Owner decision 2026-09-14: reach a base case for the CTO assistant across four
requirements (R1 indexed knowledge root, R2 MCP connectors and channels, R3
simplified permissions, R4 Bedrock inference). This note reads the current
worktree state (origin/main plus one prior research doc, checked out
2026-09-14) against each requirement and against three prior 2026-09-11
research notes in `docs/research/`:
`trusty-agents-1.0-gap-analysis-2026-09-11.md` (items a-f),
`trusty-agents-mcp-connectors-gap-analysis-2026-09-11.md` (item g),
`trusty-agents-infinite-context-audit-2026-09-11.md`. Those notes are cited,
not re-derived, except where issues opened or closed since then changed the
picture (several epic #7425 children merged 2026-09-11 through 2026-09-12,
three days before this session).

Live objects inspected directly: `~/.trusty-agents/config.toml`,
`~/.trusty-agents/agents/cto-assistant/agent.toml` and `persona.md`,
`~/trusty-agents/cto-assistant/` (the assistant's runtime home, distinct from
the `.trusty-agents/agents/` template package), `~/.aws/config`, and
`mcp__trusty-search__list_indexes`.

## R1. Indexed knowledge root (`~/trusty-mpm-projects/bob-duetto/cto/projects`)

**Current state.** The cto-assistant agent definition already declares a
store: `agent.toml` `[[stores]]` (`~/.trusty-agents/agents/cto-assistant/agent.toml`,
`name = "cto-assistant-kb"`, `tree = "okg://cto-assistant"`,
`index = "cto-assistant"`, `palace = "cto"`) — this is the single-index-per-
assistant model the codebase already uses, matching what R1 asks for in
shape. The runtime instance home at `~/trusty-agents/cto-assistant/config.toml`
carries only `id = "cto-assistant"`; no `[mcp]`, `[memory]`, or per-assistant
override table has ever been written for this instance.

`mcp__trusty-search__list_indexes` today shows no index named `cto-assistant`.
The two indexes that exist near this tree are `cto`
(`root_path=/Users/masa/trusty-mpm-projects/bob-duetto/cto`, 184,601 chunks,
covers the WHOLE cto repo, not the `projects/` subtree specifically) and
`assistant-okg-90440c4edc29b2a85d90ae31`
(`root_path=/Users/masa/trusty-agents/cto-assistant/okg`, 0 chunks — the
per-assistant OKG tree index, auto-managed, currently empty). Store-status
probing (`crates/trusty-agents/src/stores/status.rs:290`) fails soft: a
missing index degrades the store to "not connected" with the message
`` search index `{index}` is not registered on the trusty-search daemon`` —
it does not block the assistant from booting, which is why this has gone
unnoticed. `~/trusty-mpm-projects/bob-duetto/cto/projects/` exists on disk
(`ELT`, `engineering`, `gtm`, `hotstats`, `meetings`, `people`, `product`,
`research`, `security`, `systems`, `writing`).

**What is missing.**
- **Item R1-a (S, trusty-search + operator action, no code change):** create
  a trusty-search index named `cto-assistant` (or repoint the store binding's
  `index` field to whatever id is chosen) rooted at
  `~/trusty-mpm-projects/bob-duetto/cto/projects`, via
  `mcp__trusty-search__create_index`. This alone satisfies R1's literal ask —
  it does not need the multi-root work below. Verify with
  `stores/status.rs`'s probe (`connected: true`) after creation.
  `crates/trusty-agents/src/stores/config.rs:61` — `StoreConfig.index`/`palace`
  fields, confirming the binding shape accepts an arbitrary index id today.
- **Item R1-b (M, trusty-search):** issue **#7434** "index with multiple
  roots — `create_index`/`reindex` accept a roots list" is the primitive the
  1.0 model actually wants (one index per assistant, the OKG tree as one
  root, each attached project as another root) — `IndexHandle.root_path` is
  a single `PathBuf` today. Not required to reach R1's base case (R1-a
  suffices), but is the durable fix and is a dependency of R1-c.
- **Item R1-c (M, trusty-agents):** issue **#7429** "one search index per
  assistant with an OKG root plus one root per project" — depends on #7434.
  Superseded in its config surface by **#7611** ("agent config
  reorganization — automatic memory/search, Knowledge becomes Projects",
  owner ruling 2026-09-12: memory and search become automatic per assistant,
  a `[projects]` table replaces `[[stores]]`, folders become roots of the
  assistant's one index). #7611 is the current target shape; #7429/#7434 are
  its dependencies. Both OPEN, milestone "trusty-agents 1.0 — assistant
  platform" (#83).

**Ordering:** R1-a first (base case, no dependency, S). R1-b -> R1-c/#7611
is the durable multi-root replacement and can proceed independently on its
own timeline.

## R2. MCP connectors and channels

### MCP connectors

**Current state.** The per-assistant MCP tier the owner ratified 2026-09-11
(cited in the requirement) has already shipped: `crates/trusty-agents/src/assistants/mcp.rs`
defines `McpOverrides` (`servers`, `disabled`) read from the `[mcp]` table of
an assistant home's `config.toml` (ADR-0060 decision 4, issue **#7454**,
CLOSED 2026-09-11), layered over the shared `trusty_mcp::config::McpServerConfig`
global list (issue **#7452**, CLOSED 2026-09-11, delivering the shared config
file and resolver the mcp-connectors-gap-analysis doc recommended). This
post-dates and substantially closes what that 2026-09-11 doc found missing —
at the time of that doc, trusty-agents had four independent MCP-shape schemas
and no per-assistant override surface at all (§4-5 there); the override
surface now exists in code.

cto-assistant's runtime `config.toml` has no `[mcp]` table today, so it runs
on the global list only — `~/.trusty-agents/config.toml` `[[mcp.services]]`
entries: `trusty-mpm`, `granola-notes`, `duetto-memory` (disabled),
`trusty-search`, `trusty-memory`, plus (unread further in this session)
whatever follows for gworkspace via the OpenRPC `[[tool_registry.endpoints]]`
path the file's own comments describe. Per the requirement's own citation
(`docs/research/trusty-agents-mcp-connectors-gap-analysis-2026-09-11.md`),
`trusty-mcp` owns the shared config type/loader (now delivered per #7452) and
trusty-code's consumption of it is tracked separately (#5428, still OPEN —
out of scope for the assistant base case).

**What is missing.**
- **Item R2-a (S, trusty-agents, operator config):** none — the mechanism
  exists. If cto-assistant needs an MCP connector the global list does not
  grant (or needs to disable one it does not want, e.g. `duetto-memory`
  once enabled globally), write its `[mcp]` table via the existing route/tool
  rather than editing the agent template. No code work identified.
- **Item R2-b (S, verification only):** confirm live that gworkspace tools
  (Gmail/Calendar/Drive/Docs/Sheets/Slides/Tasks) actually dispatch for
  cto-assistant end to end — issue **#7451** ("MCP connectors — one config
  authority in trusty-mcp, global and per-assistant tiers", OPEN, milestone
  #83) is the parent tracking issue; its children g1/g2/g3 (trusty-mcp,
  trusty-mpm, trusty-code sides) are owned by a different session per the
  issue body's own scope note. g4 (trusty-agents per-assistant overrides) is
  the piece delivered by #7454/#7452 above.

### Channels (Slack minimum)

**Current state.** Slack has both directions in code:
`crates/trusty-agents/src/slack/{mod.rs,pairing.rs,handlers.rs}` — Socket Mode
gateway, pairing state machine, inbound handlers, outbound `chat.postMessage`
senders — but `mod.rs:1-12` states it drives `ctrl::run_pm_task_with_history`,
the PM/orchestrator loop, not a named assistant persona directly; routing a
Slack conversation to a specific assistant (cto-assistant) is a binding, not
automatic. The generic `channel` tool
(`crates/trusty-agents/src/tools/channel.rs:26-28`, actions list/read/send)
is the per-assistant-facing surface, backed by
`crates/trusty-agents/src/api/server/agent_channels.rs` and routes
`GET`/`PUT /api/agents/{name}/channels[...]`.

Issue **#7427** ("two-way channel connectors — gworkspace, slack, telegram,
notion", CLOSED 2026-09-12) delivered the **config-level** binding plumbing
only: its closing comment states explicitly that binding round-trips via
PUT/GET verify, but the six **credential-bearing end-to-end delivery
checks** (Slack inbound, Slack outbound, Telegram inbound, Telegram outbound,
gworkspace inbound, gworkspace outbound) were "deferred by the owner" and
"NOT covered by this close," with no follow-up issue filed for them. Notion
is tracked separately and still has zero connector code
(`crates/trusty-channels/src/lib.rs` exposes only `slack`/`telegram`) — issue
**#7437** OPEN, milestone "Backlog · mcp".

Live check of `~/trusty-agents/cto-assistant/` and `~/.trusty-agents/` finds
**no `cto-assistant.channels.json`** anywhere on this machine — no channel
binding has ever been written for this specific assistant instance, despite
history: issues **#3852**/**#3855**/**#3139** (all CLOSED, pre-1.0) describe
an earlier native-Slack-for-CTO-Assistant delivery under the OLD channels
model, which the 2026-09 refactor (#7427, and the in-flight **#7609** merge of
listeners into channels) has since superseded — that earlier binding did not
survive the refactor as live config. The `channel` tool itself is also absent
from both `assistant/agent.toml`'s base `[tools].allow` and
`cto-assistant/agent.toml`'s delta list (both read in full this session) — so
even with a binding written, the model has no tool grant to call `channel`
read/send today.

**What is missing.**
- **Item R2-c (S, operator config, no code):** write a Slack channel binding
  for `cto-assistant` via `PUT /api/agents/cto-assistant/channels` (the route
  #7427 delivered and verified at the config level).
- **Item R2-d (S, trusty-agents, one-line agent.toml change):** add
  `"channel"` to `cto-assistant/agent.toml`'s `[tools].allow` (or to the base
  `assistant` template, if every persona should have it) so the model can
  actually call list/read/send once bound.
- **Item R2-e (M, verification, not code):** the six deferred credential-
  bearing checks from #7427's closure are the real remaining R2 risk — no
  issue currently tracks them. This is a genuine backlog gap: closing the
  base case for R2 requires at minimum a live Slack inbound + outbound round
  trip for cto-assistant specifically, which nothing today schedules.
- **Item R2-f (L, trusty-channels, tracked):** Notion connector, #7437, OPEN.
  Not required for R2's "at minimum Slack" floor.
- Listener/channel unification (**#7609**, OPEN, milestone #83) is in
  flight and will move where a per-assistant Slack binding is stored
  (`<name>.channels.json` today, per its issue body) — sequence R2-c after
  #7609's storage-migration slices land, or expect to re-bind once it does.

## R3. Permissions simplified — subagents and skills

**Current state — subagents.** ADR-0024 decision 4 (editable whitelist,
fail-closed, server-side narrow-only floor) **has shipped** and is exactly at
the state the owner ratified. The floor is
`crates/trusty-agents/src/agents/delegation.rs:78` —
`pub(crate) const ASSISTANT_REACHABLE_SUBAGENTS: &[&str] = &["research-agent", "ticketing-agent"];`
— a hardcoded, server-owned, narrow-only ceiling (module doc,
`delegation.rs:42-68`: "a config (or a GUI PATCH) that names `engineer` must
be refused by CODE, not merely absent from a curated list"). The enforcement
point is `tools::delegate::DelegateToAgentTool::execute`
(`crates/trusty-agents/src/tools/delegate.rs:240`), and the reporting surface
(`api::server::agent_subagents`) is required to agree with it by the same
module's own design. `cto-assistant/agent.toml`'s `[subagents]` section
already declares `delegate_allowed = ["research-agent", "ticketing-agent"]`
— the exact floor, non-coding only (research + ticketing). **R3's subagent
half is done; no further code or config work is needed for cto-assistant.**

**Current state — skills.** `cto-assistant/persona.md` and `agent.toml`'s
`system_prompt.skills` list four Duetto-specific skills
(`cto-duetto-org`, `cto-apex-framework`, `cto-bob-voice`,
`izzie-metro-north`) plus whatever the base `assistant` template grants by
union (`extends = "assistant"`, base-first, deduped merge via
`crate::agents::extends::merge_extends`). Unlike subagents, **no server-owned
floor or editable-whitelist mechanism was found for skills** in this
session's reading of `agents/config.rs`, `agents/extends/`, or
`agents/delegation.rs` — a skill is granted by simply naming it in
`system_prompt.skills`, with no analogous `ASSISTANT_REACHABLE_*` constant
narrowing what an assistant-kind agent may load. This session did not find
any skill in the four named that is coding-oriented, so cto-assistant's
current skill roster already satisfies "non-coding only" in practice, but
the guarantee is by content review, not by a mechanical floor the way
subagents now have one.

**Tool allow-list, not asked for by name but adjacent to "permissions
simplified":** `cto-assistant/agent.toml`'s `[tools].allow` is a manually
curated ~15-entry delta list on top of the base's ~35-entry Google Workspace
surface, each entry carrying multi-paragraph historical justification
comments (four separate `#NNNN` incidents referenced inline: #3938, #3987,
#4461, #4473). This is the complexity the owner's "we've overcomplicated
permissions" reads most directly against, but the requirement as stated
scopes R3 to "subagents and skills," so tool-allow simplification is noted
here as an adjacent observation, not scored as a requirement gap.

**What is missing.**
- **Item R3-a: none for subagents.** Already at the ratified floor.
- **Item R3-b (M, trusty-agents):** no open issue was found proposing a
  skills floor/whitelist analogous to `ASSISTANT_REACHABLE_SUBAGENTS`. If the
  owner wants the same mechanical guarantee for skills that subagents now
  have (a server-owned "non-coding skills only" floor an editable list
  narrows), that is net-new work with no tracking issue today — flagged as a
  gap in the backlog, not just the implementation, mirroring the sub-agent
  precedent before decision 4 shipped.
- **Item R3-c (S, docs/no code):** ADR-0024's own text should be checked
  against `docs/adr/0024-...md` for whether it already scoped skills and
  this session simply did not find the clause; worth a five-minute doc-only
  follow-up before opening R3-b as new work, since duplicating a decision the
  ADR already covers would be wasted scope.

## R4. Inference via Duetto Bedrock permissions

**Current state.** `trusty_common::inference::bedrock` is a complete,
tested `InferenceAdapter` implementation
(`crates/trusty-common/src/inference/bedrock/mod.rs:130` `BedrockAdapter`,
`:226` `impl InferenceAdapter for BedrockAdapter`) using the AWS Converse
API, standard credential chain (env / `~/.aws/credentials` / SSO / IMDS), and
region resolution `TRUSTY_AWS_REGION` > `AWS_REGION` > `us-east-1`
(`resolve_bedrock_region`, `:68,83`). trusty-agents does **not** call this
adapter directly for its own chat dispatch; it has its own Bedrock client
(`crates/trusty-agents/src/llm/bedrock/{mod.rs,client.rs,convert.rs}`) that
wraps `aws-sdk-bedrockruntime` independently, though it does reuse
`trusty_common::inference::bedrock::conversation_messages` for message
conversion (`llm/bedrock/client.rs:150`) rather than re-implementing that
piece — a partial, not total, duplication of the common-entry-point rule
CLAUDE.md states for this workspace.

Bedrock **is** a reachable, pinnable provider today:
`crates/trusty-agents/src/llm/provider_pin.rs:88-95` — `const PINNABLE:
[ProviderId; 6] = [OpenRouter, Anthropic, Bedrock, Fireworks, AtlasCloud,
Local]` — so `[agent].provider_id = "bedrock"` in `agent.toml` is a supported,
fail-closed pin (`ProviderPinError::{Unknown,Unpinnable,MissingCredential}`).
The model-routing prefix is `bedrock/<model_id>`
(`crates/trusty-agents/src/llm/adapter/mod.rs:293-295`), e.g.
`bedrock/us.anthropic.claude-sonnet-4-6` (the cross-region inference-profile
id form the requirement names, confirmed live in
`crates/trusty-common/src/inference/bedrock/tests.rs:1515,1561` doc comments:
"`AWS_PROFILE=cto`) and a reachable `us.anthropic.claude-*` inference
profile").

`cto-assistant/agent.toml` today declares `model = "claude-sonnet-4-6"` with
**no `bedrock/` prefix and no `[agent].provider_id`** — it is not pinned, so
`pick_credentials()` (`crates/trusty-agents/src/llm/credentials.rs:108-129`)
resolves the ambient credential in its own three-tier order (ClaudeCode >
AnthropicDirect > OpenRouter) — Bedrock is never a fallback in that
function; it is reachable only via an explicit pin or `/provider bedrock` in
the REPL. `~/.aws/config` has **no profile literally named `duetto`** — the
closest is `[profile cto]` (`region = us-east-1`, part of an `sso-session 1m`
block), already the profile other Duetto-facing tools reference by
convention (per the Bedrock module's own doc comment, "`AWS_PROFILE=cto`
works with zero code changes"). AWS profile selection is a **process-global
env var** (`AWS_PROFILE`), not a per-agent config field — no code path lets
`agent.toml` name a profile independent of the process environment.

**What is missing.**
- **Item R4-a (S, operator config):** set `[agent].provider_id = "bedrock"`
  and `model = "us.anthropic.claude-sonnet-4-6"` (or leave `model` and let
  `pinned_model_slug` rewrite it — verify which is required) in
  `cto-assistant/agent.toml`. No code change; the pin mechanism already
  exists and Bedrock is in `PINNABLE`.
- **Item R4-b (S, operator/ops, no trusty-agents code):** run the
  trusty-agents process (or at minimum the cto-assistant instance, if/when
  per-agent process isolation exists — none was found this session) with
  `AWS_PROFILE=cto` in its environment, since that is the actual profile
  name on this machine and there is no per-agent override. Clarify with the
  owner whether "the duetto AWS profile" names a profile that should exist
  under a literal `duetto` name (rename/alias `cto` or add a new one) or
  whether `cto` was always meant.
- **Item R4-c (M, trusty-agents):** the duplicate Bedrock client
  (`crates/trusty-agents/src/llm/bedrock/`) versus
  `trusty_common::inference::bedrock::BedrockAdapter` is a genuine
  common-entry-point violation per CLAUDE.md's domain-consolidation rule —
  not blocking R4's base case (the duplicate is functional and already
  partly reuses the common conversion helpers), but worth scheduling
  separately. No open issue was found naming this consolidation.
- **Item R4-d (S, verification):** no issue or config currently proves a
  live Bedrock round trip for any assistant persona under `AWS_PROFILE=cto`
  in this session's search; #530 (Bedrock for trusty-analyze, CLOSED) and
  #6882 (Bedrock PoC for trusty-mpm, CLOSED) are precedent elsewhere in the
  workspace but neither exercises `trusty-agents`'s dispatch path. Treat
  R4-a+b as needing a live smoke test before calling R4 done.

No open GitHub issue was found (searched `bedrock duetto`, `AWS_PROFILE
bedrock`, `provider_id bedrock`) that tracks "route an assistant persona
through Bedrock" as a scoped work item — R4 is presently pure configuration
plus a verification step, not backlog work, aside from R4-c's cleanup.

## Suggested ordering

1. **R1-a** — create the `cto-assistant` trusty-search index over
   `.../cto/projects` (S, unblocks R1 base case today, no dependency).
2. **R4-a + R4-b + R4-d** — pin cto-assistant to Bedrock, confirm
   `AWS_PROFILE`, smoke-test (S+S+S, no dependency on anything else here).
3. **R2-d** — grant the `channel` tool (S, no dependency).
4. **R2-c** — bind Slack for cto-assistant (S) — do after checking #7609's
   storage-migration state, since the binding's on-disk shape is mid-
   migration.
5. **R2-e** — live Slack in/out verification for cto-assistant (M,
   depends on R2-c/R2-d).
6. **R3-c** then, if still needed, **R3-b** — confirm ADR-0024's skills
   scope, then open a skills-floor issue only if the ADR does not already
   cover it (S then M).
7. **R1-b/#7434 -> R1-c/#7429 -> #7611** — durable multi-root/Projects
   model; independent timeline, not required for base case.
8. **R4-c** — Bedrock client consolidation; independent cleanup, not
   required for base case.
9. **R2-b, R2-f** — gworkspace live verification and Notion connector;
   lowest priority for "soon."

Items 1-3 are same-day operator/config actions with no code changes. Item 4
needs one agent.toml/API call. Item 5 is the only genuine "prove it works"
item blocking a true base case for R2.

## Improvement recommendations

**Symptom:** `cto-assistant/agent.toml` names a trusty-search index
(`index = "cto-assistant"`) that has never existed on this machine, and the
store degrades silently (fail-soft `connected: false`) rather than surfacing
anywhere the owner would see it day to day.
**Cause:** `stores/status.rs` treats a missing index as a normal degraded
state (by design, so a index can warm-boot), with no periodic or startup
surfacing beyond an on-demand status probe.
**Change:** consider a one-time boot-time warning (not a hard failure) when
a declared `[[stores]]` index has never been registered, distinct from
"index exists but is still indexing."
**Evidence:** `crates/trusty-agents/src/stores/status.rs:290`; live
`mcp__trusty-search__list_indexes` shows no `cto-assistant` index; the
persona has presumably been running in this degraded state since the store
binding was written (#4015/#4325 era, pre-2026-08).

**Symptom:** three of the four requirements in this task (R1, R2, R3) turned
out to be either fully or mostly shipped in code already (per-assistant MCP
overrides #7454/#7452, subagent floor ADR-0024 decision 4, Slack two-way
plumbing #7427) but not yet exercised as live configuration for this specific
assistant instance.
**Cause:** epic #7425's children close on "config-level verification," which
proves the mechanism works in the abstract, not that any particular
assistant instance uses it.
**Change:** when a platform-capability issue closes, consider a companion
checklist item "at least one live assistant instance is bound to this,"
so a capability's existence and its actual use for the flagship persona
(cto-assistant) do not drift apart the way this session found them to.
**Evidence:** #7427's closing comment explicitly defers six credential-
bearing checks with "no follow-up filed"; this session found zero channel
bindings and zero `[mcp]`/`[projects]` overrides for cto-assistant despite
the underlying mechanisms all existing.

🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools
