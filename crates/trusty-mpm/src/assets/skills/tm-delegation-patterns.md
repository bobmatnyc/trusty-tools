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

## The Full Routing Table

This is the authoritative per-agent routing surface. It was resident in the PM
prompt until #5087 moved it here; the prompt keeps only the four choices that
get made wrong, plus the roster of agents this project actually received. Route
to a name only if the roster lists it.

Every name in the first column is the deployed `subagent_type`, spelled exactly
as the Agent tool takes it. Pass it verbatim — a prose title like "Documentation
Agent" or "API QA" is not an agent and fails to dispatch (issue #4594).

| `subagent_type` | Delegate when — triggers | Model | Notes |
|---|---|---|---|
| `research` | codebase understanding, investigating approaches, analyzing files, architecture, system design, RFC drafting, technical roadmap, implementation plan, feature decomposition, trade-off analysis | sonnet | Grep, Glob, multi-file Read, WebSearch |
| `engineer` (or `rust-engineer`, `python-engineer`, `typescript-engineer`, … per language) | code changes, implementation, refactor | opus | Prefer the language-specific engineer whenever one exists |
| `code-analyzer` | reviewing a proposed solution BEFORE implementation; static analysis, correctness, architectural health | sonnet | The phase-2 "Code Analysis" agent; verdict APPROVED / NEEDS_IMPROVEMENT / BLOCKED. `code-analyzer` and `code-critic` are separate agents, not interchangeable |
| `code-critic` | adversarial review of code that already exists and passes its tests; APPROVE/WARN/BLOCK verdict | opus | Dispatch-gated by test-ladder rung — see "code-critic Dispatch Standard" below. NOT design critique, NOT every engineer dispatch |
| `local-ops` | localhost, PM2, npm, docker / docker-compose, ports, processes; every `make` and `mise run` target; build, dist, clean, install, setup; version, bump, release, publish, deploy (`pyproject.toml`, `package.json`) | sonnet | Default fallback for ops / infra / build, including anything unknown or ambiguous. The generic `ops` agent is DEPRECATED |
| `qa`, `web-qa`, `api-qa` | test, verify, check, regression, deployment verification; `make`/`mise run` `test`, `lint`, `check` (or `engineer`); browser, screenshot, click, navigate, DOM, console errors → `web-qa`; APIs → `api-qa` | sonnet | For browser work use `web-qa` — never chrome-devtools, claude-in-chrome, or playwright directly |
| `documentation` | docs, README, API docs, guides | haiku | Style consistency, organization standards |
| `ticketing` | every Issue operation — create, update, close, label, assign, milestone, comment, dedupe (P6) | sonnet | Route by artifact: the Issue is always ticketing's, and no PR mutation ever is. See `tm-ticketing` |
| `version-control` | the whole PR lifecycle incl. its title and body, plus branches, push/rebase/merge/tag, complex git, stacked PRs, post-merge verification, and reclaiming merged worktrees (P7) | haiku | Check git user for main-branch access. Dispatch it WITHOUT `isolation` — ADR-0056 leaves it in the checkout it is given. Policy comes from `tm-workflow` via the PM |
| `security` | pre-push credential scan, vulnerability assessment | sonnet | Secret scanning, attack-vector detection |
| `mpm-skills-manager` | creating/improving skills, recommending skills, stack detection | sonnet | Triggers: "skill", "stack", "framework" |

When the user says "just do it" or "handle it", delegate the full pipeline:
`research` → `engineer` → `local-ops` → `qa` → `documentation`.

## Agent Selection by Trigger Keyword

| Keywords | Agent |
|---|---|
| localhost, PM2, docker-compose, port, process | `local-ops` |
| vercel, edge function, serverless | `vercel-ops` |
| gcp, google cloud, IAM, OAuth consent | `gcp-ops` |
| browser, screenshot, click, navigate, DOM, console errors | `web-qa` |
| API, endpoint, HTTP, curl-shaped verification | `api-qa` |
| ticket, issue, PROJ-123, #123 | `ticketing` (see `tm-ticketing`) |
| PR, branch, merge, stacked | `version-control` (see `tm-workflow`) |
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

