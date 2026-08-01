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

# DOC-62 — OKG Sources: Per-Assistant Knowledge Sources, Scheduled Refresh, and the Untrusted-Content Boundary

**Status:** Draft
**Spec ID:** `SPEC-OKGSRC-01~draft` … `SPEC-OKGSRC-10~draft` (DOC-62)
**Subsystem:** trusty-agents — assistant home / OKG store, source catalog, scheduled refresh, credentials consumption, Knowledge config pane; trusty-kb — `okg` engine (ledger, registry, entity tree); trusty-search — index over the store
**Owner:** Engineering (trusty-agents) / Bob Matsuoka
**Last-updated:** 2026-08-01
**DOC-N claim:** `DOC-62`, scan-before-claim per DOC-38 §4.1. Verified free by scanning every filename claim and header self-label under `docs/specs/**` and `docs/trusty-installer/research/02-design/**`: the highest claimed number is `DOC-61` (Canonical Agent Standard). `DOC-55` is claimed by `okg-universal-importer.md` via self-label only (its filename carries no number), which is why a filename scan alone is insufficient. Matches the "next free `DOC-N` = `DOC-62`" hint in `docs/specs/README.md`; `scripts/check_doc_numbers.sh` was green before and after this document landed.
**Builds on:** DOC-55 [Universal OKG Importer](./okg-universal-importer.md) `SPEC-OKGIMPORT-03~draft`/`-04~draft` (the extraction layer and the `Connector` contract this document does **not** re-specify); DOC-57 [Five-Section Agent Configuration](./agent-config-five-sections.md) `SPEC-AGENTCFG-03~draft` (the Knowledge section this document adds a sub-surface to) and `SPEC-AGENTCFG-05~draft` (Listeners, the existing poll machinery); DOC-58 [K-d Attached Search Indexes](./DOC-58-knowledge-kd-attached-indexes.md) `SPEC-KDIDX-01~draft` (the two-tier curated-vs-attached principle this document depends on)
**Related issues:** #4325 (per-assistant home directory — **in flight, PR #4523**); #3904 (epic: universal assistant-driven OKG importer); #4283 (index→OKG entity extraction); #4007 (epic: curated stores vs attached indexes); #4289 (index a new directory from the config UI); #4363 (extract-entities UI trigger); #4406 (which store is canonical — superseded in framing by §2 here); #4040 (epic: unified credential authority — this document is a **consumer**, never a parallel mechanism)

---

## 1. Executive Summary

An assistant instance owns a directory of knowledge — its **OKG store** — and that
store is built and kept current from **OKG Sources**: a local directory, Gmail,
Drive, Slack, Notion, Granola, and whatever comes next. This document specifies
the source catalog, the scheduled refresh, the credential dependency, the
observability surface, and — most importantly — **the boundary that keeps
ingested third-party content from being read as instruction**.

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

**Second: "OKG" names two unconnected stores today, and this document picks
one.** §2 states it unambiguously so downstream tickets stop building against
different targets. This is the substance of #4406.

**Third: an OKG store has two population paths, and they are specified
separately.** Per the owner (2026-08-01), *"extraction is an OPTION for any bound
store."* **Path 1 — OKG Sources** are configured pipelines over the seven kinds
in §4; they refresh **automatically** on a user-managed schedule. **Path 2 —
bound stores** are any index the assistant binds; pulling entities out of one is
an **option invoked by an explicit command**, never automatic. The paths differ
by source kind, not by processing stage, and §3 keeps them first-class and
distinct rather than unifying them behind a flag. The contamination guard belongs
to path 2 alone (§3.2), and §3.3 specifies the deliberate overlap case where one
directory is both.

The genuinely new surface area here, relative to DOC-55, is: the named source
roster and its per-source constraints (§4); the credential dependency on #4040
(§6); **scheduled refresh, which does not exist in any form** (§8); the
untrusted-content boundary (§5); the source-type registry as an open extension
point (§10); and the Sources sub-surface of the Knowledge pane (§11).

---

## 2. SPEC-OKGSRC-01 — The Store: Which "OKG", and Where {#SPEC-OKGSRC-01~draft}

### 2.1 Which store (settles #4406's framing)

