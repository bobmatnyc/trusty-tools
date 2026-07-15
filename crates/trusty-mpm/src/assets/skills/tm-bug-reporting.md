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
construction:

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
For these, delegate to the **Version Control** agent to file a `gh issue
create` against `bobmatnyc/trusty-tools` directly, using the same
information standard below (title, labels, structured body). Apply the shipped
issue defaults — `--assignee @me --label trusty-mpm` (create the label first if
missing: `gh label create trusty-mpm --description "Created/managed by a
trusty-mpm session" --color 8250df`) — **in addition to** the `bug` label and
any context labels below. The MCP pipeline is preferred whenever a fingerprint
exists; the manual path is the fallback, not the default.

## Bug Report Content Standard

**Title**: brief, descriptive, under ~70 chars. "PM delegates to
non-existent agent" not "Bug in system".

**Labels**: always `bug`; add context labels as applicable — `agent-error`,
`skill-error`, `documentation`, `high-priority` (critical functionality
broken).

**Body structure** (used by both the automated pipeline's scrubbed body and
the manual Version Control fallback):

```markdown
## What Happened
[clear description]

## Expected Behavior
[what should have happened]

## Steps to Reproduce
1. ...
2. ...

## Context
- Agent/skill: [name if applicable]
- Error message: [full error if available]
- Crate/version: [if known]

## Impact
[how this affects users/workflow]
```

## Related Skills

- `tm-postmortem` — session-level orchestration of the same pipeline after a
  work session, plus what's worth reporting vs. not
- `tm-circuit-breaker` — CB#6 covers delegating GitHub issue *operations*
  generally (not the bug-report MCP tools, which the PM may call directly)
