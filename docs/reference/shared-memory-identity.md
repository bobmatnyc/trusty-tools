# Shared-Memory Identity — the seven decisions

**Status:** implemented in `trusty-common` `memory_core` (#5902). The CLI
commands, the `.trusty-memories/` git layout, and the export-time secret-scan
gate are a follow-up PR and are NOT in the code this describes.

**Related:** [#1683](https://github.com/bobmatnyc/trusty-tools/issues/1683) is the
open spec for a remote shared-memory service. This is the git-native path toward
the same user story; #1683 stays open.

## The problem

The same fact recorded on two machines for one project must converge to ONE
memory once it is exported, committed to git, pulled, and imported elsewhere.
Nothing in the palace made that possible. `Drawer.id` is a v4 UUID minted at
write time, so no part of a drawer's identity derives from what it says, and the
palace had no export or import at all — the two copies could not meet.

The closest working precedent is `trusty-agents memories export/import`
(`crates/trusty-agents/src/cli/memories_cmd/ops.rs`), JSONL over its own redb
store with whole-file sha256 idempotency. Its per-record upsert key is
`imported:{machine_id}:{id}`, which is exactly the defect: machine plus local id
can never recognise two machines' copies of one fact as the same fact.

Sharing scope is per-project only.

## Decision 1 — the hash covers the memory BODY only

Not tags, not `room_id`, not `drawer_type`, not importance.

Metadata is rewritten by routine housekeeping. `dream::helpers::merge_into`
appends a loser drawer's text into the survivor and unions its tags;
`dream::cycle::apply_consolidation_result` writes an LLM-authored canonical body
with a rewritten tag set. A metadata-inclusive digest would fork identity on a
consolidation pass that changed nothing a reader would call a new fact.

Implemented as `memory_core::content_hash::memory_content_hash`.

## Decision 2 — the hash coexists with `Drawer.id`, it does not replace it

`Drawer.id: Uuid` is three things at once: the HNSW vector-store key, the
`drawer:{id}` KG triple subject, and the value in the `DRAWERS_BY_FACT_KEY` slot
index (`memory_core/palace.rs`, `retrieval/tier_c.rs`). Replacing it means
rewiring all three, for no gain — the UUID answers "which row is this" and the
digest answers "is this the same fact", and both questions are live.

`Drawer.content_hash: ContentHash` is a new field beside it.

### Refinement made during implementation: the field is DERIVED, not persisted

The digest is a pure function of `content`, so storing it in the redb
`DrawerRecord` would create a second source of truth that can disagree with the
first — and it would: `dream::helpers::merge_into` rewrites `content` in place on
the in-memory drawer table. Persisting it would also mean a positional postcard
migration (`DrawerRecord` already carries a three-deep decode fallback chain) for
a value that is recomputable in microseconds.

So the field is a cache with exactly four maintenance points:

| Point | What it does |
|---|---|
| `Drawer::new` | computes it |
| `Drawer::set_content` | recomputes it — the only supported way to change a body |
| `kg_redb::read_ops::load_drawers` | derives it per row on hydration |
| `L1Cache::load_l1_cache` | `refresh_content_hash` per drawer |

Nothing reads the digest from disk, so a stale digest is unreachable and no
migration exists. `#[serde(default)]` decodes pre-field JSON to
`ContentHash::UNSET`, and the hydration paths replace the sentinel.

## Decision 3 — one hashing entry point, owned by `memory_core`

There was no shared hash entry point. `trusty-common` alone holds four
independent `Sha256::new()` sites: `error_capture/fingerprint.rs`,
`memory_core/semantic_consolidation/types.rs` (a batch cache key),
`symgraph/registry.rs`, and `symgraph/contracts/mod.rs`. A fifth ad-hoc call
would put the normalization contract somewhere no other caller can find it.
CLAUDE.md's common-entry-point rule applies.

`symgraph::SymbolRegistry::content_hash` was explicitly NOT reused. It is
code-symbol identity: a bare sha256 over raw source, whose whole purpose is to
change when the bytes change, including on a re-indent or a line-ending flip.
Memory identity needs the opposite. Same primitive, opposite contract, so they
must not share a function — and `memory_content_hash`'s domain separator makes
the two digest spaces provably disjoint rather than merely conventionally
distinct.

## Decision 4 — normalize before hashing, and only for hashing

Nothing normalized memory text: `PalaceHandle::remember_with_options` stores
`content: String` exactly as received — no trim, no Unicode form, no line-ending
policy. Two clients that recorded the same sentence hold bytes differing in ways
no reader would call a difference.

The contract, in order (`memory_core::content_hash::normalize_for_hash`):

1. `\r\n` and lone `\r` become `\n`.
2. Unicode NFC.
3. Trailing whitespace removed per line.
4. All trailing whitespace and newlines removed from the string as a whole.

Deliberately NOT normalized: leading whitespace, interior blank lines, and case.
Indentation distinguishes a memory holding a nested code block from the same text
flush-left, and those are different facts.

**This is a versioned, breaking-to-change contract.**
`CONTENT_HASH_VERSION` is folded into the digest preimage, so a v2 normalization
mints ids in a different space rather than silently overlapping v1. Changing any
rule re-mints every id in every exported file in every repo that ever ran the
code, and two clients on different versions stop converging. Bumping it is a
breaking change.

Normalization applies to HASHING only. `Drawer.content` keeps the caller's bytes
verbatim; rewriting stored content would be a silent data migration of every
palace on disk.

## Decision 5 — supersede via the existing KG-triple mechanism

An edited memory has different content, so a different hash, so it is a different
memory. The old one must not orphan.

The mechanism already existed in `dream::cycle`: assert
`Triple { subject: "drawer:{orig}", predicate: "superseded_by", object: "drawer:{canonical}" }`,
and — the load-bearing half, [#1713](https://github.com/bobmatnyc/trusty-tools/issues/1713) —
mark the original evictable ONLY once that triple write durably succeeds. No
second supersede concept was invented and no tombstone field was added.

What changed: the writer was extracted to `memory_core::share::supersede`, and
`dream::cycle::record_provenance_and_collect_superseded` now calls it. Both paths
assert one edge shape, so a fix to the guarantee lands once. The eviction
decision stays with the dream cycle, because only that caller evicts.

## Decision 6 — import merge rule: earliest `created_at` wins

Colliding hashes mean identical bodies, so nothing about the fact is in dispute
and a merge discards almost nothing — the palace has no author, machine, or
session field for two copies to disagree about. `Palace`, `Wing`, `Room`, and
`Drawer` carry no provenance at all; the only provenance in the model is
`Triple.provenance: Option<String>`, free-form and optional on KG edges.

What must not happen is the timestamp regressing to whichever import ran last.
`created_at` feeds temporal decay and the recency tie-break in
`drawer_listing_order`, so re-importing an old shared file would silently promote
every fact in it to "written today" and re-rank the palace.

Tags union and importance takes the maximum, on the same principle: a merge may
add information, never remove it.

## Decision 7 — JSONL, one record per line, no embedding vector

One memory per line, so a git diff shows added and removed facts rather than a
reflowed blob, and a half-written file loses only its last line. The record
carries the content hash, body, tags, `created_at`, `drawer_type`, room label,
importance, and both version numbers (`SHARE_FORMAT_VERSION` and
`CONTENT_HASH_VERSION` — separate, because adding a field is compatible while
changing normalization is not).

The room travels as the LABEL, not the `room_id`. Room ids are UUIDv5 over
`(wing_id, lowercased label)` (ADR-0027), so both machines mint the same id from
the same label, and the label survives arriving at a palace that has never seen
that room.

**No embedding vector**, unlike `trusty-agents`' `ExportRecord`. Three reasons: a
384-dimension `f32` array serializes to roughly 4–6 KB of JSON, an order of
magnitude larger than the body it describes, which makes a committed file's diff
unreadable and its history heavy; the receiving machine's embedder, dimension, or
model revision need not match the sender's, so an imported vector can be silently
wrong in a way no assertion could catch; and re-embedding on import is cheap
against a warm process-wide embedder. The vector is a derived index, not the fact.

The lossiness argument cuts the other way from what one might expect: an
embedding IS a lossy encoding of its source text, so it would leak content — but
the body is in plaintext on the same line, so the vector adds no disclosure the
file does not already make. That is an argument about the file as a whole, which
is Decision 8's territory.

## The four properties, and where they are proven

| Property | Meaning | Test |
|---|---|---|
| Idempotent | importing the same export twice changes nothing | `import_is_idempotent` |
| Additive | importing a superset adds only what is new | `import_of_a_superset_adds_only_the_new` |
| Convergent | two machines' overlapping exports produce one memory | `two_machines_converge_on_one_memory` |
| Monotone in time | a merge keeps the earlier `created_at`, in either order | `merge_keeps_the_earlier_created_at_in_either_order` |

All in `crates/trusty-common/src/memory_core/share/tests.rs`.

## 🔴 Open: the export path can carry secrets

`memory_core::filter::check_secret` runs at WRITE time only. It runs on no export
path, because until #5902 there was no export path. Any credential that predates
the secret filter, or that its heuristics missed, is already sitting in a palace,
and `export_palace_records` will copy it out verbatim.

That is acceptable while the destination is a local file, which is all this PR
builds. It is NOT acceptable for the git workflow this primitive exists to serve:
the follow-up PR must re-scan every record it is about to commit and fail the
commit on a hit. #1683 makes the same point about its own upload path and calls
the secret filter a hard security boundary.

Nothing in the current code enforces that. It is a gap, stated here so the next
reader inherits it rather than discovers it.