**Normative.** The `okg` directory specified here is the **trusty-kb markdown
entity tree** — the store written by `KbStore::put_entity`
(`crates/trusty-kb/src/okg/mod.rs`) and populated by the `okg_ingest_*` tools.
It is **not** the trusty-memory knowledge graph behind `kg_assert` / `kg_query`
(`crates/trusty-memory/src/service/core_kg.rs`).

The owner's phrasing — "an OKF store indexed by trusty-search" — settles this on
its own: a trusty-search index is built over a **filesystem tree**, and the
trusty-memory KG is not a filesystem tree. Stating it explicitly is the point.
#4406 verified that the two stores are unbridged, that nothing calls between
them, and that `kg_query` returns empty for both populated palaces. Any bridge
between them is **out of scope here** and remains #4406's question.

**S-1.1** Every ticket descending from this document names the trusty-kb entity
tree as its target. A ticket that means the trusty-memory KG is not descended
from this document and must say so.

### 2.2 Where the store lives — and a conflict that must be resolved now

The owner's 2026-08-01 statement places the store at:

```
<trusty-agents home> / <agent name> / okg
```

and is explicit that **the agent name sits directly under the home, with no
intervening `assistants` segment**.

Two facts collide with that, and both are load-bearing:

1. **Today's path is neither.** `knowledge_dir()`
   (`crates/trusty-agents/src/tools/okg/mod.rs:93-95`) resolves
   `$KB_KNOWLEDGE_DIR`, else `<home>/.trusty-agents/knowledge`, and the tree is
   `<knowledge_dir>/<slug(agent)>` — a flat, dotted, shared pool addressed by
   naming convention, with no `okg` segment at all. Moving to the owner's layout
   is a **migration of existing trees**, not a greenfield choice.

2. **In-flight PR #4523 (#4325) builds a *different* path.** It defines
   `ASSISTANTS_DIR_NAME = "trusty-agents"` and `ASSISTANTS_SUBDIR = "assistants"`,
   resolving to `~/trusty-agents/assistants/<instance>/okg`. Its own test asserts
   `root.join("izzie").join(OKG_DIR)` under an `assistants` root. That PR is open
   as this document is written.

The difference is one constant. The cost of getting it wrong is a second
migration of user-visible, user-editable directories — which #4325 explicitly
designs for humans to browse and edit. **This document does not choose.** See
**Q1** (§13). Until Q1 is answered, no ticket may land a path change, and #4523
is the incumbent.

**S-1.2** Whatever segment set Q1 selects, the layout obligations are fixed:
the store is per **instance** (not per type), dotless and user-browsable, and
`ensure()`-style creation is additive and never overwrites a user edit — the
posture PR #4523 already implements and this document adopts unchanged.

### 2.3 What the store contains

```
<assistant home>/
├── instructions.md
├── config.toml            # or config.yaml — see Q2
├── agents/
├── okg/                   # THIS document's subject
│   ├── <collection>/…     # entity + document markdown, the trusty-kb tree
│   └── _sources/
│       ├── registry.toml        # SourceSpec rows (DOC-55 §2.1)
│       ├── <source-id>.jsonl    # append-only item ledger
│       ├── <source-id>.index.jsonl  # index-feed journal
│       └── <source-id>.runs.jsonl   # NEW — the extraction log (§11)
└── attachments/
```

`_sources/` is the existing DOC-55 layout, unchanged. This document adds exactly
one file to it: the per-source **run log** (§11.2), which is what makes
extractions displayable rather than merely having happened.

---

## 3. SPEC-OKGSRC-02 — Two Population Paths {#SPEC-OKGSRC-02~draft}

An OKG store is populated by **two distinct paths**, distinguished by the **kind
of source**, not by processing stage. The owner's formulation (2026-08-01):
**"extraction is an OPTION for any bound store."** The two paths differ in
trigger, in ownership, and in contamination risk, and this document specifies
them as first-class and separate. They are deliberately **not** unified into one
pipeline with a flag.

### 3.1 The two paths

| | **Path 1 — OKG Sources** | **Path 2 — Bound stores** |
|---|---|---|
| What it is | A configured pipeline over an external corpus | Any search store or index the assistant binds |
| The seven kinds | directory, Gmail (sent, dated), Drive directory, Slack sent, Slack channel, Notion directory, Granola | Not a source kind at all — an existing index |
| Trigger | **Automatic**, on a user-managed schedule | **Explicit command only** — never automatic, never a side effect |
| Optionality | Configured once, then runs | Extraction is an **option** available for the bound store |
| Output | Lands in the `okg` store and is indexed by trusty-search | Entities pulled OUT of the bound store INTO the OKG |
| Governing statement | 2026-08-01 OKG Sources | 2026-07-29 assistant store model |
| Owned by | §4–§9 of this document | #4283, framed by §3.3 here |

