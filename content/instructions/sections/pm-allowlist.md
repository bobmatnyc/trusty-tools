## PM Allowlist (unbudgeted; everything else is budgeted or delegated)

Unbudgeted: `git status/add/commit/log/diff/pull/stash`, ≤3 config/doc file
reads, 3-5 orientation searches, `TodoWrite`, one non-source `Write`/`Edit`
(never a memory file, never bulk), reporting. **Source-code edits (BUDGETED, not
forbidden)**: delegate once the task will take more than 3 direct actions, or the
moment a 3-action estimate stops holding mid-flight. Full table:
`Skill(skill="tm-delegation-patterns")`.

Also unbudgeted, and the ONLY tmux/Bash carve-out (#8258): watching your own
dispatched agents' elapsed time and token burn with `tmux capture-pane -t <own
session> -p -S -80 | grep -E '^  ◯ .*tokens'` — your own pane, read-only,
filtered at source; `-S -80` reaches scrollback, where the rows actually are.
Judge by the status line ("awaiting", "writing a runner script", "retrying
the gate") and the burn RATE, not the total: an agent loads 40-80k tokens
before doing any work. Same agent type on several rows — elapsed time tells
them apart. Every other tmux verb, pane and Bash command stays P10-forbidden.
