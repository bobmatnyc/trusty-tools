# 0001. Design, research, and ADR docs live in the top-level `docs/` tree

- **Status:** Accepted
- **Date:** 2026-05-29
- **Scope:** Workspace-wide
- **Supersedes / Superseded by:** —

## Context

trusty-tools consolidates many crates under one Cargo workspace. Documentation
had drifted: some crates carried large in-crate `docs/` trees (notably
`crates/open-mpm/docs/` with research, design, developer, user, architecture,
performance, and archive material), while the workspace `CLAUDE.md` documents a
canonical top-level layout — *"Documentation is organized by published crate,
not by topic. Each crate gets a directory under `docs/`."*

In-crate documentation trees create problems:

- They get packaged into the published `.crate` tarball unless explicitly
  excluded, bloating downloads with research and binary assets (PDFs, PNGs).
- They split a crate's docs across two locations (in-crate `docs/` *and*
  top-level `docs/<crate>/`), so readers and the doc-build tooling can't rely on
  a single source of truth.
- They diverge from the convention every other crate already follows.

Rustdoc and the crate `README.md`, by contrast, *should* stay in-crate: rustdoc
is generated from source and the README is what crates.io renders.

## Decision

We will keep all design, research, specification, user, developer, and ADR
documentation in the **top-level `docs/` tree**, organized per crate
(`docs/<crate>/...`) with the standard subdirectories. **Rustdoc comments and
each crate's `README.md` remain in-crate.** No crate carries its own `docs/`
directory. Crate READMEs link out to the top-level tree.

This ADR's implementing change migrates `crates/open-mpm/docs/` into
`docs/open-mpm/` and removes the in-crate directory.

## Consequences

- **Positive:** one canonical location per crate's docs; published tarballs stay
  lean; doc tooling (the workspace mdBook, see ADR/SUMMARY) can index a single
  tree; the convention is now uniform across the workspace.
- **Positive:** ADRs gain a clear home — workspace-wide in `docs/adr/`,
  crate-specific in `docs/<crate>/decisions/` (the hybrid rule).
- **Negative / follow-up:** crate READMEs and any source comments that pointed at
  in-crate `docs/` paths must be updated to the new locations (done for open-mpm
  as part of this change).
- **Neutral:** runtime behavior is unaffected — open-mpm still writes per-run
  telemetry to the *consuming project's* `<cwd>/docs/performance/`, which is
  unrelated to this crate's own documentation.
