---
name: tm-bug-reporting
description: Bug reporting protocol for the PM and agents — routes through the MCP-native list_recent_errors / preview_bug_report / report_bug pipeline
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [bug-reporting, github, issues, pm-required]
effort: medium
---

# tm Bug Reporting Protocol

## When to Report a Bug

Report when encountering: a PM instruction gap or error, agent malfunction,
skill content error or outdated information, a daemon crash or unexpected
behavior, missing/incorrect documentation, or a configuration error/invalid
default.

## Single Repository, No Routing Decision Tree

trusty-tools is a single monorepo — unlike claude-mpm's three-repo split
(`claude-mpm` / `claude-mpm-agents` / `claude-mpm-skills`), there is no
routing decision between repos. Every bug — core daemon, agent behavior,
skill content — files against `bobmatnyc/trusty-tools`, and the MCP tool
already knows this: `report_bug`'s description states it files into
`bobmatnyc/trusty-tools` directly.

## The Real Filing Pipeline (MCP-Native, Not `gh issue create`)

This is the entire mechanism — no manual `gh issue create` template
construction. **Before calling any of these**, load their schemas if not
already in your tool list — `ToolSearch(query:
"select:mcp__trusty-mpm__list_recent_errors,mcp__trusty-mpm__preview_bug_report,mcp__trusty-mpm__report_bug")`
— see `tm-tool-usage-guide`'s "Deferred MCP Tool Loading" section; absence
from your loaded list does not mean these tools are unavailable:

1. **`mcp__trusty-mpm__list_recent_errors(limit)`** — surfaces recently
   captured ERROR-level events across trusty-search, trusty-memory,
   trusty-analyze, and trusty-mpm, each with a dedup `fingerprint`.
2. **`mcp__trusty-mpm__preview_bug_report(fingerprint)`** — shows the exact
   scrubbed issue body (paths/tokens/secrets redacted) and proposed labels.
   Files nothing.
3. **`mcp__trusty-mpm__report_bug(fingerprint, confirm: true)`** — files the
   issue, or posts a "+1 occurrence" comment on an existing open issue with
   the same fingerprint instead of duplicating. Requires
   `TRUSTY_BUGREPORT_GITHUB_TOKEN` to be configured; on success returns
   `{ filed, deduped, issue_url, issue_number }`.

**`confirm` must be explicitly `true`.** The PM must always call
`preview_bug_report` first and get the user's explicit go-ahead before
calling `report_bug` with `confirm: true` — never file silently on the
user's behalf.

## When There Is No Error-Capture Fingerprint

Some bugs (a skill's documented command no longer exists, a stale path in an
instruction file, a UX gap the user reports directly) have no corresponding
daemon-captured error event — `list_recent_errors` will not surface them.
For these, delegate to the **`ticketing`** agent, which runs its own search/dedup
gate and applies the shipped issue defaults and label families from
`tm-ticketing` — do not restate them in the brief. The MCP pipeline is preferred
whenever a fingerprint exists; the manual path is the fallback, not the default.

## Bug Report Content Standard

**Title**: brief, descriptive, under ~70 chars. "PM delegates to
non-existent agent" not "Bug in system".

**Labels**: the label families in `tm-ticketing` apply unchanged — a type label
(`bug` for almost every bug report), the owning component/crate, and a `P0`-`P3`
priority only when the report itself asserts severity. 🔴 Never invent a label
the repository does not carry; check `gh label list` first. (`agent-error`,
`skill-error`, and `high-priority` were named here and exist in no trusty-tools
repo — #5202.)

**Body**: the sparse form in `tm-ticketing` applies — problem, decisive evidence,
one to four closure conditions, no `##` headings. A bug report conveys what
happened, what was expected, how to reproduce it, and the agent/skill/crate
involved, in prose, in under ten lines. The automated pipeline's scrubbed body is
generated and is not yours to reshape; this governs the manual fallback.

## Related Skills

- `tm-postmortem` — session-level orchestration of the same pipeline after a
  work session, plus what's worth reporting vs. not
- `tm-ticketing` — the canonical label families, sparse-body form, and the
  search/dedup dispositions this protocol files under
- `tm-circuit-breaker` — CB#6 covers delegating GitHub issue *operations*
  generally (not the bug-report MCP tools, which the PM may call directly)
