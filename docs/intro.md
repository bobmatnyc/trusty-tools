# trusty-tools documentation

Welcome to the documentation for **trusty-tools**, a unified Rust workspace that
consolidates the entire trusty-* AI tooling ecosystem — shared libraries,
daemon/MCP servers, the MPM orchestration platform, and supporting tools, all
co-located under one Cargo workspace.

This book is built from the `docs/` tree. Documentation is organized **by crate**
(`docs/<crate>/...`), with cross-cutting architecture decisions captured as
**ADRs** in [`docs/adr/`](adr/README.md).

## How this book is organized

- **Architecture Decisions** — workspace-wide ADRs (Nygard format). The bar for
  writing one is *architecturally significant **and** costly to reverse*.
- **Per-crate sections** — each crate's specification, design, research, user,
  and developer docs, plus its crate-specific decision records. Full spec
  coverage (PRD + Architecture + Components + Decisions) is complete for all
  major crates: **open-mpm**, **trusty-mpm**, **trusty-search**,
  **trusty-memory**, **trusty-analyze**, **trusty-common**, and
  **trusty-git-analytics**. Lighter-tier library and sidecar crates
  (**trusty-embedderd**, **trusty-bm25-daemon**, **trusty-gworkspace**,
  **trusty-cto-db**, **tc-services**, **open-mpm-agent-api**, **open-mpm-local**)
  each carry an Overview and a `SPEC.md`.

## Conventions

- Workspace-wide decisions live in [`docs/adr/`](adr/README.md); crate-specific
  decisions live in `docs/<crate>/decisions/`.
- Each crate's `README.md` and rustdoc stay **in-crate**; everything else lives
  here in `docs/`. (See [ADR-0001](adr/0001-docs-live-top-level.md).)

For build commands, conventions, and the full crate inventory, see the
workspace `CLAUDE.md` at the repository root and the project
[README on GitHub](https://github.com/bobmatnyc/trusty-tools).
