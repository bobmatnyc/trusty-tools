# can an aborted non-force reindex lose acknowledged puts? (trusty-search)

Tracker bobmatnyc/trusty-tools#10737. Read-only static analysis; cargo not run.
Date 2026-11-09.

## Verdict: CONDITIONAL (yes on origin/main; on the #10806 head the loss still happens but is signalled, with two residual gaps)

Pins (all `crates/trusty-search/src/...` in <trusty-tools checkout>):

- (a) origin/main = `03db42f7a8223692e7067c55e115fc7dfa9b9c04`. The reindex/, core/indexer/mod.rs, ingest/ and server/files.rs paths cited below are identical to merge-base `6428e79eae` (`git diff --stat 6428e79eae origin/main -- <those paths>` is empty), so main's numbers equal the PR base's.
- (b) #10806 head = `66e11729bffd82632bf06cba8adf10a5c40cc7b6`.

Main citations are `M:file:line`, head citations `H:file:line`. Files not named under H are unchanged from M.

## 1. What "acknowledged" means

- A put (`index_file_report`) takes `handle.indexer.read()` (M:service/server/files.rs:64, H:67) and holds it to the end of the function, then calls `index_file_outcome`.
- The redb commit of the chunks and entities runs before the reply: `commit_corpus_to_redb` (M:core/indexer/ingest/commit.rs:149 calls it; it does `upsert_batch` at :427 on whatever `self.corpus` is installed).
- With NO corpus installed, `commit_corpus_to_redb` returns `true` (M:commit.rs:412-414) and `corpus_write_failed` is `self.corpus.is_some() && ...` (M:commit.rs:147), so a put against a detached indexer is acknowledged as durable (M:ingest/mod.rs:540, :587).
- The only write refusals are `refuse_if_deleted`, `refuse_if_closed_for_shutdown` (M:ingest/mod.rs:406-409; shutdown_detach.rs:35) and `refuse_incremental_write`, which checks `corpus_open_failed` only (M:core/indexer/quarantine.rs:311-314). A staged-swap detach (`corpus == None`, `corpus_ever_wired`, not yet quarantined) is not refused for puts. Only the snapshot writers test that state (M:quarantine.rs:245).
- Because the put holds the read guard, `swap_corpus_store`/`take_corpus_store`/`set_corpus_store` (which need `&mut`, via the write guard) cannot interleave with one put. They CAN interleave between puts.

## 2. Non-force reindex path on origin/main (03db42f7)

Entry: `spawn_reindex*` -> detached `tokio::spawn(run_reindex)` (M:service/reindex/orchestrator.rs:181). Nothing ties the task to the client connection.

| Step | Where | What it does |
|---|---|---|
| walk, resume probe | M:runner.rs:308-321 | `probe_resume` may adopt an older staging file |
| begin staging | M:runner.rs:440 -> M:corpus_swap.rs:121-323 | snapshot live `Arc` (:163-173), `open_fresh` + `copy_all_from` on a blocking worker, NO lock held (:194-205) |
| SWAP | M:corpus_swap.rs:307-308 | `indexer.write()`, `swap_corpus_store(staging)`. From here every put lands in staging only |
| begin error arm | M:runner.rs:453-478 | only reachable before the swap (copy failure removes the tmp, :271-281). Live intact, no put affected |
| batch loop | M:runner.rs:621-659 | writes to staging. `mem_limit_hit` breaks the loop (:653-658) |
| prune | M:finish.rs:223-232 | removes staged paths not on disk, pushed paths included (non-force, not memory-aborted) |
| decide | M:finish.rs:261 -> M:staging.rs:78-92 | memory abort or `ReindexOutcome::Failed` = `Rollback`; `Ready` = `Commit` |
| settle | M:finish.rs:322 -> M:finish_teardown.rs:99-156 | `resolve_corpus_swap`. No early return exists between the swap and this call except a panic/cancel |
| COMMIT | M:finish_teardown.rs:112 -> M:corpus_swap.rs:623-711 | live lock (:651), `release_staging_for_promotion` takes the corpus (:482-493), rename (:668), reopen (:680), `set_corpus_store` (:693) |
| ABORT | M:finish_teardown.rs:143 -> M:corpus_swap.rs:767-857 | `take_corpus_store` (:776, guard dropped at :778), delete staging (:788-808), reopen live (:814-833), `set_corpus_store` (:836-840) |

