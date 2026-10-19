---
name: tm-delegation-patterns
description: Delegation matrices and agent-selection decision trees for the trusty-mpm PM, plus PM re-engagement of a parked or CI-waiting subagent — what to do when an agent hands back with CI pending, checks unsettled, or a backgrounded wait it expects to wake it
user-invocable: false
metadata:
  version: "1.2.0"
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

## Where Each Task Lives

This file holds the rules every delegation needs. Everything else sits in one
file per task under `references/`, beside this file. Read the file your task
needs, and only that file. A pointer elsewhere that names a section of
`tm-delegation-patterns` resolves here: find the section name in the middle
column.

| Task | Sections | File |
|---|---|---|
| Map a shape of work to an agent chain; pick the deploy agent for a platform | "Full-Stack Feature", "API-Only Development", "Web UI Only", "Local Development / Infra", "Bug Fix", "Platform-Specific Deploy" | [references/workflow-chains.md](references/workflow-chains.md) |
| Size a task as one delegation or several; collapse split delegations; batch bug fixes | "Task Complexity: One Delegation or Several", "Batching: the Anti-Patterns" | [references/sizing-and-batching.md](references/sizing-and-batching.md) |
| Keep the mechanism out of a brief; relay a reviewer's fix; write criteria a wrong implementation fails; surface agent suggestions | "What a Brief Carries — and What It Must Not", "Acceptance Criteria a Wrong Implementation Fails", "Architecture Suggestions From Agents", "Delegation Best Practices" | [references/briefs-and-criteria.md](references/briefs-and-criteria.md) |
| Choose the model for a dispatch; set per-agent defaults | "Per-Agent Model Overrides and the Cost Model" | [references/models.md](references/models.md) |
| Decide whether a change earns a `code-critic` round; brief the critic | "code-critic Dispatch Standard" | [references/code-critic.md](references/code-critic.md) |
| Declare `isolation: "worktree"`; salvage a dead agent's tree; write a claim drawer; declare file ownership for concurrent dispatches | "Worktree Isolation on a Dispatch", "Salvaging a Dead Agent's Worktree", "Cross-Workstream Coordination (memory claim drawers, DOC-53)", "Concurrent Dispatch: Declare File Ownership" | [references/worktree-isolation.md](references/worktree-isolation.md) |
| Check what the PM may do directly; decide when the PM may stop and ask | "The PM Allowlist, in Full", "Autonomous Execution — When the PM May Stop and Ask" | [references/pm-allowlist-and-autonomy.md](references/pm-allowlist-and-autonomy.md) |

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
| `code-critic` | adversarial review of code that already exists and passes its tests; APPROVE/WARN/BLOCK verdict | opus | Dispatch-gated by test-ladder rung — see "code-critic Dispatch Standard" in `references/code-critic.md`. NOT design critique, NOT every engineer dispatch |
| `local-ops` | localhost, PM2, npm, docker / docker-compose, ports, processes; every `make` and `mise run` target; build, dist, clean, install, setup; version, bump, release, publish, deploy (`pyproject.toml`, `package.json`) | sonnet | Default fallback for ops / infra / build, including anything unknown or ambiguous. The generic `ops` agent is DEPRECATED |
| `qa`, `web-qa`, `api-qa` | test, verify, check, regression, deployment verification; `make`/`mise run` `test`, `lint`, `check` (or `engineer`); browser, screenshot, click, navigate, DOM, console errors → `web-qa`; APIs → `api-qa` | sonnet | For browser work use `web-qa` — never chrome-devtools, claude-in-chrome, or playwright directly |
| `documentation` | docs, README, API docs, guides | haiku | Style consistency, organization standards |
| `ticketing` | every Issue operation — create, update, close, label, assign, milestone, comment, dedupe (P6) | sonnet | Route by artifact: the Issue is always ticketing's, and no PR mutation ever is. Every issue gets a milestone, relationships and state — `tm-ticketing`, "Every issue: milestone, relationships, state" |
| `version-control` | the whole PR lifecycle incl. its title and body, plus branches, push/rebase/merge, complex git, stacked PRs, post-merge verification, and reclaiming merged worktrees (P7). A plain annotated tag on explicit PM instruction is version-control's; a release tag bound to a `cargo publish` is `local-ops`'s | sonnet | Check git user for main-branch access. Dispatch it WITHOUT `isolation` — ADR-0056 leaves it in the checkout it is given — EXCEPT a dispatch that commits source, which declares `isolation: "worktree"` (#8156). Policy comes from `tm-workflow` via the PM |
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

## Retry Protocol (delegated work came back failing)

1. **`SendMessage` the SAME agent** — never open a new delegation to fix a
   previous one. The agent fixes and re-verifies inside its own context, at zero
   context-reload cost. A worktree agent's tree must still exist first — see
   PM Re-Engagement (#8004).
2. Only re-delegate once that agent has failed 3+ times on the same issue
   (CB#10).

| Scenario | Action |
|----------|--------|
| Build/test/lint failure | `SendMessage` the originating agent with the error output |
| `engineer` reports "tests pass" with no raw output | `SendMessage`: "show raw test output" |
| Agent failed 3+ times on the same issue | Re-delegate to a different agent, or escalate |
| A named deliverable is missing | `SendMessage`: "the prompt requires <deliverable>, please create it" |

## Reopening a Finished Task

The Retry Protocol above assumes the agent is still failing. **Live evidence
that reopens a task an agent already finished gets a fresh agent dispatched at
the branch tip with the evidence, not a `SendMessage` into the finished
agent.** Three sequential `SendMessage`s into one already-finished engineer
pushed it past the 250K context notice mid-task.

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
  Time Box / Token Box: [N minutes / M tokens]
  Verification: Evidence of criteria met
```

**Dispatch budget, every brief.** State a time box and a token box. Default
when unstated: 45 minutes or 150k tokens for a single-crate fix; 20 minutes
or 60k for a research or ticketing pass. Size up for cross-crate work and say
so in the brief.

The PM checks the box, not the clock — no periodic status pings. A message
to a busy agent queues until its next tool round and costs it a turn; the
harness already notifies on completion. Past the box, send ONE corrective
message with a specific instruction. A second overrun means stop and
re-dispatch with a narrower brief, never another nudge.

**Scratch directory, every brief (#7791).** Name the agent's own scratch
directory: `<scratchpad>/<issue>-<round>/` or `<scratchpad>/<agent-id>/`. The
agent writes scratch files only there (BASE-AGENT). Agents dispatched
together get distinct directories; generic names at the scratchpad root
collide.

**A brief commissioning a prose document names no line or word cap (#8309).**
The 500-SLOC cap is code only. A cap on a research report, spec or ADR forces
rewrites and collides with mandatory citations. Ask for the Write Plainly style
instead; it limits verbosity.

A brief never asks for output that scales with build length ("raw output",
"full log") — it asks for gate summary lines per the gate-output rule
(BASE-AGENT, "Gate Output: Quote Results, Summarize Progress").

**Rust-conditional line (#8192).** When the Detected Project Stack includes
`rust-engineer`, the brief must instruct the agent to prefix
`RUSTC_WRAPPER=sccache CARGO_BUILD_JOBS=<n>` inline on every cargo command,
taking `<n>` from this machine's `tm doctor` output and defaulting to `2`.
Inline per command, because an agent's shell environment does not persist
between Bash calls — a single `export` at the top of the task protects
nothing. The rule itself lives in the `rust-delivery-workflow` skill; the
brief carries the number, which is machine-local.

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

**Check the worktree still exists first (#8004).** This applies to EVERY
resume of an agent that held an isolated worktree — CI outcome, retry, a
released HOLD, an owner ruling. Claude Code removes an unchanged worktree the
moment its agent stops, and a clean tree can be reclaimed between turns;
`SendMessage` then resumes the agent in the main checkout, where it cannot
commit (ADR-0061, #5649). Run `git worktree list` and look for that agent's
tree before re-engaging. Tree present — `SendMessage` it as above. Tree gone —
do not resume: re-dispatch fresh with `isolation: "worktree"` and restate the
base commit and branch in the brief; never `SendMessage` into the main checkout.
`tm hook --pm-guard` refuses that `SendMessage` and names the missing tree.

**Cross-check `state` before calling anything green.** Treat `bucket` as
advisory: under GitHub API eventual-consistency lag it can report a false DONE
while a check has not settled.

**A handed-over branch may not be green.** The agent's own gate can pass while
CI fails — a file the engineer never touched can already carry `cargo fmt`
drift, so `cargo fmt --check` is red before re-engagement even starts
(trusty-things#287). State that explicitly in the re-engagement brief: run the
full gate first, since the branch may not be green as handed over. Do not
assume the hand-over inherited a clean baseline.

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

## The Mandatory Closing Instruction on an Engineer Delegation

Moved out of the instruction package by #7423. Every engineer delegation ends
with this text, verbatim:

> Before returning: run linters/formatters, fix any issues, run tests, verify all
> pass. Verify ALL deliverables from the prompt are present (README, config,
> etc.). Report gate summary lines per the gate-output rule (BASE-AGENT, "Gate
> Output: Quote Results, Summarize Progress") — never a full raw log. Plus this
> project's own doc gates, if it defines any — its CLAUDE.md names them and
> `scripts/` holds them; name the ones you ran. A project that defines none owes
> no such run.

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

## Related Skills

- `tm-circuit-breaker` — the enforcement layer behind these patterns
- `tm-workflow` — the phase/gate model these chains slot into
- `tm-verification-protocols`, `tm-git-file-tracking` — the evidence and
  tracking steps every chain in `references/workflow-chains.md` assumes