**There is no tension between the two decisions.** "Never as a side effect"
governs pulling entities out of a corpus the user bound for reading; "refreshed
automatically on a schedule" governs pipelines the user configured for exactly
that purpose. Automatic is the whole point of path 1; explicit is the whole point
of path 2.

**S-2.1** Path 1 pushes automatically on its schedule. Configuring an OKG Source
**is** the user's instruction to keep it current; no further per-run consent is
required, and requiring one would defeat the feature.

**S-2.2** Path 2 extracts **only when explicitly asked**. Binding a store never
extracts from it, and no schedule, refresh, or background job may invoke path-2
extraction. This document specifies no automatic path-2 trigger and forbids one.

### 3.2 Why path 2 must stay explicit — the contamination guard

The guard belongs to **path 2 only**, and the reason is mechanical rather than
stylistic.

**Verified against `origin/main` (4e67493b).** `feed_bound_index`
(`crates/trusty-agents/src/tools/okg/index_feed.rs:32-50`) runs at the tail of
every OKG ingest and pushes what that run wrote into **the agent's bound search
index** — resolved by `resolve_feed` → `crate::stores::bound_index_for_tree`
(`:53-77`) and pushed through `feed_source`/`HttpIndexFeed`. So the binding is
**bidirectional in effect**: an index bound for reading also receives OKG output.

The consequence, stated plainly: if extraction from a bound store were automatic,
binding a **populated** corpus — an existing code index, a document index such as
the 201k-chunk `cto-duetto` index — would cause generated entities to be mixed
back into a store the user never intended to mutate. The user bound it to *read*
it.

**S-2.3** Explicit-only extraction is the mitigation for that hazard, and it is
the whole mitigation. A path-2 extraction is a deliberate act against a named
store, at a moment the user chose.

**S-2.4** Path 1 does not carry this hazard and is not constrained by it. An OKG
Source's output lands in the assistant's **own** store and its **own** index
(§9) — a tree the product generated and owns — never in a third-party corpus the
user bound for reading.

### 3.3 The overlap case: a directory that is both

Source kind (a) is deliberately **both**: an arbitrary directory registered as an
OKG Source is *also* attached to the assistant as a project and *also* indexed by
trusty-search (§4.2). That one directory is therefore simultaneously a path-1
source and a path-2 bindable index. The owner's model implies this edge and it
must not be left to chance.

**S-2.5 — Path 1 owns a directory it ingests; path 2 must not re-ingest it.**
When a directory is registered as an OKG Source, its trusty-search index is
recorded as **path-1 owned**. Offering path-2 extraction over that index would
re-derive, from chunks, content the ledger already ingested from the files — a
second copy of the same corpus, arriving by a second route, with a different
`item_id` scheme and therefore invisible to the ledger's dedup.

**S-2.6** The surface therefore **declines path-2 extraction for a path-1-owned
index and says why**, rather than silently succeeding. The user is told the
directory is already an OKG Source and that its content is already flowing.

**S-2.7** A **non-owned** bound index — one the user attached that no OKG Source
produced — remains fully eligible for path-2 extraction. S-2.5 narrows the offer
to exactly the double-ingest case; it does not restrict bound-store extraction
generally.