On main the reply carries no index identity; that is what #10738 adds.

### What an abort does to a put acknowledged after the swap

- The put is in `index.redb.tmp` only. `abort_staged_corpus_swap` unlinks that file (M:corpus_swap.rs:791) and reattaches the old live file. The put is gone from the durable corpus. No caller-visible signal exists. LOSS, undetected.
- The in-memory chunk map is not touched by the abort. `invalidate_corpus_caches` is called only from adoption (M:corpus_swap.rs:374; sole non-test caller, idle_evict.rs:407). So the put stays searchable from memory. Graceful stop re-upserts the whole in-memory map into the attached corpus (M:core/indexer/persist.rs:639-660, via `flush_all_indexes_on_shutdown`, M:service/shutdown_flush.rs:210; it skips an index whose reindex is Running, doc :175-192). So the loss is durable only across a hard stop (SIGKILL, OOM, crash, power) or any path that rehydrates from the corpus before a graceful flush. I did not trace idle eviction's rehydrate to say whether it drops the put.

### Per abort trigger, main

| Trigger | Result for a put acked after the swap |
|---|---|
| Ctrl-C of the CLI | No effect. The CLI has no cancel call (`grep -i "cancel\|abort\|ctrl"` in `commands/reindex_engine` hits only comments and a timeout test, no cancel call); the task is detached (orchestrator.rs:181). The run commits. Not an abort |
| Error return in the run (`Failed`, memory abort, producer panic -> `producer_failure`) | Rollback arm (:143). Put lost, undetected |
| Promotion refused (#7991) or path unresolvable | `commit_staged_corpus_swap` returns false (:661, :643); staging stays attached and serves. Next run's `probe_resume` hits `DatabaseAlreadyOpen` and discards it (doc M:corpus_swap.rs:604-611). Puts lost, undetected |
| Task panic or cancel (runtime shutdown on SIGINT/SIGTERM to the daemon) | `ReindexTerminationGuard::drop` only pushes a terminal frame (M:guard.rs:148-207). No restore, no delete. Staging stays attached in-process; file stays on disk. Next boot attaches live, which lacks the puts (kill/crash is the same). A later run can adopt the staging file (M:corpus_swap.rs:356-385, checkpoint age/config permitting) and then the puts survive; discard (`TRUSTY_REINDEX_RESUME` off, stale checkpoint, any force run's `sweep`, M:corpus_swap.rs:186) loses them. Adoption also swaps out live without copying it, so puts made after the restart are dropped at promotion |
| Concurrent put during the abort itself | See residual R1 |
| Put between `copy_all_from` and the swap (#839 window) | Goes to LIVE (M:corpus_swap.rs:163-173 releases the guard before the copy). Abort keeps live, so it survives an abort. A COMMIT drops it (promotion renames over live). The in-code comment names the window (H:corpus_swap.rs:241-248) |

Answer for (a): YES. Any put acknowledged between M:corpus_swap.rs:308 and M:corpus_swap.rs:776 is lost from the durable corpus by every abort arm, and nothing tells the caller.

## 3. Same path at the #10806 head (66e11729bf)

`abort_staged_corpus_swap` is byte-for-byte unchanged (H:corpus_swap.rs:664-754 versus M:767-857). The PR does not stop the loss. It adds a generation bump on every arm that discards writes, so a journalling caller sees `identity.generation` rise past the one its put reply carried.

What the PR adds, in order:

1. `begin_staged_corpus_swap` snapshots `path_writes_finished()` before the copy (H:corpus_swap.rs:168-179). Under the SAME write guard as the swap it calls `mark_before_swap` (H:corpus_swap.rs:313-318 -> H:service/index_identity.rs:340-371). That writes `identity_bump_pending` into the staging corpus if a path write finished in the carryover window (non-force), then `staged_reindex_in_flight` into the outgoing LIVE corpus (:379-385). A marker failure removes staging and returns `Err` before the swap (:364-369), so the run aborts with live attached.
2. `CodeIndexer::set_corpus_store` now calls `reconcile_on_attach` (H:core/indexer/mod.rs:1220). A corpus with a pending marker, a mismatching stamp, a missing stamp or `created_empty` bumps and re-stamps (H:index_identity.rs:245-279, :302-320).
3. Rollback: `abort_staged_corpus_swap` (H:finish_teardown.rs:167) reopens live through `set_corpus_store`. That attach sees the live marker and bumps under the write guard. Then `bump_with_pending(.., "staged_corpus_discarded")` bumps again (H:finish_teardown.rs:168-176). Memory abort takes the same arm.
4. Promotion not landed (deferral, unresolvable path, reopen failure): `bump_with_pending(.., None, "promotion_not_landed")` (H:finish_teardown.rs:122-131). A restart is covered by the live marker.
5. Restart or crash mid-run: the live file still carries `staged_reindex_in_flight`. The next attach bumps and clears it. Adoption re-marks live before swapping it out (H:corpus_swap_adopt.rs:62), and returns `Err` if the marker fails.
6. Prune: marker written into staging before the first removal; a failed marker write prunes nothing (H:prune.rs, hunk at ~180-250; H:index_identity.rs:395-405).
7. Quarantine-detach arms (abort cannot reopen live, `Err`/`Ok(None)`): `quarantine_detached_corpus` bumps `corpus_detached` (H:core/indexer/quarantine.rs, +2 lines at 404).
8. Put reply order: the identity is read at the end of `index_file_report` (H:server/files.rs:129) while the put's read guard is still held, so a bump made under the write guard (`set_corpus_store`) cannot slip in before that reply is built. A put reply therefore carries the pre-bump identity.

Per trigger, head:

| Trigger | Loss | Signalled? | Evidence |
|---|---|---|---|
| Rollback / memory abort | yes | yes, generation +2 | H:finish_teardown.rs:167-176; test `a_rollback_that_discards_a_put_raises_the_generation` (identity_10738_tests.rs:187-216) asserts both the loss and the rise, force and non-force |
| Promotion deferral | yes (staging never promoted) | yes | H:finish_teardown.rs:122-131; `a_deferred_promotion_raises_the_generation` (:225-245) |
| Kill / crash / restart | yes unless the next run adopts | yes, at the first attach | live marker, H:index_identity.rs:379-385; `a_restart_mid_staged_reindex_raises_the_generation` (:376) |
| Carryover-window put | lost on commit | yes | H:corpus_swap.rs:317; `a_write_in_the_carryover_window_raises_the_generation` (:258) |
| Prune of a pushed path | lost on commit | yes | H:prune.rs; `a_prune_that_removes_a_pushed_path_raises_the_generation` (:451) |
| Marker write fails | none (run refused) | n/a | H:index_identity.rs:364-369 |

Answer for (b): the data loss is still YES. "Loss with the identity unchanged" is NO on every arm above. Two residual gaps follow.

## 4. Residual gaps at the head (static reading; not executed)

### R1. Put acknowledged while the indexer is detached (abort and commit windows)

- Abort: `take_corpus_store` under the write guard (H:corpus_swap.rs:673), guard dropped at :675. The staging delete (:688) and `open_serialized` of the live file (:715) run with NO guard held. `set_corpus_store` is at :733-737. For that span `corpus == None`, `corpus_open_failed == false`.
- A put arriving then passes every refusal (section 1), is written to memory only, is acknowledged `indexed: true`, and stamps `record_incremental_commit`. The window is as long as an unlink plus a redb open of the live file. On the 1-2 GB corpora in this fleet that is not microseconds. (list_indexes shows 2.3 GB for trusty-tools.)
- Abort arm, head: SIGNALLED. The reattach bumps through the live marker (point 2), after the put's reply was built under G. Detected, but the put was acked non-durable. Graceful-stop flush would persist it from memory (persist.rs:645-660); a hard stop would not.
- Commit arm, head: NOT SIGNALLED. `release_staging_for_promotion` takes the corpus (H:corpus_swap.rs:388-390), the rename (:565) and reopen (:573-584) follow, and `set_corpus_store` is at :587-590. A put in that span is memory-only. The promoted file is a copy of live made at `copy_all_from` and carries stamp G equal to the stored identity. The live marker leaves with the renamed-over old file. `reconcile_on_attach` finds no pending marker and a matching stamp, so no bump. This is outside the question ("aborted"), but it is the same guarantee on a successful run. Confidence: medium; I did not execute it.
- This is the exact hazard #9459 closed for shutdown ("With no corpus they landed in memory only and answered `Ok`", M:core/indexer/shutdown_detach.rs:1-8). The staged-swap detach was not given the same refusal.

### R2. Task panic/cancel leaves staging attached and no in-process bump

`ReindexTerminationGuard::drop` (M:guard.rs:148-207, unchanged) never reattaches live. Puts keep going to staging under identity G, and status reads stay healthy, until a restart's attach bumps through the live marker. The loss only becomes real across a restart or discard, and the restart bumps, so the guarantee holds eventually. Delayed signal, low severity. Not covered by a test.

Also open, from the critic comment on #10738 (not mine): no test covers the bump in `quarantine_detached_corpus`, the loader failure arms or `record_migration_failure`.

## 5. Smallest failing scenarios

Main (undetected loss, 6 steps): index with `a.rs` -> `begin_staged_corpus_swap` (incremental) -> `index_file_report("pushed.rs", ..)` returns `indexed: true` -> `resolve_corpus_swap` with `Rollback` -> read live `list_indexed_files()` and see `pushed.rs` absent. This is the shape of the head's existing rollback test, minus the generation assertion.

Head (R1, abort): same fixture, then run `abort_staged_corpus_swap` with the live reopen parked (hold the per-path `open_serialized` gate for the live file, or the redb lock, as `a_deferred_promotion...` does with `CorpusStore::open(&live)`), and fire `index_file_report` while it is parked. Expected after the fix: the put is refused (non-2xx, `indexed: false`) OR is durable in live after reattach. Today: `indexed: true`, absent from live, generation rises only after the reply.

## 6. Tests that would catch it (error-arm regression tests, none exist at 66e11729bf)

All belong in `service/reindex/identity_10738_tests.rs` using its `staged_index`, `begin`, `resolve`, `put_generation` helpers.

1. `a_put_during_the_abort_detach_window_is_refused_or_lands_in_live` (R1, abort). Park the abort between `take_corpus_store` and `set_corpus_store`; put; assert refusal or presence in live after reattach. Fails today. Fix options: refuse puts when `corpus_ever_wired && corpus.is_none()` (the predicate at M:quarantine.rs:245), or keep one write guard across the take->set span.
2. `a_put_during_the_promotion_detach_window_is_refused_or_raises_the_generation` (R1, commit). Same park, on `commit_staged_corpus_swap` between `release_staging_for_promotion` and `set_corpus_store`. Fails today (no bump).
3. `an_abort_whose_live_reopen_fails_quarantines_and_raises_the_generation`. Hold the live file so `open_serialized` fails (abort's `Err` arm, H:corpus_swap.rs:749-752). Assert the generation rose past the put's reply, and that later puts are refused. Covers H:quarantine.rs:404, which the critic found untested.
4. `a_panicked_staged_run_is_covered_by_the_next_attach` (R2). Drop the run's task mid-batch (as the restart test does with drop + `forget_process_state`, but without the process reset), run `finish`-less, then assert the next reindex or attach raises the generation. Pins the delayed-signal behaviour.
5. Cheap main-side counterpart if the PR is not taken: `rollback_discards_a_put` in `service::reindex::tests`, asserting the put is absent after `Rollback`, so the loss is a recorded, intended behaviour.

## 7. Method and limits

- Read #10738 and #10806 first. The PR thread already says "yes" for the original head `ab02b10d51`; rounds 1 and 2 (`750d6b5c72`, `0c6fc83442`) closed rollback, memory abort, deferral, restart, carryover and prune. `66e11729bf` is the last commit; I traced its code, not the thread's claims.
- `mcp__trusty-search__search` could not be used: this repo's index is registered as `trusty-tools-4e2cf878`, not `trusty-tools`, and the first call with `trusty-tools` was refused. I then read by `git show`.
- Not traced: the HNSW/vector side (`resolve_hnsw_swap` runs before the corpus settle, M:finish.rs:306 vs :322), idle-evict rehydrate effect on a lost put, SIGTERM during `commit` itself, and the watcher/reconcile writers (same write core as put at H, per the PR).
- Nothing built, run, edited or committed. This file is the only write.
