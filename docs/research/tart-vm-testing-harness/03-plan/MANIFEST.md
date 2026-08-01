# MANIFEST — `vmtest-harness/` implementation progress

**Format version:** 1
**Plan:** [01-implementation-plan.md](./01-implementation-plan.md) (DOC-3)
**Design:** [DOC-1](../02-design/01-vm-install-harness.md), [DOC-2](../02-design/02-harness-contracts.md)
**Location is deliberate:** this file lives beside the plan in `03-plan/`, **not**
inside `vmtest-harness/`. It records the history of building that directory and
must survive independently of it — including the case where a phase fails and the
directory is deleted and started again.

## Why this file exists

The plan is executed **autonomously**, in sessions that may end at any point. This
file is **the only durable progress record between sessions.** An agent resuming
work reads DOC-1, DOC-2, the plan, and then this file — and this file is the only
one of the four that tells it what has actually happened.

Treat it accordingly:

- **Observed results are pasted output, never claims.** "Checkpoint passed" is not
  an observed result. The terminal output is.
- **It is append-only in spirit.** Correct a wrong entry by adding a dated
  correction beneath it, not by deleting it. A record whose history is rewritten is
  a record nobody can audit — the same principle DOC-1 D2 applies to reversed
  design decisions.
- **A deviation is never omitted for being small.** The deviations field is where
  the next agent finds out why the code does not match the plan.

---

## Schema

### Summary table

One row per phase. Kept in sync with the phase sections below; the sections are
authoritative and the table is the index.

| Column | Values | Rule |
|---|---|---|
| `Phase` | `P1`…`P8` | Fixed. Matches the plan's phase numbers. |
| `State` | `not-started` \| `in-progress` \| `complete` \| `blocked` | See state rules below. |
| `Updated` | `YYYY-MM-DD` (UTC) | Date of the last change to this phase's section. |
| `Commit` | short SHA, or `—` | The commit whose tree the observed result was produced from. |

### Per-phase section

Every phase has a section with **exactly these seven fields, in this order**. A
field is never removed; an empty one carries its stated placeholder.

| Field | Content | Placeholder when empty |
|---|---|---|
| **State** | one of the four values | `not-started` |
| **Pass condition** | the checkpoint pass condition, **copied verbatim from the plan**. Copied rather than referenced so a stale plan and a stale record cannot silently agree. | (always populated from the start) |
| **Observed result** | the **actual output** of running the checkpoint: the command line, its output, its exit status, and the UTC date it was run. Pasted, not paraphrased. | `— not run` |
| **Files delivered** | every path created or modified, marked `create` / `modify` / `delete`. Repo-relative. | `— none` |
| **Measurements** | numeric findings this phase produced, each with the command that produced it. Several phases exist partly to produce these. | `— none` |
| **Deviations from plan** | anything done differently, with the reason, and the §F item if it resolves one. | `None.` |
| **Tasks** | task IDs completed, e.g. `P1-T1..P1-T7 complete; P1-T8 in progress`. | `— none complete` |

### State rules

- `not-started` → `in-progress` when the phase's **first task** is committed.
- `in-progress` → `complete` **only** when the checkpoint has been **run** and its
  output is pasted into `Observed result`. A phase is never `complete` on the
  strength of its tasks being done; the checkpoint is the gate.
- `in-progress` → `blocked` when a task cannot proceed and the plan does not
  resolve it. `blocked` **requires** a Deviations entry naming what is needed to
  unblock, and it halts the plan — a later phase does not start around it.
- `complete` → `in-progress` is legal (rework). Add a dated note; do not erase the
  previous observed result.

### The update rule

**Updating this file is the final numbered task of every phase in the plan** —
P1-T11, P2-T8, P3-T7, P4-T5, P5-T9, P6-T5, P7-T5, P8-T6. It is a task with an
acceptance check, not a convention and not a footnote. A phase whose final task has
not been completed is not complete, regardless of what its code does.

### Worked example of a filled section

```markdown
### Phase 1 — Transport spike

- **State:** complete
- **Pass condition:** `bash vmtest-harness/spike/spike-transport.sh` exits 0 and …
- **Observed result:** (run 2026-08-04 UTC)
  ```
  $ bash vmtest-harness/spike/spike-transport.sh; echo "exit=$?"
  READY after 33s
  N1 PASS (cargo=127 rustc=127 rustup=127)
  streamed 84930112 bytes / 5311 files
  trusty-search 0.40.0
  exit=0
  $ tart list | grep vmtest-spike | wc -l
         0
  ```
- **Files delivered:** create `vmtest-harness/spike/spike-transport.sh`;
  create `vmtest-harness/base-image.pin`
- **Measurements:** streamed bytes 84,930,112 (`… | wc -c`); …
- **Deviations from plan:** §F-10(d) resolved by narrowest reading — `tart-run.pid`
  reaped after `vm_wait_for_stopped` returned, not killed.
- **Tasks:** P1-T1..P1-T11 complete
```

---

## Summary

| Phase | State | Updated | Commit |
|---|---|---|---|
| **P1** — Transport spike (thin vertical slice) | `complete` | 2026-08-01 | `7df36745`, `c6b18e63` |
| **P2** — Host-side skeleton | `not-started` | — | — |
| **P3** — Guest bring-up | `not-started` | — | — |
| **P4** — Expectation table and `--check-table` | `not-started` | — | — |
| **P5** — Pattern (c) complete: installs, N2, oracle | `not-started` | — | — |
| **P6** — Pattern (b): branch | `not-started` | — | — |
| **P7** — Pattern (a): released | `not-started` | — | — |
| **P8** — Hardening, docs, measurement write-back | `not-started` | — | — |

**Plan status:** Phase 1 complete, 2026-07-31; **closed out 2026-08-01** with a
second observed result (the dirty-worktree validation) and two plan corrections.
`vmtest-harness/` **exists**. Phase 2 is the next phase to start.

> **A note on dates.** This file's schema mandates **UTC**. The 2026-08-01 entries
> below were produced at `2026-08-01 00:08–00:12 UTC`, which is `2026-07-31
> 20:08–20:12 EDT` on the same host that produced the 2026-07-31 entries about
> twenty minutes earlier. The record does not skip a day; it crosses midnight UTC.

**Open items carried into execution** (from DOC-1 §14 and DOC-2 open items):

