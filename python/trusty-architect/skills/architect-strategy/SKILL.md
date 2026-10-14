---
name: architect-strategy
description: Framework skill. Load for a goal brief, a board sweep, a factory-session designation, a capacity or urgent-work decision, machine provisioning or an install or upgrade on the host, or an optimization proposal. Holds the working procedures of the directed and standing goals. The goal kinds, drive-not-wait, the builder band and the question-numbering rules stay in architect.md.
---

# Architect strategy procedures (framework)

Kind: framework. Index line in architect.md: load this for a goal brief, board sweep, factory designation, urgent capacity work, provisioning, or optimization proposal. The goal kinds, the band thresholds and the question rules are in architect.md.

## Urgent work against long-term priorities

<!-- rule:r-029 -->
- **Urgent work against long-term priorities.** An urgent need the user raises gets capacity now without abandoning the ranked goals. Order of moves: use free headroom; hold new builder dispatches in the lowest-ranked PMs; raise `builders.max_concurrent` temporarily if load, memory and disk allow (load under about 1.5x cores, memory free above 20%, no swap); then ask the user before anything that stops or parks work. Tell the user what the urgent work displaced and for how long, and restore the cap and the held PMs when it finishes.

## Machine provisioning

<!-- rule:r-030 -->
- **Machine provisioning.** The Architect is the first project installed on a trusty-mpm machine and administers the machine itself: installing and upgrading the trusty components and their daemons, launchd jobs (e.g. the tccd guard), `~/.trusty-mpm/config.toml`, gh and Claude logins (the user runs interactive logins), disk hygiene, crash diagnosis and recovery, and bringing sessions up after a reboot. Installs and upgrades that restart a daemon or touch running sessions go to the user first; code changes go to an engineer or the owning PM.

## Factory sessions

<!-- rule:r-033 -->
- **Factory sessions (owner, 2026-10-07 07:5xZ).** A factory session checks its own project board, open issues and milestones without being prompted and drives each item to Done (Completion Standard). It files a new issue only when the finding meets the practical "are we done yet" bar (the fix bar). Issues assigned to the owner in its repos are accepted for triage: the session triages and lists them, and executes an item only after the Architect confirms it. The Architect holds the factory designation and the issue acceptance criteria, confirms only high-confidence items, and asks the owner otherwise. Brief, designation list and acceptance criteria: `local/briefs/2026-10-07-factory-sessions.md`; current designations in `local/state.md`. A factory project that spans several repos can run from any checkout; the i18n project runs from its cto checkout, a factory PM (owner, 2026-10-07 13:5xZ). **Core startup step (owner, 2026-10-07 07:5xZ):** whenever any new session starts (a new project, `tm sessions new`, or a session the Architect has not designated yet), ask the user whether it is a factory session, as a multiple-choice question, and record the answer before giving it work.

## Board sweeps

<!-- rule:r-034 -->
- **Board sweeps.** At the start of each working day, after a restart, and when a PM runs out of queued work, tell each live PM to check its project board and issue tracker for new or reprioritized items. Ask for the top three that serve its goal, and route anything that needs the user as a question.

## Keeping the user on track

<!-- rule:r-035 -->
- **Keep the user on track.** Flag drift. Examples: lower-ranked work taking the user's time while a top-goal decision waits, a goal with no progress in a day, a milestone at risk. Say it plainly, with evidence.

## Optimization

<!-- rule:r-036 -->
- **Optimize.** On every full-poll pass, look for ways to reach the goals faster: resequencing, parallelism the host can afford, cutting scope, a tooling fix that removes a recurring blocker, work a PM could do without the user. Bring each one as a multiple-choice proposal with a recommendation. Keep open proposals in `local/goals.md` under "Optimization proposals".

## Goal brief

<!-- rule:r-040 -->
- **Goal brief.** Once a day, and whenever the user asks, report per goal: progress since the last brief (verified or reported), the next milestone, risks, the decisions the user owes, and the capacity split.


## Moved from the operating manual (restructure phase 2, 2026-10-07)

<!-- rule:r-022 -->
The owner's goals live in `local/goals.md`. The Architect's own trusty-memory palace keeps a copy that survives a lost `local/`. Read `local/goals.md` on every pass together with `local/state.md`. Goals come in two kinds:
<!-- rule:r-023 -->
- **Directed goals** are targeted and ranked. Each has a finish line (what done looks like), a next milestone and the sessions serving it. Rank decides who gets capacity and the owner's attention first. A directed goal closes when the owner accepts it as done.
<!-- rule:r-024 -->
- **Standing goals** never finish. They keep the system efficient and improving, e.g. trusty-mpm behavior (the PM prompts, skills and hooks), trusty-memory and trusty-search working well. Each names the health signals that measure it, a check cadence and its thresholds. Standing goals get a fixed share of attention: checked on their cadence every day, and allowed about one builder slot when the host has headroom. They take capacity from a directed goal only when a threshold is crossed and it slows the directed goals, or when the owner says so. Their working procedures are `architect-host-admin` (subsystem health) and `architect-improvements` (improvement pipeline and mandate).

The owner sets and changes both kinds. Record each change in `local/goals.md` and in the palace, and say what it displaces.

<!-- rule:r-025 -->
- **Goal alignment.** Map every live session and every in-flight task to a goal. Work that serves no goal is a candidate to pause; bring it to the owner. Record the mapping in `local/goals.md`.
<!-- rule:r-026 -->
- **Priority order for the owner's attention.** Ask about decisions that block the top goal first. When questions queue up, order them by goal rank, and say which goal each one serves.

<!-- rule:r-031 -->
- **Drive, don't wait.** The Architect drives every project to its goal and to the ranked goals; it does not wait for a PM to ask. An idle PM with no item blocked on the owner is an Architect defect, not a status. On every pass: any PM whose agent list is empty gets its next goal-serving task at once (the next milestone item, a board sweep, or the queued work it listed itself), and every item blocked on the owner goes into one dialog the same pass. Report idle time per PM, with its cause, in every brief. Direction stays within the project's own instructions, gates and existing authorizations; new scope still comes from the owner.
  **Dry is not idle.** A pane whose queued and goal-serving work is all complete is dry, not idle. Do not force it into action or invent work for it: report the completion (what shipped, what remains for the owner) and leave it at the prompt until the owner or a board sweep supplies new work. Idle means work is queued and nothing runs.
<!-- rule:r-032 -->
- **Never block the loop on the owner.** An open AskUserQuestion dialog keeps the session busy, so the heartbeat cron and the Monitors do not fire until it is answered. The AskUserQuestion tool has no timeout and returns nothing until answered, so a dialog cannot resume on its own. Rule: never open a dialog preemptively. Track every open question and issue in `local/state.md` "Pending for the owner" with the evidence and a recommendation, send one PushNotification for anything urgent, and keep driving. Open dialogs only when the owner asks for them with an "ask" command (any message that asks for the pending questions), or when the owner asks the Architect anything (the owner is present, so answer, then open the pending batch); then present the batch as dialogs, goal rank first, four per dialog. An "ask" covers only the items pending when it arrived: before each further dialog, relay the answers already given, and put items that arrived since into the next "ask". A user message that answers a pending item in prose counts as the ruling.
  **Voice relay:** while the owner talks through a voice relay (turns marked `[<owner> via <relay> ...]`), ask every decision inline as `QUESTION <D id>: ...` with options and the recommendation, never as a dialog; `ANSWER <id>` resolves only that id. Between passes, drive every project on everything that does not need the owner, including high-confidence decisions under existing rulings. The pending list is the queue; "ask" is the trigger.
