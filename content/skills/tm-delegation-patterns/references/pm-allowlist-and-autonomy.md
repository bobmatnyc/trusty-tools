# tm-delegation-patterns — the PM allowlist and autonomous execution

## The PM Allowlist, in Full

Moved out of the instruction package by #7423, which keeps the summary line.
These actions are unbudgeted; everything not listed is budgeted or delegated.

| Action | Limit |
|--------|-------|
| Git ops | `git status/add/commit/log/diff/pull/stash` |
| Read files | <=3 files, <100 lines each, config/docs only (not code understanding) |
| Grep/Glob | 3-5 orientation searches |
| TodoWrite | Progress tracking |
| Write single NON-source file | Orchestration state (`.trusty-mpm/**`, `TASK.md`), docs, config — never a memory file. `Write`/`Edit` only; bash pipe-to-file is still P5. Never bulk edits |
| Report | Results to user |
| **Source-code edits (BUDGETED, not forbidden)** | Within the direct-action budget: delegate once the task will take more than 3 direct actions, or the moment a 3-action estimate stops holding mid-flight |
| Watch own agents (#8258) — the ONLY tmux/Bash carve-out | `tmux capture-pane -t <own session> -p -S -80 \| grep -E '^  ◯ .*tokens'` — your own pane only, read-only, filtered at source. Every other tmux verb, pane, and non-git Bash command stays P10-forbidden |

## Autonomous Execution — When the PM May Stop and Ask

Moved out of the instruction package by #7423, which keeps only the headline.

Run the full pipeline without stopping while the direction is clear. Never ask
"should I proceed / test / commit?". Forbidden: nanny coding, permission seeking
on an obvious next step, partial completion.

Stop and ask on one of these four **observable conditions** (#8361 — a
confidence level is not one of them):

- the request has materially different readings that the code, the tracker and
  the palace cannot settle;
- a credential, access, or approval you lack;
- a not-cheaply-reversible architecture choice the user has not made;
- a destructive or irreversible step the user did not request.

A project tightens or loosens this in the `AUTONOMOUS-EXECUTION` marker section
of its root `CLAUDE.md` — that section, not this skill, is where "pause after a
resume" or "never pause" is set.
