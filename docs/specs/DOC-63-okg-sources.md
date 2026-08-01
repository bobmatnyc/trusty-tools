---
spec_refs:
  - id: SPEC-OKGIMPORT-04~draft
    path: docs/specs/okg-universal-importer.md
    anchor: SPEC-OKGIMPORT-04~draft
  - id: SPEC-AGENTCFG-03~draft
    path: docs/specs/agent-config-five-sections.md
    anchor: SPEC-AGENTCFG-03~draft
  - id: SPEC-KDIDX-01~draft
    path: docs/specs/DOC-58-knowledge-kd-attached-indexes.md
    anchor: SPEC-KDIDX-01~draft
---

# DOC-63 — OKG Sources: Per-Assistant Knowledge Sources, Scheduled Refresh, and the Untrusted-Content Boundary

**Status:** Draft
**Spec ID:** `SPEC-OKGSRC-01~draft` … `SPEC-OKGSRC-14~draft` (DOC-63)
**Subsystem:** trusty-agents — assistant home / OKG store, source catalog, scheduled refresh, credentials consumption, Knowledge config pane; trusty-kb — `okg` engine (ledger, registry, entity tree); trusty-search — index over the store
**Owner:** Engineering (trusty-agents) / Bob Matsuoka
**Last-updated:** 2026-08-01
**DOC-N claim:** `DOC-63`, scan-before-claim per DOC-38 §4.1. **This document originally claimed `DOC-62` and was renumbered.** Two concurrent spec passes both claimed DOC-62 — this one and PR #4529 (Style Modes for Coding Delegation) — because each correctly scanned `origin/main` and found `DOC-61` as the highest claimed number, and neither could see the other's unmerged branch. #4529 keeps `DOC-62` as the earlier and further-advanced PR; this is a mechanical tie-break. `DOC-63` was verified free three ways: no filename claim or header self-label under `docs/specs/**` or `docs/trusty-installer/research/02-design/**` on `origin/main`; no claim on any remote branch; and **no claim in any open PR** — the last check being precisely the gap that produced the original collision, since `scripts/check_doc_numbers.sh` scans the tree and cannot see an unmerged reservation (its own header states this limitation explicitly). Note also that `DOC-55` is claimed by `okg-universal-importer.md` via self-label only, its filename carrying no number, so a filename scan alone is insufficient in either direction.
**Builds on:** DOC-55 [Universal OKG Importer](./okg-universal-importer.md) `SPEC-OKGIMPORT-03~draft`/`-04~draft` (the extraction layer and the `Connector` contract this document does **not** re-specify); DOC-57 [Five-Section Agent Configuration](./agent-config-five-sections.md) `SPEC-AGENTCFG-03~draft` (the Knowledge section this document adds a sub-surface to) and `SPEC-AGENTCFG-05~draft` (Listeners, the existing poll machinery); DOC-58 [K-d Attached Search Indexes](./DOC-58-knowledge-kd-attached-indexes.md) `SPEC-KDIDX-01~draft` (the two-tier curated-vs-attached principle this document depends on)
**Related issues:** #4325 (per-assistant home directory — **in flight, PR #4523**); #3904 (epic: universal assistant-driven OKG importer); #4283 (index→OKG entity extraction); #4007 (epic: curated stores vs attached indexes); #4289 (index a new directory from the config UI); #4363 (extract-entities UI trigger); #4590 (concierge offers to build an assistant — the build path, §2.1a); #4591 (templates excluded from provisioning only incidentally); #4406 (which store is canonical — superseded in framing by §2 here); #4040 (epic: unified credential authority — this document is a **consumer**, never a parallel mechanism)

---

## 1. Executive Summary

**Each agent has exactly one canonical OKG** — an OKF store, internal, **built**,
and indexed by trusty-search — **populated by one or more document STORES**, each
either a pointer to a local directory or an extract from a connected system
(Gmail, Drive, Slack, Notion, Granola). That model, stated by the owner on
2026-08-01, is the spine of this document; §2.1 unpacks it and §3 specifies the
two store kinds.

This document specifies the store roster, the scheduled refresh, the credential
dependency, search tiering, the observability surface, and — most importantly —
**the boundary that keeps ingested third-party content from being read as
instruction**.

Three properties of the model do most of the work. **Built** means the OKG is a
*derived* artifact, so it is rebuildable and idempotency is a property of the
build rather than a nice-to-have (`S-1.13`). **Two store kinds** means the
local/remote distinction is the primary type distinction, not an implementation
detail: kind 1 materializes nothing, kind 2 gets an extraction target holding its
state (§2.4). And a store's two capacities — **searchable corpus** and **OKG
contributor** — are **orthogonal**: search-only attachment is a complete,
legitimate end state, never a pending one (§3.4).

Three framing statements govern everything below.