**S-2.8** Ownership is a property of the **index registration**, recorded when
the directory source is registered (§4.2's single all-or-none operation), so the
two paths cannot disagree about who owns a corpus.

### 3.4 What this document does and does not specify

**S-2.9** This document specifies **path 1 in full**: the roster (§4), the trust
boundary (§5), credentials (§6), watermarks (§7), scheduling (§8), indexing (§9),
the extension point (§10), and observability (§11).

**S-2.10** **Path 2 is #4283's**, and is not re-specified here. This document
constrains it in exactly three ways — it stays explicit (S-2.2), it declines
path-1-owned indexes (S-2.6), and its results are counted and displayed
separately from path-1 ingestion (§11) so a user can always tell which path
produced what.

---

## 4. SPEC-OKGSRC-03 — The Source Roster {#SPEC-OKGSRC-03~draft}

A **source** is one registered binding of an assistant's OKG store to an
external corpus. Sources are rows in `_sources/registry.toml` (DOC-55 §5.3); a
source type is the `kind` in the open `Locator::Connector` variant. This section
specifies the seven named types and their per-type constraints; §10 specifies how
an eighth arrives.

### 4.1 The roster

| # | Type | Locator params | Windowed? | Constraint (normative) |
|---|---|---|---|---|
| a | `directory` | `path`, `extensions`, `recursive` | no — full corpus | Also attached to the assistant as a **project** and indexed by trusty-search. See §4.2. |
| b | `gmail` | `identity`, `months_back`, `query` | **yes** | **SENT messages only** (§4.3). Idempotent, date-bounded, extendable by N more months. |
| c | `drive` | `identity`, `folder_id`, `recursive` | no, iff fully recursive | Additional directories are additional sources, not a widened one. |
| d | `slack-sent` | `workspace`, `months_back` | **yes** | **SENT messages only** (§4.3). |
| e | `slack-channel` | `workspace`, `channel`, `months_back` | yes | One source per channel. Additional channels are additional sources. |
| f | `notion` | `workspace`, `page_or_database_id`, `recursive` | no, iff enumerable | Additional Notion directories are additional sources. |
| g | `granola` | `identity` (API key ref), `months_back` | yes | Meeting transcripts. Credential is an **API key**, not OAuth — §6. |

Every one of these is a DOC-55 `Connector` (`kind` + `list` + `fetch`) and
inherits `C1`…`C8` unchanged. **This document adds no dedup, no tombstoning, and
no entity-writing semantics** — that would be a second implementation of
machinery DOC-55 already owns.

### 4.2 (a) is two bindings, and the duality is the requirement

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

### 4.3 SENT-only: what it is and what it is not

Gmail (b) and Slack (d) are constrained to the user's **own outbound** content.
This is simultaneously a signal-quality choice (outbound writing is the best
available proxy for how the user thinks and what they commit to) and a **partial
security mitigation** (§5.4). It is not a full mitigation, and §5.4 says why.

**S-3.3** The SENT-only constraint is enforced **in the connector's `list`, at
the query**, not by filtering after fetch. Gmail: `in:sent`. Slack: the
authenticated user as author. A post-fetch filter would mean untrusted inbound
content was fetched, extracted, and held in memory before being discarded, and
one bug away from being written.

**S-3.4** (c) `drive`, (e) `slack-channel`, (f) `notion`, and (g) `granola` have
**no equivalent constraint and cannot have one** — a Drive folder, a channel, a
Notion tree, and a meeting transcript are all inherently multi-author. §5 governs
them.

### 4.4 Extension of an existing source

The owner requires all extractions to be extendable: N more months of Gmail,
more Drive directories, more Slack channels, more Notion directories.

**S-3.5** Extending a **window** (b, d, e, g: `months_back` grows) is an
`upsert` of the same source row — the DOC-55 additive path, preserving
`added_at` and the ledger — and MUST fetch only the delta. §7.2 specifies the
watermark state that makes "only the delta" true.

**S-3.6** Extending **coverage** (c, e, f: another directory, channel, or page)
is a **new source row**, never a mutation of an existing one. Rationale: each
gets its own ledger, its own watermark, its own schedule, its own credential
scope, and its own failure state. A composite source hides which half failed.

---

## 5. SPEC-OKGSRC-04 — The Untrusted-Content Boundary {#SPEC-OKGSRC-04~draft}

**This is the most important section in this document.** Every remote source in
§4 ingests content the user did not author and does not control, into a store an
assistant then reads as knowledge. That is a prompt-injection surface **by
construction**, not by accident, and the surface widens with every source type
added.

### 5.1 This repo already has an active threat model on exactly this shape

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

**S-4.1** OKG Sources inherits this threat model in full. No source type may be
added that widens an assistant's reach without the capability-grant review §12
separates out.

### 5.2 What exists today, and the one thing that does not

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

### 5.3 Normative requirements

**S-4.3 — Every ingested item is labelled at write time.** Every document
written by an OKG source carries, in its frontmatter, `source_id`, `source_kind`,
`ingested_at` (all three exist today) **plus a new `trust` field** with the value
`untrusted-external` for every source type in §4 except a `directory` source
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
reviewed as a capability grant** (§12), and any ticket that both adds a source
and widens tool reach is split into two.

### 5.4 SENT-only: an analysis, not a claim of sufficiency

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
unambiguously and irreducibly untrusted, and §5.3 is the whole of their defence.

### 5.5 Residual risk, stated for the owner rather than papered over

With S-4.3…S-4.9 implemented, the residual risk is:

> An assistant may quote, summarise, or act on attacker-authored text that was
> ingested through a legitimate source, fenced as untrusted, and nonetheless
> influenced the model's behavior.

**This risk is not eliminated by this design and cannot be.** What the design
does is bound the blast radius: the assistant holds no cross-project git
primitive, no shell reach, and no write-capable tool it did not already hold
before ingestion. **Accepting this residual risk is an owner decision**, and it
is Q3 in §13 rather than an assumption buried in a design section.

---

## 6. SPEC-OKGSRC-05 — Credentials: A Consumer of Epic #4040 {#SPEC-OKGSRC-05~draft}

### 6.1 The rule

Gmail, Drive, Slack, Notion, and Granola all require credentials. **OKG Sources
designs none of it.** Epic #4040 — "unified credential authority, delivery, and
audit" — owns principal and credential identity, ACL/scoping, default-deny,
revocation, the audit trail, and secure delivery. This document specifies OKG
Sources as a **consumer** of that authority.

**S-5.1** No OKG source type reads a token file, holds a secret, or implements a
resolution order. This restates DOC-55's connector obligation **C8** verbatim in
force. A source type receives an already-resolved, already-scoped client handle
from the credential authority and nothing else.

**S-5.2** A second credential mechanism introduced for any source in §4 is a
defect under this repo's common-entry-point rule, and is rejected at review
regardless of expedience.

### 6.2 What OKG Sources would otherwise duplicate — the concrete list

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

### 6.3 Two honesty requirements inherited from today's state

**S-5.3 — Scope truth.** The base persona's own comment
(`agent.toml:105-113`) records that the Google-backed ingest tools reuse
**write-capable** OAuth grants, not read-only ones, and that they return `None`
from `ToolExecutor::scope()` so **they are not scope-gated at all**. An OKG
source performs only reads and MUST be delivered a read-scoped credential by
#4040. Where a provider cannot issue one, the surface says so explicitly rather
than implying a read-only grant that does not exist — the error the comment
itself calls out as having previously been claimed and been false.

**S-5.4 — Revocation is observable.** When #4040 revokes or expires a
credential, the affected sources move to a named, displayed failure state (§11)
and their schedules stop. A scheduled source silently failing forever on an
expired token is the failure mode this clause exists to prevent.

**S-5.5 — Ordering.** Every source type requiring a credential this repo does not
already hold (Slack, Notion, Granola) is **blocked on #4040** and is not
scheduled. §12 marks these explicitly. Sources reusing credentials already held
(directory, Gmail, Drive) may proceed as plumbing.

---

## 7. SPEC-OKGSRC-06 — Idempotency, Watermarks, and Incremental Extraction {#SPEC-OKGSRC-06~draft}

"Idempotent, date-bounded, extendable by N more months" implies two properties
that must be made true by durable state, not by hope: re-running must not
duplicate, and extending the window must fetch only the delta.

### 7.1 Reuse, do not reinvent

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

### 7.2 The watermark: where "only the delta" lives

**S-6.2** Each source row carries a durable **watermark** in `_sources/`,
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

### 7.3 Durability of the source registry

ADR-0022 (`docs/adr/0022-knowledge-tree-sync-model.md`) decides that knowledge
trees do **not** sync with agent config, and that a new machine **re-ingests from
registered sources**. That makes the source registry and its watermarks a
first-class durability requirement rather than a cache.

**S-6.6** `_sources/registry.toml` and the watermarks are the reproducibility
record. A new machine with the same registry and the same credentials
reconstructs an equivalent store. Nothing about a store may be recoverable only
from the tree.

---

## 8. SPEC-OKGSRC-07 — Scheduled Refresh {#SPEC-OKGSRC-07~draft}

The owner's requirement: all sources are refreshed on **a schedule the user
manages**, so new mail, messages, transcripts and folder docs land in the store
and are indexed automatically.

**Nothing of this exists.** Verified: there is no cron parser, no job scheduler,
and no job registry anywhere in the workspace — grep for `cron`, `job_scheduler`,
`clokwerk`, `tokio-cron` across every `Cargo.toml` returns zero hits. DOC-55 §6.4
states the consequence outright: *"A crawl cannot be scripted, cron'd, or run in
CI."*

### 8.1 Where schedules live

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

### 8.2 Granularity, and the case against cron

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

### 8.3 Which mechanism runs it — reuse the pattern, not the module

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

### 8.4 Failure

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

### 8.5 Overlap

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

## 9. SPEC-OKGSRC-08 — trusty-search Integration {#SPEC-OKGSRC-08~draft}

The owner's framing is "indexed by trusty-search … standard file-watching
behavior." Verified against the real API, that is **half right**, and the half
that is wrong matters.

### 9.1 What is actually true

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

### 9.2 Index cardinality

**S-8.2** **One index per assistant store**, not one per source. The store is one
tree; sources are rows within it; a search over "what this assistant knows" is
one query. Per-source indexes would multiply index count by source count, fragment
every query, and multiply the leak surface named in §9.4 below.

**S-8.3** This is the **K-a curated tier** (DOC-58 `SPEC-KDIDX-01~draft`). A
`directory` source's own index (§4.2 fact 3) is a **K-d attached index** and a
different object. KD-1 holds: the two lists are never the same list, and a
surface showing an id in both dedups it and presents it once under its K-a role.

### 9.3 Creation is not automatic today, and that is a gap this spec must own

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

### 9.4 Two operational hazards, cited from real incidents

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

---

## 10. SPEC-OKGSRC-09 — The Extension Point {#SPEC-OKGSRC-09~draft}

The owner requires that future source types — Notion meeting transcripts,
Fireflies transcripts — drop in without touching the core. **The source-type set
is an extension point, not a fixed enum.**

### 10.1 What blocks that today

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

### 10.2 What a source type must provide

A source type is DOC-55's `Connector` (`kind` + `list` + `fetch` +
`enumerates_full_corpus` + `ceiling` + `chunk`) plus exactly what this document's
new obligations require:

