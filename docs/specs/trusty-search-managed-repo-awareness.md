# DOC-37 — trusty-search Managed-Repo Awareness: Canonical Identity, Base/Live Tracking, Worktree Delta Indexing

**Status:** Draft
**Subsystem:** trusty-search — index registry / identity; trusty-mpm — managed-session provisioning
**Owner:** Engineering (trusty-tools)
**Last-updated:** 2026-07-14
**Spec ID:** `SPEC-SEARCHREPO-01~draft` … `SPEC-SEARCHREPO-04~draft` (DOC-37)
**Builds on:** DOC-34 — Managed sessions launch with a tm-owned `CLAUDE_CONFIG_DIR`
under `~/.trusty-tools` (`docs/specs/managed-session-config-dir.md`, the
FULL-SEGREGATION design philosophy establishing `.base` protected clones +
`.trusty-mpm-worktree`-marked session worktrees); `trusty-common`'s
`github_path` module (issue #1220, canonical `owner/repo` derivation).
**Cross-ref:** `crates/trusty-common/src/index_id.rs` (`derive_index_id`,
`resolve_project_root`), `crates/trusty-common/src/github_path.rs`
(`derive_github_path`, `parse_github_path`), `crates/trusty-search/src/detect.rs`
(`detect_project`), `crates/trusty-search/src/service/server/indexes.rs`
(`create_index_handler`), `crates/trusty-search/src/service/colocated_storage.rs`,
`crates/trusty-search/src/allowlist/mod.rs`, `crates/trusty-search/src/commands/start/reconcile.rs`,
`crates/trusty-search/src/commands/prune_orphans.rs`, `crates/trusty-mpm/src/core/session_launch/search_index.rs`
(`register_project_index`), `crates/trusty-mpm/src/core/hook.rs`,
`crates/trusty-mpm/src/client/executor/managed.rs` (`managed_decommission`);
tracked as GitHub issue **#2611**.

> **Scope note.** This is a design/findings spec: §1 documents trusty-search's
> CURRENT behavior (with file:line evidence and a live-daemon empirical
> snapshot) toward the three repo facets tm's managed-clone workflow creates;
> §2 proposes the target design; §3 lists open questions requiring Bob's
> decision before implementation starts. No code changes ship with this spec.

---

## 0. Motivating directive (Bob, verbatim, 2026-07-14)

> since we clone tm managed repos into a protected directory, let's make sure
> trusty-search is aware and knows how to handle indexes. E.g., the main
> project, the protected project, and the worktree. In fact it should probably
> keep track of both, and index any differences.

Background: trusty-mpm (`tm`) provisions managed workspaces. For a live user
checkout (e.g. `~/Duetto/repos/duetto`), tm creates a protected managed clone
at `~/trusty-mpm-projects/<org>/<repo>/` (a `.base` clone); sessions then get
git worktrees under `.base/.worktrees/<session-id>/` (a `.trusty-mpm-worktree`
marker file exists at worktree roots). So the SAME repo content can exist in
up to three places — **live checkout**, **protected `.base` clone**, and
**N session worktrees** that diverge by branch — and trusty-search currently
treats every one of these as a wholly unrelated directory.

---

## 1. Findings — what trusty-search does TODAY (with evidence)

### 1.1 Index identity is a bare path basename, not a repo identity

`trusty-common::derive_index_id` (`crates/trusty-common/src/index_id.rs:75-82`)
returns `project_root.file_name()` verbatim (not slugified, preserved
byte-for-byte for backward compatibility with already-indexed projects).
`resolve_project_root` (`crates/trusty-common/src/index_id.rs:46-58`) walks up
from a start path to the nearest ancestor containing a `.git` entry — its own
doc comment notes `.git` is a directory in a normal clone and a **file** in a
worktree, and `exists()` matches both, "so worktrees resolve correctly." But
"correctly" here only means the walk terminates at the *worktree's own* `.git`
file — it does **not** follow the `gitdir:` pointer to the shared repository,
so each worktree resolves **itself** as the project root, not the shared
parent.

Both callers agree on this identical derivation (single source of truth,
issue #1373):
- trusty-search's own CLI/MCP auto-detection: `detect_project()`
  (`crates/trusty-search/src/detect.rs:43-60`).
- trusty-mpm's session-launch registration: `register_project_index()`
  (`crates/trusty-mpm/src/core/session_launch/search_index.rs:110-143`),
  which calls `trusty_common::resolve_project_root` then `derive_index_id`,
  `POST`s `{id, root_path}` to `/indexes` (find-or-create, idempotent), and
  best-effort triggers a reindex.

Net effect: a session launched inside `.base/.worktrees/<uuid>/` registers an
index keyed by the literal UUID directory name, unrelated to any index for
the live checkout or the `.base` clone of the same repo.

### 1.2 Empirical confirmation — live daemon `list_indexes` (this session, 2026-07-14)

The running daemon's index registry already exhibits the fragmentation §1.1
predicts, for `bobmatnyc/trusty-tools` alone:

| index_id | root_path | facet |
|---|---|---|
| `2eb72dca-de08-481b-8dfa-22ab7f81b1f9` | `.base/.worktrees/2eb72dca-…` | session worktree (this research session) |
| `f443c12d-2fb6-4ce1-9f70-2e7695306e47` | `.base/.worktrees/f443c12d-…` | another session worktree |
| `tm-trusty-tools-01` | `.worktrees/tm-trusty-tools-01` | yet another worktree |
| `trusty-tools` | `/Users/masa/Projects/trusty-tools` | an unrelated **live checkout**, not even the managed `.base` clone |

The protected `.base` clone root itself
(`/Users/masa/trusty-mpm-projects/bobmatnyc/trusty-tools/.base`, no
`.worktrees/<id>` suffix) has **no index entry at all** — it is never indexed
as its own facet, because sessions launch from worktrees, never from `.base`
directly.

The same pattern repeats across the daemon's ~75 registered indexes: e.g.
`duetto-backend` and `duetto-frontend` are two *different* index ids pointing
at the exact same `root_path` (`/Users/masa/Duetto/repos/duetto`) — a straight
duplicate full index of one directory — and there are separate
`tm-*`/session-UUID indexes for `apex`, `apex-companion`, `duetto`,
`mcp-services`, `code-intelligence`, `cto`, `xflux`, `demo-agentic-ux`, each a
fully independent full index with zero relationship to its siblings.

### 1.3 The fragmentation is known and partially mitigated, but only at the storage layer, never at the identity layer

`crates/trusty-search/src/service/colocated_storage.rs:1-19` documents (issue
#403) that colocated `<root>/.trusty-search/` storage was chosen specifically
because "two worktrees of the same repo share a physical path but are at
different filesystem paths; they should have independent indexes" — the
design **explicitly chose full independent per-worktree indexes** with no
linkage or delta mechanism. A beneficial side-effect: because index storage
lives *inside* the worktree directory, `git worktree remove --force` deletes
the on-disk index data along with the worktree — but the daemon's in-memory
`DashMap<IndexId, Arc<IndexHandle>>` registry entry survives until either a
restart or a manual `trusty-search prune-orphans` (issue #489,
`crates/trusty-search/src/commands/prune_orphans.rs`), a CLI command an
operator must run by hand. Nothing in tm's session-decommission path
(`crates/trusty-mpm/src/core/hook.rs` "a git worktree was removed" event,
`crates/trusty-mpm/src/client/executor/managed.rs::managed_decommission`)
calls trusty-search's `DELETE /indexes/:id`. Orphaned worktree indexes
accumulate indefinitely.

Separately, `crates/trusty-search/src/allowlist/mod.rs:1-21` documents an
earlier incident where trusty-search "previously auto-registered any
directory it encountered (cwd probes, MCP calls, transient worktrees),
creating 74 unrequested indexes including private directories with personal
data" — the resulting default-deny allowlist governs **whether** an index may
be created, not **what identity** it is created under; it does not deduplicate
by repo identity either.

### 1.4 A ready-made canonical-repo-identity primitive already exists — and is unused by trusty-search

`crates/trusty-common/src/github_path.rs` implements exactly the canonical
repo identity trusty-search is missing: `derive_github_path(dir)` shells out
to `git -C <dir> config --get remote.origin.url` (which transparently resolves
worktree `gitdir:` pointers — see its own doc comment, `github_path.rs:196`)
and parses the URL into a slugified `{owner, repo}` pair via
`parse_github_path`. It is currently consumed only by (a) tm's
managed-workspace-root path convention (`~/trusty-mpm-projects/<owner>/<repo>/`,
issue #1220) and (b) trusty-memory's palace-ID derivation (#1217).
trusty-search's `index_id.rs` does not call it, use it, or know it exists.

### 1.5 Git-aware delta reconciliation infrastructure already exists — just not applied across facets

`crates/trusty-search/src/commands/start/reconcile.rs` (issues #1670/#1672)
already reconciles a *single* index against its own git history at daemon
boot: for indexes with a stored `indexed_head_sha`, it runs
`git diff --name-only <stored>..HEAD` and reindexes only the changed files
(falling back to a full reindex above `FULL_REINDEX_THRESHOLD = 250` files).
This is the exact mechanism the delta-indexing proposal in §2.3 needs — it
just needs to be pointed at a **different index's** committed tree (the base
facet) instead of the same index's own prior HEAD. Similarly, the `search`
HTTP endpoint's existing `branch`/`branch_files` handling already computes
"files changed on the current branch relative to a merge-base" server-side
(via `git merge-base HEAD <branch>` + `git diff --name-only`) for a different
purpose (score-boosting branch-modified chunks) — the same file-list
computation is reusable for delta scoping.

---

## 2. Design proposal — trusty-search managed-repo awareness

### 2.1 Repo identity

Introduce a canonical **`RepoIdentity`** derived the same way
`github_path::derive_github_path` already does (git remote origin URL,
slugified `{owner}/{repo}`), with a content-hash fallback (first-commit SHA,
e.g. `git rev-list --max-parents=0 HEAD`) for repos with no remote. Every
index gains an optional `repo_identity: Option<RepoIdentity>` field alongside
its existing `index_id`/`root_path`. `index_id` remains the primary key
(backward compatible with every existing index and every caller that already
derives it from a basename) — `repo_identity` is an added join key, not a
replacement.

### 2.2 Facet model: live / base / worktree-delta

For any `RepoIdentity`, trusty-search should be able to enumerate its known
**facets**:

- **`live`** — the user's original checkout (whatever path had this identity
  before tm provisioned anything).
- **`base`** — the tm-managed protected clone at
  `~/trusty-mpm-projects/<owner>/<repo>/.base` (or `…/<owner>/<repo>/` when
  unworktreed — per DOC-34's FULL-SEGREGATION convention). This facet is
  currently **never indexed at all** (§1.2) and should become the canonical
  full index for a managed repo — the thing every worktree's delta is
  computed against.
- **`worktree-delta`** — one per session worktree, computed against the
  `base` facet's HEAD, and delta-indexed for only the changed files (reusing
  the reconcile.rs git-diff-and-reindex mechanism, §1.5) — never a full walk
  of the worktree tree.

A registry-level `GET /repos/:identity/facets` (or folded into
`GET /indexes?repo_identity=…`) lists all known facets and their kind/root/
freshness for a given identity — this directly answers "does trusty-search
know about the live checkout, the protected clone, AND this worktree" for a
given repo.

### 2.3 Delta indexing for worktrees (the core of Bob's ask)

Do **not** full-index each worktree. Register a lightweight **overlay index**
per worktree:

1. On worktree-session provisioning, resolve `repo_identity` and find (or
   create) the `base` facet's full index.
2. Compute the worktree's diff vs. its merge-base
   (`git merge-base HEAD origin/main` / the base facet's indexed commit),
   reusing exactly the file-list mechanism the `search` endpoint's existing
   `branch_files` parameter already computes server-side, plus the
   reconcile.rs git-diff-and-reindex path (§1.5).
3. Index ONLY the changed/added files under the worktree's own `index_id`,
   marking that index as `kind: delta`, `overlay_of: <base index_id>`.
4. **Query time**: a search against a `delta` index first serves matches from
   its own (small) overlay corpus, then fans out to the `base` index for
   every chunk NOT shadowed/deleted by the overlay, merging via the existing
   RRF fusion path. Each result carries a `facet` field (`"live" | "base" |
   "worktree-delta:<id>"`) so callers can tell which physical copy a hit came
   from — line numbers and content can legitimately differ between facets.

This turns an O(full repo) reindex per worktree into O(diff size) — matching
the philosophy already proven in `reconcile.rs` (§1.5), just applied
cross-index instead of intra-index.

### 2.4 Lifecycle

- **Registration**: at tm session-launch time (today's
  `register_project_index()` call site in
  `crates/trusty-mpm/src/core/session_launch/search_index.rs`), additionally
  pass `repo_identity` (already computable via `trusty_common::github_path`,
  which trusty-mpm depends on transitively) and `overlay_of` when the launch
  root is a worktree under a tm-managed `.base` tree (detectable via the
  existing `.trusty-mpm-worktree` marker file). `POST /indexes` gains optional
  `repo_identity` / `overlay_of` fields, purely additive — omitted fields keep
  today's flat-index behavior.
- **Base facet bootstrap**: the FIRST session provisioned under a given
  `.base` clone should register/reindex the `base` facet once (idempotent,
  same find-or-create pattern `register_project_index` already uses) rather
  than leaving it perpetually unindexed (§1.2).
- **Cleanup**: wire `DELETE /indexes/:id` into tm's
  `managed_decommission`/worktree-removal hook path
  (`crates/trusty-mpm/src/core/hook.rs` "a git worktree was removed" event,
  `crates/trusty-mpm/src/client/executor/managed.rs::managed_decommission`) —
  today neither calls trusty-search at all (§1.3). This closes the
  orphan-accumulation gap that currently requires manual
  `trusty-search prune-orphans`.
- **Staleness**: the `base` facet refreshes via the existing boot-time /
  file-watcher reconcile paths; delta facets refresh their overlay whenever
  the worktree's merge-base or working tree changes (file-watcher already
  running per-index; only the diff-vs-base recomputation is new).

### 2.5 Query-time semantics

- Agents in a session worktree keep passing the SAME `index_id` they are
  pinned to today (no behavior change for the common case — MCP tool
  defaults already resolve to the session's pinned index).
- What changes under the hood: if that `index_id` is registered as a `delta`
  facet, results are the overlay-then-base merge described in §2.3, and each
  result's `facet` field discloses provenance.
- `search_all` (fan-out across every registered index) should collapse
  facets of the same `repo_identity` by default (dedupe live/base/delta
  triplicate hits) with an opt-out flag for callers that explicitly want
  cross-facet comparison (e.g. "what changed in my worktree vs. base").

### 2.6 Layering: trusty-search API first, tm integration second

This is explicitly phased so trusty-search's daemon-side model is useful
standalone (any caller, not just tm) before tm is taught to populate it:

- **Layer 1 (trusty-search daemon/API surface)**: `RepoIdentity` type,
  `repo_identity`/`overlay_of` fields on `IndexHandle` + `POST /indexes`, the
  facet-listing endpoint, delta-registration + overlay-query-merge logic,
  reuse of `reconcile.rs`'s git-diff engine for delta computation. No tm
  changes required to land this — it is exercisable via the CLI/HTTP API
  directly with manually-supplied `repo_identity`/`overlay_of`.
- **Layer 2 (trusty-mpm integration)**: `session_launch/search_index.rs`
  computes and passes `repo_identity`/`overlay_of` automatically; the
  worktree-removal hook wires `DELETE /indexes/:id`; base-facet bootstrap on
  first managed-clone provisioning.

### 2.7 Phasing

**MVP** (Layer 1 minimum, no tm changes):
- `RepoIdentity` type + `derive` reusing `github_path` logic (move/re-export
  from `trusty-common`, already a shared dep).
- Additive `repo_identity` field on index registration + `GET /indexes?repo_identity=`
  filter (no delta/overlay yet — just makes the existing fragmentation
  *visible* and groupable).
- `trusty-search prune-orphans` gains a `--repo-identity` grouped report mode
  so an operator can see, for one repo, every live/base/worktree index and
  their staleness at a glance — pure read-side value, ships fast, de-risks
  the harder delta work.

**Follow-up**:
- `overlay_of` + delta indexing (§2.3) — the highest-value, highest-complexity
  piece; depends on MVP's identity plumbing.
- Query-time overlay-then-base merge + `facet` field on results.
- tm Layer 2 wiring (auto-populate identity/overlay at session launch,
  base-facet bootstrap, decommission cleanup hook).
- `search_all` facet-dedup default + opt-out flag.

### 2.8 Crates touched

- **trusty-search** (primary): `core/registry.rs` (`IndexHandle` gains
  `repo_identity`/`overlay_of`), `service/server/indexes.rs`
  (`create_index_handler` accepts the new fields), a new
  `core/repo_identity.rs` (or reuse trusty-common's), delta-query merge logic
  in the search path, `commands/start/reconcile.rs` extended for cross-index
  diffing, `commands/prune_orphans.rs` extended for grouped reporting.
- **trusty-common**: `github_path.rs` is the natural home for
  `RepoIdentity`/`derive_github_path` to be reused (or promoted) by
  trusty-search — no functional change needed there, just a new consumer.
- **trusty-mpm** (Layer 2): `core/session_launch/search_index.rs`
  (`register_project_index` passes identity/overlay), `core/hook.rs` +
  `client/executor/managed.rs` (decommission → `DELETE /indexes/:id`).

---

## 3. Open questions for Bob

1. **Base-facet ownership**: should the `.base` clone's full index be created
   eagerly the moment `tm` provisions the managed clone, or lazily on the
   first session that needs it? (Eager costs indexing time up front for repos
   that may never get a session; lazy means the first session pays that cost
   inline.)
2. **Delta staleness threshold**: reuse the existing
   `FULL_REINDEX_THRESHOLD = 250` files constant from `reconcile.rs` for
   "worktree diff too big, fall back to full index" — or pick a different
   threshold tuned for typically-small worktree diffs?
3. **Live checkout facet**: should trusty-search actively try to discover a
   repo's `live` facet (e.g. by remembering the pre-managed-clone path when
   tm first provisions), or is `live` simply "any other index that happens to
   share the same `repo_identity`," discovered opportunistically rather than
   tracked?
4. **Cross-facet dedup default for `search_all`**: collapse to one hit per
   `repo_identity` by default, or keep today's flat fan-out and require an
   explicit `dedupe_by_repo_identity: true` opt-in to avoid surprising
   existing callers?
5. **Orphan cleanup trigger**: wire `DELETE /indexes/:id` synchronously into
   `managed_decommission`, or make it a best-effort async fire-and-forget
   (matching the existing best-effort pattern for index registration at
   launch) so decommission is never blocked by a slow/down search daemon?
6. **MVP scope sign-off**: does the phasing in §2.7 match Bob's priority (get
   identity + visibility fast, defer delta-indexing complexity), or should
   delta indexing for worktrees be pulled into the first shippable increment
   given it's the most concrete pain point named in the directive?

---

## References

- GitHub issue **#2611** — findings + design tracking, mirrors this spec.
- DOC-34 — `docs/specs/managed-session-config-dir.md` (FULL-SEGREGATION
  philosophy, `.base` clone / worktree provisioning convention).
- Issue #1373 — single-source-of-truth index-id derivation between
  trusty-mpm and trusty-search (`derive_index_id`).
- Issue #1220 — `github_path` canonical `owner/repo` derivation for the
  managed-workspace-root convention.
- Issue #403 — colocated `<root>/.trusty-search/` storage (per-worktree
  independent storage rationale).
- Issue #489 — `trusty-search prune-orphans`.
- Issues #1670/#1672 — boot-time git-diff / mtime reconciliation
  (`commands/start/reconcile.rs`).
