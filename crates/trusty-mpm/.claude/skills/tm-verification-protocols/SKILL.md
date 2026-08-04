---
name: tm-verification-protocols
description: QA verification gate and evidence requirements for the trusty-mpm PM
user-invocable: false
version: "1.0.0"
category: pm-workflow
tags: [qa, verification, evidence, pm-required]
effort: high
---

# QA Verification Gate Protocol

## Mandatory QA Verification Gate

The PM MUST delegate to QA (or the `mcp__trusty-review__*` gate) BEFORE
claiming work complete. No completion claim without evidence — this is CB#8
in `tm-circuit-breaker`.

**Applies to**: UI features, local server UI, API endpoints, bug fixes,
full-stack features, test modifications — any user-facing or behavior-changing
work.

**Correct sequence**: implementation → PM delegates to QA → PM waits for
evidence → PM reports *with* the QA verification attached.

## Verification Requirements by Work Type

| Work Type | Agent / Tool | Required Evidence | Forbidden Claim |
|---|---|---|---|
| Local server UI | `web-qa` | Browser navigation + snapshot + console check | "page loads correctly" |
| Deployed web UI | `web-qa` | Screenshot + console logs | "UI works" |
| API / server | `api-qa` | HTTP responses + logs | "API deployed" |
| Local backend / daemon | `local-ops`, or `mcp__trusty-search__search_health` / `mcp__trusty-memory__memory_recall` for trusty-* services | Process status, health-check response | "running on localhost" |
| Code diff quality | `mcp__trusty-review__review_diff` / `review_pr` | Review verdict (APPROVE/WARN/BLOCK) with findings | "the diff looks fine" |
| CLI tools | Engineer / Local Ops | Command output + exit code | "tool installed" |

## Forbidden Phrases (Circuit Breaker Violation)

Never say these without attached agent evidence: "production-ready", "page
loads correctly", "UI is working", "should work", "looks good", "seems
fine", "it works", "all set", "ready for users", "deployment successful".

Always say instead: `"[Agent] verified with [tool/method]: [specific
evidence]"`.

## Evidence Quality Standards

**Good evidence** is specific, measurable, attributed, and reproducible:
- File paths and line numbers; URLs and endpoints tested; HTTP status codes
- "12 tests passed, 0 failed"; "HTTP 200 OK"; "no console errors found"
- "web-qa verified via browser navigation"; "the engineer ran the project's
  test gate (e.g. `cargo test`, `npm test`, `pytest`, or `go test ./...`)"
- Reproducible steps: exact command run, exact URL navigated

**Insufficient evidence (violations)**: "works", "looks good", "should be
fine", "deployed successfully" with no health check, "tested it" with no
steps shown, or the PM's own unverified assessment ("I checked and it
works" — the PM did not check; it must delegate the check).

## Required Evidence by Claim Type

| Claim | Required Evidence |
|---|---|
| Implementation complete | Engineer confirmation, files changed (paths), commit hash |
| Deployed | Ops confirmation, health-check response, process status |
| Bug fixed | QA repro (before), Engineer fix (files), QA verify (after) |
| Any status | `[Agent] verified with [tool]: [specific evidence]` — never "I think"/"likely"/"looks good" |

## Browser / UI Verification

The PM must never assert browser or UI state itself. Delegate to `web-qa`,
which owns the browser tool priority chain (native Claude Code Chrome first,
then MCP fallbacks — see the `web-qa` agent and `webapp-testing` /
`skills-webapp-testing` skills for the concrete tool sequence). Required
evidence from web-qa: navigation result, page snapshot/content, screenshot,
console error check, and relevant network request status.

```
✅ CORRECT: "web-qa verified: navigated to http://localhost:3000, page shows
   login form, no console errors, GET /api/config → 200 OK"
❌ WRONG:   "The page loads correctly at localhost:3000"  (no agent evidence)
```

## Example Good Report

```
Work complete: input-validation guard (A2)

Implementation: the engineer added validate_input in src/<module>/<file-a>
and the warning-log calls in src/<module>/<file-b>.
Files: src/<module>/<file-a> (+45), src/<module>/<file-b> (+12).
Commit: a15150fc

Testing: <the project's test command, e.g. `cargo test`, `npm test`, `pytest`>
  37 passed; 0 failed; 0 ignored

All acceptance criteria met.
```

## Enforcement

This is CB#8 in `tm-circuit-breaker`: PM claims completion without QA
delegation → BLOCK, delegate to QA now. 3-strike model applies (WARNING →
ESCALATION → FAILURE).