| Obligation | Provided as | Why it is required here |
|---|---|---|
| **Auth** | A *declaration* of the credential it needs (provider, shape, scope) — resolved by #4040, never held | §6; C8 forbids holding one |
| **Enumerate** | `list(locator, window, max) -> Listing` — descriptors, not bodies | Lets the ledger be consulted before fetch (C3) |
| **Fetch delta** | `fetch(&[ItemRef]) -> Vec<FetchedBlob>` | Called only for items the ledger reports not-current |
| **Normalize to OKF** | `FetchedBlob { bytes, type_hint, fields }` | Extraction to OKF text is DOC-55 §4's job, **not the source type's** — the source type never parses formats |
| **Watermark** | Report the interval actually covered by a completed `list` | §7.2; without it "N more months" cannot be a delta |
| **Schedule defaults** | A suggested interval and its floor | §8.2; a provider knows its own rate limits |
| **Trust posture** | Whether the corpus can be author-constrained, and how | §5.3; must be declared, and defaults to `untrusted-external` |

**S-9.2** A source type provides **no** dedup, tombstoning, entity writing,
watermark *storage*, extraction, credential storage, or scheduling. Those are all
core. This is what makes the extension point real rather than nominal.

### 10.3 Registry shape

**S-9.3** Mirror the house precedent: a family constructor returning trait
objects — `okg_source_types() -> Vec<Arc<dyn SourceType>>`, exactly as
`okg_tools()`, `izzie_tools()`, and `git_tools()` already do — with lookup keyed
on `kind()`. The trait is defined at the network seam, as `IndexFeed` is, so the
whole drive loop is testable against a fake with no daemon and no network.

