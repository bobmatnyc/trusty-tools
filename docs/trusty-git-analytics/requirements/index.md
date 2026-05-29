# Requirements Documentation

Navigation index for trusty-git-analytics requirement specifications.

## Documents

- **[overview.md](./overview.md)** — System overview, pipeline architecture, and Rust port goals
- **[configuration.md](./configuration.md)** — Full YAML configuration schema reference
- **[database-schema.md](./database-schema.md)** — SQLite schema for all tables and migrations
- **[cli-commands.md](./cli-commands.md)** — All CLI subcommands and flag reference
- **[classification.md](./classification.md)** — Four-tier classification cascade specification
- **[collection.md](./collection.md)** — Git extraction and API fetching stage
- **[reporting.md](./reporting.md)** — Report generation formats and metrics
- **[rust-architecture.md](./rust-architecture.md)** — Rust-specific design decisions

## Source Material

The Python predecessor lives at `/Users/masa/Projects/gitflow-analytics`. Refer to its
source under `src/gitflow_analytics/` for canonical behavior when porting features.

## Version Notes

- **v1.0.6**: Added `llm_fallback_threshold`, `llm_fallback_concurrency` config fields; wired `ticket_regex` for JIRA/GitHub/Linear/ADO adapters; ADO `fetch_prs` flag; `pr_reviewers` table (migration `0011`).
