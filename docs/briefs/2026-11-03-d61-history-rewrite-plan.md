# D61 / D19 trusty-tools history rewrite: freeze and rewrite plan

Revised 2026-11-03 after the Architect review (09:48Z, basis Bob D61/D04). Nothing was run against origin; all checks here were reads. Plan owner, freeze coordinator and unfreeze owner (recommended, Architect confirms): tm-dogfood. Executor: tm-trusty-issues. Execution only on the Architect's GO. Steps F0-F4 are GO. Step 5 onward (protection lift, push) waits for blockers B1-B4 and the ref-count check in section 4.
Detail lives in gitignored `.trusty-mpm/sessions/df-records/` (d19-runbook.md, d19-plan-20261102-2233.md, d19-scripts/). No sensitive value appears here.

## 1. Purpose and source rulings
- D8d (Bob via Architect, 2026-11-01): rewrite main before the 1.12.0 cut, replace the blobs, move 127 tags (exception to #6178), send one GitHub Support request. Order: merges, rewrite, cut.
- D19: the removed content is an idle-pane test fixture from commit 26c066888a (PR #8890; `tests/fixtures`, `corpus.json`, residue in 14 files): a spend figure, URLs, a private issue reference, account identifiers. The runbook records no credential-shaped strings and no emails. Also bundled: De8 (38 commit SHAs, one text hit) and Ded (564 commits, 53 refs/tm, already deleted).
- D61 (Bob, 2026-11-03 ~01:22Z): check matt-mini for a clone, then freeze main and rewrite. Mini check done 09:43Z, no clone (depth-8 caveat).
- This is not a key-leak purge. Key rotation (df-A04) is a separate owner action.

## 2. Scope
Ranged rewrite `26c066888a^..` over every head and tag containing 26c066888a (11-02 22:33Z: 34 heads + 127 tags = 161 refs; recount at F1). Full history would re-sign 4991 of 5208 commits; the range changes about 1136.
`d19-scripts/gen_filter.py <refs-to-rewrite.txt> blob_cb_d19.py` writes `filter-cmd.sh` (git filter-repo `--partial --refs ... --blob-callback ... --replace-text d19-residue-replace-rule.txt --message-callback ...`). Do not retype the command.
- Blob callback maps 14 old blobs to the existing synthetic blob. The replace-text file (gitignored, 64 lines) holds old strings and never leaves df-records. The message callback strips one De8 id (#10610).
- Expected: main tree differs from old main in exactly 1 path (De8 turn_active fixture, approved 16:45Z).
- Not rewritable: refs/pull/* (about 5066; 1072 hold 26c) and 37 refs/backup/* (none hold 26c).

## 3. Freeze and pre-push blockers
Base moved: origin/main is bac374040d (measured bcbea788ed plus c0f0fbdfcf #10729, bac374040d). Redo F1, dry run and leases on it.
- Merge queue: no auto-merge (`gh pr list --state open --json number,autoMergeRequest --jq '.[]|select(.autoMergeRequest!=null)'` prints nothing). 24 open PRs; all but #10557, #10445, #10719 carry `do-not-merge`. No PR is closed; PR branches are rewritten in place.
- Technical lock: install `d19-scripts/d19-gate.sh` as pre-push at F0. tm's push-guard reinstall drops it, so do not run `tm repair push-guard` until reopen. Recheck with `grep -c` before F4.
- Duration about 1 h plus 28-40 min CI on the new main.

### B4. Live sessions and unfreeze owner
Live at 09:50Z and possibly holding a trusty-tools clone: tm-trusty-issues (executor), tm-trusty-tui, tm-mpm-utils-01 (EVO), tm-trusty-tools (busy; not in the earlier list), tm-trusty-okf (clone unknown), tm-dogfood, tm-architect. Not live: tm-memory, tm-review, tm-search, tm-secrets.
- Acks so far: tm-trusty-okf has no clone (reads the shared checkout read-only). tm-trusty-tools acked; it uses the main clone with worktree `.worktrees/f93609dd-53b5-4b85-ae0b-d8b0cd20ae7b`, holds unpushed branch fix/10706-phase2-c2-h1 (1 ahead, 1ef80e2ea1), and owns open PR #10733 (pm-guard Phase 2). The F1 ref recount must include #10733's head branch (`gh pr view 10733 --json headRefName`).
- Mechanism: the Architect sends the freeze message to each live session, and each replies "frozen, no push/merge, clone path X or none". tm-dogfood keeps the ack table in `df-records/freeze-acks.txt`. A silent session blocks the push.
- Recheck `ListAgents` at the moment of step 5. A session that appeared since gets the message and an ack first. tm-dogfood owns the unfreeze message after the scan passes (step 7), not before. Hosts with a clone: this Mac, EVO; matt-mini none.
- Check: ack table has one line per live session; zero missing.

### B1. Tag and branch pushes must not fire release workflows
Read `.github/workflows/*.yml` (28 files on main). Workflows triggered by `push`:

| Workflow (name) | File | Push trigger | Risk |
|---|---|---|---|
| Binary Release (id 290891315) | release.yml | tags `*-v*`, not `content-v*` | builds binaries, GitHub release, Homebrew tap |
| Pre-publish gate (334763889) | pre-publish.yml | tags `*-v*`, not `content-v*` | publish gate runs and waits on CI |
| SemVer checks (329027095) | semver-checks.yml | tags `*-v*`, not `content-v*` | semver run per tag |
| CI, Line cap, AL2023, Agent assets, Architect sub-project, build.rs sync, Capabilities drift, Doc numbers, Doc paths, Generation artifact lint, Homebrew formula, Public docs, SLD lint, Test count, Test pointers, Token drift, UI bundle freshness, Version parity, vmtest, Website tests | ci.yml and one file each | branches `[main]` only | intended CI on the new main (step 8); fires once on the main ref |

Other triggers are not push: Content Release (dispatch, release), Cargo audit and Docker E2E (schedule, dispatch), Epic to master project (issues), Red main notifier (workflow_run). Non-main head pushes match no push trigger. Rewriting 24 open PR heads fires `pull_request` `synchronize` CI runs: cost only.
Chosen mechanism: disable the three tag workflows for the push window with `gh workflow disable`. It needs no file change on main. A disabled workflow drops the events that arrive while it is off. A repo-variable guard would need a workflow edit on main, so it is rejected. GitHub also documents that a push of more than three tags creates no tag events; that is a second layer, not relied on.
Unestablished: `linker-bench.yml` (331463420) and `zz-gate-semantics-probe.yml` (335138952) are registered workflows but absent on main and on every fetched origin head. Their triggers are unreadable here, so disable them too.
```
for id in 290891315 334763889 329027095 331463420 335138952; do gh workflow disable $id; done
gh workflow list --all | grep -E 'Binary Release|Pre-publish|SemVer|linker|zz-gate'   # all show disabled_manually
```
Run it at step 5, before Bob lifts protection. After the scan passes (step 7) and the tags are verified, re-enable:
```
for id in 290891315 334763889 329027095 331463420 335138952; do gh workflow enable $id; done
gh api repos/bobmatnyc/trusty-tools/actions/workflows --jq '.workflows[]|select(.id as $i|[290891315,334763889,329027095,331463420,335138952]|index($i))|[.name,.state]|@tsv'   # all active
gh run list --event push --created ">=<push-time>" --json workflowName,headBranch --jq '.[]|select(.workflowName|test("Release|Pre-publish|SemVer"))'   # prints nothing
```
The last command must print nothing: no tag-triggered run exists since the push. Re-enable also runs on abort and failure (B3 wrapper).

### B3. Save and restore protection and both rulesets
D3e (Bob): Bob disables the rulesets 26091257 and 26091265 on the GitHub dashboard (Settings, Rules, Rulesets, enforcement Disabled) and lifts main `allow_force_pushes` there. No admin-bypass test and no API toggle by the executor. Bob re-enables both on the dashboard.
Save before any change (files mode 600, in `/Users/masa/d19-private/`):
```
gh api repos/bobmatnyc/trusty-tools/branches/main/protection > /Users/masa/d19-private/main-protection-before.json
gh api repos/bobmatnyc/trusty-tools/rulesets/26091257 > /Users/masa/d19-private/ruleset-26091257-before.json
gh api repos/bobmatnyc/trusty-tools/rulesets/26091265 > /Users/masa/d19-private/ruleset-26091265-before.json
```
Wrapper `/Users/masa/d19-private/run-push-window.sh` (executor writes it at F1; not a repo file): `trap restore_check EXIT INT TERM HUP` is set before the B1 disable. `restore_check` (a) re-enables the five workflows, (b) re-reads the three objects into `*-after.json`, (c) diffs each against `*-before.json` after `jq -S 'del(.. | .updated_at?)'`, (d) on any diff prints `RESTORE NEEDED: <object>` and exits non-zero. A trap cannot click the dashboard, so Bob restores by hand on a diff. Restore by hand: rulesets to Active on the dashboard; protection by the full PUT body in runbook s7 (a reduced body resets omitted fields).
Post-restore check, all four must hold: empty diff for each of the three files; `branches/main/protection --jq .allow_force_pushes.enabled` prints `false`; both rulesets `enforcement` `active`; the five workflows `active`. Record the check output in `df-records/restore-check.txt`. Exit paths covered: success, push rejected, executor crash or Ctrl-C (trap), session death (checklist: tm-dogfood runs the same check before the unfreeze message).

## 4. Backup and ref-count check
- F1: `/Users/masa/d19-run/mirror.git` (clone --mirror, origin removed), `/Users/masa/d19-private/d19-pre-rewrite.bundle` (`bundle create --all`, verify, chmod 600, sha256), `restore-test.git` cloned from the bundle and fsck'd. Neither directory exists yet.
- Free-space check at F1: the repo pack is 22.79 GiB and F1-F4 keep four copies (mirror, bundle, restore test, work.git), about 92 GiB. Require `df -g /Users/masa | awk 'NR==2{print $4}'` at least 150 (GiB free). Today: 738 GiB free.
- Old main: record OLD_MAIN SHA and tree in `refs-before-sha.txt` only. Push no `archive/` tag (it keeps old objects reachable).
- Retention until Support confirms the purge, then the PM/operator deletes the bundle with `rm -P`.
- Total ref count, before (F1) and after (step 7, fresh `clone --mirror` or direct `ls-remote`):
```
git ls-remote origin 'refs/heads/*' 'refs/tags/*' | grep -v '\^{}' | awk '{split($2,a,"/");c[a[2]]++} END{for(k in c)print k,c[k]}'
```
Read-only baseline at 09:55Z: heads 52, tags 931. Expected delta: zero heads, zero tags (replacement, no add or delete). A nonzero delta fails the push step and triggers rollback review. Rewritten-ref count = `wc -l refs-to-rewrite.txt` (161 on 11-02; about 34 heads + 127 tags), and every one must show a new SHA in the after list while the other 52+931-161 refs keep theirs. Save before and after lists as `refs-count-before.txt` and `refs-count-after.txt`.

## 5. Execution (executor tm-trusty-issues, Architect GO per step)
1. F0: freeze message and acks (B4); install gate; confirm no auto-merge; `gh pr list` inventory.
2. F1: mirror, bundle, restore test, free-space check, refs-to-rewrite.txt, leases, before-count; redo on bac374040d. Abort if mirror main != OLD_MAIN.
3. Dry run on a copy (`work.git`): filter-cmd.sh, gen_tagmap.py. Require exit 0, `--contains 26c066888a` empty, 1 differing path, tag-map 127 lines, commit-map about 1140 rows.
4. `scan_post-20261102-1636.py ... --paths --expect-diff-paths 1` must exit 0 PASS; the negative control on the unrewritten mirror must exit 1.
5. (Waits for B1, B3, B4.) Save JSON, start the wrapper, disable the workflows, Bob disables rulesets and lifts main force-push on the dashboard.
6. Push (Architect GO, tm-dogfood reviews the command first): `gen_push.py` output, one atomic push, `--force-with-lease=<ref>:<old>` per ref, heads then 127 tags then main, no `--mirror`/`--all`/refs/pull, no `--no-verify`.
7. Fresh mirror of origin, rerun the scan, after-count; Bob restores; wrapper check; re-enable workflows; unfreeze message by tm-dogfood only after all pass.
8. CI on new main: one-shot `gh api actions/runs?head_sha=`; six required checks green.
9. Support request: sent by Bob from his account (not the executor, not the PM), one ticket, runbook s9 text: list C (about 4,744 commits), 17 blob ids, De8 38, Ded 564, 23 old-head commits, refs/pull cached views, 8 public forks. refs/pull/* cannot be pushed to, so old SHAs stay fetchable by SHA until Support acts.
10. Keep the gate until `for-each-ref --contains 26c066888a` is empty in every clone.

## 6. Verify
Scan PASS on the fresh clone (14 blobs unreachable, 0 long-line blobs, 127/127 tags mapped); old vs new main tree differs in 1 path; `git log -S` and `git grep` for the De8 id on main print nothing; commit count equal to old main (5227 at last read); ref-count delta zero (section 4); CI green. Known residue: crates.io sources (trusty-architect 0.1.0/0.2.0, trusty-mpm 1.7.10 to 1.11.1) hold one 9-char path-like token each; not rewritable.

## 7. Downstream
This checkout: `.git` 23 GB, 213 worktrees, 663 local heads containing 26c, 950 tags, 5 local docs commits on main. Cheapest path: new clone, then `git rebase --onto origin/main <old-base> <branch>` per live branch (old base = merge-base with OLD_MAIN, taken before fetch; for pushed branches `git branch SAVE` then `reset --hard origin/<branch>`).
1. Held PRs on origin: fetch only, rebase after Architect clearance (#10424 before #10430; #10713-#10717; #10720, #10730; #10689, #10693, #10694; rest per runbook s2).
2. tm-trusty-tools: PR #10733 (rewritten in place if its head contains 26c; fetch only, then rebase onto the new main); local-only fix/10706-phase2-c2-h1 (1ef80e2ea1, not on origin): `git rebase --onto origin/main <old-base> fix/10706-phase2-c2-h1` with old base taken before fetch, then push. Its worktree sits in the main clone, so the main-clone fetch must wait for the unfreeze.
3. Local-only #6433 stack in the boundary-file order, drop the duplicate cherry-pick, rung-6 gates once on the tip; #10696 rebase, push, open PR.
4. After: fast-forward main checkout; PM removes worktrees df-g01-10705 and df-g01-10705-allowlist; tm-mpm-utils-01 prunes its 584 EVO trees with the documented one-key prune.
Release tags move once (#6178 exception); crates.io unaffected; no tag is cut before the rewrite.

## 8. Rollback
Valid until Support purges: `gen_push.py restore-test.git refs-to-rewrite.txt refs-after-sha.txt <url>` sends old objects from the bundle clone, atomic and leased, inside the same wrapper (B1 and B3 apply again). After the purge only the bundle restores. Any stale-info rejection or partial write: stop, refreeze, redo from F1.

## 9. Open questions
1. D: Is the rewrite scope still D8d/D19 only (fixture, De8, Ded)? Recommend yes; no source ties df-A04 keys to this rewrite.
2. A (Architect): confirm tm-dogfood as unfreeze owner and tm-trusty-issues as executor. The executor also owns held PRs #10689/#10693/#10694, so tm-dogfood reviews the push command before GO.
3. D (Bob): Was the 09:43Z mini check enough (817 dirs hit the depth cap)? Recommend yes; mini installs from crates.io.
4. D (Bob): Human read of the 9-char residue token and the yank decision for 12 published crate versions. Recommend read once, then decide.
5. A: 8 public forks cannot be cleaned by us; accept and rely on Support. Recommend accept.
6. A: Are `linker-bench.yml` and `zz-gate-semantics-probe.yml` leftovers safe to leave registered after the window? Out of scope for the rewrite.
