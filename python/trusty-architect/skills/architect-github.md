---
name: architect-github
description: Architect skill. Load before any `gh` call, before answering a merge or PR clearance question, before a hold or merge, when reading review threads, branch protection or required checks, or when a clearance must be rebased. Covers PR reading, required-check APIs, label versus hold, thread paging, pane checks before a sensitive merge, re-clearance wording, env-dependent merges and bulk-write pacing. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, github, pr, merge]
effort: medium
---

# architect-github — GitHub Mechanics

Load this before any `gh` call or PR clearance, hold or merge. Account and
org facts for this host live in `local/instance.md` and the owner's
`AGENTS.md`.

## PR reading and merge answers

<!-- rule:r-159 -->
**Read the PR before answering a merge question.** Run
`gh pr view <n> --json title,files,additions` before any merge answer; the
PM's description may paraphrase the PR. A change that lands after the
release PR cannot be "left for the next release": fold it with the project's
release tooling or it blocks the publish.

<!-- rule:r-160 -->
**Check the log for an existing owner approval** before applying a "goes to
the owner" rule: `grep -n <topic> local/actions.md` first.

<!-- rule:r-165 -->
**Read the pane before clearing a sensitive PR.** A clearance request over
SendMessage can omit a decision the PM put to the owner in its pane only.
Before clearing a promote PR, or any PR that carries private or personnel
content, capture the PM pane and check its "Decisions for you" list. A PR has
merged with personnel detail that the PM had flagged only in its pane.

<!-- rule:r-166 -->
**Re-clearance wording.** When a cleared PR must be rebased, say "send me the
new head and WAIT; merge only after my CLEARED reply names that SHA". "Send
me the new head, then merge" has been read as permission to merge after
sending.

<!-- rule:r-178 -->
**A merge of main after clearance.** A clearance and its credential scan
still cover a branch after a clean merge of the base branch when the
three-dot file list (`git diff --name-only <main>...<head>`) is unchanged and
no file conflicted: the base's content was scanned when it landed. A changed
three-dot list, any conflict, or any auto-merged overlap in a source file
needs a fresh scan of the delta and a re-clear.

<!-- rule:r-078b -->
**Env-dependent merges.** Before clearing a PR whose behaviour depends on
environment variables, list the project's variable names and targets and
confirm the deploy trigger. After a hosting rollback, production can stay
pinned to the rolled-back deployment until one is promoted; later merges do
not go live on their own. Before clearing a change to always-loaded agent
content, ask for the resident-budget tests in its gate list and for the
version bump of any published crate whose `src/` changed.

## Checks, holds and threads

<!-- rule:r-161 -->
**`gh pr checks --required` omits required checks that never started.** A
required context that no job produces is absent from the list, which reads
as "not required" and leaves auto-merge waiting forever. Read
`gh api repos/<o>/<r>/branches/main/protection/required_status_checks`
instead.

<!-- rule:r-162 -->
**Replace a required-check list with PATCH, not PUT.** A PUT to
`.../protection/required_status_checks/contexts` has returned an empty body
and changed nothing.
`jq -n '{strict:false,contexts:[...]}' | gh api -X PATCH repos/<o>/<r>/branches/main/protection/required_status_checks --input -`
applies. Save the before-list first and verify with a fresh GET.

<!-- rule:r-163 -->
**A label is not a merge hold.** GitHub auto-merge ignores labels such as
`do-not-merge`; a labelled PR has merged. A hold means
`gh pr merge <n> --disable-auto`, then verify that `autoMergeRequest` is
null. Tell PMs this when relaying any hold.

<!-- rule:r-164 -->
**Review threads page at 100.** `reviewThreads(first:100)` silently drops
the rest, and one review round can post dozens of threads. Always page with
`after:$endCursor` (`gh api graphql --paginate`) and compare `totalCount`
before calling a PR "0 unresolved".

<!-- rule:r-126 -->
**Bulk gh writes trip GitHub's secondary limit.** A tight loop of GraphQL
writes returns "API rate limit already exceeded" while `gh api rate_limit`
still shows the hourly budget nearly full. The limit is per account, so every
session on that login fails at once. Test the block with one read,
`gh api graphql -f query='{viewer{login}}'`; `rate_limit` does not show it.
Brief a bulk write to pace itself (about one write per second, back off on
the error) and to use REST where an endpoint exists.

<!-- rule:r-127 -->
**GitHub counts.** `gh issue list` returns 30 results unless `--limit` is
set, so do not count issues with it. Read counts from the milestones API
(`gh api 'repos/<o>/<r>/milestones?state=all&per_page=100' -q '.[] | "\(.title) \(.open_issues)"'`);
quote the URL in zsh.

## Related skills

- `architect-completion-standard`: when a PR is Done and what blocks it.
