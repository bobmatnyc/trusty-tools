# 0008. Project-identity convention: full-path slug of the nearest git root

- **Status:** Accepted
- **Date:** 2026-06-08
- **Scope:** Workspace-wide (the `trusty-controller`/`tctl` control plane and
  every project-scoped tool it manages — trusty-search, trusty-memory, and any
  future per-project state holder; consumed by the trusty-controller design set
  under `docs/trusty-controller/research/02-design/`, esp. DOC-3 §8, DOC-6
  cross-tool agreement, and DOC-8 auto-config)
- **Supersedes / Superseded by:** —

## Context

`trusty-controller` (`tctl`, ADR-0006) and every project-scoped tool it manages
need **one** stable, collision-free project identity that binds every
project-scoped operation to the right working directory. A single id keys all
per-project state across tools: it is used as both the trusty-search `IndexId`
and the trusty-memory palace id, so the controller can ensure, reference, and
report a project's state consistently across the stack.

Today **two** id-derivation schemes coexist in the live codebase and they
**disagree** (a third, basename-based scheme also exists in
`crates/trusty-agents/src/ctrl/socket.rs::project_id_from_path` — outside the
controller's direct scope, but worth acknowledging the count):

- `crates/trusty-search/src/detect.rs` derives the id from the **basename** of
  the detected root (`my-project`). It is short and human-friendly but
  **collides** when two repos share a basename (e.g. `~/work/api` and
  `~/personal/api` both resolve to `api`).
- `crates/trusty-search/src/service/fs_discovery.rs::id_from_path` derives the id
  from a **full-path slug** (`Users_mac_workspace_my-project`). It is
  collision-free and stable across restarts, but it is a **pure string slug** —
  its doc comment states a **precondition** that the caller pass a canonical
  (symlink-resolved) path; it does **not** call `canonicalize()` itself. So
  symlink-safety today is a caller convention, not part of the
  id-derivation contract. It is also not human-friendly.

Both forms are **live in the daemon registry** at the same time: the registry has
been observed holding *both* `trusty-tools` and `Users_mac_workspace_trusty-tools`
registered for the same root. The ambiguity is real, not hypothetical, and the
controller — which must hold zero tool-specific logic — cannot tolerate two
disagreeing identities for one directory.

The detection walk itself already exists and is sound (`detect_project()`: walk
up from cwd → first `.git` root → else first tool marker → else fallback to cwd
with a `DetectionMethod::Fallback` warning). What is missing is a single
canonical **id-derivation rule** layered on top of that walk, plus explicit
handling of worktrees and monorepo subdirectories. The choice is **costly to
reverse**: it is baked into the daemon registry keys, every tool's per-project
state directory, the controller's ensure/report logic, and DOC-8's auto-config
keys — changing it after tools ship would require a registry/state migration
across the whole stack. That clears the repo's ADR bar
(`docs/adr/README.md`).

## Decision

We will adopt a single canonical project-identity rule:

1. **Canonical id = the full-path slug of the *canonicalized* nearest enclosing
   git root.** Walk up from the cwd to the first ancestor containing `.git`, then
   **`canonicalize()` that root path first** (resolving symlinks, `.`/`..`) and
   only then apply the path-slug. The contract is **canonicalize-then-slug**, not
   a bare slug: identity is a single `trusty_common::canonical_project_id(path)
   -> Result<String>` function that canonicalizes internally before slugging, so
   the slug step **never receives a raw path** and callers cannot forget the
   canonicalization step. The result is still a full-path slug
   (e.g. `Users_mac_workspace_my-project`). The full-path slug **wins** over the
   basename scheme; the divergent `detect.rs` basename usage is the loser and
   must be reconciled to this rule.

2. **The git-root basename is a display-only alias.** The short, human-friendly
   basename (`my-project`) is retained purely as a display name for UX; it is
   **never** used as the keying identity and may collide freely without
   consequence.

3. **No git root and no marker → cwd path-slug + `Fallback` warning.** When
   neither a `.git` root nor a tool marker is found up the tree, derive the id
   from the **cwd path-slug** and emit a `Fallback` warning. The controller does
   **not** refuse to operate; it degrades to a usable (if less stable) identity
   and surfaces the warning.

4. **Worktrees get their own id, keyed on their working-directory path.** A git
   worktree shares the underlying repository but lives at a distinct working
   directory; its id is the path-slug of *that* working directory, so a worktree
   is a distinct project with its own index/palace.

5. **Monorepo subdirectories share the root's id.** A monorepo has one `.git` at
   the top with many logical sub-projects beneath it; by rule (1) every subdir
   resolves to the **same** enclosing git root and therefore **shares one** id,
   index, and palace. Per-subdir sub-project identities are created **only** via
   an explicit per-subdir marker (e.g. trusty-search's existing
   `trusty-search.yaml` multi-index file), never implicitly.

6. **`canonicalize()` failure → lexical-absolutize slug + `Fallback` warning.**
   `canonicalize()` is `realpath(3)`: it requires the path to **exist** and can
   fail on a broken symlink component or a permission error. When it fails,
   `canonical_project_id` falls back to slugging a **lexically-absolutized** path
   (absolutize against the cwd and normalize `.`/`..` **without** symlink
   resolution) and emits a `Fallback`/degraded warning. This mirrors the
   "no git root and no marker → cwd path-slug + `Fallback` warning" rule in §3:
   the controller **never refuses** to operate; it degrades to a usable (if less
   stable) identity and surfaces the warning.

## Consequences

**Easier / positive:**

- **Collision-free and stable.** The full-path slug cannot collide across repos
  sharing a basename and is stable across daemon restarts. Note the scope of the
  existing proof precisely: `id_from_path`'s `stable-and-safe` unit test proves
  **determinism + character-safety only** — it does **not** exercise
  symlink/canonicalization equivalence. Symlink-safety follows from the
  canonicalize-then-slug contract (Decision §1), and proving it requires a
  **new symlink-equivalence test** (a follow-up); the existing test must not be
  cited as evidence of symlink-safety.
- **One identity across tools.** A single id keys trusty-search indexes and
  trusty-memory palaces alike, giving DOC-6 the cross-tool agreement it requires
  and DOC-8 a stable auto-config key. The controller can ensure/report a
  project's state with zero tool-specific identity logic.
- **Predictable monorepo/worktree behaviour.** Subdirs of a monorepo
  deterministically share one id (matching today's `detect_project` walk, which
  stops at the first `.git`), while worktrees are cleanly distinct.
- **Display-name UX preserved.** Keeping the basename as an alias means users
  still see `my-project`, not `Users_mac_workspace_my-project`, in human-facing
  output.
- **Never refuses on a bare directory.** The fallback rule keeps the controller
  usable outside a repo, trading a warning for availability.

**Harder / trade-offs / follow-up:**

- **Migration required.** The basename users in
  `crates/trusty-search/src/detect.rs` must be migrated/reconciled to the
  path-slug scheme; the daemon registry currently holding both forms for one root
  needs to converge on the slug. This is a one-time **stateful re-key migration**,
  designed in DOC-6 §7.1: re-key each registry entry by recomputing the canonical
  slug from its `root_path` and **reuse the colocated index data in place — NOT a
  from-scratch reindex** (the data is `root_path`-addressed, so the id is only the
  in-memory registry key); trusty-memory palaces get an alias/rename. It is carried
  by trusty-search's existing forward-only `_meta` schema-migration framework
  (`core::migration`, with a `schema_version` bump — idempotent and crash-safe), so
  the first ensure after the flip sees the project as `exists`/`fresh`, not a
  rebuilt `pending`.
- **Canonical helpers hoisted into `trusty_common`.** The canonical
  project-identity helpers (`detect_project` plus the `canonical_project_id`
  contract function over the `id_from_path` slug) will be hoisted into
  `trusty_common` as the single shared implementation (decided in DOC-6 Q9), so all
  tools consume one slug implementation rather than each carrying its own.
  Crucially, the hoisted `canonical_project_id` **canonicalizes internally** (per
  Decision §1) rather than hoisting the bare slug — so no call site can forget the
  `canonicalize()` step. This avoids the footgun of exposing `id_from_path`'s
  caller-side canonicalization precondition as a per-call-site responsibility.
- **Slug ergonomics.** The canonical id is not human-friendly; all human-facing
  surfaces must deliberately use the display alias rather than the slug.
- **Marker discipline for sub-projects.** Teams wanting per-subdir identities in
  a monorepo must opt in with an explicit marker; this is intentional but is one
  more thing to document for those users.
- **Case-insensitive volumes — residual limitation.** On case-insensitive volumes
  (APFS), `canonicalize()` resolves symlinks and `.`/`..` but does **not**
  guarantee case-folding of the final path components, so two differently-cased
  spellings of the same directory (`/Proj` vs `/proj`) **may** still produce
  divergent ids. This is a named known limitation, not a solved case. Case-folding
  is a **deferred follow-up** scoped to case-insensitive volumes only: a blanket
  lowercasing would change every id and risks wrongly merging genuinely
  case-distinct directories on case-sensitive volumes, so it is intentionally
  **not** done in v1.