**S-9.4** Registration is the **only** core edit adding a source type requires.
No match arm in the runner, no enum variant, no registry-format change.

### 10.4 The two named future cases, walked through

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

## 11. SPEC-OKGSRC-10 — Observability: The OKG Sources Sub-surface {#SPEC-OKGSRC-10~draft}

The owner requires that OKG Source extractions be **logged and displayed in an
OKG configuration subpane**.

### 11.1 Where it goes

**S-10.1** Sources are a sub-surface of the **Knowledge** pane (DOC-57
`SPEC-AGENTCFG-03~draft`), not a new top-level pane. Knowledge is already "one
pane over N sub-surfaces" — K-a store bindings, K-b knowledge tools, K-c MCP
knowledge endpoints, and K-d attached indexes (DOC-58). Sources are **K-e**: what
fills K-a. A separate pane would force a user asking "what does this assistant
know?" to correlate across two places, which is the exact failure DOC-57 §8.2 G-2
created the Knowledge pane to fix.

### 11.2 The log

**S-10.2** Each run appends one record to `_sources/<id>.runs.jsonl` — the one
file this document adds to the existing `_sources/` layout. Per record:
run id; trigger (`scheduled` | `manual` | `assistant`); start and end; the window
covered; items listed / fetched / ingested / skipped / errored; index counters
(`indexed`, `removed`, `pending`); the terminal state; and on failure the
**reason**, classified per S-7.7.

