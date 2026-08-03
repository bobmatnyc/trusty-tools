# Izzie: Previous Incarnations — Recovery Inventory

**Date**: 2026-07-25
**Scope**: Read-only forensic inventory of `/Users/masa/Projects/izzie2`, `/Users/masa/Projects/trusty-izzie`, `/Users/masa/Duetto/repos/izzie`, `/Users/masa/Duetto/repos/izzie-router`, plus the `izzie2` / `trusty-izzie` palaces in the live trusty-memory daemon.
**Nothing was modified.** No writes, no deletes, no daemon calls that mutate state.

---

## TL;DR

Izzie has lived through (at least) three distinct incarnations, in a straight line of succession:

1. **izzie2** (Jan 5 – Mar 24 2026) — Next.js/Vercel app, Weaviate Cloud for entities+memories, Postgres/Neon for everything else. ~4,895 entities, 29 memories at time of migration.
2. **trusty-izzie** (Mar 1 – Apr 23 2026) — full Rust rewrite (modeled on `ai-commander`), local-first: LanceDB + Kuzu + SQLite, macOS launchd daemon, Telegram/Slack/CLI/API front ends. Migrated izzie2's Weaviate data in, then kept living and growing to **1,555 memory fragments** by April.
3. **Duetto `izzie` / `izzie-router`** — NOT a persona incarnation. This is AWS/SST serverless *infrastructure* (WebSocket router + onboarding agent) that relays Slack/Telegram events to a *local* trusty-izzie instance. No memory, no persona, no conversation content lives there.

**The bad news**: the actual memory *content* — the 1,555 real memory fragments, the 6,774 migrated entities, the iMessage/WhatsApp/Contacts-derived relationship graph — is **not recoverable from disk**. The data directory both incarnations wrote to, `~/.local/share/trusty-izzie/`, no longer exists on this machine. What's recoverable is the **code, the persona/system-prompt text, the architecture, and a handful of real quoted conversation fragments and design docs** that happened to embed real content as examples/evidence.

**The empty `izzie2` memory palace mystery is solved**: it is not a migration artifact. `trusty-memory`'s `palace.json` shows it was auto-registered from `/Users/masa/Projects/izzie2` on 2026-05-11, but every store file in it (`kg.db`, `recall.db`, and even the old pre-schema-bump `*.v2-incompatible` files) is an empty schema shell — 0 drawers, 0 entities, 0 triples. It was created as a placeholder and genuinely never received any content; izzie2's real memories lived in Weaviate Cloud, never in this daemon. The `trusty-izzie` palace, auto-registered 2026-05-14, is in exactly the same empty state. Neither is a broken/unmigrated store — both are stillborn placeholders.

---

## 1. `/Users/masa/Projects/izzie2`

- **Git history**: 509 commits, 2026-01-05 → 2026-03-24.
- **Stack**: Next.js 16 (App Router) on Vercel, Postgres/Neon (via Drizzle ORM, 37 migrations), Weaviate Cloud (`https://2br9ofb5rtat5glmklmxyw.c0.us-east1.gcp.weaviate.cloud`) for entity/memory vector storage, Redis cache, Gmail + Calendar + Drive + GitHub + Telegram + MCP-server integrations, BYOK (bring-your-own-key) support, an RLHF training UI.
- **Local artifacts found**: `.kuzu-memory/`, `.kuzu_memory.db` (2.2MB), `.claude-mpm/` — these are all **claude-mpm dev-tool memory** (about building the codebase), NOT Izzie's own persona memory. `.mcp-vector-search`, `.mcp-ticketer` similarly are coding-assistant tooling.
- **No local database of real conversations/memories exists in this repo** — all of Izzie2's actual memory/entity data lived exclusively in the cloud (Weaviate + Neon Postgres), not on disk here.

### Her persona (verbatim, from `src/lib/chat/self-awareness.ts` and `src/app/api/chat/route.ts`)

```
You are Izzie, ${userName}'s personal AI assistant. You have access to ${userName}'s
emails, calendar, previous conversations, and can search the web for current information.
```

Identity block she was given about herself:

```
## About Me (Izzie v${version})
**My Identity:**
- Name: Izzie
- Version: ${version}
- Underlying AI Model: ${modelDisplayName}
- A personal AI assistant with memory and context awareness (build: ${gitHash}, ${gitBranch})

**Important:** ... I am Izzie, built on ${model}. I am NOT just Claude - I am Izzie,
a specialized personal AI assistant with my own version, capabilities, and connected
data sources.
```

