---
name: tm-postmortem
description: Analyze session errors captured across trusty-* daemons and route them through the bug-reporting pipeline
user-invocable: true
version: "1.0.0"
category: pm-workflow
tags: [postmortem, analysis, errors, pm-recommended]
effort: medium
---

# /tm-postmortem

Analyze recently captured ERROR-level events across the trusty-* daemon fleet
(trusty-search, trusty-memory, trusty-analyze, trusty-mpm) and route real bugs
through the automated bug-reporting pipeline. Unlike claude-mpm's postmortem
(which parsed ad-hoc script/skill/agent logs), trusty-mpm's error capture is a
first-class MCP surface — this skill is a thin orchestration layer over it,
not a log-parsing exercise.

## Usage

```
/tm-postmortem [--limit N]
```

## The Real Pipeline

1. `mcp__trusty-mpm__list_recent_errors(limit)` — lists recently captured
   ERROR-level events across all trusty-* daemons. Each entry carries a
   64-char hex SHA-256 `fingerprint` (for deduplication), an occurrence
   count, the originating crate, and a one-line summary.
2. For each fingerprint worth reporting, call
   `mcp__trusty-mpm__preview_bug_report(fingerprint)` — shows the exact
   scrubbed GitHub issue body that would be filed, including what was
   redacted (paths, tokens, secrets) and the proposed labels. **Nothing is
   filed by this step.**
3. Present the preview to the user and get explicit consent.
4. Only on explicit consent, call
   `mcp__trusty-mpm__report_bug(fingerprint, confirm: true)` — files (or
   deduplicates onto an existing open issue for the same fingerprint) in
   `bobmatnyc/trusty-tools`. Returns `{ filed, deduped, issue_url,
   issue_number }`.

The PM never calls `report_bug` with `confirm: true` without having shown the
user the `preview_bug_report` output first and gotten an explicit "yes, file
it" — this is the same "ask before creating" discipline as ticket creation
(see `tm-ticketing`).

## What Counts as Worth Reporting

- A recurring error (occurrence count > 1) not already tracked
- A crash or panic in a daemon, not a client-side transient (timeout,
  connection refused during a known restart window)
- A skill/instruction/documentation defect discovered during a session (file
  it against `bobmatnyc/trusty-tools` the same way — single-repo monorepo,
  no separate skills/agents repos to route between)

## What Does NOT Need Reporting

- A one-off transient network blip with no repeat occurrence
- User error (wrong command, misconfigured environment) — no daemon defect
- Something already fixed on `main` but not yet in the user's installed
  binary (`cargo install --path ... --locked` first, then re-check)

## No `TRUSTY_BUGREPORT_GITHUB_TOKEN` Configured

`report_bug` returns an actionable error message telling the user to set
`TRUSTY_BUGREPORT_GITHUB_TOKEN`. Do not attempt a manual `gh issue create` as
a workaround — surface the error and the fix (set the env var), then retry
the pipeline.

## Example Session

```
PM: mcp__trusty-mpm__list_recent_errors(limit=20)
  -> 3 errors: 2 unique fingerprints, one seen 4 times (trusty-search)

PM: mcp__trusty-mpm__preview_bug_report(fingerprint="a1b2...")
  -> title, scrubbed body, labels: [bug, trusty-search]

PM: "Found a recurring trusty-search panic (4 occurrences). Preview attached.
     File this issue?"
User: "yes"

PM: mcp__trusty-mpm__report_bug(fingerprint="a1b2...", confirm=true)
  -> { filed: true, deduped: false, issue_url: "...", issue_number: 1853 }
```

## Related Skills

- `tm-bug-reporting` — the general (not session-postmortem-triggered) routing
  decision tree for filing issues
- `tm-verification-protocols` — evidence standards this skill's own reports
  must meet before claiming a fix
