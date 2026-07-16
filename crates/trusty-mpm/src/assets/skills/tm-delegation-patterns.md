---
name: tm-delegation-patterns
description: Delegation matrices and agent-selection decision trees for the trusty-mpm PM
user-invocable: false
version: "1.0.0"
category: pm-reference
tags: [delegation, agents, patterns, pm-required]
effort: high
---

# Delegation Patterns

Native Claude Code already knows how to invoke the Agent/Task tool — this
skill is the *selection* layer on top of that: which agent, in what order,
for which shape of work. Every agent named below is a real, currently
bundled trusty-mpm agent (verified against `bundle_all.rs`'s `ALL` table);
do not invent agent names not in this list.

## Full-Stack Feature

**Chain**: Research → Code Analysis → (`react-engineer` / `web-ui-engineer`) +
language Engineer → Ops (deploy) → Ops (verify) → `web-qa` + `api-qa` →
Documentation.

**Example**: "Add a user dashboard with analytics" → Research investigates
frameworks → Code Analysis reviews the approach → `react-engineer` builds
the UI, `rust-engineer`/`python-engineer` (per stack) builds the analytics
endpoint → `local-ops` deploys to staging and verifies (health check, logs)
→ `api-qa` tests the endpoints, `web-qa` tests the dashboard → Documentation
updates the API docs and user guide.

## API-Only Development

**Chain**: Research → Code Analysis → language Engineer → Ops (deploy, if
needed) → Ops (verify) → `api-qa` → Documentation.

## Web UI Only

**Chain**: Research → Code Analysis → `react-engineer` / `svelte-engineer` /
`web-ui-engineer` (per stack) → Ops (deploy) → Ops (verify) → `web-qa` →
Documentation.

## Local Development / Infra

**Chain**: Research → Code Analysis → Engineer → `local-ops` (start/PM2/Docker)
→ `local-ops` (verify: logs + health check) → `qa` → Documentation.

## Bug Fix

**Chain**: Research (reproduce/investigate) → Code Analysis → Engineer (fix)
→ Ops (deploy, if applicable) → Ops (verify) → `web-qa`/`api-qa` (regression)
→ **Version Control** (PR).

## Platform-Specific Deploy

| Platform | Ops Agent |
|---|---|
| Vercel | `vercel-ops` |
| Google Cloud | `gcp-ops` |
| Local (PM2/Docker/localhost) | `local-ops` |

**Not bundled in trusty-mpm** (do not delegate to these — they don't exist
here): `railway-ops`, `clerk-ops`, `digitalocean-ops`. If a task needs one of
these platforms, use the closest real agent (`local-ops` as the fallback) or
tell the user no dedicated ops agent exists yet.

## Agent Selection by Trigger Keyword

| Keywords | Agent |
|---|---|
| localhost, PM2, docker-compose, port, process | `local-ops` |
| vercel, edge function, serverless | `vercel-ops` |
| gcp, google cloud, IAM, OAuth consent | `gcp-ops` |
| browser, screenshot, click, navigate, DOM, console errors | `web-qa` |
| API, endpoint, HTTP, curl-shaped verification | `api-qa` |
| ticket, issue, PROJ-123, #123 | `ticketing` (see `tm-ticketing`) |
| PR, branch, merge, stacked | `version-control` (see `tm-pr-workflow`) |
| skill authoring, skill catalog | `mpm-skills-manager` |
| agent authoring, agent catalog | `mpm-agent-manager` |
| code review, adversarial verdict | `code-critic` |
| memory palace, cross-session recall | `memory-manager` |

Language-specific Engineer selection follows `Cargo.toml` (rust-engineer),
`tsconfig.json` (typescript-engineer), `pyproject.toml`/`setup.py`
(python-engineer), `go.mod` (golang-engineer), `pom.xml`/`build.gradle`
(java-engineer), `.csproj` — see `AGENT_DELEGATION.md`'s Language Detection
section for the full table; when unknown, Research is mandatory (never
default to a guess).

## Long-Wait Delegation (chunked-repoll — issue #2833)

Any delegation that ends in a wait longer than the bash tool's 10-minute
per-invocation ceiling — `gh pr checks` (CI ~10-20 min), a release cut, or
`cargo test --workspace` (~16 min) — is where subagents strand: they background
the wait and end their turn (a **park**), or degenerate into a 30-second blind
poll loop (**spam**). Both come from not sizing the wait to its real duration.
Make the wait first-class in the delegation prompt instead of hoping the agent
improvises it:

1. **Keep the gate under the ceiling.** Ask for **crate-scoped** gates
   (`cargo test -p <crate>`, `cargo clippy -p <crate>`), never
   `cargo test --workspace` — the scoped run finishes inside one invocation.
2. **Name the blocking primitive.** For CI, instruct: "wait with
   `gh pr checks <pr> --watch --fail-fast` in the foreground; it blocks silently
   and prints once. Re-issue it verbatim if the 10-min ceiling cuts it off. Do
   NOT background it, do NOT sleep-poll it, do NOT end your turn to monitor it."
3. **Forbid both failure modes explicitly.** "Parking (ending your turn to wait)
   and spamming (a tight `still pending` poll loop) are both wrong. Block quietly
   in the foreground; message only when state changes."
4. **PM back-stop.** If the subagent parks anyway, `SendMessage` the SAME agent a
   resume nudge (see PM_INSTRUCTIONS "Parked-Subagent Detection & Nudge") — never
   a fresh delegation.

This is prevention (steps 1-3, in the delegation prompt) plus a PM-side
detect-and-nudge back-stop (step 4). The daemon idle-nudge (#2621) does NOT
cover in-conversation subagents — they have no tmux pane — so this protocol is
the only mitigation below the managed-session layer.

## Delegation Best Practices

1. **Provide context** — background the agent needs, not just the task.
2. **Clear acceptance criteria** — how the agent knows it's done.
3. **Wait for completion** — don't interrupt an in-flight delegation.
4. **Collect evidence** — get specific artifacts back (see
   `tm-verification-protocols`).
5. **Track files immediately** — right after the agent returns (see
   `tm-git-file-tracking`).
6. **Chain verification** — QA after implementation, always (CB#8).

## Related Skills

- `tm-circuit-breaker` — the enforcement layer behind these patterns
- `tm-workflow` — the phase/gate model these chains slot into
- `tm-verification-protocols`, `tm-git-file-tracking` — the evidence and
  tracking steps every chain above assumes
