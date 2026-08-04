# 0027. Rooms become a real, named, enumerable entity; Wing becomes the scope boundary; Closet is an index, not a level

- **Status:** Proposed
- **Date:** 2026-08-04
- **Scope:** crates `trusty-common` (`memory_core`), `trusty-memory` (MCP + HTTP
  + service layer); read-side impact on `trusty-mpm` (TUI health panel) and the
  `trusty-memory` Svelte UI
- **Reversibility Cost:** **High** — this introduces a persistent redb table and
  an id-minting rule over 94 live palaces holding 1.4 GB of real memory data.
  The mitigation designed in below is that the change is **additive-only**: it
  writes a new table and never rewrites a `DRAWERS` row, so a rollback is
  "stop reading the new table", not a data migration. Reversing the *semantics*
  (what a Room means, what a Wing is for) after callers depend on them is the
  expensive half.
- **Decision Drivers:** two of four documented hierarchy levels have no
  implementation (`palace.rs:1`); `Wing` has zero construction sites anywhere in
  the workspace; `Room` has no backing table and its identity is a 16-byte
  XOR-fold of a `Debug` string (`retrieval/layers.rs:49-58`) with provable
  structured collisions; the only user-visible "wing" number in the product is
  actually a room count (`service/helpers.rs:287`, `service/types.rs:33`); two
  divergent room parsers split the same user intent into two different rooms
  across MCP and HTTP; the canonical open epic **#3064** requires "one shared
  palace and room-per-agent-type" with a read/write boundary between agent
  types; the 500-SLOC production cap constrains where new logic can live.
- **Supersedes / Superseded by:** —

---

## Context

### C1. What the code documents versus what the code does

`crates/trusty-common/src/memory_core/palace.rs:1` declares the model:

> `//! Memory Palace data model: Palace -> Wing -> Room -> Closet -> Drawer.`

Verified against `origin/main` at `e2737547`:

| Level | Type exists | Ever constructed in non-test source | Persisted | Reachable over MCP |
|---|---|---|---|---|
| Palace | `palace.rs:37` | yes | `palace.json` (`store/palace_store.rs:127`) | yes |
| **Wing** | `palace.rs:47` | **no — zero sites** | **no** | **no** |
| **Room** | `palace.rs:96` | **no — zero sites** | **no table** | partially (as a string argument) |
| **Closet** | **no type at all** | n/a | n/a | n/a |
| Drawer | `palace.rs:178` | yes (`retrieval/handle.rs:585`) | `DRAWERS` redb table in `kg.db` | yes |

Evidence for `Wing`: the only occurrences workspace-wide are the struct
definition (`palace.rs:47-51`), the re-export (`memory_core/mod.rs:31`), four
doc-comment mentions, and — significantly — `crates/trusty-mpm/src/tui/health/types.rs:151`,
a UI field whose own doc comment reads *"Wing count … **distinct rooms across
drawers**"*. Evidence for `Closet`: `grep -rn -E '(struct|enum|type)\s+Closet'`
over `crates/` returns nothing.

`Room` is a special case: the *type* is dead, but the *concept* is live. Drawers
carry `room_id: Uuid` and it is populated on every write — by hashing, not by
lookup:

```rust
// crates/trusty-common/src/memory_core/retrieval/layers.rs:49-58
pub fn room_to_uuid(room: &RoomType) -> Uuid {
    let label = format!("{room:?}");
    let mut bytes = [0u8; 16];
    for (i, b) in label.bytes().enumerate() {
        bytes[i % 16] ^= b.wrapping_add(i as u8);
    }
    Uuid::from_bytes(bytes)
}
```

Its own doc comment states the intent: *"until we wire a Room table, callers need
a stable mapping."* The same admission is repeated at `retrieval/handle.rs:580-582`
and `retrieval/layers.rs:204-208`.

### C2. Rooms are already in production use — the audit premise is half right

The claim that auto-capture collapses everything into `General` is correct for
`memory_note` (hard-pinned at `crates/trusty-memory/src/tools/memory_ops.rs:250`)
and correct as a *default*, but it is not what the live data shows.

Measured 2026-08-04 against the running daemon (`127.0.0.1:7070`, trusty-memory
v0.22.0, data root `~/Library/Application Support/trusty-memory/palaces`):

- **94 palaces** with `palace.json` (102 directories on disk), 1.4 GB total.
- **1,531 drawers / 1,395 vectors / 12,464 KG triples** across the 6 palaces
  currently resident in the open-handle cache. (`GET /api/v1/palaces` no longer
  force-opens uncached palaces — issue #4637 — so registry-wide drawer totals
  are not observable without opening all 94.)
- Largest palace `trusty-tools`: **1,039 drawers, 9,362 triples, 1,005 vectors,
  and 12 distinct `room_id` values.**

Inverting the fold on those 12 `room_id`s (the fold is directly invertible for
labels shorter than 16 bytes, and matchable against a rainbow table of the nine
built-in variants otherwise) gives the actual room population of the largest
live palace:

