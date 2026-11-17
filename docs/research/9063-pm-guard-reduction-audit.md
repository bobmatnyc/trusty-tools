# pm-guard reduction audit (2026-10-27)

Issue: [#9063](https://github.com/bobmatnyc/trusty-tools/issues/9063). Plan: [`docs/roadmap/trusty-mpm.md`](../roadmap/trusty-mpm.md) ([PR #9062](https://github.com/bobmatnyc/trusty-tools/pull/9062)). Cut gate: [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) step 9. Source read at `origin/main` `dc11a7e7f3`. Read-only audit: no code was changed or run.

## Summary

| Measure | Value |
|---|---|
| Rules audited (deny paths of `tm hook --pm-guard`) | 44 |
| KEEP | 28 |
| CUT | 8 (4 now, 4 gated on a live sandbox row, [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) 9a-9d) |
| MERGE | 8 (into a surviving rule; no behavior change) |
| Denies recorded, 2026-09-27 to 2026-10-26 | 6,949 (one host, one operator) |
| Denies from the 4 ungated cuts | 6 (0.09%) |
| Denies from the 4 gated cuts | up to 1,344 (shared slugs); exact split unavailable, see Data |
| False positives, 30 days | **not measurable from the log** (it records denies, not outcomes). Seven filed false-positive issues in the window are in "Filed false-positive issues". |

Expected effect:

- The four ungated cuts (R40, R41, R43, R44) remove the PM delegation rules, the opt-in persona gate and the context hard stop. Each already has a mechanical successor or ships off. That deletes about 3,400 lines of module and test code (`pm_guard_cost.rs`, `pm_guard_deny_by_default.rs`, `pm_guard_budget/`, `core/agent_cost.rs`) plus the P1/P4/P5 verb classifier in `pm_guard_bash/mod.rs`. It cuts the sed/awk, redirection and build-verb lexer that produced the oldest false-positive issues ([#2664](https://github.com/bobmatnyc/trusty-tools/issues/2664), [#2734](https://github.com/bobmatnyc/trusty-tools/issues/2734), [#2745](https://github.com/bobmatnyc/trusty-tools/issues/2745), [#5356](https://github.com/bobmatnyc/trusty-tools/issues/5356)).
- The four gated cuts (R07, R11, R13, R15, plus R21 merged into R15) follow [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) step 9a-9d. They take the three largest Bash text families out of the guard: the secret-file name rule, the trust-anchor Bash half and the Architect pane rule. None is removed before step 8 passes.
- The merges drop eight deny slugs. Rules keep their verdicts. The audit trail loses eight labels.
- Rules that stay are the ones with no sandbox substitute: the Read/Grep/Write/Edit tools, git state of shared checkouts, dispatch and worktree lifecycle, credential printing and tm-specific verbs.

Recommendation key: KEEP = stays. CUT = remove. CUT (gated) = remove only after the named [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) step-8 live row passes, else record as kept. MERGE = fold into the named rule.

## Data: where pm-guard decisions are logged

Source: `<data_dir>/trusty-mpm/pm-guard-denials.jsonl`, written on every deny by `audit_denied_tool` (`C/pm_guard_deny_log.rs:128`) through `append_denial` (`crates/trusty-mpm/src/daemon/bug_report/denials.rs:98`), which [#8722](https://github.com/bobmatnyc/trusty-tools/issues/8722) added. On this host: `/Users/masa/Library/Application Support/trusty-mpm/pm-guard-denials.jsonl`, 6,949 lines, first record 2026-09-27 04:49 UTC, last 2026-10-26 14:23 UTC, 0 unparseable. The record carries check slug, tool, command (secret-scrubbed), cwd, session and reason. `list_recent_errors` and `preview_bug_report` read the same store ([`denials.rs:1`](../../crates/trusty-mpm/src/daemon/bug_report/denials.rs)). The `mcp__trusty-mpm__list_recent_errors` tool was not available to this audit, so counts come from the file. Counts below are per slug, grouped as `pm-guard denied <tool> [<slug>]`.

Limits:

1. **No outcome field.** The log cannot tell a correct deny from a false positive. The per-rule false-positive count is therefore unknown, not zero. The 30-day column in the table is the deny count.
2. **One host.** Only this machine's sessions, and only sessions with the `TRUSTY_NO_BUG_CAPTURE` unset (`C/pm_guard_deny_log.rs:89`).
3. **Slug granularity.** One slug covers several rules: `secret-file-read` covers R15, R16, R17, R18, R19 (`C/pm_guard_architect_envfile.rs:132-155`); `destructive-delete` covers R13 and R14; `trust-anchor-write` covers R10 and R11; `d4-floor` covers R03-R06; `secret-file-read` also takes `Read`/`Grep`/`Write`/`Edit` calls (16 of 956). Shared counts are shown on the first row of each group and marked "shared".
4. **Front-loaded.** 6,742 of 6,949 denies fall in the weeks starting 2026-09-21 and 2026-09-28. Weeks starting 10-05 to 10-26 hold 207 (`read-only-dispatch` and `secret-file-read` fall to 0 and 1 after 10-05). The log does not say why: fewer sessions, the fixes of [#9021](https://github.com/bobmatnyc/trusty-tools/pull/9021) (2026-10-01) and [#9154](https://github.com/bobmatnyc/trusty-tools/pull/9154) (2026-10-04), or a different cause. Not established.
5. **Retry loops inflate counts.** `main-checkout-commit` has 719 denies but 35 distinct (session, command) pairs. `read-only-dispatch` has 4,382 denies and 4,246 distinct pairs. Count pairs, not rows, when judging noise.
6. **Not logged here:** allowed calls, the daemon `/hooks` audit POST carries `pm_guard_decision: "deny"` only (`C/pm_guard_deny_log.rs:135-143`), and the egress/push/supervisor hooks (separate entry points, outside this audit).

Method to get real false-positive counts: add an outcome per deny. For each deny record, look in the same session for the next call to the same tool within 5 minutes. Mark it "rework" when the command was rewritten and then allowed, and "retry" when it was repeated and denied again. Review "rework" rows by hand. A scrubbed-command listing for a rule:

```
python3 -I -c "import json,re,sys; [print(r['timestamp_secs'], r['fields'][:300]) for r in map(json.loads, open(sys.argv[1])) if '[secret-file-copy]' in r['message']]" "<path to pm-guard-denials.jsonl>"
```

Leads from the counts (not false-positive counts): `secret-file-copy` is a copy/move rule, yet its 186 denies start with `cat` (75), `cd` (51) and `grep` (13); `read-only-dispatch` denies start with `git` (1,503) and `cd` (1,346) of 4,382. Both deserve the outcome review above before any narrowing.

### Filed false-positive issues in the window (created 2026-09-27 to 2026-10-27)

Titles read; bodies not re-checked. [#9001](https://github.com/bobmatnyc/trusty-tools/issues/9001) (`$'…'` refused as tmux and SQL text), [#9006](https://github.com/bobmatnyc/trusty-tools/issues/9006) (program-text exemptions), [#8945](https://github.com/bobmatnyc/trusty-tools/issues/8945) (heredoc and wrapper-script text read as commands), [#8927](https://github.com/bobmatnyc/trusty-tools/issues/8927) (unbalanced `${…}` word; closed Won't Do into [#9063](https://github.com/bobmatnyc/trusty-tools/issues/9063)), [#8869](https://github.com/bobmatnyc/trusty-tools/issues/8869) (credential-consumer CLIs), [#9360](https://github.com/bobmatnyc/trusty-tools/issues/9360) (unclosed paren keeps a heredoc body as code), [#9794](https://github.com/bobmatnyc/trusty-tools/issues/9794) (no sanctioned `import kuzu` path). That is 7 issues, almost all in the Bash text rules R02, R15, R17, R20.

## Rule table

`C` = `crates/trusty-mpm/src/bin/tm/commands`. Order follows `pm_guard` ([`C/pm_guard.rs:352`](../../crates/trusty-mpm/src/bin/tm/commands/pm_guard.rs)). "Deny 30d" is the slug count from the log (see Data). Rows with the same slug share a count.

### A. Floors that hold under both bypasses (`TRUSTY_MPM_DISABLE_HOOKS`, `TRUSTY_MPM_PM_UNRESTRICTED`)

| ID | Rule (slug) | Enforced at | Incident / issue | Rec | Reason | Deny 30d |
|---|---|---|---|---|---|---|
| R01 | unclassifiable-payload | `C/hook_stdin.rs:284`, `:309` | [#7975](https://github.com/bobmatnyc/trusty-tools/issues/7975) | KEEP | A payload that does not parse is a delivery fault; denying is cheap and has no substitute. | 0 |
| R02 | unclassifiable-command | `C/pm_guard.rs:453`; `C/pm_guard_floor.rs:260`; `C/pm_guard_bash/ansi_c_decode.rs:132`; `C/pm_guard_bash/mod.rs:294` | [#6660](https://github.com/bobmatnyc/trusty-tools/issues/6660); false positive [#9001](https://github.com/bobmatnyc/trusty-tools/issues/9001) | KEEP | Precondition of every lexical rule; remove it only with the last lexical rule. | 40 |
| R03 | d4-floor: network upload (curl/wget/scp/rsync/nc) | `C/pm_guard_bash/floor_d4_rules.rs:57`; `C/pm_guard_floor.rs:207` | [#8878](https://github.com/bobmatnyc/trusty-tools/issues/8878) (D4 deny-set) | KEEP | Not on the step-9 list; no other layer sees the upload. | 47 (shared R03-R06) |
| R04 | d4-floor: destructive disk tools | `C/pm_guard_bash/floor_d4_rules.rs:267` | [#8878](https://github.com/bobmatnyc/trusty-tools/issues/8878) | KEEP | Irreversible, low false-positive surface. | shared |
| R05 | d4-floor: python upload | `C/pm_guard_bash/floor_d4_python.rs:92` | [#8878](https://github.com/bobmatnyc/trusty-tools/issues/8878) S5 | MERGE into R03 | Same deny class as R03; the heuristic (network module plus local read) lists its own residual interpreter bypasses, so it is a second spelling of R03, not a separate floor. | shared |
| R06 | d4-floor: force-push to default branch | `C/pm_guard_bash/force_push.rs:160` | [#8878](https://github.com/bobmatnyc/trusty-tools/issues/8878) | KEEP | Irreversible on the shared remote. | shared |
| R07 | architect-pane | `C/pm_guard_bash/architect_pane.rs:99`; `C/pm_guard_floor.rs:216` | [#8902](https://github.com/bobmatnyc/trusty-tools/issues/8902) | CUT (gated, [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) 9a) | Step 9a lists it; the private tmux server per workspace ([#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) step 4, partly built, `-L` launch split to [#10101](https://github.com/bobmatnyc/trusty-tools/issues/10101)) is the replacement. | 40 |
| R08 | tmux-target-unresolved | `C/pm_guard_bash/tmux_exact_target.rs:83`; `C/pm_guard_floor.rs:223` | [#9001](https://github.com/bobmatnyc/trusty-tools/issues/9001), [#8443](https://github.com/bobmatnyc/trusty-tools/issues/8443); owner ruling 2026-10-01 | KEEP | Owner ruling; tmux prefix matching is not visible to the sandbox. | 3 |
| R09 | fleet-relaunch | `C/pm_guard_bash/fleet_relaunch.rs:77`; `C/pm_guard_floor.rs:231` | [#10033](https://github.com/bobmatnyc/trusty-tools/issues/10033), [#9092](https://github.com/bobmatnyc/trusty-tools/issues/9092) | KEEP | tm-specific verb; only the guard knows it. | 0 |
| R10 | trust-anchor-write (Edit/Write tools) | `C/pm_guard.rs:629`; `C/pm_guard_trust_anchor.rs:154`, `:178` | [#8878](https://github.com/bobmatnyc/trusty-tools/issues/8878) (ruling 2026-09-29) | KEEP | The sandbox does not gate the Write/Edit tools. | 106 (shared R10-R11) |
| R11 | trust-anchor-write (Bash half: cp/mv/ln/sed -i) | `C/pm_guard_trust_anchor.rs:178` | [#8878](https://github.com/bobmatnyc/trusty-tools/issues/8878) | CUT (gated, [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) 9b) | Step 9b lists it. | shared |
| R12 | protected-path-write | `C/pm_guard.rs:635`; `C/pm_guard_protected_paths.rs:118` | [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) step 7 | KEEP | New, and it exists because the sandbox does not cover Write/Edit. | 0 |
| R13 | destructive-delete: filesystem-root arms | `C/pm_guard_floor.rs:268`; `C/pm_guard_bash/destructive_delete.rs:180` | [#4031](https://github.com/bobmatnyc/trusty-tools/issues/4031), owner ruling on [#3973](https://github.com/bobmatnyc/trusty-tools/issues/3973) | CUT (gated, [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) 9c) | Step 9c lists "rm root arms". | 56 (shared R13-R14) |
| R14 | destructive-delete: repo root, `.git`, worktree arms | `C/pm_guard.rs:489` | [#4031](https://github.com/bobmatnyc/trusty-tools/issues/4031); false positive [#7190](https://github.com/bobmatnyc/trusty-tools/issues/7190) | KEEP | Protects another session's worktree; not in step 9. | shared |

### B. Secret and credential rules

| ID | Rule (slug) | Enforced at | Incident / issue | Rec | Reason | Deny 30d |
|---|---|---|---|---|---|---|
| R15 | secret-file-read, Bash half: file-name rule, script body, substitution | `C/pm_guard.rs:602`; `C/pm_guard_secret_read.rs:649`, `:663`; `C/pm_guard_secret_script.rs:195`; `C/pm_guard_secret_substitution_read.rs:90` | [#7266](https://github.com/bobmatnyc/trusty-tools/issues/7266), [#8879](https://github.com/bobmatnyc/trusty-tools/issues/8879), [#8931](https://github.com/bobmatnyc/trusty-tools/issues/8931); false positives [#9006](https://github.com/bobmatnyc/trusty-tools/issues/9006), [#8945](https://github.com/bobmatnyc/trusty-tools/issues/8945), [#7533](https://github.com/bobmatnyc/trusty-tools/issues/7533), [#7498](https://github.com/bobmatnyc/trusty-tools/issues/7498), [#7479](https://github.com/bobmatnyc/trusty-tools/issues/7479) | CUT (gated, [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) 9d) | Largest rule family (about 17,600 lines with tests incl. R17) and the most false-positive issues; step 9d lists it. | 956 (shared R15-R19; 940 Bash) |
| R16 | secret-file-read, Read/Grep tool half | `C/pm_guard_secret_read.rs:1198` | [#7266](https://github.com/bobmatnyc/trusty-tools/issues/7266) | KEEP | Step 9d names the Bash half only; the sandbox does not gate Read/Grep. | shared |
| R17 | credential-print | `C/pm_guard_bash/credential_print.rs:260`; `C/pm_guard_architect_envfile.rs:151` | [#8596](https://github.com/bobmatnyc/trusty-tools/issues/8596), [#8248](https://github.com/bobmatnyc/trusty-tools/issues/8248); false positive [#8869](https://github.com/bobmatnyc/trusty-tools/issues/8869) | KEEP | Names no file, so no file-based layer sees it; not in step 9. | shared |
| R18 | pod/process env dump, launchd plist env read | `C/pm_guard_bash/pod_env_dump.rs:116`; `C/pm_guard_bash/process_env_dump.rs:96`; `C/pm_guard_secret_env_files.rs:97` | [#7648](https://github.com/bobmatnyc/trusty-tools/issues/7648), [#8756](https://github.com/bobmatnyc/trusty-tools/issues/8756), [#8523](https://github.com/bobmatnyc/trusty-tools/issues/8523) | MERGE into R17 | Same invariant (no credential value in the transcript) and the same slug; one rule, one test surface. | shared |
| R19 | gh api / curl secret DELETE | `C/pm_guard_secret_gh_api_delete.rs:124` | [#8875](https://github.com/bobmatnyc/trusty-tools/issues/8875) | KEEP | Judged by method and endpoint, not file; damage rather than leak. | shared |
| R20 | kuzu-store-read, kuzu-import-python | `C/pm_guard_kuzu_store.rs:79`; `C/pm_guard_kuzu_exception.rs:111` | [#8625](https://github.com/bobmatnyc/trusty-tools/issues/8625), [#9794](https://github.com/bobmatnyc/trusty-tools/issues/9794) | KEEP | Owner rulings B10 and D61; 0 denies in the window is expected for a narrow memory-import rule. | 0 |
| R21 | secret-file-copy | `C/pm_guard.rs:506`; `C/pm_guard_bash/secret_file_copy.rs:694` | [#7122](https://github.com/bobmatnyc/trusty-tools/issues/7122) | MERGE into R15 | Same secret-shaped-name test applied to a cp/mv destination; goes with R15 under 9d. Review the 186 `cat`/`cd`/`grep` denies first. | 186 |

### C. Git state of shared checkouts and worktree lifecycle

| ID | Rule (slug) | Enforced at | Incident / issue | Rec | Reason | Deny 30d |
|---|---|---|---|---|---|---|
| R22 | read-only-dispatch | `C/pm_guard.rs:461`; `C/pm_guard_bash/read_only_allow.rs:75` | [#8439](https://github.com/bobmatnyc/trusty-tools/issues/8439) (eight plists truncated, 2026-09-23) | KEEP | An allowlist, not a deny-list. Highest deny count (63%); review the allowlist for `git` and `cd` shapes before 1.9.1. | 4,382 |
| R23 | worktree-add (temp roots, disk usage) | `C/pm_guard.rs:468`; `C/pm_guard_bash/mod.rs:958`; `C/pm_guard_bash/disk_usage.rs:44` | [#3955](https://github.com/bobmatnyc/trusty-tools/issues/3955), [#3977](https://github.com/bobmatnyc/trusty-tools/issues/3977), [#7497](https://github.com/bobmatnyc/trusty-tools/issues/7497); false positive [#6946](https://github.com/bobmatnyc/trusty-tools/issues/6946) | KEEP | Agents provision worktrees through Bash; the daemon gate does not see them. | 11 |
| R24 | main-checkout-destructive | `C/pm_guard.rs:520`; `C/pm_guard_bash/main_checkout.rs:199` | [ADR-0037](../adr/0037-pm-placement-precedence-main-checkout-by-default.md) incident (no issue named) | KEEP | Irreversible loss of another session's uncommitted work. | 4 |
| R25 | head-switch | `C/pm_guard.rs:530`; `C/pm_guard_bash/head_switch.rs:73` | [#8572](https://github.com/bobmatnyc/trusty-tools/issues/8572) | KEEP | Reflog-confirmed incident; fails closed on unreadable status. | 40 |
| R26 | main-checkout-commit | `C/pm_guard.rs:550`; `C/pm_guard_bash/main_checkout.rs:352` | [ADR-0044](../adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md), [ADR-0048](../adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md); bypasses [#9127](https://github.com/bobmatnyc/trusty-tools/issues/9127), [#9146](https://github.com/bobmatnyc/trusty-tools/issues/9146) | KEEP | A commit lands on the branch other sessions stand on. 719 denies are 35 distinct commands: a retry loop to fix at the agent prompt. | 719 |
| R27 | docs-commit | `C/pm_guard.rs:558` | [ADR-0049](../adr/0049-docs-commits-are-permitted-in-a-main-checkout.md) | MERGE into R26 | A branch of R26's verdict enum, not a separate rule. | 0 |
| R28 | head-move (merge/rebase in a main checkout) | `C/pm_guard_bash/linked_worktree_head_move.rs:189`, `:214` | [ADR-0048](../adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md) decision 10, [#9127](https://github.com/bobmatnyc/trusty-tools/issues/9127) | MERGE into R29 | Same module, same daemon writer query; one "HEAD move into a held tree" rule. | 0 |
| R29 | linked-worktree-head-move | `C/pm_guard.rs:589`; `C/pm_guard_bash/linked_worktree_head_move.rs:239` | [#8161](https://github.com/bobmatnyc/trusty-tools/issues/8161) | KEEP | Moves the branch under a live agent's uncommitted work. | 11 |
| R30 | main-checkout-write | `C/pm_guard.rs:619`; `C/pm_guard_write_boundary.rs:169` | [ADR-0044](../adr/0044-main-checkout-write-boundary-and-agent-worktree-ownership.md); false positive [#8468](https://github.com/bobmatnyc/trusty-tools/issues/8468) | KEEP | The write the reported incident was made of. Already covers PM source edits in a main checkout (see R44). | 105 |
| R31 | subagent-fanout | `C/pm_guard.rs:654`; `C/pm_guard_fanout.rs:92` | [#4784](https://github.com/bobmatnyc/trusty-tools/issues/4784) | KEEP | One-level orchestration; `tools:` frontmatter cannot express it. | 59 |
| R32 | enter-worktree | `C/pm_guard.rs:666`; `C/pm_guard_enter_worktree.rs:107` | [#7172](https://github.com/bobmatnyc/trusty-tools/issues/7172) | KEEP | The harness pin cannot be moved from a hook; the refusal prevents a wedged agent. 0 denies is the goal. | 0 |
| R33 | worktree-remove | `C/pm_guard.rs:698`; `C/pm_guard_bash/worktree_remove.rs:192` | [#5791](https://github.com/bobmatnyc/trusty-tools/issues/5791), [ADR-0057](../adr/0057-version-control-owns-worktree-removal.md) | KEEP | Owner ruling 2026-08-19 and re-ruling 2026-09-02. | 61 |
| R34 | worktree-remove-recheck | `C/pm_guard.rs:715`; `C/pm_guard_bash/worktree_remove_deadline.rs:82` | [ADR-0057](../adr/0057-version-control-owns-worktree-removal.md); false positives [#8665](https://github.com/bobmatnyc/trusty-tools/issues/8665), [#8633](https://github.com/bobmatnyc/trusty-tools/issues/8633), [#8849](https://github.com/bobmatnyc/trusty-tools/issues/8849) | MERGE into R33 | The `ReCheck` branch of R33's verdict. | 57 |
| R35 | resume-worktree | `C/pm_guard.rs:727`; `C/pm_guard_resume_worktree.rs:83` | [#8004](https://github.com/bobmatnyc/trusty-tools/issues/8004) (four recurrences) | KEEP | Owner ruling 2026-09-24; prose did not hold. | 29 |

### D. Dispatch, build and PM delegation rules

| ID | Rule (slug) | Enforced at | Incident / issue | Rec | Reason | Deny 30d |
|---|---|---|---|---|---|---|
| R36 | dispatch-content | `C/pm_guard.rs:770`; `C/pm_guard_content.rs:21` | [#9011](https://github.com/bobmatnyc/trusty-tools/issues/9011) (owner ruling 09(a)) | KEEP | Refuses a PM dispatch with no content roster rather than guess. | 0 |
| R37 | dispatch (shared working tree) | `C/pm_guard.rs:851`; `C/pm_guard_dispatch.rs:151`, `:1049` | [#4480](https://github.com/bobmatnyc/trusty-tools/issues/4480), [#5923](https://github.com/bobmatnyc/trusty-tools/issues/5923) | KEEP | Two writers on one git HEAD; git does not stop it. | 2 |
| R38 | worktree-grant (deny) | `C/pm_guard.rs:806`; `C/pm_guard_worktree_grant.rs:160` | [ADR-0048](../adr/0048-dispatched-writers-get-a-worktree-and-the-write-boundary-is-enforced.md), [#5814](https://github.com/bobmatnyc/trusty-tools/issues/5814) | MERGE into R37 | Same decision (may this dispatch write here) under three slugs. | 0 |
| R39 | worktree-grant-dispatch | `C/pm_guard.rs:779`; `C/pm_guard_dispatch.rs:601` | [#5769](https://github.com/bobmatnyc/trusty-tools/issues/5769) | MERGE into R37 | As R38. | 22 |
| R40 | agent-cost (context hard stop) | `C/pm_guard.rs:881`; `C/pm_guard_cost.rs:322`; `crates/trusty-mpm/src/core/agent_cost.rs` | [#4837](https://github.com/bobmatnyc/trusty-tools/issues/4837) | CUT (now) | Ships OFF (`max_tokens = 0`), 0 denies. The warning arm is advice, not enforcement; move it out of pm-guard if kept. | 0 |
| R41 | persona-deny-by-default | `C/pm_guard.rs:959`; `C/pm_guard_deny_by_default.rs` | [#2231](https://github.com/bobmatnyc/trusty-tools/issues/2231); lesson [#2172](https://github.com/bobmatnyc/trusty-tools/issues/2172) | CUT (now) | Opt-in by env var, 0 denies; the unset path is identical to no rule. | 0 |
| R42 | build-lease | `C/pm_guard.rs:923`; `C/pm_guard_build_lease.rs:177` | [#8261](https://github.com/bobmatnyc/trusty-tools/issues/8261) | KEEP | Replaced the dispatch builder slot; enforces the machine-wide build cap. | 1 |
| R43 | file-change-budget | `C/pm_guard.rs:994`; `C/pm_guard_budget/mod.rs:262` | [#2918](https://github.com/bobmatnyc/trusty-tools/issues/2918); false positive [#5356](https://github.com/bobmatnyc/trusty-tools/issues/5356) | CUT (now) | Stateful heuristic with no turn boundary in the hook payload; 5 denies. | 5 |
| R44 | pm-tool-policy (P1/P4/P5: source edit, shell edit, build/test, network) | `C/pm_guard.rs:980`, `:1032`; `C/pm_guard.rs:1096`; `C/pm_guard_bash/mod.rs:348`, `:563` | [#1977](https://github.com/bobmatnyc/trusty-tools/issues/1977), [#2604](https://github.com/bobmatnyc/trusty-tools/issues/2604); false positives [#2664](https://github.com/bobmatnyc/trusty-tools/issues/2664), [#2734](https://github.com/bobmatnyc/trusty-tools/issues/2734), [#2745](https://github.com/bobmatnyc/trusty-tools/issues/2745) | CUT (now) | Each prohibition has a mechanical successor: source writes by R30, builds by R42, uploads by R03. Prompt text carries the rest. Removes the sed/awk/redirection/build-verb lexer. | 1 |

Retired slug in the log, no source site at this commit: `builder-cap` (6 denies; replaced by R42 in [#8261](https://github.com/bobmatnyc/trusty-tools/issues/8261)).

Not rules (exemptions and bypasses, listed so none is mistaken for a deny): `CLAUDE_MPM_SUB_AGENT` (`C/pm_guard.rs:944`), `TRUSTY_MPM_DISABLE_HOOKS` and `TRUSTY_MPM_PM_UNRESTRICTED` (`C/pm_guard.rs:372-401`), native sub-agent exemption `agent_id` ([#2014](https://github.com/bobmatnyc/trusty-tools/issues/2014), `C/pm_guard.rs:959`), supervisor profile ([#8453](https://github.com/bobmatnyc/trusty-tools/issues/8453), `C/pm_guard_profile.rs:34`), the Architect identity exemption (`C/pm_guard_floor.rs`, [#8878](https://github.com/bobmatnyc/trusty-tools/issues/8878)), the `tm env` exemption ([#8939](https://github.com/bobmatnyc/trusty-tools/issues/8939), `C/pm_guard_architect_envfile.rs`).

## Done-when check ([#9063](https://github.com/bobmatnyc/trusty-tools/issues/9063))

- Each kept rule names its incident: 28 KEEP rows carry an issue, an ADR or an owner ruling. R24 names an ADR incident with no issue number. R42 and R36 cite the design issue.
- Each cut rule is removed or recorded as kept in [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928) step 9: R07, R11, R13, R15 map to 9a-9d. R40, R41, R43, R44 have no step in [#8928](https://github.com/bobmatnyc/trusty-tools/issues/8928); they need a removal PR, which this audit does not file.
