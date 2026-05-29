# trusty-git-analytics — documentation

Developer-productivity analytics CLI / DB. Crate name: `tga`
(`crates/trusty-git-analytics/`). Walks local git repositories, classifies
every commit into a work category via a multi-tier cascade, and emits
per-author / per-week / DORA / velocity / quality reports.

This directory is the **single source of truth** for trusty-git-analytics
design, requirements, research, and user/developer documentation. The crate
`README.md` and rustdoc stay in-crate; everything else lives here
(see [ADR-0001](../adr/0001-docs-live-top-level.md)).

## Documentation map

| Subdir | What's here |
|--------|-------------|
| [`requirements/`](requirements/) | Canonical specification set ported from the Python predecessor: overview, configuration schema, database schema, CLI commands, classification cascade, collection, reporting, and Rust architecture. Start at [`requirements/index.md`](requirements/index.md). |
| [`developer/`](developer/) | Contributor docs: [architecture](developer/architecture.md), [developer guide](developer/developer-guide.md), [configuration reference](developer/configuration-reference.md), [migration from Python](developer/migration-from-python.md), [publishing](developer/publishing.md). |
| [`user/`](user/) | End-user docs: [user guide](user/user-guide.md). |
| [`decisions/`](decisions/) | **Crate-specific ADRs** (Nygard format): SQLite tuning, performance hotspots, Bitbucket PR provider. Workspace-wide ADRs live in [`docs/adr/`](../adr/). |
| [`regression-testing/`](regression-testing/) | Versioned performance/quality snapshots (`v{VERSION}-{DATE}.md`), the Rust-vs-Python [`comparison.md`](regression-testing/comparison.md), and the methodology in [`regression-testing/README.md`](regression-testing/README.md). |
| [`research/`](research/) | Investigation and design documents: [commit-effort scoping spec](research/commit-effort-spec-2026-05-27.md), [per-engineer drilldown](research/per-engineer-drilldown-2026-05-28.md). |
| [`sessions/`](sessions/) | Engineering-session narratives (none yet). |

## Where to start

- **Understanding the system?** [`requirements/overview.md`](requirements/overview.md) → [`requirements/index.md`](requirements/index.md).
- **Using the CLI?** [`user/user-guide.md`](user/user-guide.md).
- **Contributing?** [`developer/developer-guide.md`](developer/developer-guide.md) and [`developer/architecture.md`](developer/architecture.md).
- **Understanding a past decision?** [`decisions/`](decisions/) (crate-specific) or [`docs/adr/`](../adr/) (workspace-wide).

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md). The `requirements/` set mirrors the API contract
of the [gitflow-analytics](https://github.com/bobmatnyc/gitflow-analytics)
Python predecessor; `research/` files are dated point-in-time investigations.
