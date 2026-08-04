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
(java-engineer), `.csproj` — see the bundled agent-delegation section
(`assets/instructions/sections/agent-delegation.md`) for the full table; when
unknown, Research is mandatory (never default to a guess).

## Long-Wait Delegation (issues #2833, #4792)

A delegated agent's own gate (`cargo test -p <crate>`, a release build) blocks in
its foreground; **CI does not**. Agents push, take a one-shot status read, report,
and stop — the PM re-engages when checks settle. Make both halves explicit in the
delegation prompt instead of hoping the agent improvises:

1. **Keep the gate under the ceiling.** Ask for **crate-scoped** gates
   (`cargo test -p <crate>`, `cargo clippy -p <crate>`), never
   `cargo test --workspace` — the scoped run finishes inside one invocation and
   its raw output is the evidence you collect.
2. **Forbid the CI block.** "Do NOT use `gh pr checks --watch` — it streams check
   output into your context (546k tokens over 54 minutes on one PR). Push, take a
   one-shot `gh pr checks <pr>` read, report it, and end your turn."
3. **Forbid parking too.** "Do not background your gate and end the turn
   expecting a notification, and do not sleep-poll. Block quietly in the
   foreground on your own commands; hand back an observation, not a promise to
   report."
4. **PM re-engagement.** When the agent hands back with CI pending, re-read the
   status yourself and `SendMessage` the SAME agent the outcome — never a fresh
   delegation, and never a nudge back into a blocking wait (see PM_INSTRUCTIONS
   "Parked-Subagent Re-Engagement").

The daemon idle-nudge (#2621) does NOT cover in-conversation subagents — they
have no tmux pane — so PM-side re-engagement is the only mechanism below the
managed-session layer.

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
