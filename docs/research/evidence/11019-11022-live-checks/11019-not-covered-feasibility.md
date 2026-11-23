# #11019/#11025 NOT-COVERED cases: feasibility

Sources: `docs/research/evidence/11019-11022-live-checks/11087-postmerge-run1.log` (tail: four NOT-COVERED lines, then 44 passed, 0 failed);
`scripts/live-checks/11019-midbatch.sh`.

## Why the script cannot reach them

The script header (lines 19-27) states the limit. The four cases need the reindex task to die while the
daemon keeps running. A release build has no RPC that cancels a run, a graceful stop ends in
`process::exit(0)`, and the fault hooks are `#[cfg(test)]`. The script only starts the installed binary
(`start_daemon`, lines 258-270, env limited to `TRUSTY_DATA_DIR`, `TRUSTY_NO_AUTO_DISCOVER`,
`TRUSTY_MPM_ROOT`, `TRUSTY_EMBEDDERD_BIN`) and signals the whole process group. Both signals it can send
(KILL, TERM) end the process, so the guard and rollback never run in a live daemon. No script-only route
reaches the in-process abort. Not verified against trusty-tools source: this repo does not contain it.

Options considered, per case: (a) script plus chmod of the identity dir; (b) a debug env or feature flag in
the daemon that aborts the reindex task; (c) an in-process test in trusty-tools.

## Case 1: put refused during rollback (log tail line 1; script 447-448)

- Needs a window where the task is dead and rollback is still running. Rollback is short, so even with a
  hook the script would race it.
- (a) fails: chmod does not kill a task. (b) works only if the flag also holds rollback open (a pause
  hook), which is a new daemon surface. (c) holds the window with a test pause point.
- Recommendation: NEEDS-TRUSTY-TOOLS-TEST. "Test: abort the staged reindex task mid-batch, hold rollback,
  assert put returns 'is rolling back a staged reindex that died mid-run'."

## Case 2: rollback re-attaches live and raises generation (script 449-450)

- Needs the abort, then a generation probe (`probe_generation`, script line 219) before and after.
- The restart path is already covered: generation 1 -> 2 -> 3 and no staging file left (log tail,
  TERM case). That exercises reconcile-on-attach, not the guard's in-process rollback.
- (a) fails. (b) works with a plain "abort task" flag, no pause needed; the script then polls
  `probe_generation` and a put. That is a new release-visible fault flag.
- Recommendation: NEEDS-TRUSTY-TOOLS-TEST. "Test: abort the task, assert the index re-attaches live
  without restart and generation rises above the pre-abort value."

## Case 3: failed rollback bump keeps puts refused (script 451-452)

- The chmod idea (read-only identity dir) is doable from the script, but only after the abort is
  triggered, so it depends on case 2's hook. Chmod on its own does nothing: no rollback runs.
- With a flag (b) the steps would be: park the run (as the existing cases do), `chmod a-w` the identity
  dir, trigger the abort, assert puts stay refused, `chmod u+w`, restart, assert puts work and generation
  rose. Fragile: the daemon may run as a user that ignores mode bits (root), and the bump may fail for
  other reasons.
- Recommendation: NEEDS-TRUSTY-TOOLS-TEST. "Test: make the identity write fail during rollback (read-only
  dir or injected error), assert puts stay refused until restart."

## Case 4: delete racing the rollback (script 453-454)

- Needs two concurrent calls straddling the rollback window; the outcome is timing-dependent. A script
  cannot order them without a pause hook.
- (a) fails. (b) needs the pause flag from case 1. (c) controls the ordering.
- Recommendation: NEEDS-TRUSTY-TOOLS-TEST. "Test: issue a delete while rollback is held; assert it waits,
  and that a delete that keeps the index finds live attached."

## Summary

| Case | Script alone | Daemon flag | In-process test | Recommendation |
|------|--------------|-------------|-----------------|----------------|
| 1 put refused | no | needs abort + pause | yes | NEEDS-TRUSTY-TOOLS-TEST |
| 2 re-attach, generation | no | abort only | yes | NEEDS-TRUSTY-TOOLS-TEST |
| 3 failed bump | no (needs abort) | abort + chmod | yes | NEEDS-TRUSTY-TOOLS-TEST |
| 4 delete race | no | needs abort + pause | yes | NEEDS-TRUSTY-TOOLS-TEST |

COVER-BY-SCRIPT and ACCEPT-GAP are not recommended: no case is script-reachable, and the covered
restart path (44 passed, 0 failed) does not exercise the guard's in-process rollback. The four cases
should be recorded as owned by the trusty-search unit/integration tests. Keeping them as NOT-COVERED in
the script (lines 446-454) is accurate and should stay until those tests exist.