| drawers | room_id | label |
|---:|---|---|
| 779 | `47667068-7666-7200-0000-000000000000` | `General` |
| 203 | `43767577-7372-2e29-7b7d-6b7f81803038` | `Custom("status")` |
| 27 | `43767577-7372-2e29-7f78-7c762e360000` | `Custom("work")` |
| 15 | `c0454e77-7372-2e29-6c6e-6d747f767d7d` | `Custom("decisions")` |
| 6 | `506d6371-726e-746e-0000-000000000000` | `Planning` |
| 2 | `3df3414b-7372-2e29-6b71-6f6e777d7d78` | `Custom("checkpoint")` |
| 2 | `7a767577-7372-2e29-6f78-7e6e746e8131` | `Custom("gotchas")` |
| 1 each | (five more) | `Custom("milestone")`, `Custom("reference")`, `Custom("Archive")`, `Custom("Messages")`, `Research` |

**260 of 1,039 drawers (25 %) already live in eleven non-`General` rooms.** They
got there through `memory_remember`'s optional `room` argument. Room *filtering*
also already works end to end — `memory_list(palace, room="status")` returns
exactly 203 rows, and the L2 vector path enforces the same filter
(`retrieval/layers.rs:209-222`, regression-tested by
`l2_room_filter_excludes_other_rooms`, issue #3274).

So the defect is not "nothing writes to rooms". The defect is that **rooms have
no registry**: they cannot be listed, they have no durable name, their id is a
lossy hash of a `Debug` string, and nothing reconciles two spellings of the same
intent. A user of this palace today cannot discover that a room called `status`
exists without already knowing the word.

### C3. Four concrete defects in the current room identity

**C3.1 — The fold produces structured collisions for labels ≥ 17 bytes.**
`room_to_uuid` XORs byte *i* into slot *i mod 16*. The `Debug` repr of a custom
room is `Custom("<body>")` — 10 characters of overhead — so **any custom room
name of 9 or more characters is in the wrap zone**. Demonstrated concretely: for
a 20-character body, seven distinct bodies of the form `?bcdefghijklmnop?rst`
— the varying character ranging over `'a'..='g'` — hash to one identical UUID.
The repeated character lands in slot 8 twice, sixteen positions apart, so its
two contributions differ by a constant and cancel. **Correction (2026-08-04,
found while implementing T2):** an earlier draft of this paragraph claimed nine
such bodies spanning `'a'..='i'`; that overstates the equivalence class. The
cancellation holds only while adding 16 does not carry out of bit 4, which caps
the varying character at `'g'`, and `\` (0x5C) — inside the wider `'X'..='g'`
window where the arithmetic also works — must be excluded separately because
`Debug` escapes it and changes the repr's length. The *defect* is exactly as
severe as described; only the size of the demonstrated class was wrong. Pinned
by `room_identity::tests::mint_room_id_avoids_fold_collisions`. This is not
theoretical for this data — `Custom("decisions")` (9), `Custom("reference")` (9),
`Custom("milestone")` (9) and `Custom("checkpoint")` (10) are all live in
`trusty-tools` today and all sit in the wrap zone.

**C3.2 — Two divergent room parsers split one user intent into two rooms.**
`RoomType::parse` (`palace.rs:78-91`) lowercases and accepts aliases
(`docs` → `Documentation`). `helpers::parse_room`
(`crates/trusty-memory/src/tools/helpers.rs:275-288`) matches exact case with no
aliases. The HTTP write path uses the former (`service/core.rs:536`); the MCP
write path uses the latter (`tools/helpers.rs:428`). Therefore
`room="backend"` over HTTP stores into `RoomType::Backend`, and the byte-identical
`room="backend"` over MCP stores into `RoomType::Custom("backend")` — **a
different room, with a different `room_id`, invisible to a `Backend` filter.**
The live palace already shows this class of drift (`Custom("Archive")` alongside
a lowercase-shaped `Custom("status")` family).

**C3.3 — The KG's `room:` subjects are a lossy shadow, not an index.**
`kg_extract.rs:229-232` asserts `room:<label> contains drawer:<id>` and its
comment claims this is done so *"multiple drawers per room must coexist"*. The
redb triple key is `(subject, predicate)` only
(`store/kg_store.rs:189-197`), so a second `room:General contains …` assertion
**overwrites the first**. Verified live: `kg_query(palace="trusty-tools",
subject="room:General")` returns **exactly 1 triple** against 779 drawers in that
room. The same invariant silently collapses `tag:*` and `topic:*` membership
(`kg_query subject="tag:mvp"` → 1 triple). This is a real, separate defect that
must be filed; for this ADR its consequence is narrower and important: **the KG
can tell you a room *existed*, and cannot tell you which drawers are in it.**

**C3.4 — The only user-visible "wing" number is a room count.**
`service/helpers.rs:294` computes `distinct_rooms: HashSet<Uuid>` and assigns
`distinct_rooms.len()` to the field named `wing_count` (`service/types.rs:33`).
That field is rendered to users as `"{n} wings"` in
`crates/trusty-memory/ui/src/lib/views/Palaces.svelte:461` and in the TUI health
panel (`crates/trusty-mpm/src/tui/health/screen.rs:437`). Since the day it
shipped, the product has been showing a room count under the label "wings".

### C4. Prior design intent — found, and honored

Three prior tickets exist. All three are the same thread.

- **#3227** *(CLOSED — NOT_PLANNED, consolidated)* — add `memory_room` to
  `AgentInfo` with an inherit-or-override rule in `merge_extends`. Body records
  the owner's model verbatim: *"all agent TYPES (not instances) get their own
  ROOM in a SINGLE shared memory palace."*
- **#3228** *(CLOSED — NOT_PLANNED, consolidated)* — replace per-Segment palaces
  with one shared palace plus room-per-agent-type; explicitly names the target as
  *"Exercise the upstream Wing/Room hierarchy in
  `trusty-common/src/memory_core/palace.rs:37-100`"*.
- **#3064** *(OPEN — the canonical epic both were consolidated into)* —
  `epic(trusty-agents): daemon-backed Assistant memory binding`. Its scope
  includes *"Replace per-segment palaces with one shared palace and
  room-per-agent-type"* and *"Thread the resolved room identity through every
  memory-store initialization path"*. Its acceptance criteria include:
  **"Two agent types cannot accidentally read/write the same room unless
  configured to do so."**

The closing comments on #3227/#3228 state explicitly: *"this is not a claim that
the feature was implemented."* Verified — it was not. `trusty_backed.rs:5-8` still
documents one palace per `Segment`, and `room_type_for` (`trusty_backed.rs:153`)
is still dead code with no callers.

**This ADR honors #3064 rather than departing from it**, and adopts its
acceptance criterion as the definition of what a Wing is for (see D2). The
recorded owner direction "per-TYPE rooms, one palace" holds up against the code
and is adopted unchanged.

### C5. Where new code can physically go

SLOC measured with the `scripts/check_line_cap.sh` definition (code lines only).
No `memory_core` or `trusty-memory` file is in `.line-cap-allowlist.tsv`, so every
one of them is under the 500-SLOC production cap — but three are close enough to
constrain the design:

| File | SLOC / 500 | Headroom |
|---|---:|---:|
| `memory_core/retrieval/handle.rs` | **484** | **16** |
| `memory_core/store/kg_redb/write_ops.rs` | **450** | **50** |
| `trusty-memory/src/service/helpers.rs` | **457** | **43** |
| `trusty-memory/src/tools/memory_ops.rs` | 387 | 113 |
| `trusty-memory/src/tools/definitions.rs` | 367 | 133 |
| `memory_core/retrieval/layers.rs` | 354 | 146 |
| `memory_core/store/kg_store.rs` | 293 | 207 |
| `memory_core/palace.rs` | 225 | 275 |
| `memory_core/registry.rs` | 220 | 280 |
| `memory_core/store/palace_store.rs` | 208 | 292 |
| `memory_core/store/kg/ops.rs` | 110 | 390 |
| `memory_core/store/kg_redb/store.rs` | 105 | 395 |

**Consequence for the plan:** `handle.rs` can absorb a call site and nothing
more. Every unit of new room logic lands in new sibling files; the near-cap files
receive single-line delegations only.

---

## Decision

### D1. Rooms become a first-class, named, enumerable, persisted entity

We will add a real Room registry, stored in the same redb database as the drawers
it indexes, keyed so that **not one existing drawer row changes**.

#### D1.1 Storage location: `kg.db`, not a JSON sidecar

Two new tables alongside the existing `DRAWERS` table in `<data_dir>/kg.db`
(declared in `store/kg_store.rs` beside `DRAWERS` at line 57, initialised in the
same write transaction at `store/kg_redb/store.rs:154`):

```rust
/// room uuid bytes (16) -> postcard(RoomRecord)
pub const ROOMS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("rooms");
/// canonical key "<wing_uuid>\x1f<normalized_label>" -> room uuid bytes (16)
pub const ROOM_KEYS: TableDefinition<&str, &[u8]> = TableDefinition::new("room_keys");
```

We deliberately reject a `rooms.json` sidecar next to `palace.json`, despite the
precedent in `store/palace_store.rs` and despite JSON being human-repairable.
The decisive reason is **corruption recovery**: the redb open path calls
`open_or_recreate` (`store/payload_store/store.rs:70`, `OpenMode::Recreated` in
`store/kg_redb/store.rs:141`), which on an unreadable database **recreates it
empty**. A JSON sidecar would survive that event and then confidently describe
rooms whose drawers no longer exist — an authoritative-looking lie. Rooms and
drawers must be corrupted, snapshotted, and recovered as one unit. Two secondary
reasons reinforce it: the read-only snapshot fallback (issue #59) copies `kg.db`
wholesale, so a snapshot client automatically sees a consistent room+drawer pair,
whereas a live-read JSON file could be newer than the snapshot; and a postcard
record inherits the trailing-optional-field evolution pattern that `DrawerRecord`
has already survived twice (`store/kg_redb/types.rs:40-120`).

#### D1.2 Record shape

```rust
/// On-disk room row. Field order is load-bearing: postcard is positional, so
/// new fields are appended and decoded through a fallback chain exactly as
/// DrawerRecord does (store/kg_redb/types.rs:40-120).
struct RoomRecord {
    /// First-seen display spelling, e.g. "Backend", "decisions", "status".
    label: String,
    /// Canonical RoomType tag: one of the nine built-in variant names, or
    /// "Custom". The label carries the custom body; this field carries the kind.
    room_type: String,
    /// Owning wing. DEFAULT_WING_ID for every room created before D2 lands.
    wing_id: [u8; 16],
    created_at_ms: i64,
    /// false when the backfill could not recover a label and synthesised one.
    /// Surfaced to callers so a human knows a rename is wanted; never blocks a read.
    resolved: bool,
    description: Option<String>,
    /// Forward-compatibility slot for room aliasing (see D5). Empty today.
    /// When non-empty, a room filter matches `id` OR any member of this set.
    merged_from: Vec<[u8; 16]>,
}
```

A reserved all-zero UUID key in `ROOMS` holds the schema-version marker
(`{ schema_version: u32 }`) that makes the backfill idempotent.

#### D1.3 Keying — the migration-safety decision

**Room ids are read from the `ROOMS` table, never recomputed.** The table is
seeded so that every id already in use stays in use.

- **Legacy rooms** (any `room_id` observed in an existing drawer): the row is
  written with `id = the value already stored on the drawers`. That value came
  from `room_to_uuid`, and it is kept verbatim. **Zero drawer rows change.**
  `list_drawers` (`handle.rs:851`) and `retrieve_l2` (`layers.rs:209`) keep
  filtering byte-identically.
- **New rooms** (minted after this lands): `Uuid::new_v5(ROOM_NAMESPACE,
  canonical_key.as_bytes())`. UUIDv5 is a full SHA-1 digest with no 16-byte fold,
  so C3.1's structured collisions are gone for everything created from now on,
  and the id is reproducible across processes and machines without coordination.
- `room_to_uuid` is **demoted to a migration-only helper**. It moves out of the
  retrieval hot path into the backfill module, keeps its public re-export for one
  release for external callers, and its doc comment is rewritten from "until we
  wire a Room table" to "legacy id derivation — do not call for new rooms".

The canonical key is `format!("{wing_id}\u{1f}{}", label.trim().to_lowercase())`.
Lowercasing the *key* while storing the *first-seen spelling* as the label means
`Decisions` and `decisions` resolve to one room without destroying the
capitalisation a human chose.

#### D1.4 Migration: additive-only, at-open, fail-open, per-palace

**Existing drawers are neither reclassified nor moved. They are named.**

This is the answer to "do existing drawers stay in `General` or get
reclassified?" — **neither, because there is no reclassification**. A drawer
currently in `General` stays in `General`; a drawer currently in
`Custom("status")` stays there and that room merely becomes enumerable. There is
no reclassification *rule* because the migration never writes to the `DRAWERS`
table at all. It is a pure insert into a new table.

The backfill runs on palace open, over the drawer vector that
`handle.rs:318` has already materialised in memory — no extra I/O beyond at most
a few dozen inserts. It resolves each distinct `room_id` to a label in four
steps, highest confidence first:

1. **Built-in rainbow table (certain).** Compute `room_to_uuid` over the `Debug`
   repr of the nine built-in `RoomType` variants and match. This is the exact
   function that produced the id, so a match is proof, not a guess. Covers
   `General` (779 drawers in `trusty-tools` alone), `Planning`, `Research`.
2. **Direct fold inversion (certain, for reprs of at most 16 bytes).** The fold
   XORs byte *i* into slot *i mod 16*, so it only wraps from *i* = 16 onward:
   for a `Debug` repr of **16 bytes or fewer** each slot receives at most one
   byte and the fold is injective, inverting as `repr[i] = byte[i] - i`.
   Verified live: `43767577-7372-2e29-7f78-7c762e360000` inverts to exactly
   `Custom("work")` (14 bytes, two trailing zero slots). **Correction
   (2026-08-04, found while implementing T2):** an earlier draft said "shorter
   than 16 bytes", which is off by one and load-bearing —
   `Custom("status")` is *exactly* 16 bytes and is 203 drawers in the live
   `trusty-tools` palace, so a literal reading would have dumped all of them
   into the `unresolved-*` bucket. A 16-byte repr has no trailing zero slots to
   detect its length by, so the implementation tries each candidate length from
   16 down to 1 and confirms by re-hashing; the re-hash is what makes any
   accepted length certain rather than plausible.
3. **KG `room:` dictionary match (high confidence, for wrapped labels).** For ids
   that neither match a built-in nor invert cleanly (repr ≥ 17 bytes, i.e. every
   custom room of 9+ characters), enumerate that palace's KG subjects carrying the
   `room:` prefix via `KnowledgeGraph::list_subjects`
   (`store/kg_redb/read_ops.rs:82`) and hash each candidate. This converts an
   un-invertible hash into a dictionary lookup **against a dictionary the palace
   itself wrote**. Per C3.3 the KG cannot say which drawers are in a room — but
   this step only needs to know the room existed, which the subject's presence
   proves. Verified: `room:General`, `room:Planning` and `room:Research` are all
   live subjects in the `trusty-tools` palace.
4. **Unresolved fallback (never lossy).** Anything still unmatched gets a row with
   `label = format!("unresolved-{}", &id.to_string()[..8])`, `room_type = "Custom"`,
   `resolved = false`. **No drawer is touched, no drawer is reassigned, nothing
   is lost** — the drawer keeps its `room_id`, the room becomes visible, and
   `room_rename` (D3) lets a human fix the name. In the largest live palace this
   bucket is at most a handful of rooms holding single-digit drawer counts.

Operational properties, all mandatory:

- **Idempotent and re-runnable.** The backfill only *inserts* rows for `room_id`s
  that have no row yet. It never updates or deletes one, so a human rename
  survives every subsequent open.
- **Fail-open.** Any error is logged at `warn!` and the palace opens anyway, with
  room listing degraded to unresolved names. This matches the existing precedent
  at `handle.rs:308-314`, where a failed `load_drawers` falls back to L1 rather
  than making the palace unopenable.
- **Per-palace, on demand.** There is **no workspace-wide one-shot migration and
  no up-front sweep of all 94 palaces.** A palace migrates the first time it is
  opened; a palace nobody opens is never touched. Blast radius is one palace.
- **Inspectable before it writes.** A CLI `trusty-memory rooms backfill --palace <id>`
  prints the derived label, confidence step, and drawer count per `room_id` and
  **exits without writing**; `--apply` is required to write. The at-open path is
  the automatic mechanism; the CLI is the audit mechanism.

### D2. Verdict on Wing: **implement it — as the scope/ownership boundary, not a second topical axis**

`Wing` earns its complexity, but only under a purpose it does not currently have,
and only once it has a consumer. Both conditions are stated here.

**A Wing is the "who" axis. A Room is the "what" axis.**

The argument from the live data: the twelve rooms in `trusty-tools` are not twelve
topics. They are three orthogonal vocabularies crammed into one flat namespace —
*topics* (`Planning`, `Research`), *lifecycle kinds* (`status`, `checkpoint`,
`milestone`, `Archive`), and *writer scratchpads* (`work`, `decisions`,
`gotchas`). Flattening forces every axis into the room name, which is precisely
how a palace ends up with twelve labels nobody can enumerate or reason about.

The four access patterns a Wing enables that a single Room level cannot:

1. **Owner-scoped recall without enumerating topics.** "Recall everything the
   `engineer` agent type has learned" is one wing-scoped query. With one level,
   the caller must first know that agent's complete topic set — which is exactly
   the discovery problem this ADR exists to fix.
2. **The #3064 acceptance criterion.** *"Two agent types cannot accidentally
   read/write the same room unless configured to do so."* That is an
   authorization boundary, and it needs somewhere to hang the configuration. A
   room is a topic; it is the wrong place to record who may read it. **This is
   the tie-breaker** — a composite `(scope, topic)` room key could serve patterns
   1 and 3 without a Wing *entity*, but it has nowhere to store "unless
   configured to do so". A wing is where that configuration lives.
3. **Room-name reuse across owners without name mangling.**
   `engineer`/`Planning` and `pm`/`Planning` are two distinct rooms. Without a
   wing, the only way to express that is `Custom("engineer-planning")` — and that
   is demonstrably how this palace acquired twelve ad-hoc labels.
4. **Policy attached to a scope, not repeated per room.** A `scratch` wing with a
   retention TTL applies to every room inside it; today a TTL policy would have to
   be re-declared per room.

**Constraints on the implementation, so Wing does not repeat its own history:**

- **Wing ships with a consumer or it does not ship.** The `wing_id` field is
  reserved in `RoomRecord` from day one (D1.2) and defaults to a single
  `DEFAULT_WING_ID` per palace. The `WINGS` table, the wing MCP surface, and
  wing-scoped recall land **only** with the #3064 agent-identity plumbing that
  consumes them. Building a second dead level would reproduce exactly the defect
  this ADR is correcting.
- **Wing is never a required concept for a caller.** Every palace gets a default
  wing; every room defaults into it; no existing call site gains a required
  argument. A caller that never mentions a wing behaves identically before and
  after.
- **`PalaceInfo.wing_count` stops lying** (C3.4). We add a truthful `room_count`,
  keep `wing_count` serialised as a deprecated alias for one release so the TUI
  probe (`tui/health/probes.rs:433`, which already reads it with
  `unwrap_or(0)`) and the Svelte UI do not break, migrate both readers, then drop
  the alias.

### D3. Verdict on Closet: **remove it from the model; it is an index, not a level**

`Closet` was not named by the owner, has no type, and has zero implementation as
a hierarchy level. What survives under the name is
`PalaceHandle.closets: Arc<RwLock<HashMap<String, Vec<Uuid>>>>`
(`retrieval/handle.rs:95`) — a keyword → drawer-ids inverted index rebuilt on
every write (`handle.rs:753-763`) and refreshed by the dream cycle, used to add a
+0.15 topical boost during L2/L3 scoring (`layers.rs:236-240`, `layers.rs:302-306`).

That structure **cannot** be a level between Room and Drawer: it is many-to-many
(a drawer appears in every closet whose keyword its content contains), and a
hierarchy level requires exactly one parent. The doc comment describing it as one
is simply wrong.

We will therefore correct the documented model to **Palace → Wing → Room →
Drawer**, and describe closets in their true role — a cross-cutting keyword index
over drawers — wherever they are mentioned. We **keep the field name `closets`**:
renaming it is churn with no behavioural benefit, and "closet" is an apt name for
an inverted index inside a memory-palace metaphor.

Doc sites to correct: `memory_core/palace.rs:1` and `:6`;
`memory_core/mod.rs:9`; `trusty-common/src/lib.rs:250`;
`trusty-memory/src/chat/handler.rs:184`;
`trusty-agents/src/memory/trusty_backed.rs:5`;
`trusty-memory/ui/src/lib/views/Palaces.svelte:4`;
`docs/trusty-memory/spec/ARCHITECTURE.md:60` and `:362`;
`docs/trusty-memory/spec/COMPONENTS.md:83-88`;
`docs/trusty-common/spec/{PRD,ARCHITECTURE,COMPONENTS}.md`.

### D4. Write-path routing

**D4.1 — One room parser.** Delete `tools/helpers.rs::parse_room` and route every
caller through `RoomType::parse` (`palace.rs:78`), per the common-entry-point rule
in `CLAUDE.md`. This is a prerequisite for everything else in D4: routing writes
into rooms is meaningless while two code paths disagree on which room a string
names (C3.2).

*Named backward-compatibility hazard.* This changes what a **new** MCP write with
`room="backend"` means, from `Custom("backend")` to `Backend`. Existing drawers are
untouched, so a palace can legitimately end up holding both `Custom("backend")`
(legacy, now enumerable and renameable) and `Backend`. That is accepted: the
alternative is either rewriting drawer rows (rejected outright) or leaving the two
parsers permanently forked. The changelog fragment must say so explicitly.

**D4.2 — Room resolution replaces room hashing on the write path.**
`remember_with_options` currently stamps `drawer.room_id = room_to_uuid(&room)`
(`handle.rs:583-585`). It will instead call a single `resolve_or_create_room`
that looks the canonical key up in `ROOM_KEYS`, creates the row when absent, and
returns the id. Because `handle.rs` has 16 SLOC of headroom (C5), the resolution
logic lives in the new rooms module and `handle.rs` gains a call, not an
implementation.

**D4.3 — `memory_note`'s pin to `General` is lifted.**
`tools/memory_ops.rs:250` hard-pins `RoomType::General` and mirrors
`room_label_for_kg: Some("General")`. The comment justifying it
(*"memory_note is pinned to the General room"*) restates the behaviour rather
than motivating it, and no gate depends on it. `memory_note` gains an optional
`room` argument with the same semantics as `memory_remember`, defaulting to
`General` when absent. Callers that pass nothing are unaffected.

**D4.4 — What determines a drawer's room, in strict precedence order:**

1. **Caller-supplied `room`** — always wins.
2. **Caller identity (the #3064 axis)** — a server-side default room resolved from
   the calling agent type when the client declares one. The MCP stdio bridge
   already injects per-request `cwd`/`workstream` for DOC-53 attribution (see the
   `memory_remember` schema at `tools/definitions.rs:113-121`); agent-type room
   defaulting rides the same mechanism. This is the honored form of "per-TYPE
   rooms, one palace".
3. **`RoomType::General`** — the terminal default.

**Content or tag inference is rejected.** Four reasons, stated so this is not
relitigated: (i) a mis-inferred room is an *invisible* failure — a drawer inferred
into `Backend` that belonged in `Planning` is simply never returned by a
`Planning`-scoped recall, with no error anywhere; (ii) it makes the room field
non-deterministic, so identical text written twice can land in two rooms;
(iii) the codebase already has a heuristic classifier for `DrawerType`
(`filter::classify`) and it is deliberately allowed to fall back to `Unknown`
rather than guess (`handle.rs:593-596`) — the same conservatism applies here;
(iv) a room is cheap to set explicitly and expensive to un-mis-file, since
un-mis-filing means rewriting drawer rows.

### D5. Room merging is designed, and deliberately deferred

D4.1 can leave a palace with two rooms that a human considers one. The obvious
fix — reassign the drawers — is the one thing this ADR refuses to do.

The forward-compatible mechanism is recorded now and implemented later: a merge
maps a second canonical key onto the surviving room's id in `ROOM_KEYS`, and
records the absorbed id in the survivor's `merged_from` set (D1.2). Room filters
at `handle.rs:851` and `layers.rs:209` then match `room_id ∈ {id} ∪ merged_from`.
**No drawer row changes.** This is filed as a follow-up, not built here.

### D6. MCP surface

The pattern is identical to open issue **#4776** — a capability that exists in the
store and is reachable over REST but has no MCP tool — applied to a different
capability. `#4776` asks for `kg_list_subjects` over the KG; this asks for room
listing over the new `ROOMS` table. **They do not overlap and must not be
folded together.** Worth recording: once `ROOMS` exists, the KG's `room:` subjects
become a redundant and provably lossy shadow of it (C3.3). Keep the extraction —
it feeds graph clustering — but never treat it as an index.

New tools:

| Tool | Signature | Notes |
|---|---|---|
| `room_list` | `(palace, wing?) -> [{room_id, label, room_type, wing_id, drawer_count, created_at, resolved}]` | The discovery primitive that does not exist today |
| `room_create` | `(palace, label, wing?, description?) -> {room_id, label, created}` | Idempotent — returns the existing room when the key resolves |
| `room_rename` | `(palace, room_id\|label, new_label) -> {room_id, label}` | The repair path for `unresolved-*` rooms. Touches `ROOMS` + `ROOM_KEYS` only, never `DRAWERS` |

Extended tools:

- `memory_note` gains `room` (D4.3).
- **`memory_recall` and `memory_recall_deep` gain `room`.** This is a real gap:
  `retrieve_l2` already accepts and enforces `room_filter`
  (`layers.rs:179-222`, regression-tested since #3274), but the MCP recall schema
  is `palace`/`query`/`top_k` only (`tools/definitions.rs:141-151`). Room-scoped
  recall is currently reachable only through `memory_list` and the HTTP
  `/api/v1/recall` route (`service/core.rs:465`). `retrieve_l3` gains the same
  filter so `recall_deep` is not the odd one out.
- `palace_info` (`tools/palace_ops.rs:223-237`, currently returning only
  `{id, name, drawer_count, data_dir}`) gains `room_count` and, once D2 lands,
  a truthful `wing_count`.

### D7. Why this is an ADR

Four criteria are met: it is hard to reverse (a persistent table and an
id-minting rule over 94 live palaces); it fixes the *semantics* of two documented
levels that future code will build on; it deliberately rejects an obvious
alternative (rewriting drawer `room_id`s) whose reasoning must survive the people
who wrote it; and `docs/adr/` plus the `tm-adr` skill exist precisely for this
shape of decision. The schema and ticket detail are carried here rather than in a
separate `DOC-*` spec so the decision and the thing it decides cannot drift apart.

---

## Implementation sequence

Ten tickets. Every one is a single reviewable PR outcome, and every one respects
the 500-SLOC production cap given the headroom measured in C5. Estimated SLOC is
production only; test files are capped at 3000 separately.

| # | Ticket | New/changed files | Est. SLOC | Depends on |
|---|---|---|---:|---|
| **T0** | `docs: correct the palace model — Palace → Wing → Room → Drawer; closets are an index` | the 11 doc sites in D3 | 0 (docs) | — |
| **T1** | `feat(trusty-common): room identity + ROOMS/ROOM_KEYS schema (ships dark)` | new `memory_core/store/rooms.rs`; new `memory_core/room_identity.rs`; `kg_store.rs` +6; `kg_redb/store.rs` +2 | ~370 | — |
| **T2** | `feat(trusty-common): additive room backfill at palace open` | new `memory_core/store/room_backfill.rs`; hook in `registry.rs` (280 headroom) | ~290 | T1 |
| **T3** | `fix(trusty-memory): one room parser — delete helpers::parse_room` | `tools/helpers.rs` −15, call sites | ~20 | — |
| **T4** | `feat(trusty-common): write path resolves rooms through the ROOMS table` | `rooms.rs` +40; `handle.rs` +4 (16 headroom) | ~45 | T1, T2, T3 |
| **T5** | `feat(trusty-memory): memory_note accepts an explicit room` | `tools/memory_ops.rs` (113 headroom), `tools/definitions.rs` | ~25 | T4 |
| **T6** | `feat(trusty-memory): MCP room surface — room_list / room_create / room_rename` | new `tools/room_ops.rs`; new `tools/room_definitions.rs` (mirrors `task_definitions.rs`); `tools/mod.rs` +6 | ~230 | T1, T2 |
| **T7** | `feat(trusty-memory): room filter on memory_recall / memory_recall_deep` | `layers.rs` +20 (146 headroom); `tools/memory_ops.rs`; `tools/definitions.rs` | ~55 | T4 |
| **T8** | `fix(trusty-memory): PalaceInfo.room_count; wing_count stops reporting rooms` | `service/types.rs`, `service/helpers.rs` (43 headroom — delegate, do not inline); Svelte + TUI readers | ~40 | T2 |
| **T9** | `feat(trusty-common): Wing entity, default wing, wing-scoped recall` | new `memory_core/store/wings.rs`; wing MCP surface | ~300 | T1–T8 **and a #3064 consumer** |
| **T10** | `chore(trusty-memory): CLI `rooms backfill --dry-run/--apply` audit path` | new `commands/rooms.rs` | ~150 | T2 |

**Dependency order:** T0 (docs-only, unblocks nothing but stops the doc lying
immediately) → T1 → T2 → T3 → T4 → {T5, T6, T7} in parallel → T8 → T10 → **T9
last and gated**.

T9's gate is the one non-negotiable sequencing rule in this ADR: **Wing does not
land without the #3064 consumer that reads it.** Shipping a second unconsumed
level would recreate the exact defect being fixed.

**Separately filed defects (not part of this epic, discovered while designing
it):**

- **D-1 (severity: high).** The KG's one-active-triple-per-`(subject, predicate)`
  invariant silently collapses `tag:*`, `room:*`, and `topic:*` membership to a
  single drawer. `kg_extract.rs:212-215` and `:229-232` both carry comments
  asserting that multi-membership works; `encode_triple_key`
  (`kg_store.rs:189-197`) makes it structurally impossible. Live proof:
  `room:General` holds 1 triple against 779 drawers; `tag:mvp` holds 1.
- **D-2 (severity: medium).** `room_to_uuid`'s 16-byte fold admits structured
  collisions for any custom room name of 9+ characters (C3.1), four of which are
  live in `trusty-tools` today. D1.3 stops the bleeding for new rooms; existing
  ids are grandfathered by design.

---

## Consequences

**Easier.**

- A caller can discover what rooms a palace has. Today that is impossible over
  MCP and only obtainable by inverting a hash by hand — which is how the room
  table in C2 was produced for this document.
- Room-scoped recall becomes reachable from the tool surface agents actually use.
  The capability has existed and been tested since #3274; only the door was
  missing.
- `#3064`'s room-per-agent-type acquires the storage and the authorization
  anchor it has been blocked on since 2026-07-28.
- Room names survive, are renameable, and stop being a lossy hash of a `Debug`
  string.

**Harder / costlier.**

- One more redb table in the hot open path. Cost is bounded: at most a few dozen
  rows per palace, read once at open, over a drawer vector already in memory.
- The room namespace temporarily grows during D4.1's parser unification — a
  palace can carry both `Custom("backend")` and `Backend` until a human merges
  them, and merging is deferred (D5). This is an accepted, named cost of refusing
  to rewrite drawer rows.
- `room_to_uuid` and UUIDv5 coexist as id-minting rules. That duality is
  permanent for legacy rooms and is the price of not touching 1,531+ existing
  drawer rows. It is contained by the rule that ids are *read from the table*,
  so no code outside the backfill ever chooses between the two.

**Neutral / notable.**

- `wing_count` briefly reports the same number under two field names during T8's
  deprecation window.
- The backfill's step 3 (KG dictionary match) is high-confidence, not certain: it
  proves a candidate label hashes to the observed id, which for a wrapped label is
  a collision-vulnerable proof (C3.1). The `resolved` flag and `room_rename`
  exist because of exactly this residual uncertainty. It is safe because a wrong
  *label* costs a rename, while a wrong *drawer assignment* would cost data — and
  no drawer assignment is ever made.
- Measured live scale for this decision (2026-08-04): 94 palaces, 1.4 GB, 1,531
  drawers across 6 cached palaces, largest palace 1,039 drawers / 12 rooms. The
  brief's figures (127 palaces / ~1,388 drawers / 1,026 drawers in `trusty-tools`)
  are from an earlier point; both describe the same order of magnitude and neither
  changes any decision here.

---

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior decisions on 2026-08-04:

- **ADR-0009 (External-extractor KG ingest contract):** *Consistent.* This ADR
  adds no KG ingest path. It records (C3.3, D-1) that the KG's
  one-triple-per-`(subject, predicate)` invariant makes `room:` subjects a lossy
  membership shadow, and explicitly declines to make the KG the room index —
  which strengthens rather than contradicts 0009's contract boundary.
- **ADR-0010 (KG edge-kind extensibility — standard kinds plus `Custom`):**
  *Extends the same shape.* `RoomType` already mirrors 0010's design (nine named
  variants plus a `Custom(String)` escape hatch, `palace.rs:55-66`). D1.2 keeps
  that split explicit on disk by storing `room_type` (the kind) and `label` (the
  custom body) as separate fields, rather than flattening them into one string.
- **ADR-0012 (Per-instance GUID and marker-file identity):** *Consistent.* Room
  ids are per-palace content-derived (UUIDv5 over a canonical key), never
  instance-derived, so no room id depends on which daemon instance created it.
- **ADR-0018 (Loopback-only doctrine):** *Consistent.* Every new surface here is
  an MCP tool or an existing loopback HTTP route. No new listener, no new bind.
- **ADR-0026 (A credential grant does not survive delegation):** *Consistent, and
  a boundary worth naming.* D2 makes a Wing the anchor for #3064's "two agent
  types cannot accidentally read/write the same room unless configured to do so".
  That is a *storage-scope* boundary over memory, not a credential grant, and it
  neither inherits across delegation nor confers any authority — 0026's model is
  untouched. When wing-level access configuration is implemented in T9, it must
  resolve against the acting principal, not a delegator.
- **ADR-0015 / ADR-0016 (agent composition; orchestration hierarchy):**
  *Consistent.* D4.4's precedence rule takes an agent's room from declared
  configuration rather than inferring it, which is the same
  configuration-over-inference stance both carry.

No conflicts found. No prior ADR is superseded or amended.
