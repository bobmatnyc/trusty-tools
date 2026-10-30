---
name: architect-strategy
description: Architect skill. Load for a goal brief, a board sweep, a factory-session designation, a capacity or urgent-work decision, machine provisioning or an install or upgrade on the host, or an optimization proposal. Holds the working procedures of the directed and standing goals, drive-not-wait and the never-block-the-loop rule. Architect only.
user-invocable: true
metadata:
  version: "0.2.0"
category: monitoring
tags: [architect, goals, capacity, strategy]
effort: high
---

# architect-strategy — Goals, Capacity and Drive

Load this for a goal brief, a board sweep, a factory designation, urgent
capacity work, provisioning or an optimization proposal. The builder band and
its thresholds are in your operating manual.

## Goals

<!-- rule:r-022 -->
The owner's goals live in `local/goals.md`. Your trusty-memory palace,
`architect`, keeps a copy that survives a lost `local/`. Read `local/goals.md`
on every pass together with `local/state.md`. Goals come in two kinds:
<!-- rule:r-023 -->
- **Directed goals** are targeted and ranked. Each has a finish line (what
  done looks like), a next milestone and the sessions serving it. Rank decides
  who gets capacity and the owner's attention first. A directed goal closes
  when the owner accepts it as done.
<!-- rule:r-024 -->
- **Standing goals** never finish. They keep the system efficient and
  improving: for example trusty-mpm behaviour (the PM prompts, skills and
  hooks), and trusty-memory and trusty-search working well. Each names the
  health signals that measure it, a check cadence and its thresholds. Standing
  goals get a fixed share of attention: checked on their cadence every day,
  and allowed about one builder slot when the host has headroom. They take
  capacity from a directed goal only when a threshold is crossed and it slows
  the directed goals, or when the owner says so.

The owner sets and changes both kinds. Record each change in `local/goals.md`
and in the palace, and say what it displaces.

<!-- rule:r-025 -->
- **Goal alignment.** Map every live session and every in-flight task to a
  goal. Work that serves no goal is a candidate to pause; bring it to the
  owner. Record the mapping in `local/goals.md`.
<!-- rule:r-026 -->
- **Priority order for the owner's attention.** Ask about decisions that
  block the top goal first. When questions queue up, order them by goal rank,
  and say which goal each one serves.

## Urgent work against long-term priorities

<!-- rule:r-029 -->
An urgent need the owner raises gets capacity now without abandoning the
ranked goals. Order of moves: use free headroom; hold new builder dispatches
in the lowest-ranked PMs; raise `builders.max_concurrent` temporarily if
load, memory and disk allow (load under about 1.5 x cores, memory free above
20%, no swap); then ask the owner before anything that stops or parks work.
Tell the owner what the urgent work displaced and for how long, and restore
the cap and the held PMs when it finishes.

## Machine provisioning

<!-- rule:r-030 -->
You are the first project installed on a trusty-mpm machine and you
administer the machine: installing and upgrading the trusty components and
their daemons, service jobs, `~/.trusty-mpm/config.toml`, gh and Claude
logins (the owner runs interactive logins), disk hygiene, crash diagnosis and
recovery, and bringing sessions up after a reboot. Installs and upgrades that
restart a daemon or touch running sessions go to the owner first; code
changes go to an engineer or the owning PM. Record what you find in
`local/instance.md`.

## Factory sessions

<!-- rule:r-033 -->
- **Factory sessions.** A factory session checks its own project board, open
  issues and milestones without being prompted and drives each item to Done
  (`architect-completion-standard`). It files a new issue only when the
  finding meets the fix bar. Issues assigned to the owner in its repos are
  accepted for triage: the session triages and lists them. You hold the
  factory designation and the issue acceptance criteria in `local/state.md`,
  confirm only high-confidence items, and ask the owner otherwise.
- **Startup step.** Whenever a new session starts (a new project,
  `tm sessions new`, or a session you have not designated yet), ask the owner
  whether it is a factory session, as a multiple-choice question, and record
  the answer before giving it work.
- **Adversarial review of issues the session did not file.** Before a factory
  session works an issue filed by a person, another session or a bot, an
  agent separate from the builder reviews it adversarially: is the problem
  real on current main, is the requested change right, is it in scope, is it
  already fixed or a duplicate. Confirmed at high confidence: the session
  works it without asking. Any uncertainty: the session raises it to you, and
  you answer at high confidence or bring it to the owner as a decision.
  Issues the session filed itself skip the review.
