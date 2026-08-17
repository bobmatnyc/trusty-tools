# 0052. One search index per durable checkout; worktrees are tagged chunk rows, not sub-indexes

- **Status:** Accepted
- **Date:** 2026-08-17
- **Scope:** crate `trusty-search` (index registry, storage granularity, query
  result shape). References `trusty-mpm` (`search_index.rs`), DOC-37, and
  issues #2611, #1681, #5069, #5060 for contrast; no change to those crates
  or issues is made by this ADR.
- **Reversibility Cost:** High — a checkout's chunk rows would need to be
  re-partitioned into per-worktree index entities to reverse this, and every
  worktree-tagged row already written would need a migration, not a config
  flip.
- **Decision Drivers:** owner ruling, 2026-08-16/17 conversation; operational
  cost of tracking per-worktree indexes as separate registry entries; the
  efficiency cost of re-indexing files a worktree has not changed.
- **Supersedes / Superseded by:** Amends
  [ADR-0050](0050-colocated-path-tied-identity-with-delta-indexed-worktree-facets.md),
  specifically decision points 3, 5, and 6's per-worktree registration
  mechanism — ADR-0050's decisions 1, 2, 4, and 7 stay in force. Further
  amends [ADR-0008](0008-project-identity-convention.md) point 4 (already
  amended once by ADR-0012), this time nullifying it outright — worktrees no
  longer register as indexes at all. See Related Decisions.

## Context

