# trusty-review — Specification Set

> **Status:** Current · Living Document
> **Last reviewed:** 2026-06-01
> **Derived from:** source audit of `crates/trusty-review/src/` (v0.1.0) + spec docs 01–10 + open issue backlog

This directory holds the canonical product and engineering specification for the
`trusty-review` crate (`crates/trusty-review/`). It is the single authoritative
reference for what trusty-review is, what is implemented today, and what gaps remain.

## What is trusty-review?

`trusty-review` is an AI-assisted GitHub pull-request review service in the
`trusty-tools` workspace. It orchestrates LLM-backed code review as a standalone
Rust crate, consuming **trusty-search** (`:7878`, semantic code/context retrieval)
and **trusty-analyze** (`:7879`, static analysis) as sibling daemons, and driving
an LLM through a pluggable provider abstraction with co-equal **AWS Bedrock** (default)
and **OpenRouter** backends. It runs in two modes from one binary: a one-shot CLI
(`run`, `compare`, `profile`) and a long-lived webhook server (`serve`, axum, port 7880).

Its review philosophy is **fail-safe**: the default verdict is `APPROVE` and the
bot bears the burden of proof — enforced by a deterministic severity-anchored grade
floor and (in the full pipeline) a per-finding LLM verification round. Implemented
features include: forced structured JSON output, severity-anchored grade derivation
(compile-break → BLOCK), the full longitudinal contributor-profile pipeline, and the
HTTP server with GitHub webhook dispatch.

## Documents in this set

| Document | Read it when you want to know… |
|----------|-------------------------------|
| **[PRD.md](./PRD.md)** | The product: goals/non-goals, the full feature catalog tagged by implementation status (✅/🟡/🔵/⚪), verdict taxonomy, open issues, acceptance checklist, and glossary. Start here for *why* and *what*. |
| **[ARCHITECTURE.md](./ARCHITECTURE.md)** | The system shape: crate layout, module map (all `src/` paths), dependency topology (required vs optional deps, HTTP-only transport rationale), two run modes (CLI + daemon), severity-anchored grade derivation, fail-safe posture, deployment model (systemd, launchd, dry-run rollout), observability, and the 13-lesson design rationale table. Start here for *how it fits together*. |
| **[COMPONENTS.md](./COMPONENTS.md)** | Per-subsystem specs: review pipeline stages, grade derivation algorithm, structured JSON output, LLM provider trait (Bedrock + OpenRouter — all model IDs, pricing, retry/timeout), data models (Verdict/Finding/ReviewResult — all fields and confidence thresholds), integration clients (GitHub auth, trusty-search/analyze, JIRA, Slack), configuration (all env vars + TOML tables + per-repo YAML schema), HTTP API (all routes + webhook contract), CLI subcommands, contributor profile pipeline, diff summarizer (designed), persistence (designed). Start here for *the detail of one subsystem*. |

## Reading order

1. **New to trusty-review?** PRD → ARCHITECTURE → COMPONENTS.
2. **Implementing a feature?** Jump to the relevant COMPONENTS section, then verify the pipeline stage sequence and fail-safe posture in ARCHITECTURE.
3. **Evaluating product direction?** PRD feature catalog + open issues, then the gap callouts in COMPONENTS.

## Status legend

Every requirement and component is framed as **Implemented / Partial / Designed / Aspirational** and tagged inline:

| Tag | Meaning |
|-----|---------|
| ✅ **Implemented** | Built and working in v0.1.0 |
| 🟡 **Partial** | Partly built; usable but incomplete or with known caveats |
| 🔵 **Designed-not-built** | Spec exists (types, scaffolding, or detailed requirement), tracked in an issue |
| ⚪ **Aspirational** | North-star intent; no committed design yet |

## Open issues (unfinished tail)

The critical path:
- [#552](https://github.com/bobmatnyc/trusty-tools/issues/552) — full 16-stage pipeline + verification round
- [#549](https://github.com/bobmatnyc/trusty-tools/issues/549) — data models + persistence (redb dedup claim)
- [#554](https://github.com/bobmatnyc/trusty-tools/issues/554) — deployment, observability, alarms, systemd
- [#550](https://github.com/bobmatnyc/trusty-tools/issues/550) — JIRA, Slack, Confluence, GitHub Projects integrations
- [#551](https://github.com/bobmatnyc/trusty-tools/issues/551) — diff summarizer Stage A/B/C
- [#569](https://github.com/bobmatnyc/trusty-tools/issues/569) — per-PR review personalization from contributor profile (post-#552)

## Related documentation

This `spec/` set is the *what/why/gap* layer. Point-in-time docs live alongside it:

- **[../research/](../research/)** — dated investigations and decision documents (source-analysis.md is the grounded analysis of the Python predecessor system that all spec requirements cite)
- **[crates/trusty-review/README.md](../../../crates/trusty-review/README.md)** — in-crate quick-start and usage

## Provenance and maintenance

These documents were synthesized from the 10-file numbered spec set (01-architecture.md
through 10-lessons-and-rationale.md, written as design intent in May 2026) and then
updated against the live source tree to reflect what was actually implemented by v0.1.0
(commits #570–#579, June 2026). The numbered files were consolidated into this 3-file
format to match the workspace documentation convention.

When the code changes materially, update the relevant document and bump the
*Last reviewed* date. Source-path citations reflect the layout at v0.1.0.