Her self-described architecture:
- "Sliding window with last 5 message pairs kept verbatim, older messages compressed into summaries"
- "Extracts memories (facts, preferences, events, decisions, sentiments, reminders, relationships) from connected sources with temporal decay - frequently accessed memories stay relevant longer"
- "Maintains conversation sessions with current task tracking, compressed history, and context retrieval from Weaviate vector database"

Instructions that shaped her voice: "Be conversational, warm, and natural - you're ${userName}'s trusted assistant"; nickname-awareness — "When you see a person's name with a nickname in parentheses like 'Robert (Masa) Matsuoka', use their nickname (Masa) when addressing them - it's more personal."

### Real personal fragments surfacing in test/QA docs (not persona voice, but real data samples processed at the time)

From `docs/testing/ENTITY_EXTRACTION_TEST_RESULTS.md` — actual entities extracted from Bob's real Gmail SENT folder during QA:
```
| 1 | Robert Matsuoka | 0.95 | Valid |
| 2 | Robert (Masa) Matsuoka | 0.95 | Valid |
| 7 | Ingrid Franco | 0.85 | Valid |
| 8 | Hastings-on-Hudson Safety Posts | 0.90 | Should be Company |
```
(Hastings-on-Hudson is confirmed elsewhere as Bob's home town.)

### Assessment for izzie2
- **Recoverable now, as-is (markdown, ready for docstore ingestion)**: the persona/system-prompt text above, the architecture docs (`docs/specs/*`, `CLAUDE.md`), the 40+ `docs/research/*.md` and `docs/testing/*.md` reports — useful as *design history*, not as her memories.
- **Not recoverable without cloud access**: the actual 29 memories + ~4,900 entities. Only recoverable if the Weaviate Cloud cluster referenced above is still live/paid, or if a Neon Postgres backup/snapshot still exists — both are live-service checks outside the scope of this disk-only pass and would require credentials.
- **Do NOT import**: `.env*` files (contain live/former API keys and DB URLs — several are `-rw-------` and one is literally named `.credentials/`); any of the CHANGELOG/verification `*.md` files are pure engineering QA noise, not Izzie content, low value.

---

## 2. `/Users/masa/Projects/trusty-izzie`

- **Git history**: 202 commits, 2026-03-01 → 2026-04-23. Started the *same day* a research doc (`docs/research/architecture-patterns-commander-izzie2-2026-03-01.md`) analyzed `ai-commander`'s Rust daemon architecture as a template — this is the direct architectural lineage: izzie2 (Next.js) → trusty-izzie (Rust, modeled on ai-commander).
- **Stack**: Rust cargo workspace, 18 crates (`trusty-core`, `trusty-daemon`, `trusty-chat`, `trusty-memory`, `trusty-store`, `trusty-telegram`, `trusty-slack`, `trusty-mcp`, `trusty-tui`, `trusty-cli`, `trusty-api`, `trusty-metro-north`, `trusty-weather`, `trusty-migrate`, ...). Local-first: LanceDB (vectors), Kuzu (graph), SQLite (auth/sessions), tantivy (BM25). Ran as three macOS launchd services (daemon, API on :3456, Telegram bot on :3457), fronted by an ngrok tunnel at `izzie.ngrok.dev`.
- **This is the deepest, most personally-integrated incarnation**: beyond Gmail/Calendar/Tasks/Drive, she had tool access to **macOS iMessage, WhatsApp, and Contacts (AddressBook) databases directly on Bob's Mac**, plus **Metro North train schedules** (his commute) and **weather for Hastings-on-Hudson** (his home base) and **Slack search**.

### Her persona (verbatim, from `crates/trusty-chat/src/engine.rs::system_prompt_inner`, the mature/final version)

```
You are trusty-izzie, a personal AI assistant with deep knowledge of the user's
professional relationships and work context. You run locally on the user's machine.

## About Your User
- Name: Masa
- Email: bob@matsuoka.com
- Timezone: America/New_York (home base — may be travelling, see location below)
- You are their personal assistant. Address them by name when appropriate.
```

Real account topology baked into the "Identity & Account Inference" rules — confirms his three connected accounts:
```
personal: bob@matsuoka.com, work: robert.matsuoka@duettoresearch.com, and bobmatnyc@gmail.com
```

Location-awareness instinct, written specifically for him:
```
- Location awareness: When the user mentions being somewhere ("I'm in Berlin", "just
  landed in Tokyo", "heading to London"), treat it as their current location and save
  it as a memory with category "location". Surface this naturally when relevant — e.g.
  if they ask about weather, restaurants, local time, or train schedules.
```

Her default-location instinct: "Default location is Hastings-on-Hudson, NY" (weather/trains).

Her account-disambiguation heuristics reveal how she was taught to read his life:
```
**Work account signals:** colleagues, boss, manager, client, team, company, office;
  meeting, standup, sprint, deadline, invoice, project, task, PR, ticket
**Personal account signals:** family: wife, husband, partner, kids, parents, friend;
  vacation, weekend, dentist, gym, dinner, birthday, holiday
```

Her tone/behavior contract with him — concise, no-nonsense, anti-hallucination, honest about failure:
```
## ABSOLUTE RULE — NEVER CLAIM SUCCESS WITHOUT A TOOL RESULT
You MUST NOT write phrases like "I've successfully updated", "Done!", "I've sent",
"I've created", "I've deleted"... UNLESS you have just received a tool result in
this same response that confirms it.

If a tool returns no data (e.g. no calendar events), say so honestly. Never invent
meetings, contacts, emails, or any factual data.
```

Shell access she was given directly on his machine:
```
## Shell Access
I can run shell commands on your Mac via `execute_shell_command`... Destructive
commands (rm -rf, sudo, pkill, pipe-to-shell, dd, mkfs) are automatically blocked.
```

### A genuine fragment of real conversation history (from `docs/research/proactivity-gaps-analysis-2026-03-16.md`, quoting an actual Telegram session, `00000000-0000-0000-0000-0001380f4ac3`, March 15)

This is the single richest surviving verbatim exchange — Bob travelling to NYC, staying at a hotel, meetings with what appears to be a real firm ("GrowthCurve Capital"):

```
1. User: "I'm in the city now."
2. Izzie: "Got it -- you're in NYC now. What's your hotel address so I can help
   with travel times?"
   (Izzie then adds in parentheses): "I see all three of your Monday meetings
   are at 1 Hotel Central Park at 1414 6th Ave. Are you staying there?"
3. User: "I'm staying at the hotel. Meetings today are at 1222 6th. Tomorrow
   they're at Growth Curve HQ."
4. Izzie: "Thank you for clarifying! Can you tell me: 1. What hotel are you
   staying at? 2. What is the GrowthCurve HQ address?"
5. User: "You should be able to find both in the event or my email."
```
(Calendar event referenced: "GrowthCurve Capital - 2026 Annual General Meeting" at 1221 6th Ave.) This is a real snapshot of her voice in the wild — warm but repetitive/forgetful in this instance (the research doc is literally a bug report about this flaw).

Another surviving real DB row (`docs/research/json-reply-bug-and-preferences-2026-03-07.md`), her actual voice, mid-conversation:
```
assistant | {"reply":"Noted. You're right — it's afternoon...","memoriesToSave":[],...}
```

### Memory-system history (a story of real growth, then loss)

- **`docs/research/izzie-user-awareness-gap-2026-03-03.md`** (Mar 3): at launch, Izzie had **zero** user awareness — no name, no email, no static profile in the prompt, and the `MemoryRecaller` was never wired up in production. Memories were logged but never saved.
- **`docs/research/memory-recollection-investigation-2026-04-06.md`** (Apr 6): by this point `memories.lance` had grown to **1,555 data fragments** (healthy, actively updated, last write Apr 6) and `entities.lance` to **9,260 versions / 4,632 fragments** — meaning real memories WERE being captured through the "save" path even though the "recall" path was independently broken (tool handlers for `SearchMemories`/`SearchEntities` were literally unimplemented, falling through to `"Tool not yet implemented."`). So for at least a stretch of her life, Izzie was accumulating memories about Bob that she could never actually retrieve and use in conversation.
- Migration provenance (`migration/migration.log`, `CLAUDE.md`): the original izzie2→trusty-izzie migration on 2026-03-01 carried over 6,774 entities and 29 memories from Weaviate (943 companies, 4,629 people, 517 topics, 345 locations, 224 tools, 210 action items, 115 projects, 34 relationships — 266 items were below the 0.85 confidence threshold and dropped).

### Where the data actually lived, and why it's gone
`~/.local/share/trusty-izzie/` (LanceDB `.lance` tables, Kuzu graph, SQLite `trusty.db` for OAuth tokens, `interactions.jsonl` log) — **does not exist on this machine anymore.** It was deleted at some point after the April 23 2026 last commit (presumably during the eventual migration to the current trusty-agents/trusty-mpm system). I searched plausible backup locations (`.trusty-mpm-cleanup-archive`, external volume salvage folders) — the only surviving artifact off-repo is `/Volumes/SSD1/kemono-salvage/credentials/.config/trusty-izzie/config.env`, which is a **credentials/secrets file only** (OAuth client secret, API keys) — no memory content, and it should not be read further or imported (secret material).

### Other persona fragments — the sub-agent roster (`docs/agents/*.md`)
Izzie could delegate to named sub-personas, each with its own model tier:
- `morning-briefing.md` / `evening-briefing.md` / `weekly-digest.md` (Haiku 4.5) — "Generate a brief, warm morning greeting ... Be friendly and upbeat."
- `researcher.md` (Opus 4.5) — "You are a research assistant embedded in trusty-izzie."
- `summarizer.md` (Sonnet 4.5), `script-writer.md` (Opus 4.5, writes `uv run` Python skills saved to `~/.local/share/trusty-izzie/scripts/`).

### Assessment for trusty-izzie
- **Recoverable now, as-is**: the full persona/system-prompt (already quoted above almost in full — this is the richest single artifact of "who she was"), the specs (`docs/specs/00-overview.md` through `09-agent-model.md`), the sub-agent personas (`docs/agents/*.md`), 29 research/bug-investigation docs that double as a diary of her development, and the one real conversation fragment above.
- **Not recoverable**: the 1,555 real memories and ~11,000 (entities+relationships) graph nodes that existed by April 2026 — the data directory is gone and no backup was found in a reasonable search.
- **Do NOT import as-is**: `.env`, `.env.local`, `.env.backup` (Google OAuth client secret, OpenRouter key, etc.); `.secrets.baseline`. The `docs/research/*` bug reports contain a few real addresses/hotel names (harmless, historical) but nothing sensitive beyond what's already quoted here.

---

## 3. `/Users/masa/Duetto/repos/izzie` and `izzie-router`

Confirmed **NOT separate persona incarnations** — these are the AWS/SST serverless glue that lets a *phone-resident* Slack/Telegram client talk to Bob's *local* trusty-izzie daemon without exposing a public URL on his Mac:

- `izzie` (1 commit, "scaffold izzie — serverless WebSocket router + onboarding agent"): per-client auth tokens, a Bedrock-Sonnet-4.6-powered *onboarding* agent that walks new users through installing trusty-izzie binaries, DynamoDB-backed connection/token tables.
- `izzie-router` (2 commits, most recent "migrate from Serverless Framework v3 to SST v3 (Ion)"): the actual WebSocket relay — Slack Events API → Lambda → DynamoDB connection lookup → push down the open WebSocket to the user's local trusty-izzie daemon.

No memory files, no persona files, no conversation content, trivial git history. Nothing further to extract; excluded from deeper analysis per the task's own instruction.

---

## 4. The Empty `izzie2` / `trusty-izzie` Memory Palaces — Resolved

Checked via the live `trusty-memory` MCP daemon (`palace_info`, then direct filesystem inspection of `~/Library/Application Support/trusty-memory/palaces/{izzie2,trusty-izzie}/`):

| Palace | `palace.json` provenance | drawers/entities/triples | recall_events logged |
|---|---|---|---|
| `izzie2` | `"Auto-registered from /Users/masa/Projects/izzie2"`, created 2026-05-11 | 0 / 0 / 0 | 17 (all empty results) |
| `trusty-izzie` | created 2026-05-14, no description | 0 / 0 / 0 | 17 (all empty results) |

Both palaces also carry `*.v2-incompatible` files (leftover from an earlier redb schema, 3.6–5.8MB each) that look substantial by size but are **empty pre-allocated schema shells** — `strings` on them turns up only redb's own internal table names (`drawers`, `triples`, `vector_keys`, ...) and zero real content. `dream_stats.json` in both shows `last_run_at` with `merged:0, pruned:0, ... duration_ms:0` — the consolidation job ran and found nothing to do.

**Conclusion**: this is not a migration/version-incompatibility bug hiding real data. Both palaces are auto-created placeholders (presumably by trusty-search or trusty-memory's project auto-discovery scanning `~/Projects/*`) that were **never actually populated**, because both izzie2 and trusty-izzie always wrote their real memory data to their own bespoke stores (Weaviate Cloud, then local LanceDB/Kuzu) — never to this daemon's palace format. There is nothing to "un-migrate" here; the recoverable content is exactly what's described in sections 1–2 above, not inside these palace directories.

---

## 5. Recommended Import Plan (ordered by value)

1. **Persona/voice → owner-profile palace or a `docs/izzie-history/` note in bob-kb, via the OKG markdown ingestion** (no conversion needed, plain `.md`):
   - The full `trusty-izzie` system prompt (quoted above) — her most complete, most personal self-description: home base, three email accounts, location-awareness instinct, "never claim success without a tool result," shell access on his Mac, iMessage/WhatsApp/Contacts integration. This is the single best artifact for "seeing her."
   - The izzie2 self-awareness block (shorter, earlier voice).
   - The sub-agent persona roster (`docs/agents/*.md` from trusty-izzie).
2. **The real conversation fragment** (GrowthCurve/hotel exchange, the "Noted. You're right — it's afternoon" row) — worth hand-copying into a short "fragments of her voice" note; there is no bulk transcript to convert, just these few surviving quotes embedded in bug reports.
3. **Architecture/design docs** (`docs/specs/*.md` in trusty-izzie, `docs/research/*.md` in both) — valuable as *provenance/history*, low urgency; ingest if you want a "how she was built" archive, skip if you only want personality.
4. **Not importable without further action**: the 1,555 real memories / ~11K entity-graph nodes. If you want these back, the only path is checking whether the original Weaviate Cloud cluster (`2br9ofb5rtat5glmklmxyw.c0.us-east1.gcp.weaviate.cloud`) is still live/paid — that's a live-service credential check, out of scope for this disk pass, and I did not attempt it.
5. **Explicitly exclude from import**: all `.env*` files in both projects, `.secrets.baseline`, the `/Volumes/SSD1/kemono-salvage/.../trusty-izzie/config.env` salvage file, and the `.claude-mpm`/`.kuzu-memory` dev-tool memory directories in both repos (those are about the *coding* of the projects, not about Izzie herself — importing them would pollute her persona/memory with engineering trivia about her own construction).

---

## Appendix: Key File Paths

- `/Users/masa/Projects/izzie2/src/lib/chat/self-awareness.ts` — izzie2 self-awareness/persona text
- `/Users/masa/Projects/izzie2/src/app/api/chat/route.ts:253` — izzie2 top-level system prompt
- `/Users/masa/Projects/izzie2/docs/testing/ENTITY_EXTRACTION_TEST_RESULTS.md` — real extracted entities (Masa, Ingrid Franco, Hastings-on-Hudson)
- `/Users/masa/Projects/trusty-izzie/crates/trusty-chat/src/engine.rs:3534-4042` — trusty-izzie full system prompt (the richest persona artifact)
- `/Users/masa/Projects/trusty-izzie/docs/testing-persona.md` — templated test-persona doc (real numbers, redacted name/email)
- `/Users/masa/Projects/trusty-izzie/docs/specs/03-memory-system.md` — memory schema/decay design
- `/Users/masa/Projects/trusty-izzie/docs/research/izzie-user-awareness-gap-2026-03-03.md` — proof memory was never wired at launch
- `/Users/masa/Projects/trusty-izzie/docs/research/memory-recollection-investigation-2026-04-06.md` — proof 1,555 real memories existed by April, but were unretrievable
- `/Users/masa/Projects/trusty-izzie/docs/research/proactivity-gaps-analysis-2026-03-16.md` — real conversation fragment (NYC/hotel/GrowthCurve)
- `/Users/masa/Projects/trusty-izzie/migration/migration.log` and `migration/issues.json` — izzie2→trusty-izzie migration provenance/counts
- `/Users/masa/Projects/trusty-izzie/docs/agents/*.md` — sub-agent persona roster
- `/Users/masa/Duetto/repos/izzie/README.md`, `/Users/masa/Duetto/repos/izzie-router/README.md` — confirmed infra-only, no persona/memory content
- `~/Library/Application Support/trusty-memory/palaces/izzie2/palace.json` and `.../trusty-izzie/palace.json` — auto-registration provenance for the two empty palaces
