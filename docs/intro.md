# trusty-tools documentation

Welcome to the documentation for **trusty-tools**, a unified Rust workspace that
consolidates the entire trusty-* AI tooling ecosystem — shared libraries,
daemon/MCP servers, the MPM orchestration platform, and supporting tools, all
co-located under one Cargo workspace.

This book is built from the `docs/` tree. Product documentation may use a
`docs/<product>/` index, while lightweight packages can rely on their in-crate
README and rustdoc. Cross-cutting behavior contracts live in
[`docs/specs/`](specs/README.md), and architectural decisions live in
[`docs/adr/`](adr/README.md).

## How this book is organized

- **Architecture Decisions** — workspace-wide ADRs (Nygard format). The bar for
  writing one is *architecturally significant **and** costly to reverse*.
- **Product sections** — extended user, developer, research, and measurement
  material where a crate README alone would be too dense.
- **Package map** — the [workspace package and code map](reference/crate-map.md)
  links every Cargo package to its source, targets, and documentation.
- **Historical evidence** — dated research, plans, sessions, audits,
  regression snapshots, and changelogs retain the state they recorded.

## Conventions

- Workspace-wide decisions live in [`docs/adr/`](adr/README.md); crate-specific
  decisions live in `docs/<crate>/decisions/`.
- Each top-level package's `README.md` and rustdoc stay **in-crate**. Extended
  product documentation lives here when needed. See
  [ADR-0001](adr/0001-docs-live-top-level.md) and the
  [documentation layout](reference/documentation-layout.md).

For build commands, conventions, and the full crate inventory, see the
workspace `CLAUDE.md` at the repository root and the project
[README on GitHub](https://github.com/bobmatnyc/trusty-tools).