## Task Complexity: One Delegation or Several

Size the task before choosing a shape. The signals:

| Signal | Simple (1 delegation) | Complex (multi-phase) |
|--------|----------------------|----------------------|
| Scope | <200 lines, 1 file type | >500 lines, multi-service |
| External deps | None or 1 framework | DB + APIs + Docker + scheduler |
| Endpoints | ≤6 | >6 with auth, roles, events |
| Time estimate | <30 min | >1 hour |

**Simple → ONE engineer delegation carrying the full scope:** "Build this, write
tests, create README, run linters, verify all tests pass, commit." The engineer
handles everything; the Research, Code Analysis, QA and Documentation phases are
skipped under the skip conditions in the instruction package's phase table.

**Complex → the normal multi-phase workflow.**

## Batching: the Anti-Patterns

The instruction package states the target (5-7 delegations per session, ~95K of
context reloaded by each). These are the specific splits to collapse:

| Anti-pattern | Fix |
|---|---|
| Research then implement (2 delegations) | `engineer` can research + implement (1) |
| Implement then fix lint (2) | Include "fix lint" in the impl task (1) |
| Implement then commit (2) | Include "commit when done" in the task (1) |
| Sequential fixes to the same agent (N) | One delegation with full scope (1) |
| A separate docs agent for a per-task README | Include the README in the engineer delegation |
| One PR per issue for same-module bugs | One batched dispatch/PR per file-cluster; each issue keeps its own regression test and `Refs #N` line |

## Retry Protocol (delegated work came back failing)

1. **`SendMessage` the SAME agent** — never open a new delegation to fix a
   previous one. The agent fixes and re-verifies inside its own context, at zero
   context-reload cost.