**What ADR-0050 actually built, re-read precisely.** ADR-0050 replaced
per-worktree FULL copies with per-worktree DELTA facets, but a delta facet
was still a separate, independently-registered index: decision 6 references
"the existing `skip_vector` registration field already used for worktree
indexes," and the Context section describes `PersistedIndex::repo_identity`
as a join key that lets facet routing (#5069) "forward a PINNED semantic
query from a `skip_vector` worktree index to a *sibling* index sharing its
`repo_identity`." A sibling index is still a distinct `index_id` in the
registry — DOC-37's proposed shape (`kind: delta`, `overlay_of: <base
index_id>`) is exactly this: one registry row per worktree, grouped to its
base by a shared field, not folded into the base's own row set. ADR-0050
shrank what a worktree's index CONTAINS; it did not eliminate the worktree
index as a registry ENTITY.

**Why tracking those entities individually stopped being worth it, in the
owner's words:**

> The reason we switch to a SINGLE search index covering all worktrees is
> that keeping individual worktree indexes tracked was too much work. Better
> to keep it all together, use tags to identify based on worktree, and
> delete when a worktree is deleted. Each work tree is tied to its checkout.

The operational bookkeeping of tracking per-worktree indexes — registering
each one, keeping its `repo_identity` join current, routing facet queries
between siblings, reaping the sibling when the worktree goes away — cost
more than the isolation a separate registry row bought. This ADR removes the
registry row, not the delta idea: a worktree's changed content is still
computed as a delta against the merge-base (ADR-0050 decision 5's mechanism
is unchanged and is in fact what makes this ADR possible — see below), but
that delta lands as rows inside the checkout's own index rather than as rows
in a sibling index of its own.

**Why indexing only modified files is separately worth stating.** An
unmodified file in a worktree is byte-identical to the checkout's own
already-indexed copy of that file — indexing it again duplicates chunks that
already exist and answer to the same content. The stated reason is
efficiency: a worktree with one modified file should contribute only that
file's chunks, not a second copy of the unmodified 99% of the tree it did
not touch. This is an operational-cost argument of the same shape as the
one above (avoid paying for work the tree does not need), not a new
principle.

**The mechanism this depends on already exists, and it replaces one that
could not have supported it.** `resolve_branch_files`
(`crates/trusty-search/src/core/git.rs:26`, issue #122) ran
`git diff --name-only <base>..HEAD` — a comparison of two commits. A live
agent worktree is mostly UNCOMMITTED work, so a four-change fixture (one
modification, one deletion, one untracked file, one committed change) found
`resolve_branch_files` reporting only 1 of the 4 real differences: it missed
the deletion, the modification, and the untracked file entirely, because
none of the three is visible in a commit-to-commit diff. "Only index
modified files," computed that way, would have silently under-indexed most
of what a worktree actually changed.

PR #5815 (branch `worktree-agent-a91b610c98dad72c2`, merging into
`origin/main`) replaces this with `resolve_merge_base_delta`
(`crates/trusty-search/src/core/git.rs`), verified directly against that
branch's source: it diffs the merge-base against the WORKING TREE (`git diff
--name-status -z <base>`, not `<base>..HEAD`), separately adds untracked
files (`git ls-files --others --exclude-standard -z`), and returns `changed`
and `deleted` as two distinct lists rather than one collapsed list — a
deletion is not indistinguishable from an edit the way it was before. Any
step failing returns `None` rather than a partial delta, on the documented
position that a delta that looks complete but silently omits files is worse
than no delta. This is the primitive "index only modified files" needed and
did not have before #5815.

**What the current query result shape carries, verified against source.**
`CodeChunk` (`crates/trusty-search/src/core/indexer/types.rs:20-70`) has
`index_id: Option<String>` (cross-project fan-out, issue #10) and
`on_branch: bool` (branch-aware boosting, issue #122) but no field
identifying which worktree, if any, a chunk's row came from. Under the
one-index-per-checkout model this ADR adopts, `index_id` alone can no longer
answer "which worktree produced this hit" — every worktree of a checkout
and the checkout itself share one `index_id`. Satisfying query-time
provenance (see Decision, point 4) requires a new field; none of the
existing ones already serve that purpose.

**What this preserves.** trusty-search's registry invariant is unchanged.
Issue #4062 was closed `NOT_PLANNED`, and its closing comment states (quoted
verbatim, verified against the issue): *"`RepoIdentity`'s own module doc
states it is a grouping key... trusty-search's registry is
one-`root_path`-per-id, so the index id must partition — one id, exactly one
content tree."* Under this decision the CHECKOUT is that one content tree;
a worktree's provenance is a TAG on rows, never a registry key, so the
one-`root_path`-per-id partition is never asked to hold two content trees
under one id. Because the invariant holds, the collision guards from #2336,
#3993, and #2519 are untouched by this decision.

**The wider model, for grounding only.** Search is ground truth — what is in
the files; it does not converge, so two checkouts of the same repo have two
indexes. Memory is knowledge of working on a project, independent of where
that work happens; it does converge, so every checkout and worktree of a
project resolves to one palace (see
[ADR-0051](0051-palace-id-stays-hyphen-joined-owner-and-project-fields-added.md)
§1 and ADR-0012 §1). Non-git trees remain first-class in search — an OKF
store indexed as an arbitrary directory is unaffected by this decision.

## Decision

We will make the following four changes, all one decision:

1. **One search index per durable checkout, covering that checkout and every
   worktree nested under it.** No per-worktree index entity is registered in
   trusty-search's registry — a worktree is a subdirectory of the checkout
   it is provisioned from and shares the checkout's single `index_id`. This
   retires the sibling-index / `overlay_of` shape ADR-0050 decision 6 and
   DOC-37 §2.3 described: there is no second registry row to route to or
   join by `repo_identity`, because there is no second registry row.

2. **Only files a worktree has modified are indexed, at chunk granularity.**
   An unmodified file's chunks already exist in the checkout's own rows and
   are not duplicated. A worktree with one modified file contributes only
   that file's chunks to the checkout's index — not a copy of the tree, not
   a file-level marker. `resolve_merge_base_delta` (#5815) is the mechanism
   that computes which files qualify; its `changed`/`deleted` split is what
   lets a modified-then-reverted file's stale chunks be dropped correctly.

3. **Worktree provenance is a TAG on rows inside that one index.** Each
   worktree-contributed chunk row carries a tag naming the worktree it came
   from. Deleting a worktree deletes its tagged rows; it does not delete the
   checkout's index, and it does not touch any other worktree's tagged rows.
   Each worktree's tag ties it to exactly one checkout — the one it was
   provisioned from — mirroring the one-content-tree-per-id invariant this
   ADR preserves (see Context, "What this preserves").

4. **A search result identifies the worktree it came from, alongside the
   file path.** A hit whose chunk carries a worktree tag returns that tag
   together with `file`; a hit from the checkout's own untagged rows returns
   none. This is a consequence of point 1: once a worktree's rows live
   inside the checkout's own index rather than in a separately-identified
   sibling index, `index_id` can no longer disambiguate a worktree hit from
   a checkout hit, so the tag becomes part of what a query returns, not
   merely a deletion key for lifecycle management. `CodeChunk`
   (`core/indexer/types.rs:20-70`) gains a field for this; no existing field
   (`index_id`, `on_branch`) already carries it — see Context, "What the
   current query result shape carries."

   This also fixes the intended meaning of query-time shadowing, without
   designing its implementation: a worktree's modified-file chunks shadow
   the checkout's chunks for those same files when querying in that
   worktree's context, while an unmodified file's chunks hydrate directly
   from the checkout's own rows (there is nothing else for them to hydrate
   from — they are the same rows). The mechanics of how a query is scoped
   to "this worktree's context" are implementation, not decided here.

## Consequences

### Positive

- **No more per-worktree registry bookkeeping.** Registering a sibling
  index, keeping its `repo_identity` join current, and reaping it
  independently of the checkout all go away — there is one registry row per
  checkout, full stop.
- **No duplicate storage for unmodified files.** A worktree pays only for
  the files it actually changed, at chunk granularity, not for a second
  copy of everything it did not touch.
- **Deletion is mechanical.** A worktree's tagged rows are exactly the rows
  a `DELETE` on that tag needs to remove; there is no sibling index whose
  reaping could fall out of step with the worktree's own lifecycle.
- **Simpler than ADR-0050's facet-routing path.** #5069's cross-index
  routing (find a sibling sharing `repo_identity`, load it, re-run the
  query there) is unnecessary when a worktree's rows already live in the
  index being queried.

### Negative / Trade-offs

- **`CodeChunk`'s result shape changes.** A worktree-identifying field is
  new public surface on a chunk that is serialized over HTTP and MCP;
  existing consumers reading chunks positionally or by exhaustive
  destructuring need to tolerate an added field.
- **Query-time shadowing logic is now load-bearing and undesigned.** Point 4
  above states the semantics a worktree-scoped query must have (worktree
  rows shadow checkout rows for the same file; everything else hydrates
  from the checkout) but does not design how a query declares "I am asking
  in the context of worktree X" or how shadowing is implemented against the
  existing BM25/KG/HNSW query paths. This is deferred, not decided here.
- **Six existing artifacts still describe the model this ADR replaces** (see
  below) and will read as current until each is updated in its own
  follow-up. This ADR does not edit any of them.

### Neutral / Follow-up (deferred, not decided here)

This decision obsoletes the following, named here so a reader can find them;
none is edited by this ADR:

- **#5060** (closed, shipped) — `worktree_skip_vector`
  (`crates/trusty-mpm/src/core/session_launch/search_index.rs:105`) still
  registers a separate `index_id` per worktree. Registration call sites like
  this one need to stop minting a worktree `index_id` at all.
- **DOC-37** (`docs/specs/trusty-search-managed-repo-awareness.md`, a merged
  spec, PR #2612) — its §2.2–2.3 overlay/delta-index design is the sibling-
  index shape this ADR retires.
- **#2611** (open design issue) — proposes registering "a lightweight
  overlay index per worktree... marking that index as `kind: delta`,
  `overlay_of: <base index_id>`," the exact mechanism this ADR removes.
- **#1681** (open epic) — its retained WI-6 acceptance criterion reads "Git
  worktree detection + ephemeral sub-index linked to parent"; a sub-index is
  no longer the target shape.
- **#5069** (open) — "the tm checkout (base facet) is not registered as an
  index" is a symptom of the sibling-index model; under one index per
  checkout there is nothing to route a semantic query TO other than the
  checkout's own already-registered index.
- **`crates/trusty-mpm/docs/ARCHITECTURE-MEMORY-SESSIONS-SEARCH.md` §3**
  ("Per-Worktree Search-Index Model") presents independent per-worktree
  indexes as current design with no supersession banner, unlike §2 of the
  same file, which already carries one pointing at ADR-0037.

## Open Questions

Named, not answered here:

1. **Query-scoping mechanism.** How a caller declares "resolve this query in
   worktree X's context" (an explicit parameter, an env-derived default, or
   something else) is not decided — see Decision point 4 and Consequences.
2. **`CodeChunk`'s new field — name and wire shape.** This ADR requires a
   worktree-identifying field on a search result; it does not name the
   field or decide its JSON/MCP shape.
3. **Tag-deletion mechanics.** "Tagged rows are deleted when the worktree is
   deleted" states the outcome, not the trigger — whether deletion is driven
   by the same worktree-reap path ADR-0050 named as a dependency (#5790 /
   `DOC-66 §3`) or by a separate mechanism is not settled here.
4. **BM25/KG split (ADR-0050 decision 4).** Unaffected by this ADR and still
   open as ADR-0050 left it.

## Related Decisions

Vetted against prior ADRs (`docs/adr/INDEX.md`) on 2026-08-17:

- **ADR-0050 (Colocated, path-tied identity with delta-indexed worktree
  facets):** **Amends, specifically decision points 3, 5, and 6's
  per-worktree registration mechanism.** Decision 1 (colocated, path-tied
  storage), decision 2 (one index per project copy — the partitioning
  invariant this ADR also preserves), decision 4 (BM25/KG split), and
  decision 7 (trusty-memory unaffected) all stay in force unchanged; this
  ADR is a refinement of HOW a worktree's delta lands, not a reopening of
  where index data lives or how a checkout's own identity is derived.
  Decision 5's core mechanism (index only the delta against the
  merge-base) is preserved and is what this ADR builds on; what changes is
  that the delta lands as tagged rows in the checkout's own index rather
  than as rows in a separately-registered sibling index. Decision 3's "one
  index covers worktrees" language is consistent with this ADR's outcome in
  intent but was implemented (per decision 6 and the Context section) as a
  sibling-index-with-shared-`repo_identity` mechanism; this ADR replaces
  that mechanism, not the intent behind the words.

  **Pronoun clarification (2026-08-17), recorded here per the immutability
  rule (`docs/adr/README.md`, DOC-46 §4) rather than by editing ADR-0050's
  Context or Decision text in place:** ADR-0050 decision 1 (and its
  identical restatement in Context) reads "the daemon serves whichever
  project **it** is asked about from whichever path **it** is registered
  at" — two occurrences of "it" eight words apart with different referents.
  The first "it" is the PROJECT (the daemon serves whichever project is
  named in the request); the second "it" is the DAEMON (registered at the
  path where the daemon itself is bound to serve that project). This
  ambiguity already caused one agent to misread the sentence as describing
  a rival index-resolution rule. ADR-0050's own Decision and Context text
  is left as originally accepted, per the same immutability rule ADR-0037
  and ADR-0044 already apply this way; this note is the clarification of
  record.

- **ADR-0008 (Project-identity convention: full-path slug of the nearest
  git root):** **Amends (again), nullifying point 4 outright.** ADR-0012
  already amended point 4 once ("worktrees get their own id, keyed on their
  working-directory path" became "worktrees get their own id, as an
  ephemeral sub-index linked to parent"); ADR-0050's Related Decisions
  section then declared point 4 "unaffected" by ADR-0050's own changes.
  That claim does not survive this ADR: under decision 1 above, a worktree
  registers no id of its own at all, so the clause is not merely refined
  again — it no longer applies, though ADR-0008's remaining project-identity
  rules (full-path slug for a non-worktree root, monorepo-subdirectory
  handling) are untouched and stay in force, so the document as a whole
  stays Amended rather than Superseded.

- **ADR-0012 (Per-instance GUID and marker-file identity):** **Consistent on
  identity, superseded on §7's worktree-indexing mechanic.** §2's full-path
  slug PRIMARY key, §3's GUID/move-relink mechanism, and §1's palace
  derivation (referenced, not owned, by ADR-0012) are all untouched. §7's
  "ephemeral sub-index linked to parent" — the mechanism ADR-0050 decision 6
  narrowed to a delta facet and this ADR now removes entirely in favor of
  tagged rows — no longer describes how a worktree is indexed.

- **ADR-0051 (Palace identifiers stay hyphen-joined; `owner`/`project`
  fields added):** **Consistent, different subsystem.** ADR-0051 amends
  ADR-0012 §1 (trusty-memory's palace identity) and is unaffected by this
  ADR, which touches only trusty-search's index registry. Both ADRs agree
  that a worktree does not get its own identity primitive — ADR-0051 because
  a palace already converges across every worktree of a project, this ADR
  because a worktree's search content is now tagged rows in its checkout's
  one index — but neither decision depends on the other holding.

- **Issue #4062 (closed, `NOT_PLANNED`):** **Consistent — this ADR is the
  positive statement of the invariant #4062's closing comment protected.**
  See Context, "What this preserves," for the verified quotation. The
  collision guards from #2336, #3993, and #2519 are untouched.

No conflict found with any Accepted ADR.
