# D61 / D19 trusty-tools history rewrite: freeze and rewrite plan

Date 2026-11-03 09:50Z. Read-only research; nothing was run against origin. Plan owner and freeze coordinator: tm-dogfood. Executor: tm-trusty-issues (no reason found to move it). Execution only on the Architect's GO.
Detail lives in gitignored `.trusty-mpm/sessions/df-records/` (d19-runbook.md, d19-plan-20261102-2233.md, d19-scripts/). This file adds the D61 sequencing and the 11-03 re-measure. No sensitive value appears here.

## 1. Purpose and source rulings
- D8d (Bob via Architect, 2026-11-01 16:54-16:57Z; supervisor palace drawer 316bb671): rewrite main before the 1.12.0 cut, replace the blobs, move 127 tags (exception to #6178), send one GitHub Support request. Sequence: merges, then rewrite, then cut.
- D19 (d19-runbook.md): the removed content is an idle-pane test fixture (python/ and crates/ trusty-architect `tests/fixtures`, `corpus.json`, residue in 14 files) from commit 26c066888a (PR #8890). Content class: a spend figure, URLs, a private issue reference, account/owner identifiers. The runbook records zero credential-shaped strings and zero emails. Its Support text says "no credentials or secrets". Also bundled: De8 (38 commit SHAs, one text hit in a fixture) and Ded (564 commits, 53 refs/tm, already deleted).
- D61 (Bob, 2026-11-03 ~01:22Z; trusty-tools and supervisor drawer b658f104): check matt-mini for a clone, then freeze main and run the rewrite. Step 1 done 09:43Z (no clone found, with the depth-8 caveat).
- So the rewrite is NOT a key-leak purge. df-A04 (key rotation) is a separate owner action; I found no source tying it to this rewrite. D1d (aipr Neon password, purge only) concerns another repo. Not established: whether the rewrite purpose was widened after 11-02 22:40Z.

## 2. Scope
Ranged rewrite, not full history (full history re-signs 4991 of 5208 commits; the range changes ~1136). Range `26c066888a^..` over every head and tag that contains 26c066888a (11-02 22:33Z: 34 heads + 127 tags = 161 refs; recount at F1, branches keep appearing).
Invocation: built by `d19-scripts/gen_filter.py <refs-to-rewrite.txt> d19-scripts/blob_cb_d19.py` into `filter-cmd.sh`, which runs `git filter-repo --partial --refs <range/refs> --blob-callback ... --replace-text df-records/d19-residue-replace-rule.txt --message-callback ...` (rewrite names must be taken from gen_filter.py, not retyped).
- blob callback: 14 old fixture/corpus blob ids map to the existing synthetic blob (HEAD tree stays identical, paths kept).
- replace-text file (64 lines, gitignored): whole old lines and token runs mapped to synthetic text, plus 2 literal rules for one De8 commit id. Contains old strings, so it never leaves df-records.
- message callback: strips that De8 id from one commit message (#10610).
- Expected: main tree differs from old in exactly 1 path (the De8 turn_active fixture, approved exception 16:45Z).
- Not rewritable: refs/pull/* (about 5066; 1072 hold 26c), 37 refs/backup/* (none hold 26c).

## 3. Freeze (tm-dogfood coordinates; Architect sends the messages)
Base moved: origin/main is now bac374040d (2 commits after the measured bcbea788ed: c0f0fbdfcf #10729, bac374040d). Dry run and leases must be redone on this base.
- Stop pushes and merges: every session with a clone of this repo. Known workstreams from labels/session dirs: tm-dogfood, tm-trusty-issues, tm-trusty-tui (#10720, #10730), tm-mpm-utils-01 (EVO, 584 rsync'd trees), plus tm-memory, tm-review, tm-search, tm-secrets if live. Could not establish the live set (no `tm session ls` access here); the Architect must confirm.
- Merge queue: confirm no auto-merge (`gh api pulls --jq select(.auto_merge!=null)` prints nothing); 24 open PRs, all but #10557, #10445, #10719 carry `do-not-merge`.
- Technical lock: install the d19 pre-push ancestry gate at F0 (d19-gate.sh; tm's push-guard reinstall silently drops it; do not run `tm repair push-guard` until reopen). Bob lifts main force-push protection only at the push step and restores it after the scan (full PUT body, runbook s7). No branch lock is needed with 1 h freeze; the freeze is by message plus gate.
- Open-PR handling: no PR is closed. PR branches on origin are rewritten in place. Close #10630 only if it still exists (not in the open list now; check). Held PRs stay held.
- Duration about 1 h plus 28-40 min CI on the new main.

## 4. Backup
- `/Users/masa/d19-run/mirror.git` (git clone --mirror, `remote remove origin`), before/after ref lists.
- `/Users/masa/d19-private/d19-pre-rewrite.bundle` (`bundle create --all`, verify, chmod 600, sha256), `restore-test.git` cloned from the bundle and fsck'd.
- Old main tag: local-only record of OLD_MAIN SHA and tree in `refs-before-sha.txt`; do NOT push an `archive/` tag (it would keep the old objects reachable).
- Retention: until GitHub Support confirms the purge (runbook F12), then PM/operator deletes with `rm -P` for the bundle. Neither /Users/masa/d19-run nor d19-private exists today; both are created at F1.

## 5. Execution (executor tm-trusty-issues, Architect GO per step)
1. F0: freeze message; install gate; confirm no auto-merge; `gh pr list` inventory.
2. F1: mirror, bundle, restore test, refs-to-rewrite.txt, leases; recompute Support list C. Abort if mirror main != OLD_MAIN.
3. Dry run on a copy (`work.git`): run filter-cmd.sh, gen_tagmap.py. Require: exit 0, `--contains 26c066888a` empty, 1 differing path, tag-map 127 lines, commit-map about 1140 rows.
4. Run `scan_post-20261102-1636.py ... --paths --expect-diff-paths 1`: require exit 0 RESULT PASS; negative control on the unrewritten mirror must exit 1.
5. Bob lifts main force-push protection.
6. Push (Architect GO): `gen_push.py` output, one atomic push, `--force-with-lease=<ref>:<old>` for all refs, heads then 127 tags then main, no `--mirror`/`--all`/refs/pull, no `--no-verify`. Pusher must be Admin (ruleset bypass role 5); fallback: disable rulesets 26091257 and 26091265 temporarily.
7. Fresh `clone --mirror` of origin, rerun the scan; restore protection and rulesets (expect `allow_force_pushes` false, both rulesets active).
8. CI on new main: one-shot `gh api actions/runs?head_sha=`; six required checks green.
9. Support request (Bob's account, one ticket, runbook s9 text): list C (about 4,744 commits), 17 blob ids, De8 38, Ded 564, 23 old-head commits, refs/pull cached views, 8 public forks. Caveat: refs/pull/* cannot be pushed to; only Support can drop them, so old SHAs stay fetchable by SHA until they act. D92 precedent: the joanies request judged disabling enough; here a purge is the ruling.
10. Reopen message; keep the gate until `for-each-ref --contains 26c066888a` is empty in every clone.

## 6. Verify
- Scan PASS on fresh clone (14 blobs unreachable from heads/tags, 0 long-line blobs, 127/127 tags mapped, 931 tags total).
- Old vs new main tree: one differing path (De8 fixture). `git log -S` and `git grep` for the De8 id on main print nothing.
- Commit count: old main 5227 (origin/main now); new count must be equal (replacement, no deletion), only SHAs change.
- gitleaks/trufflehog not used by the existing scripts; optional extra pass, but the content class is not credential-shaped, so a scanner will not prove it. Run only if the Architect wants it.
- CI green on new main; 6 required checks.
- Known residue: crates.io sources (trusty-architect 0.1.0/0.2.0, trusty-mpm 1.7.10 to 1.11.1) contain one 9-char path-like token each; not rewritable; sensitivity needs one human read (D04), undecided.

## 7. Downstream
Measured now: this checkout `.git` 23 GB (pack 22.79 GiB), 213 worktrees, 663 local heads containing 26c066888a, 950 tags, 5 local commits on `main` ahead of origin/main (docs; need rebase onto new main before any push). Cheapest: new clone, then `git rebase --onto origin/main <old-base> <branch>` per live branch, discard the old clone. Old base = `git merge-base <branch> <OLD_MAIN>` taken before fetch; if the branch was pushed, use `git reset --hard origin/<branch>` after `git branch SAVE`.
Order:
1. Held PRs already on origin (rewritten in place): fetch only, then rebase onto the new main after Architect clearance: #10424 before #10430; #10713, #10714, #10716, #10717 (dnm, tm-dogfood); #10715 release/1.12.0-prep (rewritten with its heads; keep held); #10720, #10730 (tm-trusty-tui); #10689, #10693, #10694 (tm-trusty-issues); the rest per runbook s2.
2. Local-only (not on origin, all contain 26c): #6433 stack in the order the boundary file gives: fix/6433-agents-no-http (6 ahead), fix/6433-agents-events (7), fix/6433-consumers-uds (9), fix/6433-ui-uds (13), linearize 1 -> 2 -> 3 -> 4 and drop the duplicate cherry-pick; feat/6433-agents-api-uds (1 ahead, does NOT contain 26c; just rebase). Then re-run rung-6 gates once on the tip and a credential scan, then push.
3. #10696 (fix/10696-dup-origin-registry, 1 ahead, local only): rebase --onto, push, open PR.
4. Cleanup after: fast-forward main checkout; PM removes worktrees df-g01-10705 and df-g01-10705-allowlist.
5. EVO: tm-mpm-utils-01 prunes the 584 trees by the documented one-key prune; never rm.
Release tags: 127 move once (#6178 exception); crates.io unaffected; no tag is cut before the rewrite.

## 8. Rollback
Valid until Support purges: `gen_push.py restore-test.git refs-to-rewrite.txt refs-after-sha.txt <url>`, which sends old objects from the bundle clone; atomic leased push, then restore protection. After the purge, only the bundle restores. Any stale-info rejection or partial write: stop, refreeze, redo from F1.

## 9. Open questions
1. D: Is the rewrite scope still D8d/D19 only (fixture, De8, Ded)? Recommend yes. If keys from df-A04 are in repo history, say so; I found no evidence.
2. A: Executor tm-trusty-issues per the Architect; recommend keep. Concrete risk to name: it also owns held PRs #10689/#10693/#10694, so tm-dogfood should review the push command before GO.
3. A: Re-run the dry run on bac374040d and refresh leases (161 refs and 1136 map rows are stale). Recommend yes, at F1.
4. D (Bob): Was the 09:43Z mini check enough (817 dirs hit the depth cap, Library/Documents/Desktop excluded)? Recommend yes; the runbook says mini installs from crates.io.
5. D (Bob): Admin force-push on main: toggle `allow_force_pushes` (default) or test admin bypass? Recommend toggle.
6. D (Bob): Human read of the 9-char residue token and yank decision for 12 published crate versions. Recommend read once, then decide.
7. A: 8 public forks cannot be cleaned by us; accept, rely on Support. Recommend accept.
8. A: Live set of sessions and hosts holding a clone (I could not enumerate). Architect to confirm before the freeze message.
