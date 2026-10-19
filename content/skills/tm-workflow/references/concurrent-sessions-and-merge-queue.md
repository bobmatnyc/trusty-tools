# tm-workflow — concurrent sessions and the merge queue

## Concurrent Sessions on One Repo

Another tm session may be working this repo right now. Read what it is doing
yourself — never ask the user to relay it.

1. **List, then read, before starting work that could collide.**
   `mcp__trusty-mpm__session_list` returns every managed session with `id`,
   `name`, `cwd`, `state`, and `source_id`. Match on `source_id` /
   `workspace_path`, and exclude your own id.
   `mcp__trusty-mpm__session_activity` then returns that session's raw tmux pane
   content plus `runtime_active`, `pending_decision`, and `state` — its merged
   commits, its running agent and elapsed time, its queued work, and its open
   questions, without asking it anything.
2. **Claim the work on the issue before you start.** Assign yourself, or comment
   the claim. A claim on the tracker survives a relaunch and both sessions
   already read it; a pane message survives neither.
3. **Verify every `session_send`.** It types characters into a pane; it does not
   deliver a message. A long single-line message landed as a bracketed paste
   (`[Pasted text #1][Pasted text #2]`) and the submit never fired — the text sat
   unsent in the target's prompt buffer and the receiving session never saw it.
   After each send, call `session_activity` and confirm the message was submitted
   rather than left in the buffer.
4. **Re-read rather than wait for a reply.** Call `session_activity` again. The
   other session may be mid-agent-run for many minutes and owes you no answer.

`session_proxy_focus` / `session_proxy_message` / `session_proxy_summary` address
a session by a stored focus key instead of an id. `session_proxy_message` is the
same pane injection as `session_send` and carries the same defect.
`session_proxy_summary` gives a lifecycle digest without the raw pane.

## Merge-Queue Ownership — the Procedure

The rule is in the instruction package's workflow section: one session owns a
repository's merge queue, an owner's authorization is scoped to the PRs it named,
and green required contexts alone never license a merge. This is how to run it.

**Establish who owns the queue before merging anything.** Ownership is a claim in
git/GitHub state, not an understanding between sessions. Read it from the open
PRs: `gh pr list --json number,author,assignees,isDraft,labels,headRefName`. If
another session's PRs are open on the same base branch and you were not told you
own the queue, you do not — hand your merge-ready PR to that session and report
that you routed it rather than merging.

**Per-PR pre-merge check, in this order.** Required contexts are the last gate,
not the first:

```bash
gh pr view <PR> --json isDraft,labels,assignees,reviewDecision,statusCheckRollup
gh pr view <PR> --json reviews --jq '.reviews[-3:][] | {author:.author.login,state}'
```

Stop on any of: `isDraft: true`; a hold label (`do-not-merge`, or whatever the
project uses); `reviewDecision: CHANGES_REQUESTED`; an unresolved `code-critic`
BLOCK in the PR comments. A critic verdict is a PR comment, so
`statusCheckRollup` cannot see it — read the comments
(`gh pr view <PR> --comments`) before a batch merge, once per PR, not once per
batch.

**Holding someone else's PR.** Convert it to draft (`gh pr ready --undo <PR>`) or
apply the hold label. Send the message too, for awareness — but the message is
never the hold. It loses the race; tonight's incident had a block arrive four
minutes after the merge.

**Handing the queue off.** Say which repository, which base branch, and which PRs
are in flight. The receiving session re-reads state rather than trusting the
list — the handoff is a notification, and the PRs are the record.

On a repository with `tm pr open`, `tm pr queue-check [--base main] [<pr>]` runs
this whole table itself — draft, hold label, `CHANGES_REQUESTED`, an unresolved
`code-critic` BLOCK, then the live required contexts, in that order — and exits
nonzero on the first stop condition it finds.
