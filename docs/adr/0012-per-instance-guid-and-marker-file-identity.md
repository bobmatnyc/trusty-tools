# 0012. Per-instance GUID and marker-file canonical identity for trusty-search indexes

- **Status:** Proposed
- **Date:** 2026-06-25
- **Scope:** trusty-search (index discovery, registration, move-relink,
  same-GUID collapse). Cross-concerns: trusty-memory (project vs session identity),
  trusty-agents (worktree tracking), trusty-controller (registry/palace sync).
  Also referenced by issue #1680 (case-insensitive aliases) and epic #1681
  (canonical index/source identity model).
- **Supersedes / Superseded by:** ADR-0008 (partial — specifically the worktree
  clause: "Worktrees get their own id, keyed on their working-directory path"
  is SUPERSEDED by decision §7 below).

## Context

Today trusty-search uses an implicit string-ID keying model derived from ADR-0008:
each index is keyed by the full-path slug of its root (`Users_mac_workspace_my-project`).

**Limitations of the current model:**

1. **No stable instance identity across moves.** If a user `mv ~/work/project ~/archive/project`,
   the full-path slug changes (`Users_mac_work_project` → `Users_mac_archive_project`). Move-relinking today is a fragile heuristic in `start_restore.rs::try_locate_moved_root` that searches for a single unclaimed index entry by comparing checksums — this fails when multiple unclaimed entries exist or when git-origin matches neither.

2. **No GUID for same-GUID-collapse deduplication.** The daemon registry can hold duplicate entries for the same source (live case observed: `apex` and `APEX` collided). There is no mechanism to recognize and collapse duplicates except by string-matching the full path, which fails for moved sources.

