# 0050. Colocated, path-tied index identity with delta-indexed worktree facets

- **Status:** Accepted
- **Date:** 2026-08-16
- **Scope:** crate `trusty-search` (index storage location, identity, worktree
  facet handling, embedding scope). References `trusty-memory` for contrast
  only; no change there.
- **Reversibility Cost:** High — colocated per-path storage and a delta-facet
  data model are both baked into every registered index's on-disk layout;
  reversing either after data has accumulated needs a per-index migration,
  not a config flip.
- **Decision Drivers:** owner ruling, 2026-08-16 conversation; observed
  storage growth in this repo's own colocated corpus; DOC-37's unimplemented
  facet proposal; ADR-0012's path-tied identity precedent; the worktree
  coverage gap the current per-worktree full-index model produces.
- **Supersedes / Superseded by:** none. Accepts (with named divergences) the
  design proposed in DOC-37. Consistent with ADR-0012's identity model;
  extends ADR-0012 §7's per-worktree indexing mechanic. See Related
  Decisions.

## Context

**Where index data lives today.** Issue #403 (recorded in
`crates/trusty-search/src/service/colocated_storage.rs:1-19`) put every
index's on-disk data at `<root_path>/.trusty-search/`, inside the project
tree it indexes, specifically so that "two worktrees of the same repo share
a physical path but are at different filesystem paths; they should have
independent indexes." ADR-0012 (2026-06-25) built the current identity model
on top of that: the full-path slug of the canonicalized project root is the
PRIMARY registry key (§2), a randomly-minted per-instance GUID is a
supplementary anchor for move-relink and de-duplication (§3), and each git
worktree is registered as its own "ephemeral sub-index linked to
parent" — its own **full** walk of the worktree tree, tagged `is_worktree:
true` with a `parent_guid` for auto-pruning (§7). This is the model in
production. An earlier proposal in this conversation to move index data out
of the project tree into the global data directory (`~/Library/Application
Support/trusty-search/...`) was considered and **rejected**: project state
stays colocated with the project files it describes, because the two are
closely synced and colocation is what makes `git worktree remove --force`
also reclaim the index data (ADR-0012 §"Positive" consequences).

**What the current per-worktree full-index model costs.** DOC-37 §1.2's
empirical daemon snapshot (2026-07-14) found the fragmentation this predicts:
independent full indexes per session worktree, an unindexed protected base
clone, and (in this repo alone) two different index ids pointing at the same
`root_path`. This repo's own colocated corpus, measured directly against the
main checkout at `/Users/bob/trusty-mpm-projects/bobmatnyc/trusty-tools/.trusty-search/`
on 2026-08-16:

| File | `du` (actual disk) | apparent (`ls -la`) size |
|---|---|---|
| `.trusty-search/` total | 739 MB | — |
| `index.redb` | 597 MB | 1,077,940,224 bytes (1.08 GB) |
| `hnsw.usearch` | 134 MB | 140,714,860 bytes (140.7 MB, decimal) |
| `hnsw.keys.json` | 7.9 MB | 8,301,999 bytes (8.3 MB) |

The brief's figures (739 MB total, 1.08 GB `index.redb`, 140 MB
`hnsw.usearch`, 8.3 MB `hnsw.keys.json`) match, with one nuance worth
recording: `index.redb`'s *apparent* size (1.08 GB, what `ls -la` and every
byte-count report) is larger than its *actual* disk usage (597 MB, what `du`
reports) — it is a sparse file. Neither figure is wrong; they answer
different questions, and both are cited above.

**Whether the walker sees worktree content today.** `crates/trusty-search/src/service/walker.rs:93-94`
lists `.claude` in `SKIP_DIRS`, matched on **basename**, against every path
component encountered during a recursive walk. A full or incremental walk
rooted at a project's own top-level `.trusty-search`-registered index
therefore never descends into `.claude/worktrees/*` — that content is
structurally invisible to the main project's own index. This directly
answers one of the brief's named open questions: **today, the only search
coverage for a worktree's content comes from that worktree's own,
separately-registered, independently-full colocated index** — created when
something (a `tm` session launch, or manual registration) points
`root_path` directly at the worktree directory, which is not itself a
walked *component* of a `SKIP_DIRS` match, only descendants named `.claude`
are. If the current per-worktree full-index model were removed without
replacing it with something that indexes worktree content another way,
worktree search coverage would silently disappear — the base project's
index has never covered it and structurally cannot without a change to
`SKIP_DIRS` (which this decision does not make; the general recursive
walker's exclusion of `.claude` is preserved, and delta indexing reaches
worktree-changed files by a different, targeted mechanism — see Decision).

**Prior art already partially implemented ahead of formal acceptance.**
DOC-37 (`docs/specs/trusty-search-managed-repo-awareness.md`, Status: Draft)
proposed exactly this direction in §2: a `RepoIdentity` grouping key, a
`live`/`base`/`worktree-delta` facet model (§2.2), and delta indexing for
worktrees reusing `commands/start/reconcile.rs`'s existing git-diff-and-
reindex mechanism (§1.5, §2.3). Independently of DOC-37's Draft status, part
of its Layer 1 MVP (§2.7) has already shipped: `PersistedIndex::repo_identity`
and a `?repo_identity=` filter on `GET /indexes` exist in
`crates/trusty-search/src/service/server/indexes.rs`, a `skip_vector`
registration field exists and is set for worktree indexes, and facet routing
(#5069, `crates/trusty-search/CLAUDE.md`'s documented behavior for
`POST /indexes/:id/search`) already forwards a PINNED semantic query from a
`skip_vector` worktree index to a sibling index sharing its `repo_identity`
that was built with vectors. This decision is substantially the formal
acceptance of DOC-37's direction, made after part of it was already built —
not a green-field proposal.

**A ready-made path-independent identity primitive exists and is not used
here.** `crates/trusty-common/src/project_index_id.rs` (epic #4207, replacing
an approach closed won't-do in #4063) derives `ProjectIdentity{origin, root,
operator}` and renders it as a hashed, three-component `index_id`. Its own
module doc states it is derivation-only: "Nothing here is wired into
`ensure_project_indexed`, `trusty-search serve`, or the daemon's resolution
path." `root` is still the canonical path — the scheme partitions on path
plus origin plus operator, hashed rather than slugged, but it is still
fundamentally path-anchored, not path-independent. This decision leaves it
unwired: it neither adopts nor narrows it. The colocated, path-tied identity
this decision confirms (below) already satisfies the partitioning property
`project_index_id.rs` was built for — N clones of a repo derive N distinct
identities by construction, because each clone's colocated storage lives at
its own path — so nothing here creates pressure to wire in the hashed
scheme. A future decision could still adopt it for its `origin`/`operator`
components (cross-machine dedup, display grouping); this ADR does not decide
that either way.

**How a session is told which index to use.** ADR-0042 (#4181) removed the
prior `.mcp.json`-injection mechanism (a `serve --index <id>` stub written
into the workspace file); `crates/trusty-mpm/src/core/session_launch/search_index.rs:14-19`
documents the replacement: the resolved index id is exported as the
`TRUSTY_INDEX` environment variable by the session spawn
(`core::mcp_session_env`), and `trusty-search serve` honours it since #5394
(`crates/trusty-search/src/main.rs:53,596-601`) — clap fills the same `index`
field from either the flag or the env var, pinning the MCP session's default
`index_id` and fan-out scope. This decision's identity model is path-tied
(the daemon serves whichever project it is asked about from whichever path
it is registered at); `TRUSTY_INDEX` is a second, explicit channel that
names an index id directly, set once at launch. Whether `TRUSTY_INDEX`
continues to name the base project's id for every session regardless of
worktree, or whether a session launched inside a worktree should instead
receive the worktree's own delta-facet id, is not settled by anything read
for this ADR — recorded as an open question below rather than decided here.

**Worktree lifetime is a dependency of this design, not a settled
guarantee.** Under a delta model, a worktree's index data is keyed to that
worktree's existence: a delta facet with no worktree behind it is dead
weight with no natural trigger to remove it, so the design's storage cost
is bounded by how reliably worktrees get reaped, not by how many are
legitimately in use. The state of that reaping, as of this writing:

- Issue #5790 (merged, `feat(trusty-mpm): register agent worktrees against
  their delegation and reap them on exit (#4311)`) built registration on
  dispatch and reaping on `SubagentStop`, behind six refusal gates (dirty
  tree, unpushed commits, unknown git registry, etc.). It is **merged but
  not running** — the installed daemon predates it and has not been
  restarted, because restarting drops live sessions.
- The registrations `#5790` makes are **in-memory only**. A daemon restart
  returns every registered tree to unowned. `#5790`'s own PR body names the
  fix — a `DOC-66 §3` sentinel-payload extension for durable ownership — as
  proposed and not built.
- `git worktree list` on this machine currently shows 17 worktrees, the
  large majority agent-prefixed session/dispatch trees. I did not
  independently verify a specific orphan-count delta (an "11 to 13" figure
  was reported to me but I have no baseline measurement of my own to check
  it against), and a live process on port 7070 reported to me as an orphaned
  `trusty-memory` instance was not observed at the time I checked
  (`lsof -i :7070` returned nothing). Both are plausible given the above and
  neither changes the structural point: the reaping this design would lean
  on is built, unreleased, and not durable across a daemon restart.

This decision proceeds anyway, on the position that the delta model is
sound in isolation. But it depends on worktree lifetime being reliably
bounded, and that dependency does not currently hold. See Decision point 5
and Consequences.

## Decision

We will make the following six decisions, and record trusty-memory's
unchanged status as a seventh, non-decision, item for contrast.

1. **Index data stays colocated at `<project-root>/.trusty-search/`, path-
   tied.** The daemon serves whichever project it is asked about from
   whichever path it is registered at. Project search state stays with the
   project files it describes, because the two are closely synced — an edit
   invalidates index state, and colocation is what already makes `git
   worktree remove --force` reclaim index data as a side effect (ADR-0012).
   The earlier proposal to relocate index data into the global data
   directory is rejected.

2. **One index per project copy.** N clones of a repo at N different paths
   produce N fully independent indexes; they cannot collide, because the
   colocated path IS the identity. This is the property that makes the
   owner comfortable with path-tied identity in a place where a
   path-independent scheme (`project_index_id.rs`, #4063's predecessor)
   previously failed by trying to be a grouping key and a partitioning key
   at once.

3. **That one index covers worktrees.** `.claude/worktrees/` (ADR-0036) lives
   inside the project root, so a worktree is a subdirectory of the indexed
   tree, not a separate project. This decision extends the single project
   index to also cover worktree-changed content, rather than giving each
   worktree its own separate full index the way ADR-0012 §7 does today.

4. **BM25 and the knowledge graph become separate indexes.** They currently
   share one `index.redb`. Splitting them is part of this decision; the
   storage-layer mechanics of the split are implementation, not decided
   here.

5. **Worktrees are indexed as DELTAS against the merge-base, not as full
   copies.** Only files that differ from the merge-base are indexed for a
   given worktree; unchanged files exist once, in the base. The merge-base
   is used rather than main's current HEAD so that deltas stay stable while
   main moves — the owner's stated reason: "we're tracking deltas in
   worktrees." This decision depends on worktrees being reliably reaped
   when their session ends (see Context, "Worktree lifetime"); a delta
   facet has no size ceiling of its own and a leaked worktree leaks its
   delta with it. This decision does not design the reaping mechanism — a
   separate investigation is running on that — but records the dependency:
   the storage-bound property this decision is partly justified by (no more
   full per-worktree copies) only holds if worktree lifetime is bounded,
   and today that bound is not durable across a daemon restart.

6. **Embeddings are computed for the main project only.** Worktree-changed
   files get BM25 and KG coverage but no vectors of their own, enforced by a
   path exclusion (extending the existing `skip_vector` registration field
   already used for worktree indexes today). A worktree's semantic search
   reaches vectors only via facet routing to the base (already partially
   built, #5069). Consequence: keeping the main project's embeddings fresh
   matters more under this design than it did before, because a stale base
   embedding is now the ONLY semantic signal available to every worktree
   derived from it, not just to queries against the base itself.

7. **trusty-memory is unchanged.** One palace per project, keyed
   `owner/repo`, shared across all worktrees — ADR-0012 §1 already decided
   this and it is not revisited here. trusty-memory's knowledge graph is a
   separate implementation from trusty-search's KG; they share no storage.

## Consequences

### Positive

- **No more O(worktrees) full copies.** The dominant cost driver DOC-37
  §1.2 measured — one full independent index per session worktree,
  duplicating every unchanged symbol N+1 times — is dissolved rather than
  filtered at query time. A worktree that changes ten files pays for ten
  files, not for a second copy of the repo.
- **The hydration problem is resolved by construction, not by keying.** An
  HNSW hit returns a `chunk_id` whose text lives in the BM25/redb corpus;
  under a shared-embedding, per-worktree-chunk design, that pointer could
  reference content a worktree never indexed. Under the delta/facet model,
  a changed file's chunks shadow the base's for that worktree, and an
  unchanged file's chunks hydrate from the base directly — no content-hash
  keying scheme is needed to keep a hit's text and its facet in sync.
- **Consistent with the identity model already in production (ADR-0012).**
  Nothing about registry keying, GUIDs, or move-relink changes; this
  decision only changes what content is indexed under a worktree's existing
  id.
- **Storage growth is bounded to actual diffs**, once worktree lifetime is
  bounded (see Negative below — this is conditional, not automatic).

### Negative / Trade-offs

- **Depends on a guarantee that does not currently hold.** As stated in
  Context and Decision point 5, the storage-bound argument above requires
  reliable worktree reaping. `#5790` built it; it is not running, and even
  once it is, its registrations do not survive a daemon restart until the
  `DOC-66 §3` durable-sentinel follow-up lands. Until both are true, a
  leaked worktree's delta index leaks with it, silently, the same way an
  un-reaped full worktree index leaks today — the failure mode changes
  shape (smaller per-instance cost) but does not disappear.
- **Keeping main's embeddings fresh now matters more.** Under decision 6, a
  stale base embedding degrades semantic search for every worktree derived
  from it, not just for queries against the base. This raises the cost of
  a slow or skipped base reindex compared to the current per-worktree-full-
  index model, where a stale worktree index only degrades that one
  worktree's own search.
- **Non-git and orphan-branch project copies have no defined base facet.**
  See Open Questions.
- **BM25/KG storage split is new work**, not free — decision 4 requires a
  storage-layer change beyond what today's single `index.redb` does.

### Neutral / Follow-up (deferred, not decided here)

- The exact on-disk/schema mechanics of the BM25/KG split (decision 4).
- The worktree-reaping durability gap named above — tracked by the
  in-flight investigation and `DOC-66 §3`, not by this ADR.
- Whether/how `project_index_id.rs` (#4207) is ever wired in for its
  `origin`/`operator` components — left open, not required by this
  decision.

## Open Questions

Named, not answered here:

1. **Base facet for a non-git project, or a git repo with no merge-base**
   (e.g. an orphan branch). What does "delta against the merge-base" mean
   when there is no merge-base to compute?
2. **Revalidation.** What happens to a worktree's delta when the merge-base
   itself moves — for example, after a rebase? Does the delta recompute
   against the new merge-base, and if so, on what trigger?
3. **Lazy embedding for worktree-changed files.** Given decision 6 excludes
   worktree-changed files from the main embedding pass, should they be
   embedded lazily on demand (e.g. the first time a semantic query pins to
   that worktree) rather than never?
4. **Delta lifecycle vs. worktree lifecycle.** What happens to a delta
   index when its worktree is reaped? Is reaping the trigger that deletes
   the delta index, or are the two cleaned up independently (and if
   independently, by what, and on what schedule)? This is the sharpest edge
   of the worktree-lifetime dependency named in Decision point 5 and is
   explicitly not answered here.
5. **What `TRUSTY_INDEX` names for a worktree-launched session.** Today it
   pins an MCP session to one index id, set once at launch (see Context,
   "How a session is told which index to use"). Under this decision, should
   a session launched inside a worktree receive `TRUSTY_INDEX` pointing at
   the base project's id (relying entirely on facet routing to reach the
   worktree's delta), or at the worktree's own delta-facet id directly? Not
   settled by anything read for this ADR.

## Related Decisions

Vetted against prior ADRs (`docs/adr/INDEX.md`) on 2026-08-16:

- **ADR-0012 (Per-instance GUID and marker-file identity):** **Consistent
  on identity, Extends on worktree indexing mechanics.** ADR-0012 §2's
  full-path-slug PRIMARY key and colocated-storage preservation are exactly
  what this decision confirms (point 1–2); nothing here touches the
  GUID/move-relink/same-GUID-collapse mechanism. ADR-0012 §7 amended
  ADR-0008 to make each git worktree its own "ephemeral sub-index linked to
  parent" — but that sub-index was still a **full** walk of the worktree
  tree, tagged with `parent_guid` purely for auto-pruning. This decision
  extends §7's worktree handling: instead of a full independent walk per
  worktree, only the diff against the merge-base is indexed, hydrating
  unchanged content from the base facet. The parent-linkage concept survives
  in spirit (a worktree's delta is still attributed to its base); its
  mechanics change from "index everything, tag the parent" to "index the
  diff, hydrate from the parent."
- **ADR-0008 (Project-identity convention: full-path slug of the nearest
  git root):** **Consistent.** Point 4 — "worktrees get their own id, keyed
  on their working-directory path" — is unaffected. This decision changes
  what content is indexed under a worktree's id, not how that id is
  derived.
- **ADR-0036 (All worktrees are siblings under `.claude/worktrees/`):**
  **Consistent, with an inherited dependency.** Decision point 3 above
  states that one project index covers its worktrees because they are
  subdirectories of the project root — true regardless of ADR-0036's
  specific migration, since both the pre-migration shape (`./.worktrees/`)
  and the post-migration shape (`.claude/worktrees/`) nest inside the
  project root either way. What this decision DOES inherit from ADR-0036 is
  its own named, unresolved risk: ADR-0036's Consequences state "existing
  worktrees are not relocated by this decision," so the migration to a
  single sibling location is not itself complete. That incompleteness does
  not break decision 3's "worktrees are nested inside the project root"
  premise (both shapes satisfy it), but it does mean an implementation of
  this ADR must not assume a single worktree-root constant; it must handle
  both `./.worktrees/<name>` and `.claude/worktrees/<name>` until ADR-0036's
  migration lands.
- **DOC-37 (trusty-search Managed-Repo Awareness, Draft spec, cited per
  DOC-46's ADR↔Spec cross-linking rule):** **This ADR is substantially the
  acceptance of DOC-37's §2 proposal**, with named divergences: (a) DOC-37's
  §2.2 base facet was written against the retired `.base` bare-clone
  topology and needs re-reading against ADR-0036's sibling-worktree shape
  instead — this ADR does that re-reading implicitly by keying "base" to
  the project root rather than to a `.base` clone; (b) DOC-37 phases the
  `repo_identity` field and `skip_vector`-style exclusion as future MVP work
  (§2.7); this ADR's Context records that both already shipped ahead of
  DOC-37's own Draft status being resolved; (c) DOC-37 does not discuss
  worktree-lifetime dependency at all — this ADR adds it as an explicit,
  load-bearing dependency (Decision point 5) that DOC-37's proposal is
  silent on. DOC-37 should be updated to Accepted (or superseded outright)
  as a follow-up; this ADR does not itself change DOC-37's Status field.
- **`crates/trusty-common/src/project_index_id.rs` (#4207 derivation, not
  an ADR):** **Left unwired, not adopted, not in conflict.** See Context,
  "A ready-made path-independent identity primitive exists." This decision
  does not require it and does not preclude adopting it later for its
  `origin`/`operator` components.
- **Issue #5790 / `DOC-66 §3` (worktree registration and reap, not an
  ADR):** **Dependency, not vetted as a conflicting decision.** #5790 is
  merged but not running in production, and its registrations are
  in-memory only pending the `DOC-66 §3` durable-sentinel follow-up. See
  Context, "Worktree lifetime is a dependency of this design," and Decision
  point 5.

No conflict found with any Accepted ADR.
