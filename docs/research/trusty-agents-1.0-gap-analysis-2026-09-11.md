# trusty-agents 1.0 Gap Analysis (2026-09-11)

Scope: `crates/trusty-agents`, `crates/trusty-agents-common`, `crates/trusty-agents-local`.
Supporting crates (out of scope for changes): `crates/trusty-kb`, `crates/trusty-channels`,
`crates/trusty-gworkspace`.

## Baseline facts

- Crate versions: `trusty-agents` 0.39.1, `trusty-agents-common` 0.7.2,
  `trusty-agents-local` 0.1.4 (all in `crates/<name>/Cargo.toml`).
- `trusty-agents-local` (`crates/trusty-agents-local/src/main.rs`) is a 15-line
  pass-through binary: `fn main() { trusty_agents::run_to_completion() }`. Its
  Cargo.toml doc comment records that #3732 removed its only reason to exist
  (a plugin-install edge for CTO-DB tools). No crate depends on
  `trusty-agents-local` (`grep -rl trusty-agents-local crates/*/Cargo.toml`
  returns only its own manifest) — it is a leaf, installed as a second binary
  alongside `tagent`.
- `trusty-agents-common` is consumed by `trusty-agents`, `trusty-code`,
  `trusty-kb`, and `trusty-mpm` (`Cargo.toml` grep).
- `trusty-kb` is consumed only by `trusty-agents`. `trusty-channels` is consumed
  by `trusty-agents`, `trusty-code`, `trusty-gworkspace`. `trusty-gworkspace` is
  consumed by `tc-services`, `trusty-agents`, `trusty-common`, `trusty-memory`.