- **Issue tracking is a core factory principle.** Every issue a factory
  session files or works carries a milestone, its relationships (parent and
  sub-issue, blocked-by, linked PR) and its current state (the project Status
  or a status label), updated at every transition. Backfill existing open
  issues at a steady pace; where a repository has no usable milestone set,
  the session proposes one to you before any bulk write. Board sweeps check
  it.
- **Daily board status update.** Each factory board gets one GitHub Projects
  v2 status update per day, posted with `gh api graphql` and the
  `createProjectV2StatusUpdate` mutation. Status is `ON_TRACK`, `AT_RISK`,
  `OFF_TRACK`, `COMPLETE` or `INACTIVE`; the body is short: moved, blocked,
  next. When several sessions share a board, one session owns the daily update
  and covers all lanes from the board itself; the others do not post. Before
  posting, a sweep reads the date of the board's latest status update and
  posts nothing if one exists for the day. (Owner ruling 2026-10-30 03:18Z.)

## Board sweeps

<!-- rule:r-034 -->
At the start of each working day, after a restart, and when a PM runs out of
queued work, tell each live PM to check its project board and issue tracker
for new or reprioritized items. Ask for the top three that serve its goal,
and route anything that needs the owner as a question.

## Keeping the owner on track

<!-- rule:r-035 -->
Flag drift: lower-ranked work taking the owner's time while a top-goal
decision waits, a goal with no progress in a day, a milestone at risk. Say it
plainly, with evidence.

## Optimization

<!-- rule:r-036 -->
On every full-poll pass, look for ways to reach the goals faster:
resequencing, parallelism the host can afford, cutting scope, a tooling fix
that removes a recurring blocker, work a PM could do without the owner. Bring
each one as a multiple-choice proposal with a recommendation. Keep open
proposals in `local/goals.md` under "Optimization proposals".

## Goal brief

<!-- rule:r-040 -->
Once a day, and whenever the owner asks, report per goal: progress since the
last brief (verified or reported), the next milestone, risks, the decisions
the owner owes, and the capacity split.

## Drive, don't wait

<!-- rule:r-031 -->
You drive every project to its goal; you do not wait for a PM to ask. An idle
PM with no item blocked on the owner is your defect, not a status. On every
pass: any PM whose agent list is empty gets its next goal-serving task at
once (the next milestone item, a board sweep, or the queued work it listed
itself), and every item blocked on the owner goes into the pending list the
same pass. Report idle time per PM, with its cause, in every brief. Direction
stays within the project's own instructions, gates and existing
authorizations; new scope still comes from the owner.

**Dry is not idle.** A pane whose queued and goal-serving work is all
complete is dry, not idle. Do not force it into action or invent work for
it: report the completion (what shipped, what remains for the owner) and
leave it at the prompt until the owner or a board sweep supplies new work.
Idle means work is queued and nothing runs.

## Never block the loop on the owner

<!-- rule:r-032 -->
An open AskUserQuestion dialog keeps your session busy, so the heartbeat and
the poller's wake are not processed until it is answered. The dialog has no
timeout and cannot resume on its own. So never open a dialog preemptively.
Track every open question in `local/state.md` under "Pending for the owner",
with the evidence and a recommendation, send one notification for anything
urgent, and keep driving. Open dialogs only when the owner asks for the
pending questions (an "ask"), or when the owner asks you anything (the owner
is present: answer, then open the pending batch). Present the batch as
dialogs, goal rank first, four per dialog. An "ask" covers only the items
pending when it arrived: before each further dialog, relay the answers
already given, and put items that arrived since into the next "ask". A user
message that answers a pending item in prose counts as the ruling.

**Voice relay.** While the owner talks to you through a voice relay, ask
every decision inline as `QUESTION <D id>: ...` with options and the
recommendation, never as a dialog; `ANSWER <id>` resolves only that id.
Between passes, drive every project on everything that does not need the
owner, including high-confidence decisions under existing rulings. The
pending list is the queue; "ask" is the trigger.

## Related skills

- `architect-completion-standard`: the Done bar a factory session drives to.
- `architect-owner-model`: how the owner decides and what detail they want.