- **RC-1 — unified daemon health envelope.** Does not exist. **Scoped around, not
  a blocker**: the oracle asserts **liveness only** for daemon health (plan P5-T7).
- **RC-2 — `tctl install` cargo-absent exit code.** Unpinned at
  `crates/trusty-installer/src/commands/install.rs:826`. **Pinned by plan P5-T2**;
  N2's predicate stays deliberately weak until then.
- **Full base-image digest.** ~~Placeholder.~~ **CLOSED 2026-07-31 by P1-T3.**
  `sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c`,
  captured untruncated from `tart list --format json` and committed as
  `vmtest-harness/base-image.pin`. DOC-2 §3.3's introspection branch applies; the
  **by-construction variant was not needed**. P2-T5 is unblocked.
- **Pattern (c) tar transport, end-to-end.** ~~Never measured.~~ **CLOSED
  2026-07-31 by Phase 1 — the transport WORKS.** 96,788,480 bytes / 5,337 files
  streamed host→guest through `git ls-files -co --exclude-standard | tar |
  tart exec -i` in 4 s, unpacked with an exact file-count match, and
  `trusty-search` built and installed from the unpacked tree in 105 s. DOC-1 §14's
  headline gap and devil's-advocate critique #9 are retired. DOC-1 D4's fallback
  re-ordering to (b) → (c) → (a) is **not** triggered.