- GitHub milestones relevant here: `PAUSED · agents` (#53, 17 open),
  `Backlog · agents` (#73, 37 open). No dedicated "1.0" milestone exists yet.

## Recommended ordering (dependency-driven)

1. **(a) Crate consolidation** first — it is nearly free (issue #7359's
   assessment checklist is already done) and every other item's PR surface
   (config structs, API routes) is cleaner to land against a settled two-crate
   layout than a three-crate one mid-flight.
2. **(d) Per-assistant/per-thread projects** next — §4.7 of
   `agent-config-five-sections.md` already makes project attachment the
   *source* feed for the assistant OKG pipeline in (c), so (d)'s attachment
   API must exist before (c)'s "one index, multiple roots" can be built
   against real inputs.
3. **(c) Memory/search reconfiguration** — needs (d)'s project attachment
   model, and is the largest architectural change (single index with
   multiple roots contradicts the current two-index, fan-out design).
4. **(f) Knowledge-graph cleanup** — depends on (c) settling what "the index"
   even is; today's drawer/OKG separation is already directionally right
   (§6.1 below) and mostly needs enforcement/audit, not a new store.
5. **(b) Channels in/out (Slack/Telegram/Notion/gworkspace)** — largely
   independent of (c)/(d), but Notion has no connector at all yet and should
   ride on whatever credential/store model (c) settles, per DOC-63 §7.1b.
6. **(e) Attachments in chat** — already has its own open issue (#7370) with
   an owner-accepted API+UI plan; can land in parallel with (b), gated only by
   (d) for the `<assistant>/attachments/<session>/` path convention, which
   already exists on disk (see item e below).

---

## a) Consolidate to 2 crates

**Current state**
- `crates/trusty-agents-local/src/main.rs:1-24` — literally
  `fn main() -> Result<()> { trusty_agents::run_to_completion() }`, no other
  logic.
- `crates/trusty-agents-local/Cargo.toml` records (in-file comment) that this
  is a "Private launcher: thin pass-through to trusty-agents::run()",
  `publish = false`.
- `tagent` (in `trusty-agents`, per CLAUDE.md's abbreviation table) calls the
  same `run_to_completion()` entry point.

**Gap**: `trusty-agents-local` has no remaining behavioral reason to exist —
confirmed by its own doc comment, not inferred. Folding it away is a pure
build/packaging change (drop the crate, keep one binary target, or keep a
thin cargo alias) with an installer/compatibility migration cost that issue
#7359 explicitly scoped but has not yet decided in the affirmative.

**Spec coverage**: none of the six specs scoped for this analysis mention
crate topology; it is purely an owner directive, not documented anywhere in
`docs/specs/`.

**Existing issues**: **#7359** `chore(trusty-agents): assess the three-crate
boundary`, OPEN, milestone `Backlog · agents`, labels include
`status:in-progress`. All three checklist boxes are already checked
("Record each crate's current consumers...", "Decide whether to retain common
and retire the redundant local launcher...", "Record the recommendation...")
but the issue itself is still open — the recommendation exists somewhere
(comments/linked doc, not fetched here) but the retirement has not shipped.
This issue already IS the ticket for item (a); a PM should read its comments
rather than open a duplicate.

**Cross-crate risk**: low. Deleting `trusty-agents-local` touches only its own
`Cargo.toml`/`main.rs` plus the root workspace member glob and installer
scripts that reference the second binary name — grep for
`trusty-agents-local` in `scripts/` and installer manifests before removing.
`trusty-agents-common`'s public API is untouched.

---

## b) Channels: native in/out connectors (gworkspace, Slack, Telegram, Notion)

**Current state**
- `crates/trusty-channels/src/lib.rs:23-24` — only `pub mod slack;` and
  `pub mod telegram;`. No Notion or gworkspace module lives in this crate.
- `crates/trusty-agents/src/tools/channel.rs:1-64` — the `ChannelTool` MCP
  tool (`name() = "channel"`), actions `list`/`read`/`send`, backed by
  `crate::api::server::agent_channels::{read,messages,send}`.
- `crates/trusty-agents/src/tools/channel.rs:66-73` — the `context()` help
  string states outright: **"Automatic Slack updates require the configured
  bot listener and existing pairing and sender permissions; Telegram is
  send-only."** Telegram has no inbound path today.
- `crates/trusty-agents/src/slack/{mod.rs,pairing.rs,handlers.rs,tests.rs}` —
  Slack has both directions (pairing + inbound handlers + outbound send).
- gworkspace (Gmail/Drive/Calendar) is wired as an MCP endpoint
  (`gworkspace`, `enabled = true` per
  `agent-config-five-sections.md` §4.4) but through the generic MCP knowledge
  surface, not the `channel` tool's saved-destination model — it is a
  read/compose tool set, not a "bound channel with inbound listener."
- Notion: `grep -rn "notion" crates/trusty-agents/src crates/trusty-channels/src
  crates/trusty-kb/src` returns only comments about Slack/Notion/Granola being
  *untrusted-by-construction sources* for search results
  (`crates/trusty-agents/src/untrusted.rs:11`,
  `crates/trusty-agents/src/tools/memory/vector_search.rs:37,514`). There is
  no Notion client, connector module, or store type anywhere in the three
  in-scope crates.

**Gap**
- Notion has zero connector code — not even a read-only one. The nearest
  planned surface is an OKG *source* (read-only ingestion), not a two-way
  channel.
- Telegram is send-only by design comment; the owner's item (b) explicitly
  wants messages "IN and OUT" for every listed channel.
- gworkspace is not modeled as a `channel` binding (list/read/send), so it is
  inconsistent with how Slack/Telegram are exposed to the agent and to the
  chat UI's channel picker.

**Spec coverage**
- `DOC-60-bus-based-agent-messaging.md` (not read in full here — 934 lines;
  scoped by title to the messaging bus, not connector build-out) and
  `agent-config-five-sections.md` §4.4's K-c MCP table are the closest specs;
  neither states a target of two-way Slack/Telegram/Notion/gworkspace parity.
- DOC-63 §7.1b (`docs/specs/DOC-63-okg-sources.md:914`) treats Gmail/Drive/
  Slack/Calendar explicitly as **read-only OKG source kinds**, not send
  channels: *"Gmail, Google Drive, Slack, and Google Calendar are channel
  source kinds"* (§4.7 of `agent-config-five-sections.md`, echoing DOC-63).
  This is a real disagreement with item (b): the current spec line treats
  these as one-directional ingestion sources feeding the OKG pipeline, while
  the owner's item (b) wants bidirectional messaging channels. The two models
  (channel-as-source vs. channel-as-conversation-endpoint) are not reconciled
  in either spec.

**Existing issues**
- **#2811** `epic(trusty-agents): declarative channels over trusty-channels and
  SessionProxy` — CLOSED, milestone `1.3.8`. Delivered the `[channels]`
  manifest section (**#3212**, CLOSED) that `channel.rs` implements today.
- **#2641** `Telegram support: telegram module in trusty-channels` — CLOSED.
- **#2636** `EPIC: native Slack/Telegram MCP servers (chat-as-tools)` —
  CLOSED, `1.3.8`.
- **#4853** `Slack pairing is not persisted...` — CLOSED, milestone
  `trusty-agents 0.39 — internal beta`.
- **#7389** `chore: verify Trusty Slack and Notion connector read access` —
  OPEN, `Backlog · agents`. Read-only verification, not connector build-out.
- **#4547** `[BLOCKED on #4040] OKG store type: Notion directory` — OPEN,
  `PAUSED · agents`. This is the closest existing ticket to "Notion", and it
  is scoped as a read-only OKG source, not a chat channel. Its blocker
  (#4040, credential authority) is CLOSED, so #4547 is unblocked but still
  open.
- **#4546** `[BLOCKED on #4040] OKG store type: Slack — sent messages and
  channels` — OPEN, `PAUSED · agents`. Same shape, for Slack-as-source.
- No open issue proposes Telegram-inbound or a gworkspace `channel` binding.

**Cross-crate risk**: adding Notion means a new module in `trusty-channels`
(low risk, additive) plus a new `ChannelKind`/binding variant surfaced through
`trusty-agents::tools::channel` and `agent_channels` API — touches
`trusty-agents-common` only if channel binding types are promoted there for
reuse by `trusty-code` (which already depends on `trusty-channels` directly,
per `crates/trusty-code/Cargo.toml`). Telegram-inbound requires a new listener
path in `trusty-channels::telegram`, consumed by `trusty-agents`,
`trusty-code`, and `trusty-gworkspace` (all three depend on
`trusty-channels`) — a wire-format change there is a 3-consumer, rung-4 change
per CLAUDE.md's test ladder.

---

## c) Memory and search properly configured

**Current state**
- Memory: `crates/trusty-agents/src/memory/store.rs:20-37` defines
  `enum Segment { AgentMemory, CodeIndex, Context, Brief, History }` — palace
  segmentation today is by **content type**, not by assistant identity, and
  there is no per-assistant palace concept in this enum.
- `crates/trusty-agents/src/memory/trusty_backed.rs:70-266` — `TrustyBacked`
  holds one palace per `Segment` under a shared `data_root`
  (`palace_id_for(segment)`), confirming palaces are keyed by segment, not by
  assistant instance.
- Search: `crates/trusty-agents/src/agents/config.rs:315-450` — `ToolsConfig`
  carries `search_indexes: Option<Vec<String>>` and
  `enforce_search_indexes: Option<bool>`, resolved via
  `resolved_search_indexes()`. This is a **list of named indexes an agent may
  query**, not "one index per assistant with multiple roots."
- `DOC-63-okg-sources.md` §3.4 / §10.5
  (`docs/specs/DOC-63-okg-sources.md:1254-1313`) formalizes the opposite
  design from the owner's ask: *"S-14.3 Tiers are queried **concurrently**...
  there is **no fan-out** — a call reaches one index"* per tier, and
  §4.7 of `agent-config-five-sections.md` states plainly: **"The two indexes
  serve different purposes: a project's index covers its eligible current
  files; the Assistant's index covers its derived OKG business entities."**
  This is a direct, quotable disagreement with owner item (c), which wants
  **one** trusty-search index per assistant with the OKG tree as one root and
  each used project as another root — not two (or N) separate indexes reached
  by concurrent fan-out.

**Gap**
- No memory concept of "one palace per assistant, fan-out to other
  assistants' palaces if selected in settings" exists anywhere in
  `crates/trusty-agents/src/memory/`. `Segment` has no `Assistant(id)`
  variant, and there is no settings surface for cross-assistant palace
  selection.
- Search is architected as N separate indexes (project index(es) + the
  Assistant's own OKG index) queried via concurrent fan-out, not a single
  index with multiple roots. Multi-root single-index search would be a
  different trusty-search-side capability (out of scope for changes here, but
  the owner's ask implies trusty-search itself would need a multi-root index
  primitive that does not appear to exist — worth flagging to trusty-search's
  own backlog).

**Spec coverage**
- `agent-config-five-sections.md` §4.7 is the current normative design and
  actively contradicts the owner's single-index-multi-root model (quoted
  above).
- `DOC-63-okg-sources.md` §10.5 (`SPEC-OKGSRC-14~draft`) is the fan-out design
  that would need to be superseded, not extended, if item (c) is accepted
  verbatim.
- Memory's per-assistant-palace-with-fan-out model is not documented in any
  of the six scoped specs; `trusty-agents-product-spec.md` §SPEC-AGENTS-08
  (`docs/specs/trusty-agents-product-spec.md:385`) discusses memory
  classification but predates the Assistant-instance model entirely (no
  mention of "Assistant" as a first-class identity).

**Existing issues**
- **#4282** `Cap attached search indexes at 10 per assistant, and ship the
  selection GUI` — OPEN, `PAUSED · agents`. Closest existing ticket to a
  per-assistant index cap, but caps *multiple* indexes rather than unifying
  to one multi-root index.
- **#4283** `OKG entity extraction: pull entities from attached search
  indexes into the canonical per-assistant OKG store` — OPEN, `Backlog ·
  agents`.
- **#4531** `epic: OKG Sources — per-assistant knowledge sources, scheduled
  refresh, and the untrusted-content boundary` — OPEN, `Backlog · agents`.
  This is the active epic implementing §4.7 above; it inherits the
  two-index-plus-fan-out disagreement.
- **#4539** `OKG Sources: create and register the assistant store's own
  trusty-search index` — OPEN, `Backlog · agents` — literally creates a
  *second* index, the opposite of the owner's one-index model.
- **#4325** `Assistant store model: app-generated per-assistant home
  directory with explicit OKG entity extraction` — CLOSED, milestone `M2 —
  trusty-agents internal driver beta`. Delivered the `AssistantHome`
  scaffolding item (d)/(e) build on.
- No open issue proposes cross-assistant memory palace fan-out or a
  single-index multi-root search model. This is a genuine spec-and-backlog
  gap, not just an implementation gap.

**Cross-crate risk**: high if pursued as "one multi-root index." trusty-search
itself (out of scope crate, not one of the three named `trusty-kb`/
`trusty-channels`/`trusty-gworkspace` either — it's a fourth dependency) would
need a multi-root index primitive; `trusty-agents-common`'s public API is not
obviously implicated since index/palace types live in `trusty-agents` and
`trusty-common`/`trusty-memory`, not in `trusty-agents-common`.

---

## d) Projects configured per assistant or per chat thread

**Current state**
- `crates/trusty-agents/src/assistants/home.rs:75-104` (`AssistantHome`
  module doc) already establishes the target model: *"Sources are the union of
  registered project folders attached to the Assistant's chats"* (§4.7,
  `agent-config-five-sections.md:296-403`).
- `crates/trusty-agents/src/api/server/projects.rs:1-60` — existing
  `GET /api/projects`, `POST /api/projects`, `GET /api/projects/:name`
  handlers back a project navigator, but per the file's own doc comment this
  is a general project-registry surface (`ProjectRegistry`,
  `discover_active_projects`), not yet the per-assistant/per-thread
  attachment API that §4.7 specifies (`PUT
  /api/agents/{name}/knowledge/pipeline/projects`, "Revisioned per-chat
  registered project selection").
- UI: `crates/trusty-agents/ui/src/components/KnowledgeProjectSync.svelte`,
  `crates/trusty-agents/ui/src/lib/knowledgeProjects.ts`,
  `crates/trusty-agents/ui/src/lib/knowledgeProjectSync.ts` exist and (by
  name) implement project-to-knowledge syncing already.
- `InputArea.svelte:389` references a `chatFolderError` / "Remove attachment"
  affordance for a per-chat folder attachment, suggesting per-thread project
  attachment UI is at least partially built.

**Gap**: the `PUT .../pipeline/projects` endpoint §4.7 specifies is not
confirmed live in `projects.rs` (that file predates §4.7's revision and is
described as the general project listing/registration surface, #341/#405/
#407/#451/#465). Whether project paths are *auto-added to the assistant's
index* on attachment (owner's exact wording) versus requiring a separate
extraction trigger is unresolved without reading the pipeline route
implementation directly — flag as needing direct verification before
ticketing, since §4.7 text says "Registration alone never authorizes
extraction," which is a **narrower** behavior than the owner's "project paths
auto-added to the assistant's index."

**Spec coverage**: `agent-config-five-sections.md` §4.7 is the live, current
spec and is broadly aligned with item (d) — the disagreement is the one
sentence just quoted: *"Registration alone never authorizes extraction"*
versus the owner's "project paths auto-added to the assistant's index,"
which implies attachment should be sufficient by itself.

**Existing issues**
- **#4358** `feat(trusty-agents): add per-chat project workspaces` — OPEN,
  `Backlog · agents`. This is the primary ticket for item (d).
- **#4355** `Epic: Projects surface + tasks-by-assistant in trusty-agents
  GUI` — OPEN, `PAUSED · agents`.
- **#4289** `Index a new directory from the agent config UI, with an overlap
  guard against existing index roots` — OPEN, `Backlog · agents`.
- **#3899** `Platform: tagent-native sync of agent config to
  bobmatnyc/trusty-agents-agents monorepo` — OPEN, tangential.

**Cross-crate risk**: low-to-moderate. Project attachment lives entirely in
`trusty-agents`; touches `trusty-kb`'s ingest/registry API
(`crates/trusty-kb/src/okg/{registry.rs,ingest.rs}`) if project roots feed the
OKG pipeline, which is a single-consumer (`trusty-agents`) relationship today.

---

## e) Chat attachments (text/binary/CSV), shown as objects, saved under assistant home

**Current state**
- `crates/trusty-agents/src/assistants/home.rs:83-84,96` already defines and
  documents the exact path convention the owner asked for: *"Binary files
  attached to this instance's chat"* — `pub const ATTACHMENTS_DIR: &str =
  "attachments";"*, sibling to `OKG_DIR`, both under `AssistantHome`. The
  module doc explicitly says the layout matches `<assistant>/attachments/...`
  beside `<assistant>/okg` (`home.rs:17-21`).
- `grep -rln ATTACHMENTS_DIR crates/trusty-agents/src` shows it used in
  `home.rs`, `health.rs`, `mod.rs`, and `tests/home_tests.rs` — i.e., the
  directory constant exists and is health-checked, but is not yet referenced
  from any API route or UI component (no hits in `api/server/` or `ui/src/`).
- UI: `crates/trusty-agents/ui/src/components/InputArea.svelte` has drag/drop
  ("drop" comment at line 164) and a `chatFolderError` affordance, but that is
  the *project-folder* attachment from item (d), not binary file attachments
  with thumbnails.

**Gap**: the on-disk convention item (e) asks for already exists
(`AssistantHome::ATTACHMENTS_DIR`), but nothing populates it yet, and there is
no thumbnail/click-expand UI, no CSV/XLSX parsing, and no persisted
association between an attachment and a chat message.

**Spec coverage**: none of the six scoped specs describe attachment UI or the
CSV/binary parsing pipeline. `home.rs`'s doc comments are the closest thing to
a spec and are already aligned with the owner's exact path structure.

**Existing issues**
- **#7370** `feat(trusty-agents): paste images and tables into chat` — OPEN,
  `Backlog · agents`, created 2026-09-10 (one day before this analysis).
  Its checklist is essentially item (e) verbatim: "raw images, CSV/XLSX files,
  and pasted HTML/TSV through an API with authoritative bounded validation,"
  "readable removable previews," "persist attachments with conversation
  history, expose authorized asset retrieval." This issue **is** the ticket
  for item (e); no new issue is needed, only confirmation the
  `<assistant>/attachments/<session>/<file>` path from `home.rs` is the
  target persistence layout in its implementation.
- **#4358** (per-chat project workspaces) is explicitly noted in #7370's body
  as owning "project browsing," distinguishing it from file attachments.

**Cross-crate risk**: low. Entirely within `trusty-agents` (API + UI); no
changes to `trusty-agents-common`, `trusty-kb`, `trusty-channels`, or
`trusty-gworkspace` anticipated.

---

## f) Clean up the knowledge graph: exclude memory drawers, expose only OKG (triples + definitions)

**Current state**
- `DOC-63-okg-sources.md:774` (table row) already records the separation as
  **BUILT**: *"Prompt-level fencing — recalled content is delimited and
  preambled as DATA, not instruction | BUILT, for memory drawers |
  `crates/trusty-agents/src/ctrl/pm_task/dispatch/persona_memory.rs:420-533`;
  `UNTRUSTED_PREAMBLE` at `:502`... 'drawer content is UNTRUSTED. It arrives
  from Gmail/Drive ingestion'"*.
- `DOC-63-okg-sources.md:783-786`: *"memory-drawer path, not the search
  path... assistant's memory drawers are fenced; the OKG store this document
  fills from"* — i.e., the spec already models memory drawers and the OKG
  store as two separate things.
- `crates/trusty-kb/src/okg/{docstore.rs,ledger.rs,trust.rs,ingest.rs,
  registry.rs,jsonl.rs}` is the OKG-store implementation — triples/definitions
  live here, structurally separate from `trusty-agents/src/memory/` (palace
  drawers).
- UI: `crates/trusty-agents/ui/src/components/KnowledgeGraphBrowser.svelte`
  exists as a dedicated OKG viewer, separate from any memory/drawer viewer
  component (no drawer references found in that file:
  `grep -n "drawer" KnowledgeGraphBrowser.svelte` returns nothing).
- **#4406** `[trusty-agents] Two unconnected systems are both called "OKG" —
  trusty-kb entity tree vs trusty-memory KG` — CLOSED, milestone `trusty-agents
  0.42 — Knowledge (OKG)`. This issue's title names exactly the confusion item
  (f) warns against, and it is already closed/resolved.

**Gap**: structurally, the separation the owner wants (exposed graph = OKG
triples/definitions only, memory drawers excluded) already exists in both the
data model (`trusty-kb::okg` vs `trusty-agents::memory`) and the UI
(`KnowledgeGraphBrowser.svelte` has no drawer content). The residual gap is
**audit/enforcement**, not architecture: confirm no code path feeds drawer
content into `KnowledgeGraphBrowser`'s API response, and confirm the "OKG"
naming is now unambiguous across UI copy, API field names, and MCP tool
descriptions (the historical confusion #4406 fixed could recur if a new
surface reintroduces a "knowledge" label that mixes both).

**Spec coverage**: `DOC-63-okg-sources.md` is well-aligned with item (f); no
quoted disagreement found. `DOC-58-knowledge-kd-attached-indexes.md` (not
fully read — 317 lines, title suggests it covers attached-index knowledge
surfaces) should be checked for any residual "knowledge" terminology that
blends drawers and OKG before closing this item out.

**Existing issues**
- **#4406** (above) — CLOSED, the historical fix.
- **#4363** `[trusty-agents] Extract/update-entities button: wire OKG tools to
  UI trigger` — OPEN, `PAUSED · agents` — UI-side follow-up, still relevant to
  confirming the KG browser only shows OKG content.
- **#4990** `roadmap: adopt four design ideas from omnigraph.dev for
  memory-core KG and search fusion` — OPEN, `Backlog · memory (triaged)` —
  touches trusty-memory's own KG, worth checking it does not reintroduce a
  merged surface.
- No open issue proposes a fresh audit of drawer/OKG separation as item (f)
  implies is still needed; recommend a small verification-only ticket rather
  than a new epic.

**Cross-crate risk**: low. Verification-only; if a leak is found, the fix is
localized to whichever API handler feeds `KnowledgeGraphBrowser.svelte` or to
`persona_memory.rs`'s fencing boundary.

---

## Summary table

| Item | Code exists today | Spec alignment | Open issue(s) | Effort class |
|---|---|---|---|---|
| a) 2-crate consolidation | `trusty-agents-local` is a dead pass-through | No spec; owner directive only | #7359 (assessment done, action pending) | Small, mechanical |
| b) Channels in/out | Slack (in+out), Telegram (out only), gworkspace (read/compose, not a bound channel), Notion (none) | DOC-63/§4.7 model these as one-way OKG sources, not 2-way channels — contradicts item (b) | #4547, #4546 (Notion/Slack as sources), #7389 (verify access) — none for Telegram-inbound or gworkspace-as-channel | Large |
| c) Memory/search per-assistant | Memory palaces keyed by `Segment`, not assistant; search is N named indexes, not 1 multi-root index | §4.7 explicitly designs 2 separate indexes + fan-out — contradicts item (c) | #4282, #4283, #4531, #4539 (build the 2-index model) | Large, needs spec revision first |
| d) Per-assistant/per-thread projects | `AssistantHome` + `KnowledgeProjectSync.svelte` scaffolding exists; pipeline route status unconfirmed | §4.7 mostly aligned; "registration alone never authorizes extraction" is narrower than owner's "auto-added" | #4358, #4355, #4289 | Medium |
| e) Chat attachments | `ATTACHMENTS_DIR` constant + path convention already correct; no upload/thumbnail/persistence code | No spec; `home.rs` doc comments match owner's path exactly | #7370 (already scoped to spec) | Medium, well-scoped |
| f) KG cleanup | Structurally already separated (`trusty-kb::okg` vs `trusty-agents::memory`); UI has no drawer leakage found | DOC-63 aligned | #4406 (closed, historical), #4363 | Small, audit-only |

## Notes on data not verified

- Did not read `DOC-58-knowledge-kd-attached-indexes.md` or `DOC-60-bus-based-
  agent-messaging.md` in full (317 and 934 lines respectively) — only grepped
  for `drawer`. A full pass may surface additional disagreements, especially
  in DOC-60 for item (b)'s bus-based message routing.
- Did not fetch #7359's issue comments (only the issue body), which likely
  contain the actual crate-consolidation recommendation referenced by its
  checked-off checklist.
- Did not verify live behavior of `PUT /api/agents/{name}/knowledge/pipeline/
  projects` (§4.7) against `projects.rs` — recommend a direct code read before
  ticketing item (d) to confirm current vs. spec'd behavior.