3. **No notion of "multiple indexes per source."** A single git repo has exactly one entry in the registry, by design (ADR-0008 §4 is absolute: worktrees are "distinct projects"). But the live worktree case is more subtle:
   - A developer using `.claude/worktrees/<feature>` creates a sibling checkout that legitimately deserves its own sub-index for code-browsing during agent development (branches differ, some files are feature-gated).
   - Currently, the worktree *either* pollutes the parent's index with branch-specific content *or* gets registered as a standalone project (opaque inheritance chain, no link to the parent).
   - 74 spurious worktree indexes were observed in production (see #1671 motivation), necessitating the `is_allowlisted` safeguard (auto-deregister on daemon restart if the path no longer exists).

4. **No provenance metadata.** `PersistedIndex` records `root_path`, `indexed_head_sha`, and `last_indexed_unix` but has no `git_origin` or `directory_name` for:
   - Detecting when a source has been moved vs cloned.
   - Establishing which entry to keep when same-GUID-collapse is triggered.
   - Validating that the on-disk marker file is in the intended repo.

5. **Case-insensitive volume collisions (#1680).** On APFS (case-insensitive), `apex` and `APEX` coexist in the registry as two separate entries despite resolving to the same inode. The ADR-0008 case-folding follow-up is deferred; ADR-0012 provides the mechanism (lowercase aliases and an alias-layer normalization in lookups) to defer the full worktree-identity rearchitecture.

6. **Migration hazard on move or re-key.** The colocated `.trusty-search/index.redb` lives at `root_path/.trusty-search/` so moving the root leaves it in place (existing heuristic tries to find it). Non-colocated indexes (legacy, e.g. `~/.trusty-search/apex.redb`) have no home reference and must be re-keyed at the data-move cost if the root moves or changes.

**Design anchors:**

- **Preserve the colocated storage model.** New code must NOT force migration of existing colocated indexes; they are path-addressed and the registry re-key is cheap (only update the in-memory key, data stays in place). This is ADR-0008's cardinal win: near-zero move cost.
- **Preserve the full-path slug as PRIMARY registry key.** ADR-0008 is locked in and the slug is baked into every tool's state directory and the controller's ensure/report logic. The GUID is a SUPPLEMENTARY identity used for move-relink and same-GUID-collapse heuristics, **not** a replacement primary key.
- **Preserve per-project memory identity.** trusty-memory derives palace slugs from `<git-origin>-<commit-pin>` (optionally overridden by `TRUSTY_MEMORY_PALACE`); this is sound and must remain unchanged. Palaces are project-scoped (shared across all worktrees of the same repo), not instance-scoped.

## Decision

We adopt a **hybrid model** layering a **per-instance GUID** and a **marker-file provenance record** on top of the existing full-path slug, plus **session-scoped palace namespace** for trusty-memory.

### 1. trusty-memory = per-PROJECT + optional SESSION namespace

**Preserve the current per-project palace derivation unchanged.** A palace slug is derived from `git_origin + committed_pin` (or `TRUSTY_MEMORY_PALACE` override) and is shared across all worktrees and branches of the same repo.

**Add OPTIONAL caller-supplied `session_id` (e.g., from `trusty-mpm`'s session context).** When a `session_id` is provided, create a child namespace:
- **Palace slug:** `<project-slug>` (existing, project-global)
- **Session child namespace:** `<project-slug>/<session_id>` (ephemeral, prunable after session end)

The `session_id` is **always explicitly supplied** by the caller (e.g., `trusty-mpm`, `tctl`), **never derived from the filesystem**. This is short-term wiring; the proper `session_id` param in the memory API is a follow-up.

**Benefit:** Memory scoped to a session (e.g., debug facts gathered during an agent run) does not pollute the global project palace. Cleanup is a simple `trusty-memory palace delete <project-slug>/<session-id>` after the session ends.

### 2. trusty-search = per-SOURCE-INSTANCE, full-path slug as PRIMARY key

An **instance** is a specific physical checkout at a specific absolute path. Two clones of the same repo at different paths = two instances = two indexes.

- **PRIMARY registry key:** full-path slug (ADR-0008 decision, unchanged, e.g., `Users_mac_workspace_my-project`)
- **SUPPLEMENTARY stable identity:** a randomly-minted UUID v4 stored locally, used for move-relink and same-GUID-collapse.
- **Link mechanism:** the on-disk marker file at `.trusty-search/config.yaml` (or split into `provenance.yaml` + `local.yaml`).

**Why not a deterministic GUID hash(path+origin)?**
Deterministic hashes defeat move-detection: if the GUID is `hash(origin + relpath)`, moving the directory changes `relpath` and the GUID changes, breaking the "same GUID at new path" move-relinking rule. A minted UUID survives moves unchanged and is the only reliable stable identity.

### 3. Per-instance GUID — randomly minted, stored colocated, local-only

- **Minting:** UUID v4, randomly generated, minted once on first index registration or discovery.
- **Storage:** In the marker file at `.trusty-search/config.yaml` (or `local.yaml` section), marked as LOCAL-ONLY / gitignored.
- **Lifespan:** Persists across moves; the GUID stays the same even if `root_path` changes.
- **Clones and forks:** A clone of a repo **must NOT inherit the GUID**. Each clone gets its own minted GUID on first discovery/registration.
- **Fallback:** First-write crash (rare; daemon crashes before persisting the marker) falls back to the existing `try_locate_moved_root` heuristic; no data loss, just a missed optimization.

**Why store in marker file, not in index.redb?**
The marker file is checked first, before the index is opened. If the index is missing but the marker exists, we can still identify the instance by GUID. Index.redb can be absent, corrupted, or rebuilt; the marker is the stable anchor.

### 4. Marker file `.trusty-search/config.yaml` — committed vs local split

The marker file lives at the source root inside `.trusty-search/` (colocated with `index.redb`).

**COMMITTED section** (safe for git history; collisions harmless):
```yaml
# provenance.yaml (committed)
schema_version: 1
directory_name: "my-project"
git_origin: "git@github.com:user/my-project.git"  # or HTTPS equivalent
purpose: "primary"  # or "feature-variant", "vendored", etc.
```

**LOCAL-ONLY / GITIGNORED section** (the uniqueness anchor; must NOT propagate):
```yaml
# local.yaml (gitignored)
guid: "f47ac10b-58cc-4372-a567-0e02b2c3d479"
trusty_search_id: "Users_mac_workspace_my-project"  # mirrors registry primary key
```

**Recommendation:** Split into two files (`provenance.yaml` committed, `local.yaml` gitignored) rather than one mixed file. This keeps `.gitignore` clean and makes the intent explicit. Require `.trusty-search/` (or at minimum `local.yaml`) to be gitignored; a `trusty-search doctor` check should warn if a guid was found in git history (a sign it was committed and needs cleanup).

**Rationale for the split:**
- **Provenance travels with the code** (directory rename, git-origin change due to fork/mirror). A reviewer or future maintainer can see the repo was originally from `https://github.com/…` and it's called "my-project."
- **Uniqueness anchor stays local** so clones auto-mint fresh GUIDs and no pollution across machines.
- **If both fields are in one file**, git-ignoring it is ham-fisted (loses provenance history). The split allows `provenance.yaml` to be committed for visibility while `local.yaml` is gitignored.

### 5. Boot-reconcile state machine over markers (extends #1672 reconcile.rs)

At daemon startup, reconcile the live filesystem against the registry:

| Case | Condition | Action | Notes |
|------|-----------|--------|-------|
| **Normal** | Index entry exists, marker exists, guid matches registry guid | No-op | Most common case. |
| **New marker** | Index entry exists, marker missing → mint guid, write marker | Register guid in marker file | Preserves backward-compat with colocated indexes created before guid introduction. |
| **Index missing, new checkout** | Marker missing, root_path exists, not indexed before | Mint guid, write marker, register in index | First discovery of a fresh clone. |
| **Moved source** | Guid matches live marker at new path, old entry in registry | Update root_path, re-derive slug, drop stale entry | Move-relink case. Guid acts as stable handle. |
| **Same-GUID collapse** | Two entries, same guid, both have valid markers | Keep the entry whose root exists AND has populated index.redb; drop stale entry. | DATA-SAFE: never delete index.redb itself, only the registry entry. Log warn. The source is one repo in two places (e.g., `~/project` and `/mnt/backup/project` are the same clone); consolidate. |
| **Same-root different-GUID** | Index entry at path P with guid G1; marker at P with guid G2 ≠ G1 | Keep the one matching the on-disk marker (G2), drop stale (G1) | Rare: user moved files around or marker was corrupted and regenerated. Trust the disk. |
| **Guid-conflict / two-live-roots same-guid** | Two entries, same guid, both have valid on-disk markers at DIFFERENT roots | Re-mint guid for the newer entry (by `last_indexed_unix`); log error | `cp -a` case: user cloned the entire `.trusty-search/` dir including the marker. Newer by mtime gets to keep the guid; older is re-minted and both continue. |

**Reconciliation must be idempotent.** If the daemon crashes mid-reconcile, the next boot must not replay stale transitions or corrupt the marker file. Marker writes are atomic (write to temp, rename); registry updates are in-memory until persisted. Idempotence is achieved by always checking the on-disk marker against the in-memory registry entry before acting.

### 6. Case-insensitive string IDs (issue #1680) — alias layer

Fold in the case-insensitive collision fix (#1680) as an alias layer on top of the guid + marker model:

- **Normalization:** Index lookups normalize the `IndexId` string to **lowercase** (e.g., `Apex` → `apex`).
- **Alias map:** Maintain a `lowercase_canonical: HashMap<String, String>` mapping lowercase ids to their canonical (original-case) registry keys.
- **Schema version bump:** `indexes.toml` schema_version increments; old daemons can still parse thanks to `serde(default)` for missing alias map.
- **Migration:** On first boot, scan existing entries and build the alias map. If two entries collide on lowercase (e.g., `apex` and `APEX`), apply the same-GUID/same-root collapse rule to consolidate them, then re-key to the lowercase canonical form.

**Example:**
```toml
[indexes.aliases]
"apex" = "Apex"  # user typed "Apex", daemon normalizes to "apex" for lookups
```

Lookups:
```rust
let normalized = index_id.to_lowercase();
let canonical = aliases.get(&normalized).unwrap_or(&normalized);
registry.get(canonical)
```

### 7. GIT WORKTREES = EPHEMERAL SUB-INDEX linked to PARENT (SUPERSEDES ADR-0008 §4)

**Detect a worktree** by running `git rev-parse --git-common-dir`. If it resolves to a path **outside** the source root's `.git/` directory, the root is a git worktree (worktrees place a `.git` FILE pointing to a `.git/worktrees/…` dir; the common-dir points to the parent repo's `.git/`).

**For a detected worktree:**
- Create an index entry tagged `is_worktree: true` with:
  - `parent_guid`: the GUID of the parent repo's marker file
  - `parent_git_origin`: git remote of the parent (enables linking even if parent moves)
  - `branch_name`: the worktree branch (for display/debugging)
- The worktree is **attributed to the parent source instance**, not standalone.
- It is its own **short-lived sub-index** (branch content stays searchable for agent debugging, enabling `trusty-search search` against the feature branch).
- It is **auto-pruned** when the worktree path no longer exists on daemon startup (or on explicit `trusty-search deregister --path <path>`).

**Add `is_worktree` flag to the allowlist** (`src/allowlist/mod.rs`):
```rust
is_worktree: bool,  // true = auto-pruned on boot if path missing
```

On daemon startup, any allowlisted entry with `is_worktree: true` and a nonexistent path is silently dropped from the registry. This prevents the "74 spurious worktree indexes" problem (#1671) that motivated the allowlist — the safeguard now has explicit semantics rather than relying on human cleanup.

**Current protections (unchanged):**
- SKIP_DIRS includes `.claude` and `.claude-mpm`, so the parent index is NOT contaminated by nested worktree content.
- Auto-discovery is one-level-deep, so nested `.claude/worktrees/*` are never auto-indexed.

**Cite historical evidence:** The "74 spurious worktree indexes" that motivated #1671 are a symptom of missing identity semantics. ADR-0012 formalizes the link (parent_guid, parent_git_origin) so the daemon knows which entries are safe to prune.

**Open sub-question:** How to handle a **real sibling-path feature worktree** (not under `.claude/`) that legitimately wants its own searchable index while still being linked to the parent? For now, require it to be nested under the parent (so auto-discovery skips it) or explicitly allowlisted. A future design might introduce a "child index" permission model, but that is deferred.

## Consequences

### Positive

- **Reliable move-relink via GUID.** Moving a source to a new path is detected and the registry entry is updated automatically. The user does not need to manually deregister and re-register.
- **Same-GUID collapse deduplication.** The daemon automatically detects and consolidates duplicate entries (live apex/APEX case). Data is never lost; stale registry entries are pruned and the source is attributed to the entry with the valid index.
- **Case-insensitive aliases (#1680).** The lowercase alias layer allows case-insensitive lookups and automatic case folding, unblocking #1680 without requiring the full case-folding rearchitecture.
- **Provenance travels with the code.** The committed `provenance.yaml` (git_origin, directory_name, purpose) allows future maintainers to understand where the index came from, even after a move or clone.
- **Worktrees are legible and self-cleaning.** Worktree entries are explicitly tagged with `is_worktree: true` and parent links. The daemon auto-prunes them on boot if the path no longer exists, eliminating manual cleanup burden and the spurious-index problem.
- **Near-zero migration for colocated indexes.** Existing colocated indexes do not move; only the registry entry is re-keyed with a guid backfill on first boot. Non-colocated indexes are left untouched (they can be converted via an explicit `trusty-search convert` command if desired).
- **Memory model preserved.** trusty-memory's per-project palace identity remains unchanged; session-scoped namespaces are an opt-in addition.

### Trade-offs / Negatives

- **.gitignore discipline required.** The `local.yaml` (or `.trusty-search/local/` directory) must be gitignored. If a user commits a GUID, it pollutes clones and causes same-GUID-collapse false positives. Mitigation: `trusty-search doctor` warns if a guid was detected in git history; documentation must emphasize the split.
- **GUID loss on first-write crash.** If the daemon crashes between minting a GUID and persisting the marker file, the next boot falls back to the existing heuristic. This is rare and data-safe (the index.redb is intact), but it means a single-move-relink optimization is deferred to the next move-and-index cycle.
- **Legacy non-colocated indexes require data move to re-key.** A non-colocated index (e.g., `~/.trusty-search/apex.redb`) lives outside the source root. To change its registry key (e.g., from basename `apex` to full-path slug), the data must be moved. Decision: leave non-colocated indexes untouched unless the user explicitly runs `trusty-search convert`. This avoids surprise migrations.
- **Session-scoped palaces not auto-pruned yet.** The `<project-slug>/<session-id>` namespace is created on demand but is not automatically pruned when the session ends. Cleanup requires explicit API call or `trusty-mpm` / `tctl` integration (tie-in to session-decommission workflows). This is a follow-up, not a blocker for ADR-0012.
- **Worktree sub-indexes add reconcile bookkeeping.** The state machine gains four more cases (moved-source, same-GUID-collapse, same-root-different-GUID, guid-conflict). Reconcile logic is more complex, but the complexity is localized to `reconcile.rs::reconcile_instances` and is testable.

## Migration

**At first boot after the guid+marker code lands:**

1. **Backfill GUIDs for existing colocated indexes** (idempotent):
   - For each registry entry with a valid `root_path`:
     - Check if `.trusty-search/config.yaml` (or `local.yaml`) exists.
     - If it does and has a `guid`, use it (already initialized).
     - If not, mint a fresh UUID v4, write the marker file (with committed provenance and local guid/trusty_search_id), and update the in-memory registry entry.
   - Result: every index has a GUID by the end of the boot.

2. **Collapse the live apex/APEX case** (via same-GUID/same-root collapse rule):
   - Scan for lowercase collisions in the registry.
   - If two entries collide, apply the collapse rule: keep the one with populated index.redb, drop the stale entry.
   - Re-key to lowercase canonical form.

3. **Leave non-colocated indexes untouched:**
   - If a legacy entry points to a non-colocated index (`~/.trusty-search/apex.redb`), do NOT move the data. The entry remains keyed by its old id (e.g., basename `apex`). Users can opt-in to migration via `trusty-search convert <old-id> <new-slug>` (a follow-up command).
   - This avoids surprise data moves and keeps the first boot fast.

4. **Bump `indexes.toml` schema_version** (with `serde(default)`):
   - Existing indexes without a guid can be parsed by old daemons (they simply ignore the guid field).
   - New daemons backfill guids on boot.
   - Old daemons can still read the updated `indexes.toml`; they will just re-create guids on the next boot, which is harmless.

## Open Questions

1. **Single mixed marker file vs split files?**
   - **Mixed** (`config.yaml` with committed+local sections): simpler disk layout, .gitignore more complex.
   - **Split** (`provenance.yaml` committed, `local.yaml` gitignored): cleaner .gitignore, two files to manage.
   - **Recommendation:** Split. It makes intent explicit and allows provenance visibility in git history without leaking uniqueness.

2. **git_origin normalization for equality comparisons** (e.g., SSH ↔ HTTPS, `.git` suffix):
   - `git@github.com:user/project.git` vs `https://github.com/user/project.git` represent the same remote.
   - Should they be normalized to a canonical form for same-GUID detection and parent linking?
   - **Recommendation:** Normalize via `git remote get-url` (which resolves aliases) and strip trailing `.git`. Store the normalized form in the marker and use it for comparisons. This allows the user to use either SSH or HTTPS; the marker records the resolved canonical form.

3. **Session-palace pruning strategy** (tie-in to `trusty-mpm` / `tctl`):
   - Who is responsible for cleaning up `<project-slug>/<session-id>` palaces after a session ends?
   - Options:
     - **Auto-prune on session-decommission:** `trusty-mpm` calls `trusty-memory palace delete <project-slug>/<session-id>` when a session ends.
     - **TTL-based prune:** Mark palaces with an expiry; daemon prunes stale ones on boot.
     - **Explicit cleanup:** User / script must call the delete command.
   - **Recommendation:** Auto-prune on session-decommission (Option 1). Tie the `session_id` param to `trusty-mpm`'s session lifecycle. This is a follow-up integration with `tctl` session-management.

4. **GUID-conflict tie-break when `last_indexed_unix` is absent:**
   - The guid-conflict case (two live roots, same guid due to `cp -a`) uses `last_indexed_unix` to decide which entry is newer.
   - What if both entries have no `last_indexed_unix` (very early indexes or corrupted metadata)?
   - **Recommendation:** Fall back to filesystem mtime of the index.redb file itself, or the marker file. If neither exist, re-mint for both and log a warning.

5. **macOS case-insensitive-volume slug folding** (deferred from ADR-0008):
   - On APFS, `/Proj` and `/proj` are the same inode but may produce different slugs depending on how the path is normalized.
   - ADR-0008 deferred this. Does ADR-0012 solve it as a side-effect of the alias layer?
   - **Recommendation:** The alias layer solves the registry-keying problem (case-insensitive lookups) but does NOT solve the slug-generation problem (a path may still canonicalize differently on APFS depending on the input case). Recommend deferring the full solution to a future ADR scoped to case-insensitive volumes only, once more data about real-world APFS layouts is gathered.

6. **Sibling-path feature-worktree handling** (from decision §7):
   - A developer might create a worktree at `~/projects/my-project-feature` (sibling to the parent at `~/projects/my-project`) rather than nesting it under `.claude/worktrees/`.
   - Should this automatically register as a worktree sub-index, or does it need explicit allowlisting?
   - **Recommendation:** Require explicit allowlisting or nesting under the parent for now. A future design might introduce a "child index" or "linked worktree" permission model, but that is deferred pending real-world usage patterns.

## References

- **Issue #1680:** Case-insensitive string ID collisions (e.g., apex/APEX).
- **Epic #1681:** Canonical index/source identity model (umbrella for ADR-0012 and related work).
- **Issue #1672:** mtime-based boot-reconcile for non-git indexes (merged; reconcile.rs exists).
- **Issue #1671:** Spurious worktree indexes (74 in production; allowlist implemented).
- **Issue #1670:** Git-based boot-reconcile (merged; reconcile.rs includes git path).
- **Issue #860:** Historical move-relink request.
- **ADR-0008:** Project-identity convention (the full-path slug is locked in; ADR-0012 layers GUID on top).
- **ADR-0006:** trusty-controller lifecycle (references cross-tool identity and palace sync).
- **docs/trusty-controller/research/02-design/DOC-6:** Cross-tool identity agreement (palace sync, index naming).
