# System Overview

## Purpose

**trusty-git-analytics** (`tga`) is a developer productivity analytics tool that analyzes Git
repositories to produce engineering metrics, classify work types, and surface team-level
insights. It is a Rust port of the Python `gitflow-analytics` tool with full API compatibility
(same YAML config, same SQLite schema, same CLI flags).

## Pipeline

The system implements a three-stage pipeline with SQLite as the shared persistence layer:

```
┌──────────────────┐   ┌───────────────────┐   ┌──────────────────┐
│   Stage 1:       │   │   Stage 2:        │   │   Stage 3:       │
│   tga collect    │──▶│   tga classify    │──▶│   tga report     │
│                  │   │                   │   │                  │
│ - git extraction │   │ - rule-based      │   │ - CSV reports    │
│ - GitHub API     │   │ - LLM-based       │   │ - JSON exports   │
│ - JIRA API       │   │ - JIRA mapping    │   │ - Markdown narr. │
│ - identity res.  │   │ - issue type lkp  │   │ - DORA metrics   │
│ - PR review data │   │ - taxonomy remap  │   │ - velocity       │
└──────────────────┘   └───────────────────┘   └──────────────────┘
        │                       │                      │
        ▼                       ▼                      ▼
  ┌─────────────────────────────────────────────────────────┐
  │              SQLite (gitflow_cache.db + identities.db)  │
  │              WAL mode for concurrent read/write          │
  └─────────────────────────────────────────────────────────┘
```

### Stage 1: Collection (`tga collect`)
- Extracts commits from each configured Git repository using **libgit2** (via `git2` crate)
- Fetches associated GitHub PRs / reviews and JIRA tickets via async HTTP (`reqwest` + `tokio`)
- Resolves developer identities (manual mappings, exact email, GitHub username, fuzzy match)
- Writes commit + PR + issue rows into `cached_commits`, `pull_request_cache`, `issue_cache`

### Stage 2: Classification (`tga classify`)
- Reads commits from cache
- Applies four-tier classification cascade (manual override → issue type → JIRA mapping → LLM → rule-based fallback)
- Writes qualitative results into `qualitative_commits` and `daily_commit_batches`
- Optional taxonomy remap from `change_type` to custom `work_type`

### Stage 3: Reporting (`tga report`)
- Aggregates cache contents into CSV / JSON / Markdown outputs
- Computes DORA metrics, velocity, activity scoring, quality reports
- Optional anonymization of developer identities

`tga analyze` runs all three stages in sequence.

## Goals vs Python Predecessor

| Goal | Python | Rust |
|------|--------|------|
| Repository extraction speed | Sequential subprocess per commit | Parallel libgit2 via rayon |
| HTTP concurrency | Sequential requests (sync) | Concurrent async via tokio |
| Classification throughput | Per-commit dispatch | Batched parallel via rayon |
| Pattern matching | Per-pattern regex | aho-corasick multi-pattern FSM |
| Memory safety | Runtime errors possible | Compile-time guarantees |
| Binary distribution | Python + venv | Single static binary |

## Performance Targets

- Repository extraction: **5–10x** faster than Python predecessor
- Classification: **3–5x** faster
- API fetching: **2–4x** faster
- Overall pipeline: **4–8x** faster on multi-repo workloads

## Related Documents

- [Configuration schema](./configuration.md)
- [Database schema](./database-schema.md)
- [CLI commands](./cli-commands.md)
- [Classification cascade](./classification.md)
- [Collection stage](./collection.md)
- [Reporting stage](./reporting.md)
- [Rust architecture decisions](./rust-architecture.md)
