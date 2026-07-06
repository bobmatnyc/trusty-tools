# trusty-code — documentation

Claude-Code-compatible, per-project execution harness with MPM orchestration
brain. Crate lives in `crates/trusty-code/` (binary: `tcode`; formerly
`trusty-harness` / `tharn`). This is an original best-of-breed coding harness
per epic [#2052](https://github.com/bobmatnyc/trusty-tools/issues/2052).

This directory is the single source of truth for trusty-code architecture
decisions, compatibility research, regression testing, and engineering-session
documentation. The crate `README.md` and rustdoc stay in-crate.

## Documentation map

| Subdir | What's here |
|--------|-------------|
| [`research/`](research/) | Investigation, audit, and decision documents — the Claude-Code compatibility spec, architecture decisions, compatibility sub-issue analysis. Indexed in [`research/README.md`](research/README.md). |
| [`regression-testing/`](regression-testing/) | Versioned performance / integration-test snapshots tied to tcode releases (`v{VERSION}-{YYYY-MM-DD}.md`). Methodology in [`regression-testing/README.md`](regression-testing/README.md). |
| [`sessions/`](sessions/) | Engineering-session narratives (`SESSION-{DATE}-{topic}.md`). Indexed in [`sessions/README.md`](sessions/README.md). |

## Where to start

- **Vision & Architecture (RE-VISION approved)?** [`vision-and-architecture-spec.md`](vision-and-architecture-spec.md) — foundational spec for the re-visioned trusty-code: daemon-first token-efficient coding harness (supersedes old benchmark scope). Includes axioms, feature audit, phased roadmap, and open questions for owner decision.
- **What is tcode and how does it relate to trusty-agents / Claude Code?**
  [`research/claude-compat-spec-2026-06-02.md`](research/claude-compat-spec-2026-06-02.md)
  — the authoritative compatibility specification, including the full
  configuration-surface inventory, precedence rules, divergence points, and
  compatibility sub-issue breakdown (C1–C9).
- **Architecture decisions?** [`research/README.md`](research/README.md) for the
  full index.
- **Performance / integration-test snapshots?**
  [`regression-testing/README.md`](regression-testing/README.md).

## Conventions

Subdirs follow the workspace documentation conventions described in the root
[`CLAUDE.md`](../../CLAUDE.md) and mirrored from the
[`docs/trusty-search/`](../trusty-search/) worked example.

- **`research/`** — point-in-time investigations and decision documents;
  preserved as-is; dated in filename.
- **`regression-testing/`** — numeric snapshots per measured release.
- **`sessions/`** — narrative session summaries.
