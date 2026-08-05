---
name: tm-delegation-patterns
description: Delegation matrices and agent-selection decision trees for the trusty-mpm PM, plus PM re-engagement of a parked or CI-waiting subagent — what to do when an agent hands back with CI pending, checks unsettled, or a backgrounded wait it expects to wake it
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

`vercel-ops` and `gcp-ops` are platform-gated, so a project without the
matching marker never receives them. Which agents exist at all is declared in
`framework-manifest.toml` (rendered in `tm-capabilities`'s
`references/agents.md`) — check there, or the project's generated roster,
before routing. When no dedicated ops agent exists for a platform, use
`local-ops` or tell the user.

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

Language-specific Engineer selection follows the project's stack markers. The
marker for each engineer is declared in `framework-manifest.toml` and rendered
in the **Deploys When** column of `tm-capabilities`'s `references/agents.md` —
read it there rather than from a prose copy. The launch prompt's **Detected
Project Stack** section already states this project's answer. When the stack is
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
4. **PM re-engagement.** When the agent hands back with CI pending, own the gap
   yourself — see the next section.

The daemon idle-nudge (#2621) does NOT cover in-conversation subagents — they
have no tmux pane — so PM-side re-engagement is the only mechanism below the
managed-session layer.

## PM Re-Engagement (issues #2833, #4792)

An agent that pushes, takes a one-shot status read, reports, and stops has done
the right thing. Nothing wakes it again, so the work is abandoned unless the PM
re-engages. That is the whole mechanism at this layer.

**Re-engage the SAME agent.** `SendMessage` it the outcome — the merge go-ahead,
or the failing check output — once the run should have settled. Never open a
fresh delegation for work an existing agent already owns: the fresh one reloads
~95K tokens of context and knows none of the history. Never nudge an agent back
into a blocking wait.

**Cross-check `state` before calling anything green.** Treat `bucket` as
advisory: under GitHub API eventual-consistency lag it can report a false DONE
while a check has not settled.

**Sizing a `Monitor`, if you use one.** Size the interval to the known CI
wall-clock — 5 minutes or more for a ~15-minute run. Message only on state
change. If it overruns, run a one-shot `gh run view <run-id>` rather than
tightening the interval. **Never a 30-second blind poll** — that is the spam
counter-failure (#2833), the exact behavior the retired blocking waits were
replaced to avoid. Disarm the monitor the moment checks settle.

**Telling a genuine park from a correct hand-back.** A genuine park is a subagent
returning with its goal UNMET *and* a final message saying it backgrounded a
wait — "monitoring … in the background", "I'll wait for the notification", or a
background task id it expects to wake it. `SendMessage` that agent to resume the
unfinished work now. An agent that names pending CI and stops is NOT a park;
accept it and re-engage on the schedule above.

**A human-wait is a legitimate stop.** "Let me know once you approve the deploy"
is not a park. Surface it to the user; do not nudge it.

**Why blocking waits are not the fix.** They were retired for context cost, not
because they failed to work: `gh pr checks --watch` streams check output for the
whole run, and one engineer burned 546k tokens over 54 minutes on a single PR.
Do not restore that advice anywhere.

**Prevention.** Tell the engineer/QA to use crate-scoped gates
(`cargo test -p <crate>`, not `cargo test --workspace`) — the scoped run finishes
well under the 10-minute tool ceiling, so agents rarely re-issue at all.

## Concurrent Dispatch: Declare File Ownership

🔴 Before dispatching concurrent work, name in each brief the files owned by
every other in-flight branch, and instruct the agent to STOP and report rather
than edit one. **We stack, we do not race.**

Paste a block shaped like this into every concurrent brief:

```
🔴 Files owned by other in-flight branches — do not touch
- feat/<branch-a>: core/agent_cost.rs, bin/tm/commands/pm_guard*.rs
- fix/<branch-b>: core/agent_source*.rs, core/managed_config.rs
Stop and report if you need one.
```

The doctrine is that no two in-flight PRs share a module, and that coupled
changes stack rather than race. The ownership block is the mechanism that
enforces it: an agent cannot see the other branches, so unless the brief names
their files it will edit across the line and only git finds out.

Measured on 2026-08-03: five concurrent PRs across five branches, every brief
carrying the block — zero merge conflicts, zero rebases.

## A Running Agent's Scope Is Fixed

🔴 New work is a new agent, or it waits. NEVER add scope to a live agent.

Adding scope to a running agent feels cheaper than dispatching fresh and is the
opposite. Cost tracks **accumulated context over the agent's lifetime**: every
tool round re-sends the whole transcript, so each added task extends the life and
every subsequent round pays for the accumulation. One agent given two mid-flight
additions reached 269.9k tokens while writing a five-line changelog file — see
[#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837) for the evidence.

Several narrow agents each ending near 60k cost far less than one reaching 400k
doing identical work.

Not covered by this rule: re-engaging a parked agent with the outcome of work it
already owns (`SendMessage` after CI settles — Long-Wait Delegation item 4
above). That closes existing scope; it does not add new scope.

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