**S-10.3** The log is append-only and bounded by retention, in the same idiom as
the item and index journals. It is a display and diagnosis record — **never** an
input to a correctness decision, which stays with the ledger and the watermark.

### 11.3 What the pane shows

**S-10.4** Per source: kind, locator summary, coverage window, schedule and
whether it is enabled, last run outcome with timestamp, next run, credential
state, **and the trust label** (§5.3). The trust label is displayed, not
internal — a user is entitled to see that their assistant's knowledge includes
content other people wrote.

**S-10.5** The counters from §3 are **displayed separately and never summed**:
*ingested by an OKG Source* (path 1, scheduled) and *extracted from a bound
store* (path 2, explicit) are different numbers produced by different triggers
over different corpora. Collapsing them into one "items" figure would erase the
§3 distinction on the exact surface where a user forms their mental model of it.

**S-10.6** Index state is surfaced as its own counter, per DOC-55 §7.2.2: "N
documents ingested" alone is not a complete answer, and "ingested but not yet
searchable" is a reportable state, never an invisible one.

**S-10.7 — Never fabricate.** Inherited unchanged from DOC-57 G-4 / DOC-58 KD-13:
loading, empty, and error are three distinct states; a failing source renders
with its reason rather than being hidden; a source with no runs renders an
explicit empty state, not a zero.

### 11.4 Controls

**S-10.8** The pane is where a user **manages** the schedule the owner's
requirement gives them: enable/disable a source, change its interval, extend its
window (S-3.5), add a source, and trigger a run now. Editing writes through the
same registered path as every other write, and remains subject to §6 (a source
needing a credential the authority will not grant cannot be enabled).

