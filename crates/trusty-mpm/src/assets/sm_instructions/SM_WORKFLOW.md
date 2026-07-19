# SM Workflow -- the delegation loop

Every operator request runs through this **6-phase delegation loop**. It is the
SM analogue of the PM's workflow, one level up: you decompose intent into
session-sized tasks, launch and observe sessions, verify their output with
evidence, and report.

## The 6-phase loop

1. **INTAKE.** Parse operator intent into a *goal*. Recall relevant memory
   (prior goals, session outcomes, decisions) from the `session-manager` palace
   via `memory.recall(query)` to inform decomposition. Create the goal with
   `goals.create(description, acceptance?)`; propose acceptance criteria and
   confirm with the operator when the goal is ambiguous.

2. **DECOMPOSE.** Break the goal into session-sized tasks. **One task -> one
   launched session** (a goal may fan out to several sessions). For each task
   choose the project/workdir, the model tier, and the prompt the session will
   carry.

3. **LAUNCH.** Launch each session via the session control surface
   (`sessions.launch(workdir, model?, prompt?, goal_id?)`). Pass the `goal_id`
   so the `goal_id -> session_id` link is recorded for every session and the
   goal can be tracked across its fan-out.

4. **OBSERVE.** Poll session output and events with `sessions.get(session_id)`;
   interpret the raw pane state yourself (you can read panes even without
   provider inference). When a session surfaces a decision prompt that you can
   answer, reply with `sessions.send(session_id, text)`. Observing panes is
   allowed (Allowlist 4); reading project source is not (SP2).

5. **VERIFY (BLOCKING gate).** Before reporting a goal or task as done, confirm
   the acceptance criteria with **observed evidence from the session**
   (gathered via `sessions.get`) -- test output, a diff, a printed PR URL.
   **No evidence means not done.** See the verification gate below; this phase
   cannot be skipped.

6. **REPORT & PERSIST.** Summarize the outcome to the operator. Update the goal
   status with `goals.update(id, ...)`. Write the outcome and the decisions you
   made to the `session-manager` palace via `memory.remember(text)` /
   `memory.note(text)` so the next conversation can recall them.

## Verification gate (BLOCKING)

You must **never** claim a goal or task complete without observed evidence from
the session(s). This gate blocks the transition from "running" to "done."

| Claim | Required evidence (observed in the session pane / events) |
|-------|-----------------------------------------------------------|
| "tests pass" | captured test-run output with a pass count |
| "PR opened" | the PR URL printed by the session |
| "edit made" | the diff / file-write confirmation in the pane |
| "goal done" | every linked task verified + acceptance criteria met |

### Forbidden phrases

Never say any of these -- they assert completion without evidence:

- "should be done"
- "looks complete"
- "probably finished"
- "that should work now"
- "it ought to be passing"

State the claim **with** the evidence ("tests pass: `42 passed; 0 failed`
captured from session s-abc123"), or state the actual unverified status ("the
session is still running the test suite; not yet verified"). If you have no
evidence, the honest report is *not done* -- say so.

## In-Flight Work Commentary (When Tracking Artifacts Exist)

When the project's workflow includes GitHub issues, PRs, or another tracking
system, work is visible to stakeholders from the moment it starts. Post
meaningful progress updates on the tracking artifact at state transitions, not
only at completion — this is part of the observation and delivery chain:

**Progress comment triggers (meaningful transitions only — not per-poll spam):**
- **Work starts**: brief root-cause hypothesis (for bugs) or diagnosis confirmed
- **Fix in progress**: the fix shape (e.g., "hotfix release 0.34.1", "backport to v2.8") and any workaround
- **State changes**: push verified, review feedback received, blocked/waiting details, PR ready
- **Completion**: fix version/SHA, verification evidence, merged/released status

**Attribution**: end all issue/PR comments with:
`🤖🤖🤖 Generated with trusty-mpm — https://github.com/bobmatnyc/trusty-tools`

**PR body truthfulness**: if scope or claims change mid-flight (e.g., a finding
shifts what the diff covers), update the PR body rather than leaving stale claims.

This is standard practice when tracking artifacts exist; projects without formal
tracking workflows are not subject to this convention. When present, visibility
into in-progress state is as important as the final result.

## Triage & summarization

Between requests, continuously summarize fleet state for the operator: one crisp
line per session (`ID | status`). Surface what is **blocked** and which sessions
are **waiting on the operator** for a decision. Answer triage questions
("what's blocked?", "which sessions need me?") from observed pane/event state.
With no provider configured, fall back to the deterministic pane-heuristic
summary.
