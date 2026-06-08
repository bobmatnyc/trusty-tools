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
**disagree**:

- `crates/trusty-search/src/detect.rs` derives the id from the **basename** of
  the detected root (`my-project`). It is short and human-friendly but
  **collides** when two repos share a basename (e.g. `~/work/api` and
  `~/personal/api` both resolve to `api`).
- `crates/trusty-search/src/service/fs_discovery.rs::id_from_path` derives the id
  from a **full-path slug** (`Users_mac_workspace_my-project`). It is
  collision-free and stable across restarts (proven by its `stable-and-safe`
  test) but is not human-friendly.

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

1. **Canonical id = the full-path slug of the nearest enclosing git root.** Walk
   up from the cwd to the first ancestor containing `.git`; the canonical project
   id is the path-slug of that root (the `id_from_path` scheme,
   e.g. `Users_mac_workspace_my-project`). The full-path slug **wins** over the
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

## Consequences

**Easier / positive:**

- **Collision-free and stable.** The full-path slug cannot collide across repos
  sharing a basename and is stable across daemon restarts (already test-proven by
  `id_from_path`).
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
  needs to converge on the slug. This is a one-time reconciliation tracked as
  DOC-6/DOC-8 follow-up.
- **Canonical helpers hoisted into `trusty_common`.** The canonical
  project-identity helpers (`id_from_path`, `detect_project`) will be hoisted into
  `trusty_common` as the single shared implementation (decided in DOC-6 Q9), so all
  tools consume one slug implementation rather than each carrying its own.
- **Slug ergonomics.** The canonical id is not human-friendly; all human-facing
  surfaces must deliberately use the display alias rather than the slug.
- **Marker discipline for sub-projects.** Teams wanting per-subdir identities in
  a monorepo must opt in with an explicit marker; this is intentional but is one
  more thing to document for those users.
