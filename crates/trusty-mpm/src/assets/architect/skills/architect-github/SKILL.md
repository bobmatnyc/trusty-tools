---
name: architect-github
description: Framework skill. Load before any `gh` call, before answering a merge or PR clearance question, before a hold or merge, when reading review threads, branch protection or required checks, or when a clearance must be rebased. Covers PR reading, required-check APIs, label versus hold, thread paging, pane checks before promote PRs, re-clearance wording, env-dependent merges and bulk-write pacing.
---

# Architect GitHub mechanics (framework)

Kind: framework. Index line in architect.md: load this before any `gh` call or PR clearance, hold or merge.

## PR reading and merge answers

<!-- rule:r-159 -->
**Read the PR before answering a merge question.** Run `gh pr view <n> --json title,files,additions` before any merge answer; the PM description may paraphrase the PR. A change that lands after the release PR cannot be "left for the next release": fold it with the assemble tooling or it blocks the publish.

<!-- rule:r-160 -->
**Check the log for an existing the owner approval before applying a "goes to the owner" rule.** `grep -n <topic> local/actions.md` first (2026-09-28 02:01Z).

<!-- rule:r-165 -->
**Read the pane before clearing a promote PR (2026-10-01 21:14Z).** A clearance request over SendMessage can omit a decision the PM put to the owner in its pane. Before clearing a project promote PR (or any PR carrying private or personnel content), capture the PM pane and check its "Decisions for you" list. a project PR #205 merged with named-staff HR detail that the PM had flagged in pane only.

<!-- rule:r-166 -->
**Re-clearance wording (2026-10-03).** When a cleared PR must be rebased, say "send me the new head and WAIT; merge only after my CLEARED reply names that SHA". "Send me the new head, then merge" was read as permission to merge after sending (a project PR #1162, 05:0xZ).

<!-- rule:r-178 -->
**Merge of main after clearance (rule, 2026-10-06 00:0xZ, from a PM feedback).** A clearance and its credential scan still cover a branch after a clean merge of origin/main when the three-dot file list (`git diff --name-only <main>...<head>`) is unchanged and no file conflicted. Main's content was scanned when it landed. Any changed three-dot file list, any conflict, or any auto-merged overlap in a source file needs a fresh scan of the delta and a re-clear.

<!-- rule:r-078b (split from r-078) -->
**Env-dependent merges (2026-10-06).** Before clearing a PR whose behaviour depends on env vars, list the project's env names and targets and confirm the deploy trigger. After a Vercel rollback, Production's domain stays pinned until a deployment is promoted; later merges do not go live on their own. Before clearing a change to content/agents/BASE-AGENT.md (or any always-loaded agent content), ask for the trusty-mpm resident-budget tests in its gate list and for the version bump of any published crate whose src/ changed.

## Checks, holds and threads

<!-- rule:r-161 -->
**gh pr checks --required omits required checks that never started.** A required context no job produces (e.g. "trusty-audit-ui clippy" after the tga extraction) is absent from the list, which reads as "not required" and leaves auto-merge waiting forever. Read `gh api repos/<o>/<r>/branches/main/protection/required_status_checks` instead (2026-09-28 01:53Z, #8824).

<!-- rule:r-162 -->
**Replace a required-check list with PATCH, not PUT.** `gh api -X PUT .../protection/required_status_checks/contexts --input <json>` returned an empty body and changed nothing (2026-09-28 02:42Z). `jq -n '{strict:false,contexts:[...]}' | gh api -X PATCH repos/<o>/<r>/branches/main/protection/required_status_checks --input -` applied. Save the before-list first and verify with a fresh GET.

<!-- rule:r-163 -->
**A label is not a merge hold.** GitHub auto-merge ignores labels such as `do-not-merge`: PR #8865 merged at 13:55Z on 2026-09-28 while labelled. A hold means `gh pr merge <n> --disable-auto`, then verify `autoMergeRequest` is null. Tell PMs this when relaying any hold.

<!-- rule:r-164 -->
**Review threads page at 100.** `reviewThreads(first:100)` silently drops the rest; a trusty-review round can post 40+ threads. Always page with `after:$endCursor` (`gh api graphql --paginate`) and compare `totalCount` before calling a PR "0 unresolved". Missed on a PR #752 (128 threads, 28 hidden) 2026-09-29.

<!-- rule:r-126 -->
**Bulk gh writes trip GitHub's secondary limit.** A tight loop of GraphQL writes returns "GraphQL: API rate limit already exceeded for user ID ..." while `gh api rate_limit` still shows the hourly budget nearly full (2026-10-03 15:5xZ, roadmap dating, ~800 calls). The limit is per account, so every session on that login fails at once. Test the block with one read, `gh api graphql -f query='{viewer{login}}'`; `rate_limit` does not show it. Brief a bulk write to pace itself (about 1 write per second, back off on the error) and to use REST where an endpoint exists.

<!-- rule:r-127 -->
**GitHub counts:** `gh issue list` returns 30 results unless `--limit` is set, so do not count issues with it. Read counts from the milestones API (`gh api 'repos/<o>/<r>/milestones?state=all&per_page=100' -q '.[] | "\(.title) \(.open_issues)"'`); quote the URL in zsh (2026-09-26: a 30-item list was reported as a milestone's 67 open issues).