**S-10.9** "Extract entities" from a **bound store** is a separate, explicit
control with its own button and its own counter (§3 path 2, #4363). It is never
scheduled, never implied by binding, and is declined with a reason for a
path-1-owned index (S-2.6). Path-1 ingestion and path-2 extraction are labelled
distinctly on this surface so a user can always tell which path produced what.

---

## 12. Phased Delivery and Relationship to Existing Work

### 12.1 This document does not duplicate the M2 OKG issues

Five issues already on milestone 22 **are the unbuilt feature**, not polish on
it. This document sequences against them and absorbs none of their scope:

| Issue | What it owns | Relationship |
|---|---|---|
| #3904 (epic) | The universal importer: extraction layer, connector contract, assistant-driven crawl, deterministic CLI — DOC-55 | **Prerequisite.** §4, §7, and §10 build on its `Connector` contract. This document adds the roster, schedule, trust, and observability layers on top. |
| #4283 | Index→OKG entity extraction (the "explicit extraction command") | **Owns path 2 of §3** in full. Not re-specified here; §3 constrains it in exactly three ways (S-2.2, S-2.6, S-2.10). |
| #4325 / PR #4523 | Per-assistant home directory and store root | **Owns §2.2's layout.** Q1 is a question *for* it, filed against it. |
| #4007 (epic) | Curated stores vs attached indexes (two-tier) | **Owns §9.2's tier distinction** via DOC-58. Depended on, not restated. |
| #4289 | Index a new directory from the config UI, with overlap guard | **Owns fact (3) of §4.2.** The directory source's K-d half. |
| #4363 | Extract-entities UI trigger | **Owns S-10.9's control.** |

**Any new ticket below that overlaps one of these is a defect in this
decomposition, not a parallel effort.**

### 12.2 Two ticket classes, separated on purpose

- **Plumbing** — mechanism over corpora already reachable, no new external reach.
  Schedulable now.
- **Capability grant** — anything widening what an assistant can reach: a new
  provider, a new credential, a new corpus class. **Gated on #4040 and on the
  §5 boundary landing. Marked blocked, never scheduled.**

### 12.3 Phases

**Phase A — Foundations (plumbing).** The trust label and its carrier (§5.3
S-4.3/S-4.4); fencing lifted to a shared seam and applied to the search path
(S-4.5/S-4.6); the interval watermark (§7.2); the run log and the K-e sub-surface
(§11), read-only. Net effect: **what is already ingested becomes labelled,
fenced, and visible.** No new reach at all.

**Phase B — Scheduled refresh for already-reachable sources (plumbing).** The
per-source schedule field; the refresh runner (§8.3) with failure, overlap, and
staleness; the store's own index created with the store (§9.3). Sources: (a)
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

**Phase E — Future types.** Fireflies, Notion transcripts (§10.4). Drop-in by
construction; each still a capability grant.

---

## 13. Open Questions for the Owner

Genuine forks only. Each states what stays blocked until answered.

**Already decided, recorded here so it is not re-opened:** whether scheduled
refresh conflicts with explicit-extraction-only. It does not — the owner resolved
it on 2026-08-01 with *"extraction is an OPTION for any bound store."* OKG
Sources (path 1) push automatically on a schedule; bound stores (path 2) extract
only when explicitly asked. §3 specifies both. This is **not** an open question.

### Q1 — Is there an `assistants/` path segment, or not?

Your 2026-08-01 statement is explicit that the agent name sits **directly** under
the trusty-agents home, with no intervening `assistants` segment. **In-flight PR
#4523 builds the opposite**: `ASSISTANTS_DIR_NAME = "trusty-agents"` +
`ASSISTANTS_SUBDIR = "assistants"` → `~/trusty-agents/assistants/<instance>/okg`.
Today's shipped path is a third thing entirely
(`~/.trusty-agents/knowledge/<slug>`).

**No recommendation** — this is a product decision about a user-facing directory,
and both readings are coherent (a namespace segment leaves room for other object
kinds under the home; its absence is shorter and is what you specified). The
decision is a one-constant change **now** and a second migration of
user-editable directories **later**.

**Blocked until answered:** any path change. #4523 is the incumbent and should
not be re-pointed on a spec's authority.

### Q2 — `config.toml` or `config.yaml` in the assistant home?

The 2026-07-29 decision wrote `config.yaml`. PR #4523 built `config.toml`,
citing an owner clarification of the same date and matching every other agent
config in the repo. The provenance of the clarification is not independently
verifiable from the tree.

**Recommendation:** confirm **TOML** — it is what shipped, it matches
`agent.toml`, and a format split inside one home directory is a durable papercut.
Flagged rather than assumed because a YAML/TOML change was implied and never
confirmed. **Blocked until answered:** nothing material; this is a
confirm-or-correct.

### Q3 — Do you accept the residual prompt-injection risk in §5.5?

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

### Q4 — Should scheduled refresh be a listener, or its own runner?

§8.3 recommends **its own runner**, reusing `listeners::poll`'s *pattern*
(detached task, interval floor, backoff, `enabled = false` default) without its
*semantics* (wake, filter, persona dispatch). The alternative — modelling a
refresh as a listener connector — would inherit the Listeners pane and enable
flag for free, but would force every scheduled refresh to either wake an
assistant (contradicting the mechanical, model-free flow) or be an event that
wakes nobody.

**Blocked until answered:** Phase B's runner ticket. Lower stakes than Q1–Q3 —
recorded because it is a structural choice a reviewer could reasonably reverse,
not because the recommendation is weak.

---

## 14. References

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
- Issues: #4040 (credential authority — consumed), #3904, #4283, #4325 / PR
  #4523, #4007, #4289, #4363, #4406, #4011, #767 (trusty-search allowlist),
  #64 / #541 (index-root post-filtering)

---

## 15. Change Log

| Date | Change |
|---|---|
| 2026-08-01 | Initial draft — store identity and location, source roster, the untrusted-content boundary, #4040 consumption, watermarks, scheduled refresh, trusty-search integration, the source-type extension point, and the K-e observability sub-surface. Five open questions raised for owner sign-off. |