2. Only re-delegate once that agent has failed 3+ times on the same issue
   (CB#10).

| Scenario | Action |
|----------|--------|
| Build/test/lint failure | `SendMessage` the originating agent with the error output |
| `engineer` reports "tests pass" with no raw output | `SendMessage`: "show raw test output" |
| Agent failed 3+ times on the same issue | Re-delegate to a different agent, or escalate |
| A named deliverable is missing | `SendMessage`: "the prompt requires <deliverable>, please create it" |

## Structural Delegation Brief

```
Task: [Specific measurable action]
Agent: [deployed subagent_type]
Requirements:
  Objective: [Measurable outcome]
  Findings: [What is already known, with its evidence — not a chosen mechanism]
  Success Criteria: [Conditions a wrong implementation fails]
  Testing: MANDATORY - Provide logs
  Constraints: [Performance, security, timeline]
  Verification: Evidence of criteria met
```

## What a Brief Carries — and What It Must Not

The resident rule is in the instruction package: a brief carries findings,
evidence and constraints, not the implementation mechanism. This is the evidence
behind it.

In one session on 2026-08-15 a PM prescribed mechanisms in five briefs while
reasoning from DESCRIPTIONS of code rather than the code. An agent that opened
the source corrected it every time, and every correction was right. Read the
table as a dated record of five wrong prescriptions, never as a statement of how
any of this code behaves today.

| # | What the PM prescribed | Why the agent was right to overrule it |
|---|---|---|
| 1 | Relayed a critic's fix verbatim — "POST the rewritten input to the shared-tree-dispatch route, or upsert via `observe`" | Both were no-ops on that code. The route re-derived eligibility for itself, which an isolated input made false, and the observe path returned early on an id it already held |
| 2 | "Classify every commit segment" to close a compound-command bypass | Insufficient. `git add -A && git commit -m docs` has exactly one commit segment and the same hole. The engineer's rule — every segment must be a `cd` or THE one commit — was strictly stronger |
| 3 | "Both arrival orders converge" as the acceptance criterion for a new mutex | The test passed sequentially on one thread. Deleting the mutex left it passing |
| 4 | Told an agent a hygiene routine did a hard reset | The PM had read a comment that no longer matched its function. The agent opened the file and found a different operation |
| 5 | Detect a scratch daemon by comparing its data dir to the default | Cannot work. A scratch daemon used the DEFAULT FORMULA (`$HOME/.trusty-mpm`); only the `$HOME` it resolved against changed |

Cases 1, 2 and 5 were not answerable from any description — the answer was in
control flow. Case 4 is the argument against copying code behaviour into a brief
at all: the PM was wrong BECAUSE it trusted a written description.

The same five briefs were valuable everywhere they carried findings, evidence
and constraints — an ADR's own "it would matter if the grant were ever made
conditional", a prior false-deny incident, the Fail-Open Check. Those helped
every time. Only the prescriptions cost rounds.

**Relaying a reviewer's fix.** A `code-analyzer` or `code-critic` finding names a
real problem and usually names a remedy too. Relay the problem as a finding to
close; relay the remedy as a suggestion to VERIFY. Case 1 was a critic reasoning
from source and still wrong about the fix.

## Acceptance Criteria a Wrong Implementation Fails

Before stating a criterion, ask: what implementation would pass this and still be
wrong? Name one and the criterion does not test what you think it tests.

Case 3 is the worked example. "Both arrival orders converge" is satisfied by two
sequential calls, so it could not detect the missing mutex it existed to prove.
The criterion that worked: two threads, same `tool_use_id`, N rounds, exactly one
record each round.

| Weak criterion | What passes it while wrong | Stronger |
|---|---|---|
| "Both arrival orders converge" | Sequential calls on one thread, no mutex | Two threads, same key, N rounds, exactly one record per round |
| "The bypass is closed" | A guard that closes only the command you named | Name the class: every segment must be a `cd` or THE one commit |
| "Tests pass" | A change that added no test able to fail | One regression test that provably FAILED against the pre-fix commit |

Row three is the Fail-Open Check the instruction package already puts in
`code-analyzer` / `code-critic` briefs, applied to your own criteria.

## Per-Agent Model Overrides and the Cost Model

The instruction package's Model Selection table is the default routing, and an
explicit `model=` in an Agent call always wins. Standing per-agent defaults are
set in `~/.trusty-mpm/config.toml`, taking priority over built-in defaults and
agent frontmatter but not over an explicit `model=`:

```toml
[models.agents]
engineer = "opus"
research = "sonnet"
```

The tier aliases `haiku` / `sonnet` / `opus` are resolved to concrete models at
dispatch by `expand_model_alias`, reading `[models.tiers]` from that same file
and falling back to the built-in defaults in `core/config.rs`. Configuration is
the source of truth for what each tier means — never a model id memorized from a
prompt (#4594).

Cost rises steeply haiku → sonnet → opus. Coding tasks pay for opus because
quality dominates there; routing everything else down-tier is where the savings
come from. Read current per-token pricing from the provider rather than a ratio
pinned in a prompt.

## code-critic Dispatch Standard

The critic tier keys off the project's test-ladder rung (this repo's Rust Test
Ladder in `CLAUDE.md`) — never a parallel risk axis invented for the decision.

| Rung | Change class | Dispatch code-critic? |
|---|---|---|
| 1–2 | Docs, comments, changelog, test-only stabilization | Never |
| 3 | Localized behavior inside one crate | No — the PM reviews the diff |
| 4 | Cross-crate, public API, shared library | Only if a contract changes. Mechanical propagation does not qualify |
| 5–6 | Cross-crate contract, persistence, security, process lifecycle, release tooling, UI/API surface | Required |

Enum changes and spelling fixes are rung 1–3. No critic.

**Escalate to required regardless of rung:** the change can start, refuse, or
gate a session; it touches a trust boundary or an injection defense; it rewrites
history or force-pushes; or the PR is already at review round 3+ — evidence
something is being missed.

**Not a reason to dispatch:** a design question (send it to the owner, or the PM
decides), the PM wanting a second opinion, or confirming green CI.

## Worktree Isolation on a Dispatch

**From a main checkout, this is automatic, not a PM choice (ADR-0048).** `tm
hook --pm-guard` grants `isolation: "worktree"` to any dispatched agent that
may write, single or parallel, the moment the session is standing in a main
checkout — the write boundary above denies that agent a source edit there
anyway. Nothing needs declaring for this case.

🔴 **Do not declare `isolation` on a `version-control` dispatch (ADR-0056).** It
merges into main and reclaims merged worktrees; a worktree removes the tree that
work needs, and the isolation fences have hidden a branch ref badly enough to
force a push by SHA. The guard leaves it where it is, single or parallel, and
declaring isolation yourself overrides that. Every other writer is unchanged:
an engineer dispatched into the same directory beside it is still denied.

**From inside a worktree, pass it yourself for concurrency.** Pass
`isolation: "worktree"` on Agent tool calls when spawning 2+ parallel agents
that modify files on the SAME checkout — the case the guard cannot see,
because there is no main-checkout grant to fall back on. Not needed for
sequential agents, read-only research, or separate file trees. Use
`run_in_background: true` for fire-and-forget parallel work.

🔴 **`isolation: "worktree"` is the only sanctioned mechanism, and the PM never
authors a `git worktree add` into a dispatch prompt (#5649).**
`tm hook --pm-guard` reads the declared parameter and never the prompt, so a
hand-rolled worktree leaves the agent counted against the shared HEAD and gets
the next file-mutating dispatch denied for a collision that does not exist.

**When `isolation` is unavailable, serialize** — one file-mutating agent at a
time, each waited for before the next. Serializing always works. Hand-rolling to
parallelize anyway is what this forbids.

## Cross-Workstream Coordination (memory claim drawers, DOC-53)

Memory is awareness only — never a lock, never a message channel. git/GitHub
branch/PR/label state is the authoritative claim; the event bus (#3168 BUS-7) is
the real-time channel. Before dispatching multi-agent work on an area:

1. `memory_list(tag: "ws-claim")`, then verify any hit against live git state —
   a claim whose branch/PR is gone is void.
2. Write a claim drawer when dispatching: title `WS-CLAIM <workstream>: <area>`,
   tags `ws-claim`, `ws:<name>`, `area:<slug>`; body = scope, branch, PR/issue
   refs, expected-land condition.
3. Supersede (or `memory_forget`) the claim once the work lands or is abandoned.

A claim drawer covers the work area, never the merge queue. Who may merge on a
shared repository is its own claim: `Skill(skill="tm-workflow")`,
"Merge-Queue Ownership".

## Long-Wait Delegation (issues #2833, #4792, #5843)

A delegated agent's own gate (`cargo test -p <crate>`, a release build) blocks in
its foreground; **CI does not**, by default. Agents push, take a one-shot status
read, report, and stop — the PM re-engages when checks settle. Make both halves
explicit in the delegation prompt instead of hoping the agent improvises:

1. **Keep the gate under the ceiling.** Ask for **crate-scoped** gates
   (`cargo test -p <crate>`, `cargo clippy -p <crate>`), never
   `cargo test --workspace` — the scoped run finishes inside one invocation and
   its raw output is the evidence you collect.
2. **Name which CI pattern the agent should use** — the brief picks one rather
   than leaving the agent to improvise:
   - **Default — one-shot and stop.** "Do NOT use `gh pr checks --watch` — it
     streams check output into your context (546k tokens over 54 minutes on
     one PR). Push, take a one-shot `gh pr checks <pr>` read, report it, and
     end your turn." PM re-engagement (below) owns what happens next.
   - **`tm wait --for check`, only when the brief wants the agent to hold its
     own turn for a bounded window instead of ending it.**
     `tm wait --for check --pr <n> [--repo <owner/repo>] --timeout <secs>`
     cross-reads `state`, never `bucket` alone, never streams, and prints a
     `rerun=` command on exit 75 that the agent re-issues until exit 0 (met)
     or exit 1 (timeout — report the timeout itself and stop). This is the
     sanctioned one-shot-plus-rerun form (#5843), not a reason to prefer
     agent-side waiting generally — a run that legitimately takes 15+ minutes
     is still better handed to PM re-engagement than re-issued a dozen times
     in one turn.
3. **Forbid parking, and name `tm wait` as the replacement for a hand-rolled
   poll.** "Do not background your gate and end the turn expecting a
   notification, and do not hand-roll a `sleep`/`until` poll loop — use
   `tm wait --for run|file` for a condition you own, or the CI pattern named
   above. Block quietly on your own commands; hand back an observation, not a
   promise to report."
4. **The agent's OWN condition — not CI — always uses `tm wait`.** A sentinel
   file another process writes, or a pid the agent is waiting to exit, is
   `tm wait --for file` / `tm wait --for run` outright (see BASE-AGENT's
   "Never Narrate a Wait"). Prescribe it in the brief instead of leaving the
   agent to reach for `sleep`.
5. **PM re-engagement.** When the agent hands back with CI pending — whether
   from the default pattern or from a `tm wait --for check` that hit its
   `--timeout`, own the gap yourself — see the next section.

The daemon idle-nudge (#2621) does NOT cover in-conversation subagents — they
have no tmux pane — so PM-side re-engagement is the only mechanism below the
managed-session layer for the CI-settle notification `tm wait` cannot itself
deliver: a CI run can legitimately outlast one turn's whole budget, and nothing
brings a stopped agent back except the PM's `SendMessage`.

## PM Re-Engagement (issues #2833, #4792, #5843)

An agent that pushes, takes a one-shot status read, reports, and stops has done
the right thing. Nothing wakes it again, so the work is abandoned unless the PM
re-engages. That is the whole mechanism at this layer.

A hand-back reading `tm-wait status=timeout for=check …` is the same situation
under the `tm wait --for check` pattern (previous section) as an ordinary
one-shot CI-pending report: the agent's own bounded budget ran out, not the CI
run. Re-engage it exactly as below — never as a park (next section) and never
by nudging it to re-run `tm wait` itself, since nothing brought the agent back
to issue that re-run.

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
Do not restore that advice anywhere. `tm wait` is not a reintroduction of it —
it never streams, returns within a bounded slice printed on its own line, and
is the sanctioned in-turn wait for an agent's own condition (BASE-AGENT, "Never
Narrate a Wait"). What stays retired is `--watch` and any hand-rolled
sleep-poll for CI specifically, because a CI run can legitimately outlast a
whole turn's budget — that case is still PM re-engagement's job.

**Prevention.** Tell the engineer/QA to use crate-scoped gates
(`cargo test -p <crate>`, not `cargo test --workspace`) — the scoped run finishes
well under the 10-minute tool ceiling, so agents rarely re-issue at all. For an
agent's own file/process condition, name `tm wait` in the brief so it never
reaches for `sleep`.

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

## Architecture Suggestions From Agents

Agents surface improvement opportunities as part of their normal reporting. Cap
what reaches the user at **1-2 per session**, keep each specific rather than
vague, and ask before implementing any of them:

```
[Agent] found [issue]. Consider: [fix] -- [benefit]. Effort: [S/M/L]. Implement?
```

## Delegation Best Practices

1. **Provide context** — the findings, evidence and constraints the agent needs,
   not just the task, and not a chosen implementation mechanism (see "What a
   Brief Carries" above).
2. **Clear acceptance criteria** — how the agent knows it's done, each one
   written so a wrong implementation fails it (see "Acceptance Criteria a Wrong
   Implementation Fails" above).
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
