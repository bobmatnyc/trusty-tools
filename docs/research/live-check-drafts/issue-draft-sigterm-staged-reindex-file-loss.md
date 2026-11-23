Run a duplicate check first. If it is new, file it in bobmatnyc/trusty-tools with the title below, as related to [#3979](https://github.com/bobmatnyc/trusty-tools/issues/3979) and [#9459](https://github.com/bobmatnyc/trusty-tools/issues/9459), and link it from the [#10738](https://github.com/bobmatnyc/trusty-tools/issues/10738) comment.

---

**Title:** `trusty-search: SIGTERM during a staged reindex can leave a file unindexed after resume`

## Problem

After a SIGTERM during a staged reindex and a restart, the resumed run treated `b.rs` as done and promoted it without chunks. `b.rs` stays unindexed until it is edited or a force reindex runs.

## Evidence

**Status (2026-11-22): FIXED between 86d738c4a8 and 245263a482. The likely fix is [#11095](https://github.com/bobmatnyc/trusty-tools/pull/11095) (#11087), merged as bbcd880051.**

- FAIL at 86d738c4a8: `docs/research/evidence/11019-11022-live-checks/11022-regress-11019-run2.log:67` (b.rs not served), `:77` (43 passed, 1 failed). Daemon log `docs/research/evidence/11019-11022-live-checks/11022-regress-11019-run2-TERM-daemon.log:119` resumed with `2 chunk(s) and 3 file hash(es)`; `:131` ended `files=2 indexed_new=0 skipped=2 chunks=0`. This is the hash-without-chunk state described below.
- PASS at 245263a482: `docs/research/evidence/11019-11022-live-checks/11087-postmerge-run1.log:67` (b.rs served), `:77` (44 passed, 0 failed). Daemon log `docs/research/evidence/11019-11022-live-checks/11087-postmerge-run1-TERM-daemon.log:120` resumed with `2 chunk(s) and 2 file hash(es)`; `:132` ended `files=2 indexed_new=1 skipped=1 chunks=1`. Hashes now match chunks.
- Not a script artifact: `scripts/live-checks/11019-midbatch.sh` is v3 (aada68f) in both runs, and `git diff f7bc5aa 120d4dc -- scripts` is empty. The probe-retry and restart-order change predates the FAIL run. The daemon-side resume line changed (3 hashes -> 2), which the script cannot affect.
- Shutdown behaviour is unchanged: both TERM daemon logs show `skipping flush ... background reindex is still in progress` and `1 were not closed within 3s` (`...TERM-daemon.log:65-66` and `:66-67`). The fix is on the staging-hash/chunk consistency path, not the shutdown path.
- Limits: one FAIL run and one PASS run, same script, different binaries (both report trusty-search 0.64.0). The commit range is not bisected, and no regression test reproducing the loss was checked.

**Recommendation: file as "fixed at 245263a482 (PR #11095)" for the record, with acceptance criterion 1 reduced to confirming a regression test exists on main; do not file as an open defect.**

From the [#11019](https://github.com/bobmatnyc/trusty-tools/pull/11019) live check run 2:

- TERM shutdown logged `skipping flush ... a background reindex is still in progress` and `1 were not closed within 3s and need repair on the next open ([#9459](https://github.com/bobmatnyc/trusty-tools/issues/9459))`.
- On restart it logged `took over leftover staging corpus` and `RESUMING from checkpoint ... 2 chunk(s) and 3 file hash(es) already staged`. That means 3 hashes (a.rs, b.rs, the pushed file) but only 2 chunks.
- The final pass logged `files=2 indexed_new=0 skipped=2 chunks=0`.
- This breaks the invariant "a staged hash implies its staged chunks" (`crates/trusty-search/src/service/reindex/checkpoint.rs:27-31` at [751559afac](https://github.com/bobmatnyc/trusty-tools/commit/751559afac)).
- The SIGKILL case resumed with 3 chunks and 3 hashes, which is consistent.
- Evidence file: `docs/research/evidence/11019-11022-live-checks/11019-run2-evidence.txt`. The relevant excerpt may be attached the relevant excerpt.

## Likely cause (hypothesis)

The unclean SIGTERM close of the staging corpus commits file hashes without their chunks.

## Second hypothesis (unconfirmed)

After prune removed the pushed chunk, `file.status` still reported it present. A detached idle-eviction rehydrate may restore a removed chunk into the in-memory chunk map (`file_status.rs` reads `indexer.file_chunk_ids`).

## Acceptance criteria

1. A failing test reproduces the loss: a SIGTERM-style unclean close during a staged reindex, after a file's hash is staged but before its chunks are durable, followed by resume. The test asserts the file is served after resume. It must fail on [751559afac](https://github.com/bobmatnyc/trusty-tools/commit/751559afac).
2. Resume never skips a file whose hash is staged without its chunks.
3. The `file.status` hypothesis is either confirmed with a test or ruled out.

## Related

[#3979](https://github.com/bobmatnyc/trusty-tools/issues/3979) (resume), [#9459](https://github.com/bobmatnyc/trusty-tools/issues/9459) (repair on open), [#10738](https://github.com/bobmatnyc/trusty-tools/issues/10738), [#11019](https://github.com/bobmatnyc/trusty-tools/pull/11019). Not caused by [#11019](https://github.com/bobmatnyc/trusty-tools/pull/11019).