**First: this is a greenfield specification of an unbuilt feature, not an
extension of a working pipeline.** Verified against `origin/main` (4e67493b):
there is no CLI verb, no HTTP route, and no GUI trigger for any OKG operation —
only four `ToolExecutor`s (`crates/trusty-agents/src/tools/okg/mod.rs:57-64`),
reachable only if a model turn happens to call them. Index→OKG extraction
"exists in no form, built or specified" (#4283); the only code running in that
direction is `feed_bound_index`
(`crates/trusty-agents/src/tools/okg/index_feed.rs`), which pushes the tree OUT
to an index. Nothing in this document may be read as describing current
behavior. Where an existing mechanism is cited, it is cited as a **reusable
part**, not as a working feature.

**Second: "OKG" names two unconnected stores today, and the owner's model picks
one.** The canonical OKG is the OKF filesystem tree, **not** trusty-memory's KG.
§2.1 states it unambiguously so downstream tickets stop building against different
targets — the substance of #4406. It does **not** retire trusty-memory's KG, whose
relationship to the OKG is a separate and genuinely open question (§14 Q1).

**Third: an OKG store has two population paths, and they are specified
separately.** Per the owner (2026-08-01), *"extraction is an OPTION for any bound
store."* **Path 1 — OKG Sources** are configured pipelines over the seven kinds
in §4; they refresh **automatically** on a user-managed schedule. **Path 2 —
bound stores** are any index the assistant binds; pulling entities out of one is
an **option invoked by an explicit command**, never automatic. The paths differ
by source kind, not by processing stage, and §3 keeps them first-class and
distinct rather than unifying them behind a flag. The contamination guard belongs
to bound-store extraction alone (§4.2), and §3.3 specifies the deliberate overlap
case where one directory is both a store and a bindable index.

The genuinely new surface area here, relative to DOC-55, is: the store roster and
its per-store constraints (§5); the credential dependency on #4040 (§7);
**scheduled refresh, which does not exist in any form** (§9); **search tiering and
result provenance, which also do not exist** (§10.5–§10.6 — `vector_search`
queries exactly one index today); the untrusted-content boundary (§6); the
store-type registry as an open extension point (§11); and the Sources sub-surface
of the Knowledge pane (§12).

---

## 2. SPEC-OKGSRC-01 — The Store: Which "OKG", and Where {#SPEC-OKGSRC-01~draft}

### 2.1 The canonical model

**Owner statement (2026-08-01), the spine of this document:**

> *"Each agent has one canonical OKG which is an OKF store, internal, built,
> indexed by trusty-search. These are POPULATED by one or more document STORES,
> either a pointer to a local directory, or an extract from a connected system."*

```
        agent
          │
          ▼  exactly one
    ┌───────────────────────────────────────┐
    │  canonical OKG                        │
    │  · an OKF store        (format)       │
    │  · internal            (agent-owned)  │
    │  · BUILT               (derived)      │
    │  · indexed by trusty-search           │
    └───────────────────────────────────────┘
          ▲  populated by one or more
          │
    ┌─────┴─────────────────┬───────────────────────────┐
    │  STORE kind 1         │  STORE kind 2             │
    │  pointer to a local   │  extract from a connected │
    │  directory            │  system                   │
    │  (nothing materialized)│ (materialized target +   │
    │                       │   extraction state)       │
    └───────────────────────┴───────────────────────────┘
```

**S-1.0 — Cardinality.** Exactly **one** canonical OKG per agent. Not zero, not
many. It is populated by **one or more** stores. Store count varies; OKG count
does not.

#### The four properties, each load-bearing

- **An OKF store** — the format. OKF v0.1, whose defining rule is that `type` is
  the only required field (`crates/trusty-kb/src/schema.rs:324`).
- **Internal** — it belongs to the agent. It is **not** a user-managed corpus, and
  it is not something the user points at; the user points at *stores*, and the OKG
  is what results.
- **Built** — see below. This is the property most likely to be lost.
- **Indexed by trusty-search** — §9.

#### "Built" is the property that must not be lost {#SPEC-OKGSRC-12~draft}

**S-1.13** The OKG is a **derived artifact, not an authored one.** It is the build
product of its stores. Three consequences, all normative, none optional:

1. **Rebuildability.** Given the store set and the credentials, the OKG can be
   rebuilt. Nothing about it may be recoverable *only* from the tree — a
   hand-authored entity with no store behind it is outside the model. This is what
   ADR-0022 already assumes when it decides knowledge trees do not sync and a new
   machine **re-ingests from registered stores**.
2. **Idempotency is a property of the build, not a nice-to-have.** A build run
   twice produces the same OKG. This reframes §7 entirely: the ledger, the
   fingerprints, and the interval watermarks are not defensive plumbing bolted onto
   a fetcher — they are what makes "built" mean anything. A pipeline that
   duplicates on re-run has not built an artifact; it has appended to a pile.
3. **The store set is the source of truth, and the OKG is downstream of it.**
   Removing a store is a meaningful operation with a defined effect on the build
   (DOC-55's tombstoning), rather than an edit to a config that leaves orphaned
   content behind.

**S-1.14 — Corollary for the user-editable home.** #4325 makes hand-editing the
home expected, and S-1.13 does **not** override that: a user edit inside `okg/` is
tolerated, never clobbered by `ensure()`, and never treated as an error. What
S-1.13 forbids is the *system* treating hand-authored content as a first-class
input it depends on. Rebuild is a deliberate operation, not something a scheduled
run performs behind the user's back.

#### Which of the two "OKG" systems — settled

**Normative, and this closes #4406's question for this document's purposes.** The
canonical OKG is the **OKF store**: the trusty-kb-shaped filesystem tree written
by `KbStore::put_entity` (`crates/trusty-kb/src/okg/mod.rs`), internal, built, and
indexed by trusty-search. It is **not** trusty-memory's knowledge graph behind
`kg_assert` / `kg_query` (`crates/trusty-memory/src/service/core_kg.rs`).

The owner's own phrasing settles it independently: a trusty-search index is built
over a filesystem tree, and the trusty-memory KG is not a filesystem tree.

**S-1.15 — What this does NOT say about trusty-memory's KG.** Naming the OKF
store canonical **does not retire, absorb, or deprecate** the trusty-memory KG. It
is a separate, unconnected system — verified 2026-07-30 that nothing bridges the
two — and a prior owner decision established that every assistant gets **a memory
palace by default AND a default OKG**. Both therefore exist by design and this
document does not collapse them. What remains genuinely unresolved is the
KG's relationship to the OKG going forward, which is a **new** question (§14 Q2),
distinct from the one just answered.

**S-1.16** Every ticket descending from this document names the OKF store as its
target.

### 2.1a How an agent — and therefore its OKG — comes into existence

**Owner requirement:** *"the concierge will offer to build an assistant, or the
user can do it manually."*

**S-1.17 — Two creation paths, one result.** An assistant instance is created
either by **the concierge proactively offering to build one**, or **manually by
the user**. Both paths produce the same thing: a per-agent home containing the
canonical OKG (§2.1) and the `stores/` its OKG is populated from (§2.4). Nothing
downstream of creation distinguishes them — a concierge-built assistant and a
hand-built one are the same object, with the same layout, the same store roster,
and the same trust boundary.

**S-1.18 — The concierge flow is NOT specified here.** It is owned by **#4590**
("Concierge offers to build a personal assistant on first run"). This document's
only claim is that the OKG and its stores come into existence through that path;
the conversation, the prompting, and the first-run timing are #4590's.

#### Creation precedes provisioning — an ordering that is easy to invert

**S-1.19** The concierge (or the user) **creates** the assistant; PR #4523's
startup provisioning then **finds** it and ensures its home layout via `ensure()`.
Creation is logically first even though **provisioning is what runs first at
boot** — provisioning discovers instances that already exist and materializes
their directories; it never brings an assistant into being. Reading the order off
the boot sequence gets it backwards, and a design that has provisioning invent
instances would create homes and OKGs for agents nobody asked for.

**S-1.20 — Two agents that must never be provisioned an OKG.** Both are excluded
by PR #4523's `is_instance()` rule in `assistants/roster.rs`, which requires
`role = "assistant"` **and** either being the base `assistant` package or
declaring `extends = "assistant"`:

- **`ctrl` IS the concierge persona**, not an assistant instance. It declares
  `role = "assistant"` (a deliberate 2026-07-24 change so Concierge appears in
  role-filtered pickers) and `kind = "system-tool"`, but neither is nor extends
  the base. **The GUI's null `activeAgentId` means `ctrl`** — provisioning it a
  selectable instance home would model the one agent that is not an instance as
  if it were. It gets no OKG.
- **`personal-assistant` is a TEMPLATE, not an instance** (owner-confirmed). Its
  config declares `role = "assistant"` and `hidden = true` with **no `extends`
  field at all**, so it falls out of the rule — but only *incidentally*, because
  it happens to extend nothing rather than because anything marks it a template.
  **A template must never be provisioned a home or an OKG.** Making that explicit
  rather than incidental is **#4591**; this document states the requirement and
  leaves the mechanism there.

### 2.2 Where the store lives — settled

```
${TRUSTY_AGENTS_HOME} / <agent> / okg
${TRUSTY_AGENTS_HOME} / <agent> / stores / <store-identifier>
```

**The agent name sits DIRECTLY under the home. There is no intervening
`assistants/` segment.**

**S-1.2 — This is settled, not open.** An earlier revision of PR #4523 resolved
`~/trusty-agents/assistants/<instance>/okg` via an `ASSISTANTS_SUBDIR` constant,
which contradicted the owner's statement. **That is corrected**: #4523 now
resolves `~/trusty-agents/<instance>/` and its own module documentation records
*"no intervening `assistants/` segment"*, with both store paths taken from the
owner verbatim. This document and #4523 agree, and the question is closed.

For the record, because it explains the migration ahead: today's shipped path is
neither. `knowledge_dir()` (`crates/trusty-agents/src/tools/okg/mod.rs:92-106`)
resolves `$KB_KNOWLEDGE_DIR`, else `<home>/.trusty-agents/knowledge`, and every
tree is `<knowledge_dir>/<slug(agent)>` — a flat, dotted, shared pool addressed by
naming convention, with no `okg` segment at all. Moving to the layout above is a
migration of existing trees.

**S-1.2a** The layout obligations #4523 already implements are adopted unchanged:
the home is per **instance** (not per type), **dotless** and user-browsable, and
`ensure()` is additive — it creates only what is missing and **never overwrites a
file the user edited**, because #4325 makes external modification expected rather
than an error.

### 2.3 What the store contains

```
<assistant home>/
├── instructions.md
├── config.toml                       # TOML, settled (§2.6)
├── agents/
├── okg/                              # the OKF entity tree — one per assistant
│   ├── <collection>/…                # entity + document markdown
│   └── _sources/
│       ├── registry.toml             # SourceSpec rows (DOC-55 §2.1)
│       ├── <source-id>.jsonl         # append-only item ledger
│       ├── <source-id>.index.jsonl   # index-feed journal
│       └── <source-id>.runs.jsonl    # NEW — the extraction log (§12)
├── stores/                           # NEW — one per REMOTE store (§2.4)
│   └── <store-identifier>/
│       ├── state.toml                # extraction state: last run, coverage
│       ├── manifest.jsonl            # what was pulled, per item
│       └── content/…                 # materialized extracted content
└── attachments/
```

`_sources/` is the existing DOC-55 layout, unchanged. This document adds one file
to it — the per-source **run log** (§11.2) — and one new sibling directory,
`stores/`, specified next.

### 2.4 Remote stores get an extraction target; local directories do not {#SPEC-OKGSRC-11~draft}

**Owner requirement (2026-08-01):** a remote store — one that is **not a directory
on the user's own computer** — needs an **extraction target** that also holds
extraction state: time of last extraction, what was pulled, and so on. The path
shape is:

```
<trusty-agents home> / <agent> / stores / <store-identifier>/
```

**S-1.3 — The local/remote asymmetry is a stated requirement, not an
implementation detail.**

| | **Local directory** (kind (a)) | **Remote store** (kinds (b)–(g)) |
|---|---|---|
| Where the corpus lives | Already on the user's disk, in place | Behind an API; nothing local until fetched |
| Extraction target | **None.** Watched in place | `stores/<store-identifier>/` |
| Freshness mechanism | The trusty-search watcher over the directory (§10.1) | Scheduled fetch (§9) |
| Duplication on disk | None — the files are the corpus | One materialized copy, and only one (S-1.7) |

The asymmetry follows from the corpora themselves: a local directory is already a
durable, watchable, user-owned artifact and materializing a second copy of it
would be pure duplication. A remote corpus has no local existence at all, so
there is nothing to watch and nothing to re-read without re-fetching.

**S-1.4 — The store identifier is validated, never silently slugged.** It becomes
a directory name, so it is constrained to a conservative character set, rejected
rather than coerced when invalid, and unique within the assistant. PR #4523 set
exactly this precedent for the instance name (`AssistantInstanceId`, a *validated*
name rather than a slugged one) and this follows it. Silent slugging is
specifically rejected: two distinct stores whose identifiers slug to the same
directory would silently share extraction state, which is a data-corruption shape
rather than a naming inconvenience.

**S-1.5 — Extraction state, minimum contents.** Per remote store:

- **last extraction** — start and end timestamp of the most recent run, and its
  terminal state;
- **what was pulled** — the per-item manifest: item id, fingerprint, fetch time,
  and where it landed;
- **coverage** — the interval watermark (§8.2), in the interval form that makes
  extension work in both directions;
- **credential reference** — the #4040 handle used, never the credential;
- enough, jointly, that re-running duplicates nothing and *"extend by N more
  months"* fetches only the delta.

**S-1.6 — This is where the watermark lives.** §8's coverage state is not a
free-floating file: for a remote store it lives in that store's own directory
under `stores/`. One directory is the whole truth about one remote store.

### 2.5 Content materializes under `stores/`, then flows into `okg/`

**S-1.7 — Position, stated rather than left ambiguous.** For a remote store,
fetched content **materializes under `stores/<id>/content/`**, and the OKG
entities derived from it are written into `okg/`. `stores/<id>/` is an extraction
target, not merely a state file — which is what "extraction target" names.

Why this way, given it puts a copy on disk:

1. **A remote fetch is not re-readable.** Once a Gmail window is fetched, the only
   way to re-derive an entity without re-fetching is to have kept the bytes.
   Improving an extractor (DOC-55 §4.5 bumps `Extractor::version()` and
   invalidates exactly the affected items) then costs a local re-derivation
   rather than a full re-pull against a rate-limited third-party API.
2. **It makes the state honest.** "What was pulled" is checkable against what is
   on disk, rather than being an assertion only the ledger can make.
3. **It matches the owner's word.** An *extraction target* is a place things land.

**S-1.8 — But the corpus must not exist twice under `okg/`.** `stores/<id>/content/`
is the **fetched-bytes staging tier**; `okg/` holds the derived entities. They are
different representations, not two copies of the same one. The trusty-search index
over the store (§9) is registered over **`okg/` only** — indexing
`stores/*/content/` as well would return both the raw and derived forms for one
query, which is the duplicate-results failure this clause exists to prevent.

**S-1.9 — Retention is bounded and configurable.** Staged content is a cache with
provenance, not an archive: it is prunable without loss of correctness, because
the ledger and the entity tree are the durable record. A store whose staged
content has been pruned re-fetches on demand rather than reporting corruption.

**S-1.10 — A local directory source stages nothing.** No `stores/` entry is
created for kind (a). Its coverage state lives with its source row, and its
freshness comes from the watcher.

### 2.6 The home is one system-level value; relocating it needs a migration

**Owner requirement (2026-08-01), recorded as a constraint and deliberately not
designed here:** the trusty-agents home is a **single `${TRUSTY_AGENTS_HOME}`
system configuration that cannot be changed without a migration process.**

**S-1.11** Every path in this document — `okg/`, `stores/`, `agents/`,
`attachments/` — resolves beneath that one value. There is exactly one home
setting, and relocating it is a **migration**, not a configuration edit.

Why this is worth stating now: today there is no such single value. The only
lever is `KB_KNOWLEDGE_DIR`, a **process-global env var that relocates every
agent's tree at once** (`crates/trusty-agents/src/tools/okg/mod.rs:92-106`;
every tree is `<knowledge_dir>/<slug(agent)>`), and on `origin/main`
`AgentStoreBinding` (`crates/trusty-agents/src/stores/config.rs:49-62`) carries
`name` / `tree` / `index` / `palace` and **no root or path field at all** — PR
#4523 is what adds one. So per-store relocation is not merely unmigrated, it is
currently unexpressible.

**S-1.12 — The migration process is explicitly out of scope and deferred.** This
document does not design it, and this epic does not build it. Noted here as a
forward reference so a later relocation is understood as migration work rather
than a settings change.

### 2.7 Settled, so they are not re-litigated

- **`config.toml`, not `config.yaml`** — owner correction, 2026-07-29. TOML stays;
  there is no format migration. PR #4523 built it correctly.
- **Dotless** — the home carries no leading dot. Confirmed.
- **App-generated, not "protected"** — the home is generated by the product and
  expected to be browsed and hand-edited by the user. **Access control is out of
  scope**; earlier drafts describing it as protected were narrowed by the owner,
  and this document specifies no enforcement.

---

## 3. SPEC-OKGSRC-02 — Stores: Two Kinds {#SPEC-OKGSRC-02~draft}

### 3.0 Terminology: one noun, not two objects

The seven population types were first called **"OKG Sources"** and are now called
**document STORES**. **These are the same concept named twice, not two objects.**

**S-2.0** This document's canonical noun is **store**, matching both the owner's
latest framing and the `stores/` path segment. *"OKG Source"* is recorded as an
**earlier synonym** so a reader of tickets filed under the old name is not
confused. Modelling Sources and Stores as separate entities would be a spec-level
duplication of exactly the kind this effort exists to eliminate.

The document title retains "OKG Sources" as the feature's established name; every
normative clause below says *store*.

### 3.1 A store is exactly one of two kinds

**S-2.1** A store is **either** a pointer to a local directory **or** an extract
from a connected system. There is no third kind, and the distinction is the
**primary type distinction** — not an implementation detail of a single pipeline.

| | **Kind 1 — local-directory pointer** | **Kind 2 — connected-system extract** |
|---|---|---|
| What it is | A **pointer**. Nothing is materialized. | An **extraction** with a materialized target |
| Members | (a) an arbitrary directory | (b) Gmail · (c) Drive · (d) Slack sent · (e) Slack channel · (f) Notion · (g) Granola |
| On-disk footprint | None beyond its registration | `stores/<store-identifier>/` — state + manifest + staged content (§2.4) |
| Freshness | trusty-search watcher over the directory (§10.1) | Scheduled extraction (§9) |
| Credential | None | Brokered by #4040 (§6) |
| Coverage state | With its registration | In its own store directory (`S-1.6`) |
| Trust | `untrusted-external` unless operator-designated user-authored | Always `untrusted-external` (§6) |

**S-2.2 — Why kind 1 materializes nothing.** A local directory is already a
durable, watchable, user-owned artifact. Materializing a copy would duplicate the
corpus and create a second thing to keep in sync. It is a **pointer** precisely
because the bytes are already there.

**S-2.3 — Why kind 2 must materialize.** A remote corpus has no local existence.
There is nothing to watch and nothing to re-read without re-fetching, so an
extraction target that also holds extraction state is what makes the build
idempotent and re-derivable (`S-1.13`). §2.4–§2.5 specify it.

### 3.2 Both kinds populate the OKG automatically

**S-2.4** Both store kinds are **configured pipelines**, and both populate the
canonical OKG **automatically** — kind 1 through the watcher, kind 2 on its
user-managed schedule. Configuring a store **is** the user's instruction to keep
it current; no further per-run consent is required, and requiring one would
defeat the feature.

**S-2.5** Store output lands in the assistant's **own** OKG and its **own** index
(§9) — never in a third-party corpus. This is what makes §4's contamination guard
unnecessary for stores, and necessary for the thing §4 governs.

### 3.3 The deliberate overlap: a directory that is also a bound index

Kind (a) is registered as a store **and** attached to the assistant as a project
**and** indexed by trusty-search (§5.2). So one directory is simultaneously a
kind-1 store and a bindable search index — which §4 governs separately.

**S-2.6** A directory registered as a store has its index recorded as
**store-owned**. Offering §4 extraction over that index would re-derive, from
chunks, content the ledger already ingested from the files — a second copy by a
second route, with a different `item_id` scheme and therefore invisible to dedup.

**S-2.7** The surface therefore **declines §4 extraction for a store-owned index
and says why**, rather than silently succeeding. A **non-owned** bound index — one
the user attached that no store produced — remains fully eligible.

**S-2.8** Ownership is a property of the **index registration**, recorded in
§5.2's single all-or-none operation, so the two mechanisms cannot disagree about
who owns a corpus.

### 3.4 A store has two INDEPENDENT capacities

**S-2.9 — Searchable corpus and OKG contributor are orthogonal, not stages of one
pipeline.** A store has two capacities and they are set independently:

| Capacity | Meaning |
|---|---|
| **Searchable corpus** | Attached and indexed; reachable by search fan-out (§10.5) |
| **OKG contributor** | Extracted into the canonical OKG |

A store may be **either, both, or search-only**. There is no ordering between
them and neither implies the other.

**S-2.10 — Search-only is a COMPLETE, LEGITIMATE end state.** Owner, 2026-08-01:
*"It is perfectly legitimate NOT to sync OKG to an external store — there's a
cost of syncing stores, and a search duplication."* Two stated reasons, both
recorded here as rationale:

1. **Cost.** Extraction is not free — API calls, fetch time, storage, and for a
   connected system a rate-limited third-party budget.
2. **Search duplication.** Content indexed in an attached store and again in the
   OKG is indexed twice, and a fan-out query would then return both copies.

**S-2.11 — "Not extracted" is NEVER a pending, incomplete, or error state.** This
is normative and it constrains three surfaces:

- the **config surface** must not warn, nag, or badge a search-only store as
  unfinished;
- **health and inspection findings** must not report unextracted content as a
  defect, and must emit no metric that frames extraction coverage as a
  completeness score to be driven upward;
- the **OKG configuration subpane** (§12) renders search-only as a **stated
  configuration**, alongside extracted stores, never as a call to action.

**S-2.12 — Extraction coverage is partial by nature.** Owner: *"Not all useful
content in a store will be entity-extractable."* The OKG holds **entities**; a
store holds **documents**. Content yielding no entities is still valuable to
search. Partial coverage is therefore the expected steady state, not a quality
failure, and **search fan-out is what makes that content reachable at all** — which
is precisely why fan-out is a load-bearing mechanism rather than optional
decoration, and why search-only attachment is worth having.

**S-2.13** This is the same axis as §4: extraction from a bound store is an
explicit option, never automatic. §4 gives the *mechanism and its guard*; this
subsection gives the *reason a user would legitimately decline it*. **They are one
concept, not two mechanisms.**

---

## 4. SPEC-OKGSRC-13 — Extraction From a Bound Search Store {#SPEC-OKGSRC-13~draft}

**This is a third, distinct thing.** It is neither store kind, and it is specified
separately so it is not confused with them.

### 4.1 What it is

An assistant may bind **any** search store or index. Pulling entities **out of** a
bound store **into** the OKG is an **option** available for that store. Owner,
2026-08-01: *"extraction is an OPTION for any bound store."*

| | **Stores** (§3) | **Bound-store extraction** (this section) |
|---|---|---|
| What | A configured pipeline over an external corpus | An existing index the assistant binds for reading |
| Trigger | **Automatic** — watcher or schedule | **Explicit command only** |
| Optionality | Configured once, then runs | An option, invoked deliberately |
| Owned by | §5–§12 of this document | #4283 |

**S-13.1** Binding a store never extracts from it. **No schedule, refresh, or
background job may invoke bound-store extraction.** This document specifies no
automatic trigger for it and forbids one.

### 4.2 Why it must stay explicit — the contamination guard

The guard belongs **here alone**, and the reason is mechanical.

**Verified against `origin/main` (4e67493b).** `feed_bound_index`
(`crates/trusty-agents/src/tools/okg/index_feed.rs:32-50`) runs at the tail of
every OKG ingest and pushes what that run wrote into **the agent's bound search
index** — resolved by `resolve_feed` → `crate::stores::bound_index_for_tree`
(`:53-77`), pushed through `feed_source` / `HttpIndexFeed`. The binding is
therefore **bidirectional in effect**: an index bound for reading also receives
OKG output.

So if extraction from a bound store were automatic, binding a **populated**
corpus — an existing code or document index, such as the 201k-chunk `cto-duetto`
index — would mix generated entities into a store the user bound only to *read*.

**S-13.2** Explicit-only extraction is the mitigation, and it is the whole
mitigation: a deliberate act against a named store, at a moment the user chose.

**S-13.3** Stores (§3) carry no such hazard and are not constrained by it
(`S-2.5`).

### 4.3 Scope boundary

**S-13.4** **#4283 owns this in full**; it is not re-specified here. This document
constrains it in exactly three ways: it stays explicit (S-13.1), it declines
store-owned indexes (S-2.7), and its results are counted and displayed separately
from store ingestion (§12) so a user can always tell which mechanism produced
what.

---

## 5. SPEC-OKGSRC-03 — The Store Roster {#SPEC-OKGSRC-03~draft}

A **store** is one registered binding of an agent's canonical OKG to an external
corpus (§3.0: *"OKG Source"* is the earlier synonym). Stores are rows in
`_sources/registry.toml` (DOC-55 §5.3); a store type is the `kind` in the open
`Locator::Connector` variant. This section specifies the seven named types and
their per-type constraints; §11 specifies how an eighth arrives.

### 5.1 The roster

| # | Type | Locator params | Windowed? | Constraint (normative) |
|---|---|---|---|---|
| a | `directory` | `path`, `extensions`, `recursive` | no — full corpus | Also attached to the assistant as a **project** and indexed by trusty-search. See §5.2. |
| b | `gmail` | `identity`, `months_back`, `query` | **yes** | **SENT messages only** (§5.3). Idempotent, date-bounded, extendable by N more months. |
| c | `drive` | `identity`, `folder_id`, `recursive` | no, iff fully recursive | Additional directories are additional sources, not a widened one. |
| d | `slack-sent` | `workspace`, `months_back` | **yes** | **SENT messages only** (§5.3). |
| e | `slack-channel` | `workspace`, `channel`, `months_back` | yes | One source per channel. Additional channels are additional sources. |
| f | `notion` | `workspace`, `page_or_database_id`, `recursive` | no, iff enumerable | Additional Notion directories are additional sources. |
| g | `granola` | `identity` (API key ref), `months_back` | yes | Meeting transcripts. Credential is an **API key**, not OAuth — §7. |

Every one of these is a DOC-55 `Connector` (`kind` + `list` + `fetch`) and
inherits `C1`…`C8` unchanged. **This document adds no dedup, no tombstoning, and
no entity-writing semantics** — that would be a second implementation of
machinery DOC-55 already owns.

### 5.2 (a) is two bindings, and the duality is the requirement

The owner's statement for a directory source is that it is *also* attached to
the assistant as a "project" and *also* indexed by trusty-search. That is one
user action producing **three** facts, and they must not drift:

1. an OKG source row (the corpus is ingested into the store);
2. a project attachment on the assistant;
3. a trusty-search index over that directory.

**S-3.1** A `directory` source is registered through **one** operation that
establishes all three, or fails and establishes none. A half-registered
directory — indexed but not ingested, or attached but not indexed — is the
failure mode this clause exists to prevent.

**S-3.2** (3) is the **K-d attached-index** tier (DOC-58 `SPEC-KDIDX-01~draft`),
not the store's own index (§9). The distinction is DOC-58's two-tier principle
and this document does not weaken it: an attached index is READ-attachable;
the store's index is what the store writes to. #4289 already owns the "index a
new directory from the config UI, with an overlap guard against existing index
roots" half — this document **sequences against #4289, it does not duplicate
it.**

### 5.3 SENT-only: what it is and what it is not

Gmail (b) and Slack (d) are constrained to the user's **own outbound** content.
This is simultaneously a signal-quality choice (outbound writing is the best
available proxy for how the user thinks and what they commit to) and a **partial
security mitigation** (§6.4). It is not a full mitigation, and §6.4 says why.

**S-3.3** The SENT-only constraint is enforced **in the connector's `list`, at
the query**, not by filtering after fetch. Gmail: `in:sent`. Slack: the
authenticated user as author. A post-fetch filter would mean untrusted inbound
content was fetched, extracted, and held in memory before being discarded, and
one bug away from being written.

**S-3.4** (c) `drive`, (e) `slack-channel`, (f) `notion`, and (g) `granola` have
**no equivalent constraint and cannot have one** — a Drive folder, a channel, a
Notion tree, and a meeting transcript are all inherently multi-author. §6 governs
them.

### 5.4 Extension of an existing source

The owner requires all extractions to be extendable: N more months of Gmail,
more Drive directories, more Slack channels, more Notion directories.

**S-3.5** Extending a **window** (b, d, e, g: `months_back` grows) is an
`upsert` of the same source row — the DOC-55 additive path, preserving
`added_at` and the ledger — and MUST fetch only the delta. §8.2 specifies the
watermark state that makes "only the delta" true.

**S-3.6** Extending **coverage** (c, e, f: another directory, channel, or page)
is a **new source row**, never a mutation of an existing one. Rationale: each
gets its own ledger, its own watermark, its own schedule, its own credential
scope, and its own failure state. A composite source hides which half failed.

### 5.5 Config shape: ONE store binding, many stores nested under it

**S-3.7 — Stores nest UNDER the single `[[stores]]` binding; they are never
sibling bindings.**

This is forced by existing behaviour. `StoresConfig::validate()`
(`crates/trusty-agents/src/stores/config.rs:172-179`) emits a warning above one
binding — verbatim: *"N stores bound; the spec allows exactly one OKG store per
agent (only the first is used as the default search target)"* — pre-existing since
#3816. If each store became its own `[[stores]]` binding, **every multi-store
assistant would emit a config warning on every load**, and only the first would be
used as the default search target. The owner's core scenario — several stores
feeding one OKG — would look broken by construction.

It also mirrors the canonical model exactly: **one OKG per agent, populated by
many stores.** One binding, many stores nested under it, is that sentence in
config form.

```toml
[[stores]]
name = "izzie"
# tree / index / palace / root as today — ONE binding, unchanged

  [[stores.sources]]
  id = "gmail-sent"
  kind = "gmail"
  # … locator params, schedule, credential reference
```

**S-3.8** `config.toml` already tolerates unknown keys, so the nested array lands
without a schema fight and an older reader ignores it rather than failing.

**S-3.9** `StoresConfig::validate()`'s one-binding warning is **left exactly as
it is.** Nesting means it never fires for a legitimate multi-store assistant, so
there is no reason to weaken a check that still correctly catches a genuinely
misconfigured second binding.

*(The nested key is spelled `sources` because that is the established config noun;
per §3.0 the objects it holds are **stores**.)*

---

## 6. SPEC-OKGSRC-04 — The Untrusted-Content Boundary {#SPEC-OKGSRC-04~draft}

**This is the most important section in this document.** Every remote source in
§4 ingests content the user did not author and does not control, into a store an
assistant then reads as knowledge. That is a prompt-injection surface **by
construction**, not by accident, and the surface widens with every source type
added.

### 6.1 This repo already has an active threat model on exactly this shape

This is not a hypothetical imported from general LLM-security literature. It is
this repository's own, recorded, enforced position:

- **The base assistant persona carries no git tools because of ingestion.**
  `crates/trusty-agents/.trusty-agents/agents/assistant/agent.toml:84-87`,
  verbatim: *"`izzie` ingests UNTRUSTED content (Gmail/Drive/Calendar), so that
  put untrusted input one hop from a cross-project read primitive: a
  prompt-injection exfiltration shape."* PR #4420 stripped `git_log`/`git_status`
  from izzie and personal-assistant on those grounds.
- **It is pinned by a test.** `bundled_personas_pin_git_reach`
  (`crates/trusty-agents/src/agents/tests/loading.rs:2200`) asserts through the
  real loader that `izzie`, `personal-assistant`, and `assistant` reach **none**
  of the four git tools, with `cto-assistant` — *"a coding assistant, not a
  mail-ingesting one"* — as the deliberate carve-out.
- **The same file already calls ingested content untrusted.**
  `agent.toml:107`: *"ingested content is itself untrusted input."*
- **There is an execution gate built on the premise.**
  `crates/trusty-agents/src/tools/l0_exec.rs:8-18` states *"L0 must never ingest
  untrusted content"* and blocks shell reach from untrusted-ingesting tiers,
  citing the *"prompt injection to code execution"* path.
- **Read-side confinement is justified in these exact terms.**
  `crates/trusty-kb/src/okg/policy.rs:11-15`: *"Because the content being
  ingested is itself untrusted, a prompt-injected document could name the next
  path to read — so this is an exfiltration primitive."* Enforced at scan time,
  so a poisoned `registry.toml` row cannot bypass it on a later run.

#### The confinement runs one way, and the direction matters

**S-4.0** `AssistantHome::store_root()` (PR #4523) confines the **destination**,
not the source. An extractor cannot repoint `okg/` outside the assistant home —
which is what stops one instance writing into another's store — but **reading
*from* an arbitrary directory, Gmail, Drive, Slack, Notion, or Granola is
unaffected by that confinement.**

Stated explicitly because it is easy to read the wrong way round: a reader
scanning the store roster could assume that because writes are confined, reads
are too. They are not, and that asymmetry is exactly why this section exists.
Write confinement is what makes multi-store extraction *safe to land*; it does
nothing about what the extracted content *says*. Read scope is governed
separately, by operator configuration re-checked at access time (DOC-55 C6,
`crates/trusty-kb/src/okg/policy.rs`).

**S-4.1** OKG Sources inherits this threat model in full. No source type may be
added that widens an assistant's reach without the capability-grant review §13
separates out.

### 6.2 What exists today, and the one thing that does not

Two of the three necessary mechanisms are already built:

| Mechanism | Status | Where |
|---|---|---|
| **Capability reduction** — an untrusted-ingesting persona holds no dangerous primitive | **BUILT** and test-pinned | `agent.toml:79-91`; `loading.rs:2200`; `l0_exec.rs` |
| **Prompt-level fencing** — recalled content is delimited and preambled as DATA, not instruction | **BUILT**, for memory drawers | `crates/trusty-agents/src/ctrl/pm_task/dispatch/persona_memory.rs:420-533`; `UNTRUSTED_PREAMBLE` at `:502`, delimiters at `:420-427`, which already say *"drawer content is UNTRUSTED. It arrives from Gmail/Drive ingestion"* |
| **A trust label on the content itself** — anything marking an OKG document, chunk, or search result as untrusted-derived | **DOES NOT EXIST** | — |

The third row is the gap. Per-entity provenance frontmatter *does* exist —
`ingest.rs:275-276` stamps `source_id` and `source_kind`, plus `ingested_at` —
but it is **descriptive metadata, not a trust label**, and nothing downstream
reads it as one. Critically, when an OKG document is retrieved through
`vector_search` against the store's index, **the chunk arrives at the model with
no provenance and no fence at all**. `persona_memory.rs`'s fencing covers the
memory-drawer path, not the search path.

**S-4.2** That asymmetry is the concrete defect this section names. An
assistant's memory drawers are fenced; the OKG store this document fills from
Gmail, Drive, Slack, Notion, and Granola is not.

### 6.3 Normative requirements

**S-4.3 — Every ingested item is labelled at write time.** Every document
written by an OKG source carries, in its frontmatter, `source_id`, `source_kind`,
`ingested_at` (all three exist today) **plus a new `trust` field** with the value
`untrusted-external` for every store kind in §5 except a `directory` source
whose root the operator has explicitly designated user-authored. The label is
written by the engine, not the connector — a connector cannot mark its own output
trusted.

**S-4.4 — The label survives to the point of use.** A trust label that stops at
the file is worthless: the model sees chunks, not files. The label MUST be
carried into the trusty-search chunk payload by the index feed, and returned on
every search result drawn from an OKG store. Absent a carrier, the chunk is
treated as untrusted.

**S-4.5 — Retrieved untrusted content is fenced, reusing the existing
mechanism.** OKG content reaching a model turn is wrapped with the same
delimiter-and-preamble treatment `persona_memory.rs` already applies to drawers.
It is **reuse, not a second implementation**: the fencing constant and the
delimiters are lifted to a shared seam and applied on both paths. A second
fencing implementation here would be exactly the defect the common-entry-point
rule forbids.

**S-4.6 — Fail closed on an unlabelled chunk.** A chunk from an OKG store with no
trust label is fenced as untrusted, never passed through bare. Labels are added
over time to a corpus that already exists; the default must be safe for the
unmigrated majority.

**S-4.7 — Capability reduction remains the primary control.** Fencing is a
mitigation, not a guarantee — no delimiter reliably survives an adversarial
instruction, and this document does not claim otherwise. The load-bearing control
stays the one already pinned: an assistant that ingests untrusted content does
not hold primitives worth attacking. Consequently **every new source type is
reviewed as a capability grant** (§13), and any ticket that both adds a source
and widens tool reach is split into two.

### 6.4 SENT-only: an analysis, not a claim of sufficiency

The Gmail and Slack SENT-only constraint (§4.3) is a real, meaningful partial
mitigation. The user authored the outbound corpus, so the dominant injection
vector — an attacker mailing your assistant an instruction — is closed for those
two sources.

**It does not fully mitigate, and the spec must not imply it does:**

1. **Sent mail quotes inbound content.** A reply quotes the message replied to;
   a forward carries the original verbatim. An attacker who gets one reply
   lands their text inside the SENT corpus. This is the single largest hole and
   it is not closeable by scoping — only by quote-stripping, which is lossy and
   itself unreliable.
2. **Attachments on sent mail are third-party bytes** in the general case.
3. **Slack sent messages quote and thread-reply** to the same effect.
4. **A compromised or shared account** writes directly into the "trusted" set.

**S-4.8** SENT-only is therefore documented as *signal quality plus partial
mitigation*, never as a trust boundary. Gmail and Slack sent corpora carry the
same `untrusted-external` label as everything else. Treating them as trusted on
the strength of the constraint would be the exact error this subsection exists
to prevent.

**S-4.9** Sources (c) `drive`, (e) `slack-channel`, (f) `notion`, and (g)
`granola` have **no author constraint whatsoever**, and none is possible. A
Granola transcript is a recording of other people talking; a Notion page is
whatever a colleague wrote; a shared Drive folder is arbitrary. These are
unambiguously and irreducibly untrusted, and §6.3 is the whole of their defence.

### 6.5 Residual risk, stated for the owner rather than papered over

With S-4.3…S-4.9 implemented, the residual risk is:

> An assistant may quote, summarise, or act on attacker-authored text that was
> ingested through a legitimate source, fenced as untrusted, and nonetheless
> influenced the model's behavior.

**This risk is not eliminated by this design and cannot be.** What the design
does is bound the blast radius: the assistant holds no cross-project git
primitive, no shell reach, and no write-capable tool it did not already hold
before ingestion. **Accepting this residual risk is an owner decision**, and it
is Q2 in §14 rather than an assumption buried in a design section.

---

## 7. SPEC-OKGSRC-05 — Credentials: A Consumer of Epic #4040 {#SPEC-OKGSRC-05~draft}

### 7.1 The rule

Gmail, Drive, Slack, Notion, and Granola all require credentials. **OKG Sources
designs none of it.** Epic #4040 — "unified credential authority, delivery, and
audit" — owns principal and credential identity, ACL/scoping, default-deny,
revocation, the audit trail, and secure delivery. This document specifies OKG
Sources as a **consumer** of that authority.

**S-5.1** No OKG source type reads a token file, holds a secret, or implements a
resolution order. This restates DOC-55's connector obligation **C8** verbatim in
force. A source type receives an already-resolved, already-scoped client handle
from the credential authority and nothing else.

**S-5.2** A second credential mechanism introduced for any store in §5 is a
defect under this repo's common-entry-point rule, and is rejected at review
regardless of expedience.

### 7.1a SECRETS MUST NEVER LAND IN THE ASSISTANT HOME

**S-5.6 — Normative, and the single most important clause in this section.** No
credential — API key, OAuth token, refresh token, cookie, or bearer — is ever
written into `${TRUSTY_AGENTS_HOME}/<agent>/`. Not in `config.toml`, not in a
store's `state.toml`, not in `stores/<id>/`, not anywhere under the home.

The reason is structural, not precautionary: **the home is deliberately browsable
and user-editable.** #4325 designs it to be opened, read, and hand-edited like a
`trusty-mpm-projects` directory, and #4523 implements exactly that. Placing a
Granola API key or a Slack token in a directory the product actively invites users
to open would put secrets in the one place the design guarantees will be looked
at — and, for anyone who backs up or syncs that directory, copied.

**S-5.7** A store row therefore carries a **credential reference** — an opaque
handle resolved by #4040 at use time — and never the credential itself. This is
what `S-1.5`'s state contents already require, restated here as a prohibition so
it cannot be read as a preference.

**S-5.8** The same prohibition binds the run log (§12.2) and the extraction
manifest: neither may record a credential, and neither may echo a request header
or URL query string that could carry one.

### 7.1b What OKG Sources needs from #4040

Offered as concrete input to an epic that currently carries no child checklist.
OKG Sources needs exactly five things, and nothing beyond them:

1. **A durable, opaque reference** a store row can hold in plain text under the
   home without that text being a secret.
2. **Resolution at use time** to an authenticated client (or a token with a
   defined lifetime), performed outside the home and never materialised into it.
3. **Read-scoped grants where the provider can issue them**, and an honest,
   machine-readable answer when it cannot — see `S-5.3`.
4. **An observable revocation/expiry signal**, so `S-5.4`'s displayed
   `needs-credential` state and schedule stop are driven by the authority rather
   than inferred from a 401.
5. **Support for two credential shapes**: OAuth (Gmail, Drive, Notion, Slack) and
   **plain API key** (Granola, and Fireflies later). The API-key shape is the one
   most likely to be treated as a special case, and it must not be.

**S-5.9** Until #4040 provides these, a store type requiring a credential this
repo does not already hold stays blocked (`S-5.5`) — it does **not** ship with an
interim token file, which would be the exact defect `S-5.6` forbids.

### 7.2 What OKG Sources would otherwise duplicate — the concrete list

Named so reviewers can recognise the duplication if it is attempted:

- The Google two-tier `TokenStorage` profile resolution and refresh manager
  (`crates/trusty-gworkspace/src/api/auth/storage/mod.rs`), which the existing
  Gmail/Drive ingest tools already reuse rather than reimplement.
- The `trusty_common` three-tier key resolver (process env → `.env.local` →
  keyring `KeyStore`).
- Slack's env-var-only posture today (`SLACK_APP_TOKEN` / `SLACK_BOT_TOKEN`).
- **Notion has no first-party credential handling in this repo at all** — it is
  reachable only as an external MCP server. Notion is therefore the source type
  most likely to grow a bespoke token path, and the one to watch.
- **Granola is an API key, not OAuth** — a different credential *shape* from
  every other source here, which is precisely why it must route through the
  common authority rather than a per-source special case.

### 7.3 Two honesty requirements inherited from today's state

**S-5.3 — Scope truth.** The base persona's own comment
(`agent.toml:105-113`) records that the Google-backed ingest tools reuse
**write-capable** OAuth grants, not read-only ones, and that they return `None`
from `ToolExecutor::scope()` so **they are not scope-gated at all**. An OKG
source performs only reads and MUST be delivered a read-scoped credential by
#4040. Where a provider cannot issue one, the surface says so explicitly rather
than implying a read-only grant that does not exist — the error the comment
itself calls out as having previously been claimed and been false.

**S-5.4 — Revocation is observable.** When #4040 revokes or expires a
credential, the affected stores move to a named, displayed failure state (§12)
and their schedules stop. A scheduled source silently failing forever on an
expired token is the failure mode this clause exists to prevent.

**S-5.5 — Ordering.** Every source type requiring a credential this repo does not
already hold (Slack, Notion, Granola) is **blocked on #4040** and is not
scheduled. §13 marks these explicitly. Sources reusing credentials already held
(directory, Gmail, Drive) may proceed as plumbing.

---

## 8. SPEC-OKGSRC-06 — Idempotency, Watermarks, and Incremental Extraction {#SPEC-OKGSRC-06~draft}

"Idempotent, date-bounded, extendable by N more months" implies two properties
that must be made true by durable state, not by hope: re-running must not
duplicate, and extending the window must fetch only the delta.

### 8.1 Reuse, do not reinvent

The machinery exists in `trusty-kb::okg` and this document specifies **no new
deduplication**. Per DOC-55 §2.2, inherited unchanged:

- `Ledger::is_current(item_id, fingerprint)` — the single predicate deciding
  skip-vs-write, backed by the append-only per-source journal
  `_sources/<id>.jsonl`.
- `SourceRegistry::upsert` — additive; registering a source and widening its
  window are the same call, preserving `added_at` and the ledger. **This is
  already the mechanism S-3.5 requires**; window extension needs no new code
  path, only a source type that honours the window it is given.
- Entity-written-before-ledger-line ordering, giving crash convergence.
- `enumerates_full_corpus` gating deletion detection, so a windowed source never
  tombstones the corpus it did not enumerate.

**S-6.1** A source type supplies a stable `item_id` and an honest `fingerprint`
(DOC-55 C1/C2) and writes no dedup logic of its own. Where a system offers no
revision signal, it sets `volatile = true` rather than inventing a constant.

### 8.2 The watermark: where "only the delta" lives

**S-6.2** Each source carries a durable **watermark** — for a remote store in
that store's own directory under `stores/` (§2.4, `S-1.6`), for a local directory
source alongside its source row in `_sources/` —
recording the high-water mark of what has been enumerated: for date-bounded
sources (b, d, e, g) the covered interval `[earliest_covered, latest_covered]`;
for enumerable sources (a, c, f) the last full-enumeration timestamp and the
observed id set.

**S-6.3** Two distinct extension directions, and both must work:

- **Forward** (the scheduled case, §8): fetch `(latest_covered, now]`.
- **Backward** (the user case, "N more months"): fetch
  `[new_earliest, earliest_covered)`.

Storing a single scalar cursor supports only the forward direction and would
silently make "reach further back" a no-op or a full refetch. **The interval is
the required shape**, and this is the specific correctness trap in this section.

**S-6.4** The watermark is an **optimisation, never the correctness mechanism.**
The ledger remains authoritative: a lost, corrupt, or absent watermark degrades
to a wider fetch that the ledger then dedups. A design in which watermark loss
causes duplication is rejected.

**S-6.5 — Deletion honesty on extension.** Widening a window never enables
deletion detection for a windowed source. `enumerates_full_corpus` stays false
for (b), (d), (e), and (g) permanently, whatever the window.

### 8.3 Durability of the source registry

ADR-0022 (`docs/adr/0022-knowledge-tree-sync-model.md`) decides that knowledge
trees do **not** sync with agent config, and that a new machine **re-ingests from
registered sources**. That makes the source registry and its watermarks a
first-class durability requirement rather than a cache.

**S-6.6** `_sources/registry.toml` and the watermarks are the reproducibility
record. A new machine with the same registry and the same credentials
reconstructs an equivalent store. Nothing about a store may be recoverable only
from the tree.

---

## 9. SPEC-OKGSRC-07 — Scheduled Refresh {#SPEC-OKGSRC-07~draft}

The owner's requirement: all sources are refreshed on **a schedule the user
manages**, so new mail, messages, transcripts and folder docs land in the store
and are indexed automatically.

**Nothing of this exists.** Verified: there is no cron parser, no job scheduler,
and no job registry anywhere in the workspace — grep for `cron`, `job_scheduler`,
`clokwerk`, `tokio-cron` across every `Cargo.toml` returns zero hits. DOC-55 §6.4
states the consequence outright: *"A crawl cannot be scripted, cron'd, or run in
CI."*

### 9.1 Where schedules live

**S-7.1** A schedule is a **per-source field on the source row**, not a separate
scheduler config. One row is the whole truth about a source: locator, window,
credential reference, watermark, schedule, and last-run state. A separate
schedule table would be a second place to disagree with the registry, and the
first thing to drift when a user deletes a source by hand — which #4325
guarantees they will.

```toml
[[sources]]
id = "gmail-sent"
collection = "correspondence"
enabled = true

  [sources.locator.connector]
  kind = "gmail"
  params = { identity = "bob-personal", months_back = 12, sent_only = true }

  [sources.schedule]
  every = "6h"          # coarse interval; see S-7.2
  enabled = true
```

### 9.2 Granularity, and the case against cron

**S-7.2** Granularity is a **coarse interval** — `1h`, `6h`, `24h`, `7d` — with a
floor, not a cron expression. Rationale, in order of weight:

1. The requirement is *"a schedule the user manages"*, and a user-managed
   schedule in a GUI is a dropdown. A cron expression is an expert surface that
   fails silently when mistyped.
2. Every source here is **window-based and idempotent**. Exact firing times carry
   no semantic meaning: a refresh that fires an hour late fetches the same delta.
3. No cron parser exists, so choosing cron means adding a dependency to express
   something nothing needs.

**S-7.3** A floor applies, mirroring `MIN_POLL_INTERVAL_SECS = 15` in
`listeners/config.rs:39`: a below-floor value is **clamped up, not rejected** —
the existing house behavior. The floor for remote sources is materially larger
than the listener floor, because these are rate-limited third-party APIs, not a
local history poll. Its value is an implementation ticket, not a spec constant.

**S-7.4** `enabled = false` is the default for a newly registered schedule, per
`ListenerConfig`'s safe-by-default posture (`config.rs:74-77`). Registering a
source never silently starts background network activity against a user's mail.

### 9.3 Which mechanism runs it — reuse the pattern, not the module

There is a real fork here and this document takes a position while flagging it
(Q4).

**Listeners are the wrong home, but the right pattern.** DOC-47's `EventSource`
contract is a **push/webhook event ingress producing `Goal`s in the SM goal
store**; its `SourceKind::Poll` arm is explicitly *"a quarantined last resort …
never the primary mechanism."* A content-fetching refresh is not an event: it
wakes no agent, creates no goal, and starts no session. Modelling it as a
listener would mean every scheduled refresh either wakes an assistant — the exact
opposite of the mechanical, model-free flow §3 requires — or declares itself an
event that wakes nobody.

**S-7.5** The scheduled refresh runner is **not** an `EventSource` and does not
emit `ExternalEvent`. It reuses the *shape* of `listeners::poll::spawn_listeners`
(`poll.rs:139-160`): one detached tokio task per enabled schedule, an interval
floor, exponential backoff (`MIN_BACKOFF_SECS=2` / `MAX_BACKOFF_SECS=300`), and
a durable cursor. It does **not** reuse the listener module itself, because it
carries none of the wake, filter, or persona-dispatch semantics that module
exists for.

**S-7.6** It also fixes the anti-pattern the same file demonstrates:
`poll.rs:145-157` dispatches connectors through a hardcoded
`match cfg.connector.as_str()`. The refresh runner resolves source types through
the §10 registry. An unknown kind is a **loud error**, never a silent skip.

### 9.4 Failure

**S-7.7** A failed run is recorded (§11), never silently retried into oblivion:

- **Transient** (network, 5xx, rate limit): exponential backoff within the
  task; the source stays `enabled`. Rate-limit responses honour `Retry-After`.
- **Credential** (401/403, revoked, expired): the schedule **stops** and the
  source enters a displayed `needs-credential` state (S-5.4). Retrying a revoked
  credential on a timer is how an account gets locked.
- **Configuration** (unknown kind, locator no longer resolves, policy refusal):
  the schedule stops and the source enters `config-error` with the reason.
- **Soft errors are errors** (DOC-55 C7): a 403 returned as a 200-shaped body
  MUST be detected and raised. Reporting "0 new items" for a permission failure
  is a data-loss bug.

**S-7.8** Per-item errors never end a run (DOC-55 E3); they land in the run's
error list. One unparseable PDF does not stop a mailbox refresh.

**S-7.9 — Failure is never invisible.** A source that has not succeeded within a
multiple of its own interval is displayed as **stale with its reason**, not as
merely "last run: <old date>". This is the `never fabricate` posture DOC-58 KD-13
and DOC-57 G-4 already impose on the Knowledge pane.

### 9.5 Overlap

**S-7.10** A source has **at most one run in flight**. If a tick arrives while
the previous run is still going, the tick is **skipped, not queued** — and the
skip is recorded. A backfill of twelve months of Gmail legitimately outruns a
6-hour interval; queueing would build an unbounded backlog of runs that each
re-derive the same delta. Skipping is safe precisely because the work is
idempotent and window-based: the next tick picks up everything the skipped one
would have.

**S-7.11** A run that exceeds a hard ceiling is cancelled and recorded as
`timed-out`. Because commits are chunked (DOC-55 C5), a cancelled run keeps its
partial progress and the next run resumes from the ledger.

**S-7.12** Runs are bounded per tick by the source type's `ceiling()` (DOC-55
default 5,000). A scheduled tick is not a backfill; a first-time backfill of a
long window is the assistant-driven or deterministic path (DOC-55 §6), not the
scheduler's job.

---

## 10. SPEC-OKGSRC-08 — trusty-search Integration {#SPEC-OKGSRC-08~draft}

The owner's framing is "indexed by trusty-search … standard file-watching
behavior." Verified against the real API, that is **half right**, and the half
that is wrong matters.

### 10.1 What is actually true

- **trusty-search does watch index roots automatically.** `watcher_manager.rs`
  starts a watcher when an index is registered (warm-boot restore or
  `POST /indexes`) and stops it on delete; opt-out is env-only
  (`TRUSTY_DISABLE_WATCHER=1`); it auto-disables on network mounts and tells
  clients to push instead. There is **no `watch` flag** on `create_index` at any
  layer — watching is not a per-index client choice.
- **But the OKG path deliberately does not rely on it.** `index_feed.rs`
  implements a **push** contract: push then record, withdraw before re-push,
  refuse an index that cannot serve the tree. Its rationale is that the tree
  write is the durable record and a search daemon that is down must not lose
  ingestion work.

**S-8.1** Both mechanisms are retained and their roles are separated: the **push
feed is the correctness mechanism**; the daemon watcher is a **convergence
backstop** that catches out-of-band edits — which #4325 guarantees, since users
are expected to hand-edit the store. Neither is removed in favour of the other,
and the push feed never assumes the watcher ran.

### 10.2 Index cardinality

**S-8.2** **One index per assistant store**, not one per source. The store is one
tree; sources are rows within it; a search over "what this assistant knows" is
one query. Per-source indexes would multiply index count by source count, fragment
every query, and multiply the leak surface named in §10.4 below.

**S-8.3** This is the **K-a curated tier** (DOC-58 `SPEC-KDIDX-01~draft`). A
`directory` source's own index (§4.2 fact 3) is a **K-d attached index** and a
different object. KD-1 holds: the two lists are never the same list, and a
surface showing an id in both dedups it and presents it once under its K-a role.

### 10.3 Creation is not automatic today, and that is a gap this spec must own

**Nothing in trusty-agents ever calls `create_index`** — verified by grep. Index
creation is out-of-band ops work, and DOC-58 §9 makes GUI creation an explicit
non-goal, gated on #4011's runbook, because `create_index` over a tree the
operator does not own writes a `.gitignore` into it.

**S-8.4** That non-goal does **not** apply to the assistant's own store. The
store is app-generated, inside the assistant's own home, and owned by the
product — the exact condition DOC-58's caution excludes. The store's index is
therefore created by the system when the store is created, with `root_path` set
to the `okg` directory. This is a **narrow, argued exception** to DOC-58 §9, not
a general reopening of it: creating an index over a directory the user named
(#4289) remains gated exactly as DOC-58 leaves it.

**S-8.5** The store path must be added to trusty-search's opt-in allowlist
(`~/.config/trusty-search/indexes.toml`, issue #767) as part of store creation,
or the index cannot be created. This is a real prerequisite step that a design
assuming "just call create_index" would miss.

### 10.4 Two operational hazards, cited from real incidents

**S-8.6 — Root containment.** trusty-search post-filters every search result
whose path escapes the index root (issues #64/#541): the push succeeds, chunks
land in the corpus, and `search` returns nothing. `index_feed.rs:113-118`
(`covers()`) already checks this. **An OKG store's index is usable only when its
root contains the tree**, and a binding that fails the check is refused and
reported, never fed.

**S-8.7 — Reindex root hijack.** A reindex has previously hijacked an index to an
active worktree root and pruned the corpus. A scheduled, unattended refresh makes
this materially worse than an interactive one. Therefore: **the refresh runner
never calls `reindex`.** It calls `index_file` / `remove_file` for the entities
it wrote. A full reindex of an assistant store is an explicit, attended
operation.

**S-8.8 — Orphan indexes.** Dead worktree indexes have leaked and inflated
`fseventsd`. Deleting an assistant instance deletes its index registration, and
the store's index is registered against the stable assistant home — never against
a worktree or temporary path.

### 10.5 Search tiering: OKG first, attached stores as fan-out {#SPEC-OKGSRC-14~draft}

**Owner requirement (2026-08-01):** *"the internal OKG has priority for search,
then attached stores for fan out."*

**S-14.1 — Two tiers, ordered.** Tier 1 is the agent's canonical OKG index. Tier 2
is every attached store index. Tier 1 has priority.

#### What "priority" means — resolved, with reasoning

Two readings produce materially different latency and recall:

| Reading | Behaviour |
|---|---|
| **Short-circuit** | Query tier 1; query tier 2 only if tier 1 did not satisfy the query |
| **Rank-above** | Query all tiers, ordering results so tier 1 outranks tier 2 |

**S-14.2 — Rank-above is normative. Short-circuit is rejected**, and the owner's
own rationale is what settles it: *"Not all useful content in a store will be
entity-extractable."* A query the OKG appears to satisfy may still have better
material in an attached store — precisely the content that yielded no entities.
Short-circuiting would suppress exactly the material that search-only attachment
exists to make reachable (`S-2.12`), and it would do so **invisibly**: the user
would see a plausible answer and never learn a better one was skipped. That
converts the owner's legitimate search-only configuration into a trap.

Recall is the property being protected. Latency is bounded instead by mechanism:

**S-14.3** Tiers are queried **concurrently**, not sequentially, so fan-out costs
roughly the slowest tier rather than their sum.

**S-14.4** Each tier carries a **per-tier result budget and timeout**. A slow or
unreachable attached store degrades to fewer tier-2 results — never to a failed
search, and never to a stalled one.

**S-14.5** An unreachable tier-2 index is reported alongside the results, not
silently dropped. This is DOC-58 KD-13's never-fabricate posture: a search that
quietly covered less than the user believes is worse than one that says so.

**S-14.6** Tier order is **not** a relevance score. Tier 1 outranks tier 2 at
equal-or-comparable relevance; it does not promote a weak OKG match above a
strong attached-store match. Tier is a tie-break and an ordering preference over
comparable results, not an override of the ranker.

### 10.6 Result provenance — a verified gap, not an assumption

Epic #4007's "Done when" includes *"every result identifies its tier and
origin."* The tiering requirement above makes that clause load-bearing: **if tier
1 outranks tier 2, a consumer must be able to tell which tier produced a
result.**

**Verified against `origin/main` — it does not exist.** `VectorSearchTool`
resolves exactly **one** index per call: `effective_index_id`
(`crates/trusty-agents/src/tools/memory/vector_search.rs:221`) returns
`Option<String>`, and `daemon_query` (`:453`) queries that single index. The
agent's OKG index (`default_index_id`, `:75`) and its attached indexes
(`attached_index_ids`, `:80`) are both known to the tool, but they are offered as
**alternatives the caller chooses between**, not tiers queried together. So today:

- there is **no fan-out** — a call reaches one index;
- there is therefore **no tier or origin field on a result**, because provenance
  is implicit in the caller's own choice of `index_id`.

**S-14.7** Fan-out and per-result tier/origin ship **together**. Fan-out without
provenance would merge two tiers into an undifferentiated list at the exact moment
differentiating them starts to matter — and would silently close out #4007's
acceptance criterion rather than satisfy it.

**S-14.8** Every result carries its **tier** (1 or 2) and its **origin** (the
index id, and for a tier-1 result the contributing store where known — the
`source_id` provenance already stamped at ingest,
`crates/trusty-kb/src/okg/ingest.rs:275-276`). Origin composes with the trust
label (§6.3): a consumer can see both where a result came from and whether it is
untrusted-derived.

---

## 11. SPEC-OKGSRC-09 — The Extension Point {#SPEC-OKGSRC-09~draft}

The owner requires that future source types — Notion meeting transcripts,
Fireflies transcripts — drop in without touching the core. **The source-type set
is an extension point, not a fixed enum.**

### 11.1 What blocks that today

`Locator` (`crates/trusty-kb/src/okg/registry.rs:47-80`) is a **closed**,
externally-tagged three-variant enum. Adding Slack means editing `trusty-kb`.
DOC-55 §5.3 already proposes the fix and this document adopts it verbatim rather
than re-deciding it: keep the three existing variants (load-bearing, hand-edited,
already on disk) and add one open variant
`Locator::Connector { kind: String, params: Json }`, with `params` opaque to
`trusty-kb` and validated by the source type.

**S-9.1** The externally-tagged property is preserved: the TOML sub-table name
**is** the kind, so the two can never disagree. An unknown `kind` at ingest time
is a clear "no source type registered for kind X" error, never a silent skip.

### 11.2 What a source type must provide

A source type is DOC-55's `Connector` (`kind` + `list` + `fetch` +
`enumerates_full_corpus` + `ceiling` + `chunk`) plus exactly what this document's
new obligations require:

| Obligation | Provided as | Why it is required here |
|---|---|---|
| **Auth** | A *declaration* of the credential it needs (provider, shape, scope) — resolved by #4040, never held | §7; C8 forbids holding one |
| **Enumerate** | `list(locator, window, max) -> Listing` — descriptors, not bodies | Lets the ledger be consulted before fetch (C3) |
| **Fetch delta** | `fetch(&[ItemRef]) -> Vec<FetchedBlob>` | Called only for items the ledger reports not-current |
| **Normalize to OKF** | `FetchedBlob { bytes, type_hint, fields }` | Extraction to OKF text is DOC-55 §4's job, **not the source type's** — the source type never parses formats |
| **Watermark** | Report the interval actually covered by a completed `list` | §7.2; without it "N more months" cannot be a delta |
| **Schedule defaults** | A suggested interval and its floor | §9.2; a provider knows its own rate limits |
| **Trust posture** | Whether the corpus can be author-constrained, and how | §6.3; must be declared, and defaults to `untrusted-external` |

**S-9.2** A source type provides **no** dedup, tombstoning, entity writing,
watermark *storage*, extraction, credential storage, or scheduling. Those are all
core. This is what makes the extension point real rather than nominal.

### 11.3 Registry shape

**S-9.3** Mirror the house precedent: a family constructor returning trait
objects — `okg_source_types() -> Vec<Arc<dyn SourceType>>`, exactly as
`okg_tools()`, `izzie_tools()`, and `git_tools()` already do — with lookup keyed
on `kind()`. The trait is defined at the network seam, as `IndexFeed` is, so the
whole drive loop is testable against a fake with no daemon and no network.

**S-9.4** Registration is the **only** core edit adding a source type requires.
No match arm in the runner, no enum variant, no registry-format change.

### 11.4 The two named future cases, walked through

**Fireflies (meeting transcripts).** `kind = "fireflies"`; params
`{ identity, months_back }`; credential: API key, declared, resolved by #4040
(the Granola shape, already required by (g)); `list` enumerates meetings in the
window returning descriptors with the meeting id; `item_id` = meeting id;
`fingerprint` = the transcript revision, else `volatile = true`;
`enumerates_full_corpus` = false (windowed); `fetch` returns transcript text with
`type_hint = text/plain`; trust posture `untrusted-external` with no author
constraint possible (S-4.9). **Core edits required: none beyond registration.**

**Notion meeting transcripts.** Not a new source type at all — a Notion
*locator*, since (f) already covers Notion trees. It is `kind = "notion"` with
params naming the transcript database. This is the case that validates S-3.6:
because additional Notion directories are additional source rows, a transcript
database is a row, not a schema change. **Core edits required: none, and no new
source type either.**

Both drop in. That is the test this section had to pass.

---

## 12. SPEC-OKGSRC-10 — Observability: The OKG Sources Sub-surface {#SPEC-OKGSRC-10~draft}

The owner requires that OKG Source extractions be **logged and displayed in an
OKG configuration subpane**.

### 12.1 Where it goes

**S-10.1** Sources are a sub-surface of the **Knowledge** pane (DOC-57
`SPEC-AGENTCFG-03~draft`), not a new top-level pane. Knowledge is already "one
pane over N sub-surfaces" — K-a store bindings, K-b knowledge tools, K-c MCP
knowledge endpoints, and K-d attached indexes (DOC-58). Sources are **K-e**: what
fills K-a. A separate pane would force a user asking "what does this assistant
know?" to correlate across two places, which is the exact failure DOC-57 §8.2 G-2
created the Knowledge pane to fix.

### 12.2 The log

**S-10.2** Each run appends one record to `_sources/<id>.runs.jsonl` — the one
file this document adds to the existing `_sources/` layout. Per record:
run id; trigger (`scheduled` | `manual` | `assistant`); start and end; the window
covered; items listed / fetched / ingested / skipped / errored; index counters
(`indexed`, `removed`, `pending`); the terminal state; and on failure the
**reason**, classified per S-7.7.

**S-10.3** The log is append-only and bounded by retention, in the same idiom as
the item and index journals. It is a display and diagnosis record — **never** an
input to a correctness decision, which stays with the ledger and the watermark.

### 12.3 What the pane shows

**S-10.4** Per source: kind, locator summary, coverage window, schedule and
whether it is enabled, last run outcome with timestamp, next run, credential
state, **and the trust label** (§5.3). The trust label is displayed, not
internal — a user is entitled to see that their assistant's knowledge includes
content other people wrote.

**S-10.5** The counters from §3 are **displayed separately and never summed**:
*ingested by a store* (§3, automatic) and *extracted from a bound store* (§4,
explicit) are different numbers produced by different triggers over different
corpora. Collapsing them into one "items" figure would erase the
§3 distinction on the exact surface where a user forms their mental model of it.

**S-10.6** Index state is surfaced as its own counter, per DOC-55 §7.2.2: "N
documents ingested" alone is not a complete answer, and "ingested but not yet
searchable" is a reportable state, never an invisible one.

**S-10.7 — Never fabricate.** Inherited unchanged from DOC-57 G-4 / DOC-58 KD-13:
loading, empty, and error are three distinct states; a failing source renders
with its reason rather than being hidden; a source with no runs renders an
explicit empty state, not a zero.

### 12.4 The inspection seam needs extending — real work, not an assumption

**S-10.7a** `assistants::health::inspect()` (PR #4523) knows that `okg/` and
`stores/` exist. It knows **nothing** about last-run time, cursors, coverage, or a
failing credential, and `HomeIssue` / `HomeIssueKind` are **filesystem-shaped
today** (`Missing`, `NotADirectory`, `NotAFile`, `Unreadable`, `Malformed`).

`HomeIssueKind` is nonetheless the right home for "this store is stale" and "this
store's credential is broken", because it is the seam the **concierge already
narrates from** — which is what makes #4325's guided-remediation requirement
reachable for stores rather than a second, parallel error channel. But the kinds
must be **extended to carry store-level state**, and that extension is scoped
work this epic carries.

**S-10.7b — Nothing under `stores/<id>/` may break home inspection.** #4523
deliberately made `stores/` inert and pins it: a test writes a corrupt
`stores/some-remote-store/state.json` and asserts the home still reports healthy.
That property is preserved — a malformed store state degrades that store, never
the home.

**S-10.7c** The corollary is the reason `S-10.7a` is not optional: **inspection
will not notice a broken store until something is built to look.** A store whose
credential was revoked six weeks ago is, today, indistinguishable from a healthy
one at the inspection layer. This subsection and §12.2's run log are jointly the
data source the OKG configuration subpane renders from.

### 12.5 Controls

**S-10.8** The pane is where a user **manages** the schedule the owner's
requirement gives them: enable/disable a source, change its interval, extend its
window (S-3.5), add a source, and trigger a run now. Editing writes through the
same registered path as every other write, and remains subject to §7 (a store
needing a credential the authority will not grant cannot be enabled).

**S-10.9** "Extract entities" from a **bound store** is a separate, explicit
control with its own button and its own counter (§4, #4363). It is never
scheduled, never implied by binding, and is declined with a reason for a
store-owned index (S-2.7). Store ingestion (§3) and bound-store extraction (§4)
are labelled distinctly on this surface so a user can always tell which mechanism
produced what. A **search-only** store renders as a stated configuration, never as
pending (S-2.11).

---

## 13. Phased Delivery and Relationship to Existing Work

### 13.1 This document does not duplicate the M2 OKG issues

Five issues already on milestone 22 **are the unbuilt feature**, not polish on
it. This document sequences against them and absorbs none of their scope:

| Issue | What it owns | Relationship |
|---|---|---|
| #3904 (epic) | The universal importer: extraction layer, connector contract, assistant-driven crawl, deterministic CLI — DOC-55 | **Prerequisite.** §5, §8, and §11 build on its `Connector` contract. This document adds the roster, schedule, trust, and observability layers on top. |
| #4283 | Index→OKG entity extraction (the "explicit extraction command") | **Owns §4 in full.** Not re-specified here; §3 constrains it in exactly three ways (S-2.2, S-2.6, S-2.10). |
| #4325 / PR #4523 | Per-assistant home directory and store root | **Owns §2.2's layout.** Q1 is a question *for* it, filed against it. |
| #4007 (epic) | Curated stores vs attached indexes (two-tier) | **Owns §10.2's tier distinction** via DOC-58. Depended on, not restated. |
| #4289 | Index a new directory from the config UI, with overlap guard | **Owns fact (3) of §5.2.** The directory source's K-d half. |
| #4363 | Extract-entities UI trigger | **Owns S-10.9's control.** |

**Any new ticket below that overlaps one of these is a defect in this
decomposition, not a parallel effort.**

### 13.2 Two ticket classes, separated on purpose

- **Plumbing** — mechanism over corpora already reachable, no new external reach.
  Schedulable now.
- **Capability grant** — anything widening what an assistant can reach: a new
  provider, a new credential, a new corpus class. **Gated on #4040 and on the
  §5 boundary landing. Marked blocked, never scheduled.**

### 13.3 Phases

**Phase A — Foundations (plumbing).** The trust label and its carrier (§6.3
S-4.3/S-4.4); fencing lifted to a shared seam and applied to the search path
(S-4.5/S-4.6); the interval watermark (§7.2); the run log and the K-e sub-surface
(§11), read-only. Net effect: **what is already ingested becomes labelled,
fenced, and visible.** No new reach at all.

**Phase B — Scheduled refresh for already-reachable sources (plumbing).** The
per-source schedule field; the refresh runner (§9.3) with failure, overlap, and
staleness; the store's own index created with the store (§10.3). Stores: (a)
`directory`, (b) `gmail`, (c) `drive` — all three reuse credentials this repo
already holds. Net effect: **the sources that work today refresh on a
user-managed schedule.**

**Phase C — The open extension point (plumbing).** `Locator::Connector`; the
`SourceType` trait and registry; existing sources refactored behind it with
registry rows unchanged. Net effect: **adding a source type stops requiring a
core edit.**

**Phase D — New providers (CAPABILITY GRANTS — blocked on #4040).** (d)
`slack-sent`, (e) `slack-channel`, (f) `notion`, (g) `granola`. Each is a
separate grant, each carries its own read-confinement gate (DOC-55 C6), and each
is reviewed against §5 individually. **Not scheduled.**

**Phase E — Future types.** Fireflies, Notion transcripts (§11.4). Drop-in by
construction; each still a capability grant.

---

## 14. Open Questions for the Owner

Genuine forks only. Each states what stays blocked until answered.

**Resolved since the first draft, recorded so they are not re-opened:** the
`assistants/` path segment — the agent name sits **directly** under the home, and
PR #4523 now matches (§2.2); and the config format — **`config.toml`**, no
migration (§2.7).

**Already decided, recorded here so it is not re-opened:** whether scheduled
refresh conflicts with explicit-extraction-only. It does not — the owner resolved
it on 2026-08-01 with *"extraction is an OPTION for any bound store."* OKG
Stores (§3) populate the OKG automatically — by watcher or on a schedule; bound
stores (§4) are extracted only when explicitly asked. This is **not** an open question.

Also settled and recorded in §2.7 so they are not re-raised: **`config.toml`, not
`config.yaml`** (owner correction 2026-07-29, no format migration); the **dotless**
home spelling; and that the home is **app-generated, not access-controlled**.

### Q1 — What is trusty-memory's KG's relationship to the canonical OKG?

**Answered and no longer open:** *which* of the two systems called "OKG" the `okg`
directory is. Your 2026-08-01 canonical model settles it — the **OKF store**,
internal, built, indexed by trusty-search (§2.1).

**Still open, and a different question:** what happens to **trusty-memory's KG**
(`kg_assert` / `kg_query`). Naming the OKF store canonical does not retire,
absorb, or deprecate it, and this document deliberately does not imply that it
does. The relevant facts:

- The two are **separate and unbridged** — verified 2026-07-30 that nothing calls
  between them, and that `kg_query` returns empty for both populated palaces.
- A prior owner decision established that every assistant gets **a memory palace
  by default AND a default OKG**, so both exist by design.

The fork: do they stay two independent systems with distinct jobs (entities built
from stores vs. structured recall), does one feed the other in a defined
direction, or does one eventually subsume the other?

**No recommendation** — a product question about two systems whose intended
division of labour only you can state, and it likely warrants an ADR rather than
an issue-level decision (as #4406 itself suggests).

**Blocked until answered:** nothing in this epic — every ticket here names the OKF
store. What stays blocked is #4406's own closure, and any work bridging the two.

### Q2 — Do you accept the residual prompt-injection risk in §6.5?

With the §5 boundary implemented, an assistant may still be influenced by
attacker-authored text ingested through a legitimate source, fenced as untrusted.
**Fencing is a mitigation, not a guarantee.** The load-bearing control remains
capability reduction — the posture `bundled_personas_pin_git_reach` already pins.

Sources (c), (e), (f), (g) have **no author constraint and cannot have one**, and
SENT-only does **not** fully mitigate (b) and (d) because sent mail quotes
inbound content (§5.4).

**Recommendation:** accept, conditional on Phase A landing before Phase D, and on
every new source type being reviewed as a capability grant. **Blocked until
answered:** Phase D in its entirety. This is flagged for explicit sign-off rather
than absorbed as a design assumption, because it is a security acceptance and
those belong to you.

### Q3 — Should scheduled refresh be a listener, or its own runner?

§8.3 recommends **its own runner**, reusing `listeners::poll`'s *pattern*
(detached task, interval floor, backoff, `enabled = false` default) without its
*semantics* (wake, filter, persona dispatch). The alternative — modelling a
refresh as a listener connector — would inherit the Listeners pane and enable
flag for free, but would force every scheduled refresh to either wake an
assistant (contradicting the mechanical, model-free flow) or be an event that
wakes nobody.

**Blocked until answered:** Phase B's runner ticket. Lower stakes than Q1–Q2 —
recorded because it is a structural choice a reviewer could reasonably reverse,
not because the recommendation is weak.

---

## 15. References

- `crates/trusty-kb/src/okg/` — engine: `ingest.rs` (the `SourceItem` seam,
  provenance frontmatter at `:275-276`), `registry.rs` (`Locator`, `SourceSpec`),
  `ledger.rs`, `docstore.rs`, `policy.rs` (`DocStorePolicy`, the exfiltration
  rationale at `:11-15`)
- `crates/trusty-agents/src/tools/okg/` — the four LLM-only tools
  (`mod.rs:53-66`), `knowledge_dir()` (`mod.rs:92-106`), `index_feed.rs`
- `crates/trusty-agents/src/stores/` — `binding.rs` (`okg_tree_path`),
  `config.rs` (`AgentStoreBinding`, one store per agent), `index_feed.rs` (the
  push contract, `covers()` at `:113-118`)
- `crates/trusty-agents/.trusty-agents/agents/assistant/agent.toml:79-113` — the
  untrusted-content threat model and the two OKG boundaries
- `crates/trusty-agents/src/agents/tests/loading.rs:2200` —
  `bundled_personas_pin_git_reach`
- `crates/trusty-agents/src/tools/l0_exec.rs:8-18` — the L0 execution gate
- `crates/trusty-agents/src/ctrl/pm_task/dispatch/persona_memory.rs:420-533` —
  `UNTRUSTED_PREAMBLE` and the drawer fencing this document reuses
- `crates/trusty-agents/src/listeners/` — `config.rs` (`ListenerConfig`,
  `poll_interval_secs`, the 15s floor), `poll.rs:139-160` (`spawn_listeners`)
- `crates/trusty-search/src/service/watcher_manager.rs` — automatic per-index
  watching; `service/server/router.rs:66-160` — the real `CreateIndexRequest`
- [DOC-55 Universal OKG Importer](./okg-universal-importer.md) — the extraction
  layer and `Connector` contract
- [DOC-57 Five-Section Agent Configuration](./agent-config-five-sections.md) §4,
  §6 — Knowledge and Listeners
- [DOC-58 K-d Attached Search Indexes](./DOC-58-knowledge-kd-attached-indexes.md)
  — the two-tier principle
- [DOC-47 External Event Ingestion](./DOC-47-external-event-ingestion.md) — why a
  content-fetching scheduler is not an `EventSource`
- ADR-0022 (knowledge trees do not sync; a new machine re-ingests from registered
  sources); ADR-0024 (assistants hold authority; sub-agents are in-process leaves
  and must not be given an out-of-process ingestion path)
- `crates/trusty-agents/src/assistants/roster.rs` (PR #4523) — `is_instance()`,
  the rule that excludes `ctrl` and `personal-assistant` from provisioning
- Issues: #4040 (credential authority — consumed), #3904, #4283, #4325 / PR
  #4523, #4007, #4289, #4363, #4406, #4590 (concierge build path), #4591
  (explicit template marker), #4011, #767 (trusty-search allowlist),
  #64 / #541 (index-root post-filtering)

---

## 16. Change Log

| Date | Change |
|---|---|
| 2026-08-01 | Initial draft — store identity and location, source roster, the untrusted-content boundary, #4040 consumption, watermarks, scheduled refresh, trusty-search integration, the source-type extension point, and the K-e observability sub-surface. |
| 2026-08-01 | §3/§4 split per "extraction is an OPTION for any bound store": stores populate automatically, bound-store extraction stays explicit; contamination guard scoped to the latter; the directory overlap case specified. |
| 2026-08-01 | §2.1a added — the build path the owner stated: the concierge offers to build an assistant, or the user does it manually, both producing the same per-agent home with its OKG and stores. Flow design left to #4590. Records that creation precedes provisioning (even though provisioning runs first at boot), and that `ctrl` (the concierge itself) and `personal-assistant` (a template, #4591) must never be provisioned an OKG. |
| 2026-08-01 | Folded in four constraints from PR #4523: stores nest under the single `[[stores]]` binding (§5.5) because `StoresConfig::validate()` warns above one binding; `store_root()` confines the destination, not the source (`S-4.0`); the `HomeIssue`/`HomeIssueKind` inspection seam needs extending to carry store-level state (§12.4); and **credentials must never be written into the browsable assistant home** (§7.1a), with the five things OKG Sources needs from #4040 stated as input (§7.1b). Path-segment question resolved and removed; questions renumbered Q1–Q3. |
| 2026-08-01 | Restructured around the owner's canonical model — one built OKG per agent, populated by many stores of two kinds (`SPEC-OKGSRC-12~draft`); "store" adopted as the single canonical noun with "OKG Source" recorded as an earlier synonym; bound-store extraction split into its own section (`SPEC-OKGSRC-13~draft`). |
| 2026-08-01 | §3.4 added: searchable-corpus and OKG-contributor capacities are orthogonal, and search-only is a complete end state, never pending. §10.5–§10.6 added: OKG-first search tiering with attached-store fan-out, and per-result tier/origin provenance (`SPEC-OKGSRC-14~draft`), verified absent today. |
| 2026-08-01 | §2.4–§2.7 added: `stores/<store-identifier>/` extraction targets for remote stores with the local/remote asymmetry made explicit (`SPEC-OKGSRC-11~draft`); `${TRUSTY_AGENTS_HOME}` recorded as one system value with migration deferred; config-format, dotless, and not-access-controlled recorded as settled. |