- **Full-stack timing.** The 4–8 min figure is an extrapolation for six crates
  against what is now an eight-crate scope (widened twice on 2026-07-31: D2's
  reversal, then D3's `trusty-review` addition). **Replaced by plan P5-T8.**
- **Daemon time-to-ready.** Wholly unmeasured; DOC-2 §10.1's 60 s maximum is a
  guess. Revisited in P8-T2.
- **NEW, opened by Phase 1 — DOC-1 §6.1's payload figure is a *content* figure,
  not a *wire* figure.** The two differ by tar framing and the doc set does not
  currently distinguish them. See Phase 1 Measurements; written back in P8-T4.
- **Opened by Phase 1 — P2-T4's acceptance grep cannot pass while `spike/`
  exists.** ~~Phase 2 must resolve it.~~ **CLOSED 2026-08-01 by owner decision, at
  the plan.** P2-T4's grep is scoped with `--exclude-dir=spike`; the DOC-1 §3.2
  invariant is unchanged, only the search path. The exemption **expires at P3-T4**,
  whose acceptance now requires the argument to be deleted in the same commit that
  deletes the directory. See Phase 1 Deviations item 9.
- **Opened by Phase 1 — pattern (c)'s defining property is untested.** ~~The
  Phase 1 run streamed a **clean** worktree, so `-o` contributed zero files.~~
  **CLOSED 2026-08-01 — run against a dirty worktree, and the property HOLDS.**
  `spike-transport.sh --dirty-check` streamed a worktree carrying one modified
  tracked file, one untracked non-ignored file and one gitignored file. Observed in
  the guest: the tracked file's **working-tree** content arrived (whole-file `cksum`
  equal to the host's, sentinel as its last line); the untracked file arrived with
  its exact content; the gitignored file did **not** arrive, and its sentinel string
  occurs **zero** times anywhere in the delivered tree. DOC-1 §6.1's *"it includes
  uncommitted work"* is now observed rather than assumed, and DOC-1 D4's fallback
  re-ordering to (b) → (c) → (a) stays untriggered on this ground too. Full output
  in Phase 1 Observed result, run 2. **It was run one phase earlier than suggested**
  (here, not P3-T4/P5) because a property that is the *reason* pattern (c) was
  chosen should not be first tested by the code that depends on it.
- **NEW, opened 2026-08-01 — `--dirty-check` is not yet part of any checkpoint.**
  It is an opt-in mode of a script that **P3-T4 deletes**. The property it proves is
  a property of `source_deliver_local`, so P3-T4 should port the three sentinel
  assertions into a test of `lib/source.sh` rather than let them die with the spike.
  Deleting the spike without porting them would return this item to `open`.

---

## Phase 1 — Transport spike (thin vertical slice)

- **State:** `complete`
- **Pass condition:** `bash vmtest-harness/spike/spike-transport.sh` **exits 0** and
  its final three log lines report: (i) a streamed byte count greater than
  80,000,000; (ii) the guest's `trusty-search --version` output on stdout;
  (iii) `tart list` containing **no** `vmtest-spike-*` entry after teardown.
- **Observed result:** **MET.** (run 2026-07-31 UTC, tree `7df36745`, host: Apple
  M5 Pro, 18 physical cores, 64 GiB, macOS 26.5.2 arm64, tart 2.32.1.)

  The three checkpoint lines, verbatim, from the script's **stdout**:

  ```
  $ bash vmtest-harness/spike/spike-transport.sh > run.out 2> run.err; echo "EXIT=$?"
  $ cat run.out
  STREAMED_BYTES 96788480 FILES 5337
  trusty-search 0.40.0
  TART_LIST vmtest-spike-* entries after teardown: 0
  EXIT=0
  ```

  Condition by condition: (i) `96788480 > 80000000` — **met**; (ii)
  `trusty-search 0.40.0`, the guest's own `trusty-search --version` output —
  **met**; (iii) zero surviving entries — **met**, and independently confirmed
  below.

  The full run log (**stderr**), verbatim:

  ```
  [23:46:12] spike-transport.sh starting (pid 55130)
  [23:46:12] host repo: /Users/mac/workspace/trusty-tools-fork-worktrees/agent-a2706961fee0e64fa
  [23:46:12] --- P1-T1: host dependency set ---
  [23:46:12] tart  2.32.1
  [23:46:12] git version 2.50.1 (Apple Git-155)
  [23:46:12] jq    jq-1.7.1-apple
  [23:46:13] cargo 1.91.1 (ea2d97820 2025-10-10)
  [23:46:13] bash  3.2.57(1)-release
  [23:46:13] P1-T1 PASS (JQ_OK)
  [23:46:13] --- P1-T3: base-image pin ---
  [23:46:13] pin OK: ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c
  [23:46:13] P1-T3 PASS
  [23:46:13] --- P1-T2: clone, size, boot, poll ready ---
  [23:46:13] clone tahoe-base -> vmtest-spike-20260731T234613Z-55130
  [23:46:14] size --cpu 8 --memory 16384
  [23:46:14] boot (tart run --no-graphics, backgrounded)
  [23:46:32] READY after 18s
  [23:46:32] state: running
  [23:46:32] P1-T2 PASS
  [23:46:32] --- P1-T4: N1 precondition probe ---
  [23:46:33] N1 PASS (cargo=1 rustc=1 rustup=1)
  [23:46:33] --- P1-T5: provisioning ---
  mise WARN  mise version 2026.7.18 available
  mise WARN  mise version 2026.7.18 available
  [23:46:35] mise detected at /opt/homebrew/bin/mise (2026.6.0 macos-arm64 (2026-06-03)) — REUSED, not installed
  [23:46:36] gh detected at /opt/homebrew/bin/gh — REUSED, not installed
  [23:47:16] provisioning wall clock 40s (measured baseline PROVISION_MS=30079, i.e. 30.079s)
  [23:47:16] rustc from /Users/admin: rustc 1.91.1 (ed61e7d7e 2025-11-07)
  [23:47:16] P1-T5 PASS
  [23:47:16] --- P1-T6: THE SLICE — stream the worktree ---
  [23:47:16] host repo (read-only): /Users/mac/workspace/trusty-tools-fork-worktrees/agent-a2706961fee0e64fa
  [23:47:16] host file count (git ls-files -co --exclude-standard | wc -l): 5337
  [23:47:21] streamed 96788480 bytes in 4s
  [23:47:22] guest file count (find ! -type d): 5337
  [23:47:22] guest file count (find -type f, the plan's literal command): 5333
  [23:47:22] file counts match: G == H == 5337
  [23:47:22] target/ absent in guest, by construction
  [23:47:22] P1-T6 PASS
  [23:47:22] --- P1-T7: build trusty-search from the unpacked tree ---
  [23:47:22] rustc in crates/trusty-search: rustc 1.91.1 (ed61e7d7e 2025-11-07)
  [23:49:07] build+install wall clock 105s (measured baseline: 112s for trusty-search, 409 crates, 8 vCPU)
  [23:49:08] trusty-search installed at /Users/admin/.cargo/bin/trusty-search
  [23:49:09] trusty-search --version -> trusty-search 0.40.0
  [23:49:09] P1-T7 PASS
  [23:49:09] --- P1-T8: teardown and host-cleanliness assertion ---
  [23:49:09] teardown: vm_request_stop vmtest-spike-20260731T234613Z-55130
  [23:49:10] teardown: state 'stopped' observed 0s after vm_request_stop returned
  [23:49:11] teardown: deleted vmtest-spike-20260731T234613Z-55130
  [23:49:11] host clean: no vmtest-spike-* VM in tart list
  [23:49:11] P1-T8 PASS
  [23:49:11] === MEASUREMENTS (P1-T9) ===
  [23:49:11] boot_to_ready_s          18
  [23:49:11] provision_s              40
  [23:49:11] stream_s                 4
  [23:49:11] streamed_bytes           96788480
  [23:49:11] streamed_files           5337
  [23:49:11] build_install_s          105
  [23:49:11] stop_to_stopped_s        0
  [23:49:11] base_image_digest        sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c
  [23:49:11] total_wall_clock_s       179
  [23:49:11] === end measurements ===
  ```

  Independent host-cleanliness proof, run from a separate shell **after** the
  script exited — raw `tart list`, unedited:

  ```
  $ tart list
  Source Name                                                                                                        Disk Size Accessed      State
  local  tahoe-base                                                                                                  50   33   4 minutes ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago   stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago   stopped
  ```

  No `vmtest-spike-*` entry survives. The three pre-existing images are byte-for-byte
  the same rows as before the run (`Disk 50`, `Size 33`/`32`, `stopped`); only
  `tahoe-base`'s `Accessed` timestamp moved, which is what an APFS CoW clone does
  to its source. The base image was **not** modified, re-pulled or re-tagged.

  P1-T3 acceptance, separately:

  ```
  $ grep -Eq '^digest<TAB>sha256:[0-9a-f]{64}$' vmtest-harness/base-image.pin && echo PIN_REAL
  PIN_REAL
  ```
- **Observed result — ADDITIONAL, run 2 of 2: the dirty-worktree validation.**
  (run 2026-08-01 UTC, tree `c6b18e63`, same host.) **Added 2026-08-01; the run-1
  result above is unchanged.** This is the run the previous session's open item
  asked for. It closes the question of whether pattern (c) actually delivers
  uncommitted work — the property the clean run could not test.

  **What was asserted, and what was observed.** Three fixtures with distinct
  sentinels, all three under `vmtest-harness/spike/` so they cannot collide with a
  real path. The host's own classification of them, logged immediately before the
  stream, so the test is provably non-vacuous at both ends:

  ```
  $ git status --porcelain --ignored -- <the three fixtures>
   M vmtest-harness/spike/dirty-check-fixture.txt
  ?? vmtest-harness/spike/dirty-check-untracked.txt
  !! vmtest-harness/spike/target/
  ```

  | # | Fixture | Git state | Exercises | Expected | **Observed** |
  |---|---|---|---|---|---|
  | 1 | `spike/dirty-check-fixture.txt` (sentinel appended) | tracked, **modified** (` M`) | `-c` reading **working-tree** content, not `HEAD`'s | PRESENT | **PRESENT** |
  | 2 | `spike/dirty-check-untracked.txt` | **untracked**, not ignored (`??`) | the `-o` half — which contributed **0** files to run 1 | PRESENT | **PRESENT** |
  | 3 | `spike/target/dirty-check-ignored.txt` | **ignored** (`!!`, via `**/target/`) | the `--exclude-standard` half | **ABSENT** | **ABSENT** |

  The in-guest assertions, verbatim from the run log (**stderr**):

  ```
  [00:10:38] --- P1-T6b: dirty-worktree assertions (pattern (c) defining property) ---
  [00:10:38] sentinel 1 PRESENT (tracked, modified): VMTEST_DIRTY_SENTINEL_TRACKED_20260801T001032Z_85125
  [00:10:39] sentinel 1 content matches host exactly (cksum 4176744393 641)
  [00:10:39] sentinel 2 PRESENT (untracked, not ignored): VMTEST_DIRTY_SENTINEL_UNTRACKED_20260801T001032Z_85125
  [00:10:39] sentinel 3 ABSENT (gitignored path not present): /Users/admin/vmtest-src/vmtest-harness/spike/target/dirty-check-ignored.txt
  [00:10:39] sentinel 3 ABSENT (its ignored parent directory not present either)
  [00:10:40] sentinel 3 ABSENT (grep -rl over the whole delivered tree found 0 occurrences)
  [00:10:40] dirty run vs clean run (2026-07-31, tree 7df36745):
  [00:10:40]   streamed_bytes  96819200  (clean 96788480, delta 30720)
  [00:10:40]   streamed_files  5339  (clean 5337, delta 2)
  [00:10:40] P1-T6b PASS — pattern (c) delivers uncommitted work and still excludes ignored paths
  [00:10:40] fixtures restored: git status --porcelain is empty
  ```

  **stdout**, with the checkpoint still the final three lines:

  ```
  $ bash vmtest-harness/spike/spike-transport.sh --dirty-check > d.out 2> d.err; echo "EXIT=$?"
  $ cat d.out
  DIRTY_CHECK sentinel1=PRESENT sentinel2=PRESENT sentinel3=ABSENT bytes=96819200 files=5339 (clean run 96788480/5337)
  STREAMED_BYTES 96819200 FILES 5339
  trusty-search 0.40.0
  TART_LIST vmtest-spike-* entries after teardown: 0
  EXIT=0
  ```

  **Assertions 1 and 2 are on content, not presence.** Sentinel 1 is checked as
  the guest copy's **last line** *and* by whole-file `cksum` equality against the
  host (`4176744393 641`, identical both ends), so a transfer sourced from
  `git archive HEAD` — which passes every file-count check in this phase — fails
  here. Sentinel 2 is compared to its exact expected string. Sentinel 3's negative
  is asserted **three independent ways**: the path is absent, its ignored parent
  directory is absent, and `grep -rl` over the entire 92 MB delivered tree finds
  **zero** occurrences of the string.

  **Why sentinel 3 matters as much as 1 and 2.** `--exclude-standard` is what makes
  `-o` safe. Without it `-o` enumerates `target/` and the payload goes from ~92 MB
  to tens of GB. The pre-existing `test -d /Users/admin/vmtest-src/target` check is
  weaker than it looks — it passes **vacuously** on a host that has never built.
  Sentinel 3 cannot pass vacuously, because the file is created by the run.

  **Counts, decomposed** (both differences are fully accounted for; nothing is
  unexplained):

  | | Run 1, clean (`7df36745`) | Run 2, dirty (`c6b18e63`) | Delta |
  |---|---|---|---|
  | streamed **files** | 5,337 | **5,339** | **+2** |
  | streamed **bytes** (wire) | 96,788,480 | **96,819,200** | **+30,720** |

  `+2` files = **+1 tracked** (`dirty-check-fixture.txt`, committed at `c6b18e63`
  so that `-c` can list it) **+1 untracked** (the `??` fixture, which is the `-o`
  half doing work for the first time). Verified independently after the run:
  `git ls-files | wc -l` → **5338** and `git ls-files -o --exclude-standard | wc -l`
  → **0** (the fixture having been restored), i.e. `5338 + 1 = 5339` during the run.
  The ignored fixture contributes **0**, which is the point. `+30,720` B = two new
  files at 512-byte tar granularity plus one appended sentinel line — 30 tar blocks,
  consistent with the framing analysis in Measurement 1a below.

  **Fixture hygiene — verified, not assumed.** Restore runs from the same trap
  chain that tears the VM down, and runs **before** the VM teardown so a VM that
  refuses to stop cannot also cost the host its worktree. Both the explicit call and
  the trap call are idempotent. The run's own check and an independent one after it:

  ```
  [00:10:40] fixtures restored: git status --porcelain is empty

  $ git status --porcelain          # separate shell, after the script exited
                                    # (no output)
  $ git ls-files | wc -l
      5338
  $ git ls-files -o --exclude-standard | wc -l
         0
  ```

  A non-empty `git status --porcelain` after restore sets `FIXTURE_RESTORE_FAILED`
  and the run **dies 70** at P1-T8. It is a failure condition, not a warning.

  Independent host-cleanliness proof, raw `tart list`, unedited, from a separate
  shell after the script exited:

  ```
  $ tart list
  Source Name                                                                                                        Disk Size Accessed      State
  local  tahoe-base                                                                                                  50   33   4 minutes ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago   stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago   stopped
  ```

  No `vmtest-spike-*` entry survives. Same three pre-existing rows, same `Disk`/
  `Size`/`State`; only `tahoe-base`'s `Accessed` moved, which is what an APFS CoW
  clone does to its source.

  **Divergence recorded — provisioning took 97 s, versus 40 s in run 1 and the
  30.079 s measured baseline (3.2×).** The script logged its own
  `NOTE: provisioning exceeded 3x the measured 30.079s baseline` and continued;
  P1-T5's acceptance bound is 3×, so **this run sits just outside it**. It is
  recorded rather than smoothed over. Every other number moved the *other* way or
  not at all (boot 17 s vs 18 s; stream 3 s vs 4 s; build 103 s vs 105 s), and
  provisioning is the one step that fetches over the network (`mise use -g
  rust@1.91`, `mise use -g uv@latest`), so network variance is the obvious
  candidate — but **two data points are not a distribution**, and this is exactly
  the input P8-T2 needs when it grounds the timeouts. Nothing about it bears on the
  transport, which is what this run tested.

  Other run-2 measurements, for the record: `boot_to_ready_s 17`,
  `stream_s 3`, `build_install_s 103`, `stop_to_stopped_s 1`,
  `total_wall_clock_s 236`, same base-image digest.
- **Files delivered:**
  - create `vmtest-harness/base-image.pin`
  - create `vmtest-harness/spike/spike-transport.sh`
  - *(2026-08-01)* modify `vmtest-harness/spike/spike-transport.sh` — `--dirty-check`
  - *(2026-08-01)* create `vmtest-harness/spike/dirty-check-fixture.txt` — the
    **tracked** fixture; it must be committed for `git ls-files -c` to list it.
    Deleted with the rest of `spike/` at P3-T4.
  - *(2026-08-01)* modify `docs/research/tart-vm-testing-harness/03-plan/01-implementation-plan.md`
    — the P1-T6, P2-T4 and P3-T4 corrections
  - modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`
- **Measurements:** all six the plan asks for (P1-T9), plus the digest. Each is
  the value the script logged, with the command that produced it.

  | # | Measurement | Value | Command / source | Compared against |
  |---|---|---|---|---|
  | 1a | **streamed byte count** | **96,788,480 B** (92.3 MiB) | `git ls-files -co --exclude-standard -z \| tar -cf - --null -T - \| dd bs=1048576 \| tart exec -i …`; `dd`'s `bytes transferred` | DOC-1 §6.1's **~81 MiB** estimate — see the note below, they measure different things |
  | 1b | **streamed file count** | **5,337** | `git ls-files -co --exclude-standard \| wc -l` (host) and `find /Users/admin/vmtest-src ! -type d \| wc -l` (guest) — **equal** | DOC-1 §6.1's 5,306 (`git archive`, tracked only) |
  | 2 | **boot to ready** | **18 s** | poll `tart exec <vm> /bin/sh -c 'exit 0'` @ 2 s, from `tart run` | DOC-2 §10.1: 34.4 s first boot, **18.0 s subsequent** — this is a subsequent boot and it lands on the measured value exactly |
  | 3 | **provisioning** | **40 s** | wall clock across mise detect + `mise use -g rust@1.91` + `mise use -g uv@latest` + `~/.zshenv` | measured `PROVISION_MS=30079`. **1.33×** — inside P1-T5's 3× acceptance bound |
  | 4 | **`trusty-search` build + install** | **105 s** | `cargo install --path /Users/admin/vmtest-src/crates/trusty-search` under the §7.3 prelude, watchdogged at 900 s | measured **112 s** (409 crates, 8 vCPU, `SKIP_UI_BUILD=1`). **0.94×** |
  | 5 | **`vm_request_stop` → `stopped`** | **< 1 s** (logged `0 s`) | `date +%s` delta across `vm_request_stop` returning → first `tart list` reporting `stopped` | DOC-2 §10.1's **120 s** maximum. See the note below |
  | 6 | **base-image digest** | `sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c` | `tart list --format json \| jq -r '.[] \| .Name'` → the `…@sha256:…` OCI row | research had it **truncated only** (`sha256:a8e1…`, `vm-install-probe-findings.md:652`, `:685`) |

  Also recorded: transport throughput **≈24 MB/s** (96,788,480 B in 4 s), and
  **total run wall clock 179 s** (clone → teardown, single crate).

  **Note on 1a — DOC-1 §6.1's figure is a *content* figure; the streamed figure is
  a *wire* figure. The doc set does not distinguish them, and it should.** The raw
  content of the delivered file set measures **81,762,761 B (78.0 MiB)** across the
  same 5,337 files (`git ls-files -co --exclude-standard -z | xargs -0 stat -f '%z'
  | awk '{s+=$1} END {print s}'`), which is very close to DOC-1 §6.1's ~81 MiB. The
  **96,788,480 B actually crossing the pipe** is that content plus **15,025,719 B
  (+18.4%) of `tar` framing** — a 512-byte header per entry plus 512-byte block
  padding, which is large in relative terms precisely because this repo is many
  small files. So DOC-1 §6.1 was not wrong about the payload; it was answering a
  different question from the one it asked the implementation to answer. Both
  numbers clear the checkpoint's 80,000,000 threshold, but the content figure only
  just does — an implementation that had counted content bytes instead of wire
  bytes would have passed by 2%, on a quantity the pass condition does not name.
  P8-T4 should write back **both**, labelled.

  **Note on 1b — CORRECTION. The `-o` hypothesis is wrong, and this run did not
  test it at all.** The first draft of this entry attributed the 5,337 vs 5,306
  delta to `-o` adding untracked-but-not-ignored files, which is the mechanism
  DOC-1 §6.1 predicts. **That is false for this run.** Measured after the fact:

  ```
  $ git ls-files | wc -l                          # tracked only
      5337
  $ git ls-files -o --exclude-standard | wc -l     # untracked, not ignored
         0
  $ git status --short                             # (no output)
  ```

  The delivery worktree was **clean**, so `-o` contributed **zero** files and the
  streamed set was exactly the tracked set. The +31 over 5,306 is simply repository
  growth since the research measured it. **Consequence: DOC-1 §6.1's stated concern
  — that its figure is a "lower bound / close proxy" because `-o` adds files
  `git archive` never sees — remains UNTESTED.** The one property that most
  distinguishes pattern (c) from pattern (b), *"it includes uncommitted work"*
  (DOC-1 §6.1), was **not exercised by this run**, because there was no uncommitted
  work to include. Phase 3's promotion of this pipeline (P3-T4) or Phase 5 should
  deliberately run it against a dirty worktree at least once.

  Recorded as a correction rather than a silent edit, per this file's append-only
  rule — and because it is the same failure mode §F-3 catalogues: reasoning from a
  plausible mechanism instead of running the command.

  > **Follow-up, 2026-08-01 — the untested property has now been tested, and it
  > holds.** The paragraph above stands exactly as written about **run 1**: that run
  > streamed a clean worktree and `-o` contributed zero files. What it asked for —
  > *"deliberately run it against a dirty worktree at least once"* — was done the
  > same night, one phase earlier than it suggested. See **Observed result, run 2**.
  > `-o` contributed **1** file, `-c` was observed carrying **working-tree** content
  > rather than `HEAD` content, and `--exclude-standard` was observed **excluding**.
  > DOC-1 §6.1's "lower bound / close proxy" caveat is now grounded in a measurement
  > instead of an argument, which is the whole of the §F-3 lesson.

  **Note on 5 — the interval is below the harness's own observational floor, and
  that is the finding.** `vm_wait_for_stopped`'s **first** poll — issued
  immediately after `vm_request_stop` returned, before any `sleep` — already
  observed `stopped`. So the true interval is bounded above by the duration of one
  `tart list --format json | jq` round-trip, and cannot be resolved more finely by
  a poll whose interval is 1 s (DOC-2 §10.1). DOC-2 §10.1 labelled its 120 s
  maximum "a **judgment call** … worst-case flush duration was never measured";
  it is now known to be conservative by **more than two orders of magnitude** in
  the nominal case, and there is no reason to tighten it — an unnecessarily loose
  bound on a path that never approaches it costs nothing, and DOC-2 §10.3's
  "no retry, ever" means the budget is only ever spent on a genuinely stuck VM.
  **This does not contradict the research's K1/K1b/K1c asynchrony finding**, which
  was about *durability* — "the state flag is not a durability flag"
  (`vm-install-probe-findings.md:814-817`) — not about how fast the flag flips.
  One run is one data point; it is not a distribution.

  **Finding — N1's observed exit code is 1, not 127, and DOC-2 §6.2 called it.**
  All three of `cargo`, `rustc`, `rustup` returned **exit 1** with empty stdout
  under `command -v` at the measured base PATH. DOC-2 §6.2 wrote: *"127 is what the
  research measured for an absent `cargo` … `command -v` itself returns **1** on
  not-found; the harness asserts **non-zero**, which both satisfy … pinning a code
  that was measured for a different command would be exactly the kind of false
  precision this doc set avoids."* A harness that had pinned 127 would have failed
  **every run**. Recorded because the reasoning was right for a reason that is now
  observed rather than argued.

  **Finding — the transport imposes no build-time penalty.** The 105 s build ran
  against a tree delivered by tar-over-`tart exec -i`; the 112 s baseline (K3) ran
  against a tree delivered by guest-side `git clone`. Same crate, same sizing, and
  the tar-delivered build was marginally *faster*. The transport is not a
  build-performance risk, only — until now — a correctness one.

  **Added 2026-08-01 — run 2's numbers are in Observed result, run 2**, with the
  `+2` files / `+30,720` bytes fully decomposed and the 97 s provisioning outlier
  recorded. The table above is **run 1's** and is left as measured; a second run is
  a second data point, not a replacement for the first. Both are inputs to P8-T2
  and P8-T4.
- **Deviations from plan:**
  1. **P1-T6 acceptance: the guest file count is asserted on `find … ! -type d`,
     not the plan's literal `find … -type f`.** This repo carries **4 tracked
     symlinks** (`git ls-files -s | awk '$1=="120000"' | wc -l` → 4), which `tar`
     transfers correctly as symlinks and which `-type f` does not count. The
     plan's literal command therefore reports `G = H − 4` on a **perfectly correct
     transfer** and would fail its own acceptance. Both counts are computed and
     logged every run (`5337` and `5333`); the equality assertion uses the
     comparable set. **The plan's check is wrong as written, not merely
     inconvenient** — P3-T4 should carry `! -type d` into `lib/source.sh`.

     > **Resolved 2026-08-01 — the plan is now corrected at source, so this stops
     > being a deviation.** P1-T6's acceptance block in
     > [01-implementation-plan.md](./01-implementation-plan.md) now reads
     > `find /Users/admin/vmtest-src ! -type d | wc -l`, with a dated correction
     > stating **why** so that nobody reverts it as a typo, and P3-T4's acceptance
     > now explicitly requires the corrected form to be ported into
     > `lib/source.sh`. Run 2 logged `5339` / `5335`, the same 4-file symlink gap.
  2. **Byte counting uses `dd` as a pipeline element, not `tee >(wc -c)`.** The
     plan says only "count the bytes crossing the pipe". A process substitution's
     writer is not synchronised with the pipeline's return, so `tee >(wc -c >file)`
     races the read of `file`; `dd` is *in* the pipeline, so its
     `bytes transferred` total is complete before the pipeline exits. No new host
     dependency — `dd` is base macOS, and DOC-2's host set (`tart`, `git`, `jq`,
     `cargo`, bash ≥ 3.2) is unchanged.
  3. **P1-T3 also exercises DOC-2 §3.3's comparison and §3.2's unknown-key rule,
     one phase early.** The plan asks only that the pin file be written. The spike
     additionally reads it with the shared `awk` TSV reader, rejects unknown keys,
     refuses §3.2's placeholder digest by name, and queries `tart list` for
     `<oci_ref>@<digest>`. Purely additive; it de-risks P2-T5 and it is what
     produced the `pin OK:` line above. **The by-construction variant was not
     needed** — `tart list --format json` exposes the untruncated digest as the
     `Name` field of the OCI row, which DOC-2 §3.3 flagged as a genuine unknown and
     which is now answered.
  4. **FORWARD CONFLICT, opened here for Phase 2: P2-T4's acceptance grep cannot
     pass while `spike/` exists.** P2-T4 requires
     `grep -rln 'tart' vmtest-harness --include='*.sh' --include='vmtest'` to list
     **only** `lib/vm.sh`. `spike/spike-transport.sh` necessarily contains `tart`
     and is only deleted at **P3-T4**, one phase later. As written the two tasks
     are unsatisfiable together. **Not resolved here** — the stop rule applies and
     this is a plan defect, not an implementation decision. Suggested narrowest
     readings, for whoever owns Phase 2: exclude `spike/` from the grep with a
     comment naming P3-T4, **or** move the spike deletion forward to P2-T4. Do not
     weaken the invariant itself; it is DOC-1 §3.2.

     > **Resolved 2026-08-01 by owner decision — the first of the two suggested
     > readings.** P2-T4's grep is scoped with `--exclude-dir=spike`. The invariant
     > is untouched: DOC-1 §3.2 still says exactly one file in the production tree
     > may name the OS. What is scoped is the **search path**, not the rule.
     > **The exemption expires at P3-T4**, and that is enforced from both ends —
     > P2-T4's correction names P3-T4 as the expiry, and P3-T4's acceptance now
     > requires the `--exclude-dir=spike` argument to be deleted **in the same
     > commit** that deletes `vmtest-harness/spike/`, after which the scoped and
     > unscoped greps are the same command. Verified on the tree as it stands:
     > unscoped → `vmtest-harness/spike/spike-transport.sh`; scoped → empty.
  5. **§F-10(c) applied one phase early — the spike writes `~/.zshenv`.** DOC-2
     §11.4's rule (write it, never read it) is honoured with the reconciliation
     stated in a comment at the write site, as §11.4 requires. Nothing in the
     script reads, sources, or conditions on it.
  6. **P1-T5 also detects `gh`.** DOC-2 §11.2's table lists `gh` as detect-and-reuse
     alongside `mise`; P1-T5's prose names only `mise`. Detected at
     `/opt/homebrew/bin/gh` and reused, never installed. Additive.
  7. **The spike implements DOC-2 §10.4 watchdogs (provisioning 300 s, install
     900 s) that Phase 1 does not require.** Built from `kill -0` polling, no
     `timeout(1)`/`gtimeout`. Included because an unbounded hang would defeat the
     teardown guarantee under an externally imposed kill. Additive.
  8. **Task execution order within the phase: P1-T1 → P1-T3 → P1-T2 → P1-T4 …**
     P1-T3's `Depends` line says P1-T2, and the *digest capture* it describes does
     require a live `tart`. But `tart list` needs no VM, and the digest was already
     captured read-only before this phase began (see Measurement 6). Verifying the
     pin **before** cloning is strictly safer: a drifted base image is refused
     before a VM exists rather than after. No task's actual dependency is violated.

  9. **NEW, 2026-08-01 — two plan defects corrected at source, in the doc set's
     dated-amendment style.** Both made a task fail on **correct** work, which is
     why they are corrections to the plan rather than deviations from it.
     - **P1-T6's acceptance command:** `find … -type f` → `find … ! -type d`. See
       deviation 1 above for the mechanism (4 tracked symlinks) and the resolution
       note for what changed. The correction states its reason inline so it is not
       reverted as a typo, and P3-T4 now has to carry it forward.
     - **P2-T4's acceptance grep:** scoped with `--exclude-dir=spike`, expiring at
       P3-T4. See deviation 4 above. **Owner decision**, not an implementation
       choice — the previous session correctly applied the stop rule and left it
       open rather than inventing a contract.

     Both are recorded here *and* in the plan, because this file is the durable
     record of **why** the plan changed and the plan is the record of **what** it
     now says.
  10. **NEW, 2026-08-01 — `--dirty-check` is an opt-in mode, not a change to the
     default run.** The spike's default behaviour is byte-for-byte what run 1
     executed and still **never mutates the host worktree**; the dirty-worktree
     validation is reached only via an explicit flag. This is deliberate. The
     fixture mechanism is the one thing in the whole spike that writes outside the
     ephemeral VM, so it is (a) off unless asked for, (b) held to the same trap
     discipline as the VM, restoring **before** teardown so a VM that refuses to
     stop cannot also cost the host its worktree, and (c) fatal on failure — a
     non-empty `git status --porcelain` after restore exits **70**, it is not a
     warning. The plan does not ask for this mode; it is additive, and it is what
     closed the open item the previous run logged.

  **Not deviations, recorded so the next agent does not re-litigate them:** §F-9
  is resolved at source, so `vm_request_stop` was implemented as specified with no
  decision to make; the plan's own note that P1-T3 "reduces to recording and
  verifying" the already-captured digest was followed; **P1-T10 is `N/A —
  transport verified`**, so no `blocked` state and no product-owner sign-off is
  pending, and DOC-1 D4's (b)-first fallback is **not** invoked.
- **Tasks:** P1-T1 … P1-T11 complete. (P1-T10 recorded `N/A — transport verified`.)
  **2026-08-01: no new task IDs.** The dirty-worktree validation is additional
  evidence for **P1-T6**, whose *Do* clause already claimed the file set "**includes
  uncommitted work**" — run 2 is the first run that tested that clause. Task IDs are
  stable (plan §B); this is not P1-T12.

## Phase 2 — Host-side skeleton: driver, config, registry, `lib/vm.sh`, preflight, `clean`

- **State:** `not-started`
- **Pass condition:** all three hold, in one session —
  1. `vmtest run local --dry-run` **exits 0**, prints an effective-configuration
     banner in which every key carries an origin marker (`default` / `env` /
     `flag`), and `tart list` afterwards shows **no new VM**.
  2. `VMTEST_CPU=4 vmtest run local --dry-run` prints `cpu 4 (env)`, and
     `vmtest run local --cpu 2 --dry-run` prints `cpu 2 (flag)`.
  3. `vmtest clean --dry-run` correctly classifies a hand-created stopped
     `vmtest-*` VM as `ORPHANED (would delete)` and a `keep`-marked one as
     `KEPT (would not delete)`, deleting neither.
- **Observed result:** — not run
- **Files delivered:** — none
- **Measurements:** — none
- **Deviations from plan:** None. *(Expected entries: §F-1 `run --dry-run`
  definition; §F-5 TSV-reader placement. §F-9's shutdown initiator is **no longer a
  deviation to record** — `vm_request_stop` is specified in DOC-2 §12.2 as of
  2026-07-31.)*
- **Tasks:** — none complete *(P2-T1 … P2-T8)*

## Phase 3 — Guest bring-up: N1, provisioning, toolchain hand-off, source delivery

- **State:** `not-started`
- **Pass condition:** `vmtest run local` **exits 0**, and its log shows, in order:
  `N1 PASS` with a non-zero exit recorded for each of `cargo`, `rustc`, `rustup`; a
  provisioning block ending with `rustc_version 1.91.1`; a streamed byte count
  > 80,000,000; and a teardown after which `tart list` contains **no** `vmtest-*`
  entry. `$VMTEST_RUNDIR` is removed, and
  `ls "${VMTEST_STATE_DIR:-$HOME/.local/state/vmtest-harness}/runs/"` is empty.
- **Observed result:** — not run
- **Files delivered:** — none
- **Measurements:** — none *(expected: subsequent-boot ready time, for comparison
  with P1's first boot and the measured ~18 s)*
- **Deviations from plan:** None. *(Expected entry: §F-4 negative-probe module
  placement.)*
- **Tasks:** — none complete *(P3-T1 … P3-T7)*

## Phase 4 — `expected-binaries.tsv` and `--check-table`

- **State:** `not-started`
- **Pass condition:** `vmtest --check-table` **exits 0** against the workspace as it
  stands, printing no ADDED/REMOVED/CHANGED findings. Then, with one row
  deliberately deleted from `expected-binaries.tsv`, it **exits 60** and prints
  exactly one `REMOVED` finding naming that `(package, binary)` pair. The row is
  restored afterwards and the command exits 0 again.
- **Observed result:** — not run
- **Files delivered:** — none
- **Measurements:** — none
- **Deviations from plan:** None. *(Expected entry: any workspace drift from DOC-2
  §9.3's seed found by P4-T3. **§F-3 deduplication is no longer a deviation to
  record** — it was resolved on 2026-07-31: the decision was always right, its
  rationale was corrected, and P4-T4's and P5-T8's tripwires now enforce it.)*
- **Tasks:** — none complete *(P4-T1 … P4-T5)*

## Phase 5 — Pattern (c) complete: install steps, N2, and the full oracle

- **State:** `not-started`
- **Pass condition:** `vmtest run local` **exits 0**, and the run log shows:
  Counts below are **derived**, with today's value as the expected literal; if the
  TSV has changed, the derivation is the condition and the literal follows it.
  (i) one `cargo install --path` per value of `tsv_scope_crate_dirs` (**8** today),
  and no directory installed twice, each preceded by a `rustc --version` line
  emitted from inside that crate's directory;
  (ii) `verify_binaries` reporting **N/N in-scope binaries present**, where N is the
  count of `in_scope=yes` rows (**13** today);
  (iii) `tctl stack doctor --json` parsed, with every one of `tsv_scope_packages`'
  values (**8** today) — **including `trusty-mpm`** — satisfying
  `health ∈ {healthy, stale}`, `on_path == true`, `version != null`;
  (iv) one `verify_single_install` passing per multi-binary in-scope package
  (**4** today): `trusty-search` (2 binaries), `trusty-memory` (**3**),
  `trusty-installer` (2), and `trusty-mpm` (2);
  (v) N2 recorded with its observed exit code and stderr;
  (vi) a total wall clock, logged, which is recorded here as the **first full-stack
  measurement**.
- **Observed result:** — not run
- **Files delivered:** — none
- **Measurements:** — none *(expected: **the first full-stack wall clock**, which
  replaces DOC-1 §9's 4–8 min extrapolation; **RC-2's observed exit code and
  stderr** from P5-T2; the RC-1 / §F-7 daemon-liveness disposition)*
- **Deviations from plan:** None. *(Expected entries: §F-7 daemon start and port
  discovery, including the BLOCKED branch if it fires. The fourth
  `verify_single_install` call for `trusty-mpm` is **no longer a deviation to
  record** — DOC-2 §12.5's skeleton was amended at source on 2026-07-31 and carries
  it. Neither is §F-2's `tsv_version` contradiction — DOC-2 §1.2 was amended at
  source on the same date.)*
- **Tasks:** — none complete *(P5-T1 … P5-T9)*

## Phase 6 — Pattern (b): branch

- **State:** `not-started`
- **Pass condition:** `vmtest run branch` **exits 0** with the **same derived binary
  and package assertions as Phase 5** — N/N where N is the count of `in_scope=yes`
  rows (**13** today), over `tsv_scope_packages`' values (**8** today) — and the run
  log shows a
  guest-side `git clone` (no host→guest byte stream) and the checked-out branch
  name.
- **Observed result:** — not run
- **Files delivered:** — none
- **Measurements:** — none *(expected: guest `git clone` duration for comparison
  with the measured `GIT_CLONE_MS=50131`; total wall clock beside Phase 5's — the
  first side-by-side comparison of the two transports)*
- **Deviations from plan:** None. *(Record here if `install-branch.sh` needed
  anything beyond a different step 1 and pattern letter — that would mean the
  scenario abstraction leaked, which is a finding.)*
- **Tasks:** — none complete *(P6-T1 … P6-T5)*

## Phase 7 — Pattern (a): released

- **State:** `not-started`
- **Pass condition:** `vmtest run released` **exits 0**, and the run log shows one
  `cargo install <pkg> --locked` invocation per value of `tsv_scope_packages`
  (**8** today) — including **`cargo install tga --locked`**, **`cargo install
  trusty-mpm --locked`** and **`cargo install trusty-review --locked`** — followed
  by `verify_binaries` reporting **N/N present**, where N is the count of
  `in_scope=yes` rows (**13** today), with `tm` and `trusty-mpm` explicitly among
  them, and `tctl stack doctor --json` reporting `trusty-mpm` as installed.
- **Observed result:** — not run
- **Files delivered:** — none
- **Measurements:** — none *(expected: total wall clock; the published versions
  installed, which will legitimately differ from the working tree)*
- **Deviations from plan:** None. *(If `cargo install trusty-mpm --locked` fails
  because the crate is not on crates.io, record it verbatim and stop — that
  contradicts both `cargo search trusty-mpm` → `1.0.2` and a manifest with no
  `publish` key, and is a design-level finding about the D2 reversal, not a harness
  bug.)*
- **Tasks:** — none complete *(P7-T1 … P7-T5)*

## Phase 8 — Hardening, documentation, and measurement write-back

- **State:** `not-started`
- **Pass condition:** all four hold — (i) the `~/.zshenv` deletion drill passes:
  every assertion still passes with the file removed mid-run; (ii)
  `vmtest.defaults` timeouts are grounded in Phase 5–7 measurements, each with a
  comment naming the measurement; (iii) `vmtest-harness/README.md` exists and a
  reader who has never seen the doc set can run `vmtest run local` from it alone;
  (iv) `git grep -n 'publish = false' docs/research/tart-vm-testing-harness/`
  returns **no** claim that `trusty-mpm` is unpublished.
- **Observed result:** — not run
- **Files delivered:** — none
- **Measurements:** — none
- **Deviations from plan:** None. *(No entry expected for §F-8: the
  `02-design/README.md` correction was made at source on 2026-07-31, and P8-T5 is
  now a verification check that should deliver no diff.)*
- **Tasks:** — none complete *(P8-T1 … P8-T6)*

---

## Appendix — resuming work

A future agent picking this up, in order:

1. Read [DOC-1](../02-design/01-vm-install-harness.md) and
   [DOC-2](../02-design/02-harness-contracts.md) in full. **Do not re-litigate a
   settled decision**; note in particular that D2/D3 were **reversed on
   2026-07-31** — `trusty-mpm` is published at v1.0.2, pattern (a) covers all
   **seven** crates, and `tm` is asserted **present**, not absent. Note that **D3
   was widened again the same day** — `trusty-review` was added by owner decision,
   so the scope is **eight** crates and **thirteen** in-scope binaries (plan §A.1b).
   A doc that says "seven" is recording the state between the two amendments.
2. Read [the plan](./01-implementation-plan.md), including **§F** — the flagged
   under-specifications and their decision rules. **Ten were flagged; four (§F-2,
   §F-3, §F-8, §F-9) are resolved and six remain open.** If you hit a decision the plan
   and DOC-2 do not settle and §F does not cover, **stop and record it here**
   rather than inventing a contract.
3. Read this file's summary table. Start at the first phase that is not `complete`.
   If any phase is `blocked`, resolve that first — the plan does not route around a
   blocked phase.
4. Verify before trusting. This file records what was true when it was written; the
   repo records what is true now. If they disagree, the repo wins, and this file
   gets a dated correction.
