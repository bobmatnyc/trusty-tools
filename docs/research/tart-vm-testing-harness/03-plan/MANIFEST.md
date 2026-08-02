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
| **P2** — Host-side skeleton | `complete` | 2026-08-01 | `eee03178` |
| **P3** — Guest bring-up | `complete` | 2026-08-02 | `345e5b12`, `f181a44e`, + the 2026-08-02 defect-fix commits |
| **P4** — Expectation table and `--check-table` | `complete` | 2026-08-02 | `0c25d48f`, `12a87f28` |
| **P5** — Pattern (c) complete: installs, N2, oracle | `not-started` | — | — |
| **P6** — Pattern (b): branch | `not-started` | — | — |
| **P7** — Pattern (a): released | `not-started` | — | — |
| **P8** — Hardening, docs, measurement write-back | `not-started` | — | — |

**Plan status:** Phase 1 complete, 2026-07-31; **closed out 2026-08-01** with a
second observed result (the dirty-worktree validation) and two plan corrections.
**Phase 2 complete, 2026-08-01** — the host-side contract risk is retired: driver,
configuration, run registry, `lib/vm.sh`, preflight, `clean` and `run --dry-run`
all exist and the checkpoint was observed to pass with **no VM created by any
harness code path**. **Phase 3 complete, 2026-08-02** — the guest bring-up risk
is retired: `vmtest run local` boots a guest, proves N1, provisions it, hands
the toolchain across, streams 96.9 MB of worktree in and tears down, exit 0.
**Its two contract defects were resolved at source on 2026-08-02**, each verified
with real VM runs (see the note below). **Phase 4 complete, 2026-08-02** —
expectation-table drift is retired: `expected-binaries.tsv` carries DOC-2 §9.3's
seed verbatim and `vmtest --check-table` diffs it against the workspace's actual
`[[bin]]` targets read from `cargo metadata`. **No VM was created and none was
required.** **P4-T3 found NO DRIFT** — §9.3's seed is exactly correct as of
2026-08-02 (28 rows == 28 targets; 13 in scope; 8 crate directories; 8 packages).
**Phase 4 found one contract defect, and it is in the PLAN rather than in DOC-2:**
the checkpoint's pass condition asked for a `REMOVED` finding where §9.6's
set algebra makes a deleted table row `ADDED`. The implementation follows §9.6.
**RESOLVED AT SOURCE 2026-08-02** — the plan's Phase 4 checkpoint and P4-T5
acceptance now read `ADDED` and carry a dated correction note on the set
direction; DOC-2 §9.6 was not amended and no code changed. See Phase 4 Deviations
item 1. Phase 5 is the next phase to begin, and it needs a VM.

> **BOTH PHASE 3 CONTRACT DEFECTS ARE RESOLVED AT SOURCE, 2026-08-02**, by owner
> decision, each on the reading Phase 3 identified as the narrower/stronger fix.
> The original text is kept below rather than edited away — this doc set records
> reversals rather than silently rewriting them.
>
> - **(i) N1 — FIXED.** DOC-2 §6.2 gains the amendment *"N1 asserts REACHABILITY,
>   not base-PATH absence — what the probe now proves"*, and `negative_probe_n1`
>   gains a second channel that probes on-disk `~/.cargo/bin`, the mise shims,
>   `~/.local/bin`, `mise which`, and the PATH a login/interactive shell activates.
>   Plan **P3-T1's acceptance is reconciled** and its negative control now passes.
>   Verified on one real guest in both directions (Observed result, run 5).
> - **(ii) `--keep` — FIXED.** DOC-2 §Shell discipline **cleanup property 4** is
>   amended from "skip all three" to "skip only `vm_delete`", and §5.3's "skips
>   teardown" to "skips the deletion"; the driver stops the guest and preserves it,
>   and `vm_manual_hint keep`'s text is rewritten so every command it prints works.
>   Verified: `--keep` → `stopped` → `clean` reports `KEPT` → `clean --include-kept`
>   deletes it (Observed result, run 7).
>
> Original text, unedited:
>
> **Phase 3 found two contract defects, both on paths no earlier phase could
> reach.** Neither is papered over and neither blocks the checkpoint. **(i) N1
> is weaker than DOC-2 §6.2 reads**: it probes only the measured base PATH, so a
> guest with `rust@1.91` installed by `mise use -g` — cargo at
> `~/.cargo/bin/cargo`, a mise shim, neither on that PATH — **passes N1**. Plan
> P3-T1's own negative control is the thing that demonstrates it, and it cannot
> pass as written. **(ii) `--keep` leaves the VM `running`**, so
> `vmtest clean --include-kept` can never remove it — §5.1 condition 2 requires
> `stopped`, and §5.3's justification assumes it. Both are Phase 3 Deviations
> items 1 and 2, with observed output, and both need an owner decision.

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
- ~~**Opened 2026-08-01 by Phase 2 — P2-T4's acceptance grep is a SUBSTRING
  match, and DOC-2 §4.3 mandates a filename that contains the search string.**~~
  **CLOSED 2026-08-02 by owner decision, at the plan**, on reading (a): P2-T4's
  and P3-T4's checks are now `grep -rlnw`, each with a dated correction note
  recording that `started` is why. The DOC-1 §3.2 invariant was never in
  question. Original text retained below.
  `grep -rln 'tart'` matches the four characters wherever they occur, including
  inside the English word that §4.3 requires as one of the four run-registry
  filenames (`pid`, `vm`, `pattern`, and the one that records the run's begin
  time). The driver therefore appears in the literal grep's output on **one**
  line, and that line is the mandated filename — **not** an invocation of the
  virtualisation tool. The DOC-1 §3.2 invariant itself is **intact and
  verified**: the word-boundary form `grep -rlnw 'tart'` lists only
  `lib/vm.sh`, and the driver contains **zero** invocations. See Phase 2
  Deviations item 4 for both greps' verbatim output and the recommendation that
  P3-T4 adopt `-w`. **Not resolved here** — changing a plan acceptance check is
  an owner decision, per the stop rule.
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
- ~~**NEW, opened 2026-08-01 — `--dirty-check` is not yet part of any
  checkpoint.**~~ **CLOSED 2026-08-02 by P3-T4 — the three sentinel assertions
  were PORTED, and they now test the real `source_deliver_local` rather than a
  copy of its pipeline.** They live in `lib/source.sh` as
  `source_dirty_fixture_create` / `source_assert_dirty_delivery` /
  `source_dirty_fixture_restore`, opt-in through `VMTEST_DIRTY_CHECK=1`, with
  restore wired into the driver's cleanup trap so it runs on every exit path.
  Observed passing against a dirty worktree on 2026-08-02: whole-file `cksum`
  equality for the modified tracked file, the untracked file present, the
  gitignored file absent by three independent checks, and
  `git status --porcelain` empty afterwards. Full output in Phase 3 Observed
  result. Original text: *"It is an opt-in mode of a script that P3-T4 deletes.
  The property it proves is a property of `source_deliver_local`, so P3-T4
  should port the three sentinel assertions into a test of `lib/source.sh`
  rather than let them die with the spike. Deleting the spike without porting
  them would return this item to `open`."*
- ~~**NEW, opened 2026-08-02 by Phase 3 — N1's predicate does not assert what
  §6.2's prose claims.**~~ **CLOSED 2026-08-02 by owner decision, AT SOURCE, on
  reading (a): the code now matches the claim.** DOC-2 §6.2 carries the amendment
  *"N1 asserts REACHABILITY, not base-PATH absence"* with a five-row channel table;
  `negative_probe_n1` gained channel 2 (on-disk `~/.cargo/bin` / mise shims /
  `~/.local/bin`, `mise which`, and login+interactive shell rc PATHs), signalling by
  stdout and failing closed if it cannot run; plan P3-T1's acceptance is reconciled.
  **The negative control now produces its stated result**: on one guest, a clean
  clone gives `N1 PASS` exit 0, and the same guest after `mise use -g rust@1.91`
  gives `FAIL[30]` exit 30. Recorded limit: the rc-file channel is an **unexercised**
  guard — it contributed nothing to the observed catch, and §6.2 says so. Original
  text: *"§6.2 says N1 'asserts that the guest genuinely lacks a
  Rust toolchain at that instant" and DOC-1 §4.3 calls it "the assertion a
  golden image structurally destroys". What it actually asserts is that no
  cargo/rustc/rustup is reachable **on the measured base PATH**. A guest
  provisioned by `mise use -g rust@1.91` — the harness's own provisioning
  command — passes N1, observed. **A golden image baked the way this project
  would bake one would therefore NOT be caught**, which is the exact scenario
  DOC-1 §4.3 cites as a reason not to bake one. Needs an owner decision; see
  Phase 3 Deviations item 1 for the two candidate readings."*
- ~~**NEW, opened 2026-08-02 by Phase 3 — a `--keep` VM cannot be removed by
  `vmtest clean --include-kept`.**~~ **CLOSED 2026-08-02 by owner decision, AT
  SOURCE, on reading (a): `--keep` stops the guest and skips only the delete.**
  DOC-2 §Shell discipline cleanup property 4 is amended from "skip all three" to
  "skip only `vm_delete`" — the direction chosen because the alternative would have
  required `clean` to issue a stop, which §5.2/§5.4 forbid far more emphatically
  than property 4 required the skip. §5.3's "skips teardown" is corrected to "skips
  the deletion", and `vm_manual_hint keep` is rewritten so that every command it
  prints actually works against a `stopped` VM. **Observed end to end**: the run
  left `vmtest-p3keep2` `stopped`, `clean` reported `KEPT (would not delete)`
  exit 0, and `clean --include-kept` reported `ORPHANED (deleted)` exit 0 with the
  registry directory pruned. Original text: *"Cleanup property 4 skips request-stop
  / wait / delete entirely, so the VM is left `running`; §5.1 condition 2 requires
  `stopped`, so `clean` refuses it with exit 10 even with `--include-kept`.
  `vm_manual_hint keep`'s own text offers that command as an alternative to the
  manual pair, and it does not work. See Phase 3 Deviations item 2."*
- **NEW, opened 2026-08-02 — DOC-2 §10.1's boot-ready row is re-grounded; P8-T2
  no longer needs to.** The `:483` "~18 s subsequent" figure does not reproduce on
  this host. The row now cites **both** the original research figures and Phase 3's
  four observations, and states that the **unchanged** 150 s maximum is sized
  against the slowest observed boot (33 s, ~4.5×). A note to that effect is on
  P8-T2, whose remaining scope is the watchdog tier and the daemon-health row.

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

- **State:** `complete`
- **Pass condition:** all three hold, in one session —
  1. `vmtest run local --dry-run` **exits 0**, prints an effective-configuration
     banner in which every key carries an origin marker (`default` / `env` /
     `flag`), and `tart list` afterwards shows **no new VM**.
  2. `VMTEST_CPU=4 vmtest run local --dry-run` prints `cpu 4 (env)`, and
     `vmtest run local --cpu 2 --dry-run` prints `cpu 2 (flag)`.
  3. `vmtest clean --dry-run` correctly classifies a hand-created stopped
     `vmtest-*` VM as `ORPHANED (would delete)` and a `keep`-marked one as
     `KEPT (would not delete)`, deleting neither.
- **Observed result:** **MET, all three conditions, in one session.** (run
  2026-08-01 UTC, tree `eee03178`, host: Apple M5 Pro, 18 physical cores,
  64 GiB, macOS 26.5.2 arm64, tart 2.32.1, bash 3.2.57(1)-release.)

  The whole session, verbatim and unedited. `tart list` is captured **before**
  the first condition and **after** the last, and the two are compared below.

  ```
  $ tart list                     # BEFORE
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   36 seconds ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped

  ################ CONDITION 1 ################
  $ vmtest-harness/vmtest run local --dry-run; echo "exit=$?"
  vmtest run local (dry run)
  bash 3.2.57(1)-release
  runid 20260801T010856Z-72642
  vm vmtest-20260801T010856Z-72642
  rundir /Users/mac/.local/state/vmtest-harness/runs/20260801T010856Z-72642
  keep 0
  effective configuration (key value (origin)):
    cpu 8 (default)
    memory_mib 16384 (default)
    disk_gib 100 (default)
    guest_home /Users/admin (default)
    guest_src_dir /Users/admin/vmtest-src (default)
    guest_target_dir /Users/admin/vmtest-target (default)
    host_min_memory_gib 24 (default)
    host_warn_physical_cores 8 (default)
    boot_ready_timeout 150 (default)
    boot_ready_interval 2 (default)
    stopped_timeout 120 (default)
    stopped_interval 1 (default)
    provision_timeout 300 (default)
    install_timeout 2700 (default)
    health_timeout 60 (default)
    health_interval 1 (default)
    repo_url https://github.com/bobmatnyc/trusty-tools.git (default)
    default_branch main (default)
  exit=0

  $ tart list                     # afterwards: no new VM
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   37 seconds ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped
  $ ls -A "$HOME/.local/state/vmtest-harness/runs/" | wc -l
         0

  ################ CONDITION 2 ################
  $ VMTEST_CPU=4 vmtest-harness/vmtest run local --dry-run 2>/dev/null | grep "^  cpu"
    cpu 4 (env)
  $ vmtest-harness/vmtest run local --cpu 2 --dry-run 2>/dev/null | grep "^  cpu"
    cpu 2 (flag)

  ################ CONDITION 3 ################
  $ tart clone tahoe-base vmtest-p2fixture   # HAND-CREATED fixture, never booted
  exit=0

  $ vmtest-harness/vmtest clean --dry-run    # (i) no registry entry
  vmtest-p2fixture  stopped  ORPHANED (would delete)
  exit=0

  $ : > "$RUNS/p2fixture/keep"               # (ii) keep-marked
  $ vmtest-harness/vmtest clean --dry-run
  vmtest-p2fixture  stopped  KEPT (would not delete)
  exit=0

  $ tart list --format json | jq -r '.[]|select(.Name=="vmtest-p2fixture")|.Name+" "+.State'   # DELETED NEITHER
  vmtest-p2fixture stopped

  # teardown of the fixture: vm_request_stop -> vm_wait_for_stopped -> vm_delete
  vm_request_stop     -> 0
  vm_wait_for_stopped -> 0 (state now: stopped)
  vm_delete           -> 0

  $ tart list                     # AFTER
  Source Name                                                                                                        Disk Size Accessed      State
  local  tahoe-base                                                                                                  50   33   3 seconds ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago   stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago   stopped
  $ ls -A "$HOME/.local/state/vmtest-harness/runs/" | wc -l
         0
  ```

  **Condition by condition.** (1) exit **0**; every one of the eighteen
  configuration keys carries an origin marker; `tart list` afterwards is
  byte-identical to `tart list` before, so **no new VM** — **met**. (2) `cpu 4
  (env)` and `cpu 2 (flag)` printed literally — **met**. (3) the same
  hand-created stopped `vmtest-*` VM classified `ORPHANED (would delete)` with
  no registry entry and `KEPT (would not delete)` with a `keep` marker, and it
  was still present, still `stopped`, after both — **neither was deleted** —
  **met**.

  **BEFORE and AFTER `tart list` are identical**: the same three rows, same
  `Source`, `Name`, `Disk 50`, `Size 33`/`32`/`32`, same `stopped` state. Only
  `tahoe-base`'s `Accessed` timestamp moved, which is what an APFS CoW clone
  does to its source. The base image was **not** modified, re-pulled or
  re-tagged, and `~/.tart` was not otherwise touched.

  **On the one VM that existed during this phase — stated loudly, because the
  phase is otherwise host-side only.** Checkpoint condition 3 requires, in its
  own words, *"a hand-created stopped `vmtest-*` VM"*. No harness code path can
  produce one: `run --dry-run` halts before the clone and `clean` only ever
  deletes. The fixture was therefore created **by hand**, outside the harness,
  as an APFS CoW clone that was **never booted** — so it was in state `stopped`
  for the whole of its ~40-second life — and torn down with the mandated
  `vm_request_stop` → `vm_wait_for_stopped` → `vm_delete` ordering, using
  `lib/vm.sh`'s own implementations, never a bare stop trusted as completion.
  **Nothing the harness itself ran created a VM.** The `run --dry-run`
  invocations of conditions 1 and 2 are proved VM-free by the `tart list`
  immediately after condition 1 and by the identical before/after pair.

  **Supporting acceptance checks**, all run 2026-08-01 UTC against the same
  tree. Task acceptance, not the checkpoint; recorded because several of them
  are the only evidence that a refusal path works.

  ```
  # P2-T1
  $ bash -n vmtest-harness/vmtest                    # (silent)
  $ vmtest-harness/vmtest                            # no arguments
  usage: … (on stderr)
  vmtest: FAIL[2]: no subcommand given
  exit=2
  $ vmtest-harness/vmtest bogus ; echo exit=$?
  exit=2

  # P2-T2
  $ bash -c '. vmtest-harness/vmtest --source-only 2>/dev/null; conf_get cpu'
  8
  $ … conf_get memory_mib -> 16384 ; conf_get install_timeout -> 2700
  $ printf 'cpuu\t8\n' >> vmtest.defaults ; vmtest run local --dry-run ; echo exit=$?
  exit=10

  # P2-T3
  $ vmtest-harness/vmtest run local --runid 'a b' --dry-run
  vmtest: FAIL[2]: invalid --runid 'a b': only alphanumerics and hyphens are allowed (DOC-2 §4.2)
  exit=2
  $ vmtest-harness/vmtest run local --runid xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx --dry-run
  vmtest: FAIL[2]: invalid --runid 'xxxx…': 40 characters, maximum is 32 (DOC-2 §4.2)
  exit=2
  $ mkdir -p runs/dup ; echo $$ > runs/dup/pid ; vmtest run local --runid dup --dry-run
  vmtest: FAIL[10]: runid 'dup' is already held by the run directory …/runs/dup (pid 66623). Choose another --runid, or run `vmtest clean` if that run is over.
  exit=10
  $ auto-generated id: 20260801T010645Z-63323   matches ^[0-9]{8}T[0-9]{6}Z-[0-9]+$  -> RUNID_FORMAT_OK
  $ live peer in the registry:
  vmtest: WARN: another run 'peer' holds a live pid 66623. Concurrency is safe (§4.3) but not advised: each guest is sized at 8 vCPU / 16384 MiB.
  (warns; does NOT fail — §4.3)

  # P2-T4
  $ grep -rln 'tart' vmtest-harness --include='*.sh' --include='vmtest' --exclude-dir=spike
  vmtest-harness/vmtest
  vmtest-harness/lib/vm.sh
  $ grep -rn 'tart' vmtest-harness/vmtest          # the driver's ONLY hit, in full
  vmtest-harness/vmtest:368:    date -u '+%Y-%m-%dT%H:%M:%SZ'   > "$VMTEST_RUNDIR/started"
  $ grep -rlnw 'tart' vmtest-harness --include='*.sh' --include='vmtest' --exclude-dir=spike
  vmtest-harness/lib/vm.sh
  $ bash -n vmtest-harness/lib/vm.sh               # (silent)

  # P2-T5
  $ (pin digest corrupted to sha256:deadbeef…) vmtest run local --dry-run
  vmtest: pinned: ghcr.io/cirruslabs/macos-tahoe-base@sha256:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef
  vmtest: found:  tahoe-base ghcr.io/cirruslabs/macos-tahoe-base:latest ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c
  vmtest: FAIL[10]: base image digest does not match base-image.pin (DOC-2 §3.3). Rolling the pin is a deliberate act with its own PR (§3.4) and is NEVER a repair step inside a failing run.
  exit=10
  $ (unknown key 'digset' injected into base-image.pin)            -> exit=10
  $ (jq removed from PATH via a mirrored bin directory) vmtest run local --dry-run
  vmtest: FAIL[10]: jq not found on PATH (host dependency; the complete set is the virtualisation CLI checked immediately above, plus git, jq, cargo and bash >= 3.2)
  exit=10
  $ (pin and defaults restored)  git status --porcelain -> empty ; vmtest run local --dry-run -> exit=0

  # P2-T6, fixture (iii)
  $ mkdir -p runs/no-such-vm ; echo 99999999 > runs/no-such-vm/pid
  $ vmtest-harness/vmtest clean --dry-run
  runs/no-such-vm  —  PRUNE (bookkeeping)
  exit=0
  (the directory was still present afterwards — --dry-run prunes nothing)
  $ vmtest-harness/vmtest clean --dry-run --include-kept   # against the keep-marked fixture
  vmtest-p2fixture  stopped  ORPHANED (would delete)

  # dispatch of the not-yet-built paths
  $ vmtest-harness/vmtest run bogus --dry-run
  vmtest: FAIL[2]: unknown scenario name 'bogus' (expected local | branch | released)
  $ vmtest-harness/vmtest run local          # no --dry-run
  vmtest: FAIL[2]: the full run lifecycle (clone -> size -> boot -> negative probe -> provision -> scenario -> verify -> teardown) is delivered by plan Phases 3-7. At Phase 2 only `--dry-run` is implemented, and this driver refuses rather than pretending. Re-run with --dry-run.
  exit=2
  ```

  **P2-T6 fixture (iv) was NOT exercised, and that is recorded rather than
  glossed.** It requires a `vmtest-*` VM in state **`running`** with no
  registry entry, which means booting a guest — and Phase 2 is host-side only.
  The classifier branch exists (`cmd_clean`, the non-`stopped`/non-`suspended`
  case: it prints the VM and its state, calls `vm_manual_hint running`, and the
  command exits **10** with nothing deleted), and so does the `suspended`
  branch of §5.4 row 2, but **neither has been run**. Both should be exercised
  in Phase 3, which boots a guest anyway. See Deviations item 9.
- **Files delivered:**
  - create `vmtest-harness/vmtest` — the driver (P2-T1, T2, T3, T5, T6, T7)
  - create `vmtest-harness/vmtest.defaults` — DOC-2 §8.2's file, verbatim (P2-T2)
  - create `vmtest-harness/lib/vm.sh` — the OS boundary (P2-T4)
  - modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md` (P2-T8)
- **Measurements:** the phase is entirely host-side, so it produces none of the
  numbers the design is waiting on. Three small facts were nonetheless observed
  and are worth carrying, each with the command that produced it.

  | # | Measurement | Value | Command / source | Why it is worth keeping |
  |---|---|---|---|---|
  | 1 | **`run --dry-run` wall clock** | **0.67 s** | `time vmtest-harness/vmtest run local --dry-run` (0.08 s user, 0.17 s system) | This is the whole host-side path — config, pin comparison, two `tart list` round-trips, registry acquire/release, capacity checks. It is the floor under every run's fixed cost, and it is small enough that no phase after this one has an excuse to debug argument parsing while a VM boots (the stated reason Phase 2 exists). |
  | 2 | **`tart exec` against a `stopped` VM** | **exit 2**, stderr `VM "<name>" is not running` | `tart exec tahoe-base /bin/sh -c 'exit 0'; echo $?` | It does **not** hang and it does **not** return 0. `vm_request_stop`'s guest flush therefore fails cleanly and non-fatally on an already-stopped or unreachable guest, which is exactly the path DOC-2 §12.2 says must not be fatal. Observed working: `vm_request_stop tahoe-base` logged `guest flush failed … (logged, not fatal)` and returned **0**. |
  | 3 | **CoW clone → `stopped`, and `vm_request_stop` → `stopped` on a never-booted VM** | clone `exit=0`, state `stopped` immediately; teardown trio all returned 0 | the checkpoint transcript above | A never-booted clone is `stopped` from birth, so `clean`'s "already stopped" precondition can be fixtured without booting anything. This is what made checkpoint condition 3 reachable in a host-side-only phase. |

  Not measured, and **still** open for Phase 3+: boot-to-ready on a subsequent
  boot (P1 has one data point, 18 s), and everything in DOC-2 §10.1's daemon
  row.
- **Deviations from plan:**
  1. **§F-1 RESOLVED BY NARROWEST READING.** `run --dry-run` performs preflight,
     prints the effective-configuration banner with origins (plus the bash
     version), acquires the run-registry entry and releases it, and **halts
     before the clone**. It creates no VM and touches no guest. This is the
     largest prefix of the run lifecycle that involves no VM, which is the only
     reading consistent with `clean --dry-run`'s "full classification, no
     destruction". **It was deliberately NOT extended** — §F-1 says a
     `--dry-run` that clones and boots is not a dry run, and the implementation
     takes that literally: the code path after the dry-run branch does not
     exist yet at all. Recorded in the driver at the branch itself, so a future
     reader finds the rule where the decision lives.
  2. **§F-5 RESOLVED BY NARROWEST READING; the permitted alternative was NOT
     taken.** `conf_get`, `conf_origin`, `conf_set`, `tsv_field`, `tsv_get`,
     `tsv_validate_keys`, `log`, `warn` and `die` are defined in the **`vmtest`
     driver itself**, above the point where `lib/vm.sh` is sourced. §F-5
     explicitly permits a fifth `lib/tsv.sh` provided it is recorded; it was
     considered and rejected, because §F-5's own reasoning already settles it —
     these are driver infrastructure, not OS-boundary / provisioning /
     transport / assertion logic, so none of the four modules is their home,
     and DOC-2 §12.4 already places `die` in the driver by showing it outside
     every module table. Adding a fifth module would depart from DOC-1 §3's
     component tree for no gain. `lib/vm.sh` calls all of them, which is sound
     because function definitions are shell-global by the time `lib/` is
     sourced.

     One consequence worth stating, because it is the §F-5 argument made
     concrete: **the effective configuration is itself a
     `key<TAB>value<TAB>origin` TSV**, read by the same `awk` parser as
     `vmtest.defaults` and `base-image.pin`. Under bash 3.2 there is no
     associative array to hold it in, and DOC-2 §Shell discipline says in as
     many words that the TSV files **are** the substitute for a hash. The
     effective file lives in the run's temporary directory and is removed by
     the cleanup trap. "One parser, three files" (§3.1) is therefore literally
     true in the implementation, and it is now four uses of one parser.
  3. **`lib/vm.sh` carries THREE functions beyond DOC-2 §12.2's twelve. All
     three are forced, and each keeps the DOC-1 §3.2 invariant rather than
     bending it.**
     - **`vm_list <out_tsv_path>`** — §12.2 has **no VM-enumeration function**,
       yet §5.1's four-condition orphan test and DOC-1 §4.1's stopped-state
       refusal both require enumerating VMs *and their states*, and neither can
       be written without one. It is also what makes §3.3's digest comparison
       possible from outside `vm.sh`, because `tart list --format json` reports
       an OCI image's `Name` as `<oci_ref>@sha256:<64 hex>` — the same field
       P1-T3 used. Per §12.1 ("a function that needs to return several values
       writes a TSV to a path given as an argument") it writes a TSV rather
       than emitting rows on stdout, so §12.1's one-value stdout rule is
       honoured, not excepted.
     - **`vm_require_cli`** — DOC-1 §4.1's first preflight row is "`tart`
       present on `PATH`", and **the driver may not contain that string**. The
       check has to live behind the boundary.
     - **`vm_manual_hint <kind> <vm_name>`** — three sites must print concrete
       OS-level commands for a human: cleanup property 4's `--keep` inspection
       hint, §5.4 row 1's manual commands for a refused running VM, and §5.4
       row 2 / DOC-1 §8.2's `state.vzvmsave` unwedge procedure. All three sites
       are in the driver, which may not name the tool. Putting the *text* with
       the rest of the OS knowledge is the narrowest fix; the alternative was
       to weaken the hints to uselessness.

       This is a gap in DOC-2 §12.2's surface, not a design disagreement — the
       twelve signatures cover the *lifecycle* of one VM completely and cover
       *enumeration* and *operator guidance* not at all. **§12.2 should gain
       these three**, and P8 is the place to write that back.

       > **RESOLVED AT SOURCE, 2026-08-02.** §12.2 has gained all three, with
       > the signatures as implemented, and its surface is now **fifteen**
       > signatures rather than twelve. The amendment states per-function why
       > each one *preserves* the DOC-1 §3.2 invariant rather than bending it.
       > Plan P2-T4's *Contract* line and its *Do* list are corrected to match.
       > Done here rather than deferred to P8, as this item proposed: a module
       > surface that is wrong in the spec is silently re-derived by every
       > phase that reads it, and Phase 3 reads it next.
       > See [DOC-2 §12.2](../02-design/02-harness-contracts.md).
  4. **CONTRACT DEFECT — P2-T4's acceptance grep is a SUBSTRING match, and
     DOC-2 §4.3 mandates a registry filename containing the search string.**
     The driver appears in the literal grep's output. It appears on exactly one
     line, and that line is §4.3's mandated run-registry filename recording
     when the run began — **not** an invocation. Both greps, verbatim:

     ```
     $ grep -rln 'tart' vmtest-harness --include='*.sh' --include='vmtest' --exclude-dir=spike
     vmtest-harness/vmtest
     vmtest-harness/lib/vm.sh

     $ grep -rn 'tart' vmtest-harness/vmtest        # the driver's ONLY hit, in full
     vmtest-harness/vmtest:368:    date -u '+%Y-%m-%dT%H:%M:%SZ'   > "$VMTEST_RUNDIR/started"

     $ grep -rlnw 'tart' vmtest-harness --include='*.sh' --include='vmtest' --exclude-dir=spike
     vmtest-harness/lib/vm.sh
     ```

     **The invariant is intact and is verified by the third command.** DOC-1
     §3.2 says exactly one file in the production tree may name the OS; exactly
     one does. What fails is the *check*, not the property — and it fails on
     **correct** work, which is the same category as P1-T6's `-type f` and
     P2-T4's own `spike/` conflict, both of which were corrected at source.

     **Not resolved here, per the stop rule** — amending a plan acceptance
     check is an owner decision. The two candidate readings, for whoever owns
     that decision: (a) change the check to `grep -rlnw`, which is strictly the
     intended semantics — an invocation is always word-delimited, and `-w`
     still matches `tart-run.pid` because `-` is not a word character; or (b)
     leave the check and read its output with the one mandated exception named.
     **(a) is the narrower fix and does not weaken anything.** Rejected out of
     hand: renaming the registry file (§4.3 mandates it) or obfuscating the
     literal in the driver, which would defeat a review rather than pass one.
     **P3-T4 inherits this**: it must delete `--exclude-dir=spike` and re-run
     the grep, and it will hit the same line unless the check is amended first.

     > **RESOLVED AT SOURCE, 2026-08-02 — reading (a), by owner decision.**
     > P2-T4's acceptance grep is now `grep -rlnw`, and **P3-T4's inherited
     > check is corrected the same way**. Both tasks carry a dated correction
     > note recording that `started` is why `-w` is there, so that a later
     > reader does not "simplify" it away and re-break the check on correct
     > work. The DOC-1 §3.2 invariant was never in question and is unchanged.
     > The two P3-T4 corrections are **independent**: deleting
     > `--exclude-dir=spike` removes the first reason the grep could not pass
     > and does nothing about this one, so `-w` must survive that deletion —
     > a reviewer of P3-T4 now checks for three things, not two.
     > See [the plan](./01-implementation-plan.md), P2-T4 and P3-T4.
  5. **DOC-2 §12.3's config globals collide by name with §8.2's environment
     overrides, so the driver does not set them.** §12.3 lists `VMTEST_CPU`,
     `VMTEST_MEM_MIB`, `VMTEST_GUEST_HOME` and friends as globals holding the
     resolved configuration. §8.2's override mapping is *mechanical* —
     uppercase the key, prefix `VMTEST_` — so `VMTEST_CPU` is **also** the
     environment variable that overrides key `cpu`. A driver that assigned the
     global would be writing into its own override channel: a subsequent
     resolution would read its own value back and report origin **`env`** for
     something that came from the defaults file, and §8.3's origin marker —
     which checkpoint condition 2 tests directly — would be lying. Resolved by
     reading configuration through `conf_get`/`conf_origin` everywhere and
     assigning only the §12.3 globals that are *not* config keys
     (`VMTEST_RUNID`, `VMTEST_VM`, `VMTEST_RUNDIR`, `VMTEST_PATTERN`,
     `VMTEST_KEEP`, `VMTEST_GUEST_ENV`, `VMTEST_EXIT`, `VMTEST_CLEANUP_DONE`).
     A one-word note in §12.3 would close this.

     > **RESOLVED AT SOURCE, 2026-08-02.** §12.3 now strikes the six config
     > names from the globals table, marks them **RESERVED for §8.2's env
     > overrides**, and states the rule as three checkable clauses: config is
     > read only via `conf_get`/`conf_origin`; `VMTEST_<KEY>` names are
     > **inbound only** and assigning one is a defect; the non-config globals
     > listed above remain the driver's to set. The **origin-marker corruption**
     > is named as the reason, because that is the part that stops someone
     > re-adding the assignment as a tidy-up.
     >
     > It needed more than the one word this item predicted. The amendment also
     > records something this item did not catch: **three of the six struck
     > names were never §8.2's names anyway** — the keys are `memory_mib`,
     > `guest_src_dir`, `guest_target_dir`, which derive `VMTEST_MEMORY_MIB`,
     > `VMTEST_GUEST_SRC_DIR`, `VMTEST_GUEST_TARGET_DIR`, not §12.3's
     > `VMTEST_MEM_MIB`, `VMTEST_GUEST_SRC`, `VMTEST_GUEST_TARGET`. Assigning
     > the abbreviated forms would have created three globals overriding
     > **nothing** while three real override names went unset — a collision and
     > a near-miss in one row, and further evidence for the `conf_get` rule.
     > See [DOC-2 §12.3](../02-design/02-harness-contracts.md).
  6. **DOC-2 §10.2 budgets `tart clone` at 60 s, but §8.2 defines no key for
     that budget — while §10.3 requires a timeout message to name "the
     `vmtest.defaults` key that changes it".** The two sections cannot both be
     satisfied for this one site. Implemented as a literal `60` in `vm_clone`
     with the §10.2 citation inline, and the failure message says explicitly
     that **no key exists** rather than naming one that does not. Every other
     watchdog and poll site in `lib/vm.sh` reads its budget from configuration
     (`boot_ready_interval`, `stopped_interval`, and the timeouts passed in by
     the caller). Not fixed here: adding a `clone_timeout` key would edit
     §8.2's file, which P2-T2 requires be copied **verbatim**.

     > **RESOLVED AT SOURCE, 2026-08-02 — §10.3 amended; NO key added.** The
     > implementation stands exactly as described above, so **no code change
     > was required**: `vm_clone`'s literal 60 with its §10.2 citation and its
     > "no key exists" message is now the **reference form** the amended §10.3
     > prescribes for a built-in budget.
     >
     > The direction was chosen on a fact this item did not have: the gap is
     > **systemic**. §10.2's table has gained an `§8.2 key` column, and it shows
     > **three of five** watchdog sites with no key — `tart clone` (60 s),
     > single-crate install (900 s) and guest `git clone` (300 s) — while all
     > six §10.1 poll parameters are keyed. Adding `clone_timeout` would have
     > fixed the one site that happened to be noticed and left two identical
     > contradictions standing. §10.3 clause 3 now permits a built-in budget
     > provided the message cites the §10.2 row and says explicitly that no key
     > changes it. Reasoning is stated inline at §10.3, including §8.2's own
     > "a flag per tunable gives a surface larger than its behaviour" applied a
     > tier down, and that budgets set at ~190x/~8x/~6x measured are hang
     > detectors rather than schedules.
     >
     > **One residual, deliberately left open:** `tart clone`'s 60 s is the one
     > built-in with a plausible route to being too tight, because §3.3's
     > by-construction variant may **pull an image** on first use and that is
     > unmeasured. Recorded at §10.3 and carried to **P8-T2**; if a measured
     > pull shows it varies by host, it earns a key on the same evidence every
     > other key rests on. **Phases 5 and 6 must use the reference message form**
     > for the other two built-ins when they implement those sites.
     > See [DOC-2 §10.2/§10.3](../02-design/02-harness-contracts.md).
  7. **Two "recognised but not yet built" paths exit 2, a code §2 does not
     assign to that situation.** `vmtest --check-table` is delivered by P4-T2
     and `vmtest run <pattern>` without `--dry-run` by Phases 3–7. Both are
     dispatched (so neither is an unknown subcommand) and both refuse with an
     explicit message naming the plan task that delivers them. **2** is the
     narrowest fit in §2's table — the driver cannot act on the argument, and
     §2's guarantee for code 2, "no VM was touched", holds exactly. Both
     messages are temporary by construction and disappear when the phase that
     owns them lands.
  8. **`${BASH_SOURCE[0]}` rather than `$0`** to locate the harness directory.
     P2-T2's own acceptance check **sources** the driver
     (`. vmtest-harness/vmtest --source-only`), where `$0` is `bash` and the
     harness directory resolves to the caller's cwd — the check cannot pass
     with `$0`. `BASH_SOURCE` is correct under both execution and sourcing and
     is available in bash 3.2. The `--source-only` hook itself is additive: it
     loads configuration and dispatches nothing, and it exists because the plan
     names it in P2-T2's acceptance command.
  9. **P2-T6 fixture (iv) is NOT exercised**, and neither is §5.4 row 2's
     `suspended` refusal. Both require a VM in a state that only booting can
     produce, and Phase 2's checkpoint requires that no VM be created. The code
     paths are written and syntax-checked; they have not been run. **This is
     incomplete work, not a passed check** — Phase 3 boots a guest and should
     take both.

     > **STILL OPEN — reconfirmed 2026-08-02.** Stated explicitly because items
     > 3, 4, 5 and 6 above were resolved at source on this date and this one was
     > **not**, and a reader skimming the resolution notes should not carry the
     > momentum into this item. Nothing about it changed: both paths remain
     > **written and `bash -n`-clean but UNRUN**, and neither can be exercised
     > without a VM in a state only booting produces. The 2026-08-02 work was
     > documentation and plan edits exclusively — **no VM was created, and
     > `tart list` was identical before and after**. Concretely still unrun:
     > P2-T6 fixture **(iv)**, the `running`-VM refusal, and **§5.4 row 2's
     > `suspended` refusal** (`vmtest:648`, `vmtest:661-662`). **Phase 3 takes
     > both**, and until it does they are incomplete work.
     >
     > **CLOSED 2026-08-02 by Phase 3 — BOTH were taken, and both branches
     > behave as written.** Fixture (iv) was exercised against a hand-created
     > `vmtest-*` VM booted to `running` with no registry entry: `clean`
     > refused it `REFUSED (running, no live registry entry)` and exited **10**
     > with nothing deleted, in both `--dry-run` and non-dry form. §5.4 row 2's
     > `suspended` refusal was exercised **without ever issuing `tart
     > suspend`** — DOC-1 §8.2 records that `tart list` derives the state
     > *purely from the presence of `state.vzvmsave`*, so creating that one
     > file on a never-booted ephemeral clone reproduces it; `clean` printed
     > `WEDGED (refusing)` with the full unwedge hint and exited **10**.
     > Removing the file returned the VM to `stopped` and it was deleted by the
     > mandated path. Verbatim output in Phase 3, Observed result, obligation
     > (3). **Neither is incomplete work any longer.**
  10. **Scope of preflight's stopped-state check, stated because DOC-1 §4.1
      does not enumerate "every existing VM the harness would touch".** Read as
      three things: the pinned local base image must exist and be `stopped`
      (it is the clone source); the target VM name must not already exist (§4.1
      "no leftover `vmtest-*` VM with the target runid"); and **no** VM in the
      `vmtest-*` namespace may be in any state other than `stopped`. VMs
      outside that namespace are not the harness's and are never inspected —
      which is the same rule that keeps `clean` away from `tahoe-base`.
  11. **Output discipline, recorded because it is a choice.** A command's
      *product* goes to **stdout** — the effective-configuration banner and
      `clean`'s per-VM verdicts; every diagnostic, warning and failure goes to
      **stderr**. §12.1's "diagnostics always to stderr" rule is stated for
      `lib/` functions because §1's oracle parses their stdout; nothing parses
      the driver's own stdout. Putting the banner there makes checkpoint
      conditions 1 and 2 greppable without merging streams, and keeps `clean`'s
      verdict list pipeable.

  **Not deviations, recorded so the next agent does not re-litigate them:**
  §F-9 is resolved at source, so `vm_request_stop` was implemented as specified
  with no decision to make, and the guest-side `shutdown -h now` prohibition is
  restated at the function; the `--exclude-dir=spike` exemption was **not**
  removed and **not** broadened — it still names P3-T4 as its expiry in the
  file header; `vmtest.defaults` is DOC-2 §8.2's file **verbatim**, including
  the two keys (`health_timeout`, `health_interval`) that nothing reads until
  Phase 5; and `vm_exec` deliberately does **not** die on non-zero, because N1's
  expected result in Phase 3 is a non-zero exit.
- **Tasks:** P2-T1 … P2-T8 complete. **With one exception, named:** P2-T6's
  acceptance lists four fixtures and **three** were run — (iv), the running-VM
  refusal, requires booting a guest and is deferred to Phase 3 (Deviations item
  9). The phase checkpoint, which is the gate, requires only fixtures (i) and
  (ii) and both were observed.

## Phase 3 — Guest bring-up: N1, provisioning, toolchain hand-off, source delivery

- **State:** `complete`
  *(Corrected 2026-08-02: this field read `not-started` while the summary table
  read `complete` and the checkpoint output was pasted below it. Under the state
  rules — "`in-progress` → `complete` **only** when the checkpoint has been **run**
  and its output is pasted into `Observed result`" — `complete` is the correct
  value, and the schema's own note that "the sections are authoritative and the
  table is the index" means the wrong field was the authoritative one. Caught while
  making the three 2026-08-02 defect fixes.)*
- **Pass condition:** `vmtest run local` **exits 0**, and its log shows, in order:
  `N1 PASS` with a non-zero exit recorded for each of `cargo`, `rustc`, `rustup`; a
  provisioning block ending with `rustc_version 1.91.1`; a streamed byte count
  > 80,000,000; and a teardown after which `tart list` contains **no** `vmtest-*`
  entry. `$VMTEST_RUNDIR` is removed, and
  `ls "${VMTEST_STATE_DIR:-$HOME/.local/state/vmtest-harness}/runs/"` is empty.
- **Observed result:** **MET, every clause.** (run 2026-08-02 UTC, tree
  `f181a44e`, host: Apple M5 Pro, 18 physical cores, 64 GiB, tart 2.32.1,
  bash 3.2.57(1)-release.)

  `tart list` is captured **before** the first VM of the phase and **after** the
  last, and the two are compared below.

  ```
  $ tart list                     # BEFORE — the phase's baseline
  Source Name                                                                                                        Disk Size Accessed    State
  local  tahoe-base                                                                                                  50   33   1 day ago   stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago stopped

  ################ THE CHECKPOINT ################
  $ vmtest-harness/vmtest run local --runid p3ckpt2 ; echo "exit=$?"
  vmtest run local
  bash 3.2.57(1)-release
  runid p3ckpt2 (flag)
  vm vmtest-p3ckpt2
  rundir /Users/mac/.local/state/vmtest-harness/runs/p3ckpt2
  host repo /Users/mac/workspace/trusty-tools-fork-worktrees/agent-a15a808908e0bc0eb
  keep 0
  effective configuration (key value (origin)):
    cpu 8 (default)
    memory_mib 16384 (default)
    disk_gib 100 (default)
    guest_home /Users/admin (default)
    guest_src_dir /Users/admin/vmtest-src (default)
    guest_target_dir /Users/admin/vmtest-target (default)
    host_min_memory_gib 24 (default)
    host_warn_physical_cores 8 (default)
    boot_ready_timeout 150 (default)
    boot_ready_interval 2 (default)
    stopped_timeout 120 (default)
    stopped_interval 1 (default)
    provision_timeout 300 (default)
    install_timeout 2700 (default)
    health_timeout 60 (default)
    health_interval 1 (default)
    repo_url https://github.com/bobmatnyc/trusty-tools.git (default)
    default_branch main (default)
  vmtest: --- preflight (DOC-1 §4.1) ---
  vmtest: pin OK: ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c
  vmtest: preflight OK
  vmtest: --- clone, size, boot (DOC-1 §4.3, §8.5) ---
  vmtest: cloned tahoe-base -> vmtest-p3ckpt2
  vmtest: sized --cpu 8 --memory 16384 --disk-size 100 (DOC-1 §8.5)
  vmtest: booted (backgrounded, no graphics); run pid 3972
  vmtest: MEASURE boot_to_ready_s 33 (P1 measured 34.4 s first boot, ~18 s subsequent)
  vmtest: state: running
  vmtest: --- N1 precondition probe (DOC-2 §6.2; DOC-1 §4.2) ---
  vmtest: N1 PASS (cargo=1 rustc=1 rustup=1)
  vmtest: --- provisioning (DOC-2 §11.2; mise and gh are REUSED, never installed) ---
  vmtest: mise detected at /opt/homebrew/bin/mise (2026.6.0 macos-arm64 (2026-06-03)) — REUSED, not installed
  vmtest: gh detected at /opt/homebrew/bin/gh — REUSED, not installed
  vmtest: installed rust@1.91 (measured baseline 20.778 s)
  vmtest: installed uv@latest (measured baseline 7.947 s)
  vmtest: rustc: rustc 1.91.1 (ed61e7d7e 2025-11-07)
  vmtest: toolchain hand-off written to /Users/admin/.vmtest/toolchain.tsv and read back to $VMTEST_RUNDIR/toolchain.tsv:
      | guest_home	/Users/admin
      | cargo_bin	/Users/admin/.cargo/bin
      | mise_shims	/Users/admin/.local/share/mise/shims
      | mise_bin	/opt/homebrew/bin/mise
      | base_path	/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
      | guest_path	/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
      | rustc_version	1.91.1
  vmtest: provisioning wall clock 35s (measured baseline PROVISION_MS=30079, i.e. 30.079 s)
  vmtest: provisioning OK (rustc_version 1.91.1)
  vmtest: --- scenario local (scenario_install_local) ---
  vmtest: host repo (READ-ONLY; NEVER mounted into the guest — DOC-1 §11): /Users/mac/workspace/trusty-tools-fork-worktrees/agent-a15a808908e0bc0eb
  vmtest: host file set (git ls-files -co --exclude-standard | wc -l): 5344
  vmtest: streamed 96952320 bytes in 4s
  vmtest: guest file set (find ! -type d):     5344
  vmtest: guest file set (find -type f):       5340  (regular files only; excludes tracked symlinks)
  vmtest: file counts match: guest == host == 5344
  vmtest: target/ absent in the guest, by construction
  vmtest: streamed 96952320 bytes of git-tracked + untracked-unignored source
  vmtest: run complete: pattern 'local' reached the end of its scenario. Teardown follows.
  vmtest: teardown: deleted vmtest-p3ckpt2
  exit=0

  $ tart list                     # no vmtest-* entry
  Source Name                                                                                                        Disk Size Accessed     State
  local  tahoe-base                                                                                                  50   33   1 minute ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago  stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago  stopped
  $ ls -A "${VMTEST_STATE_DIR:-$HOME/.local/state/vmtest-harness}/runs/" | wc -l
         0
  ```

  **Clause by clause.**

  | Clause | Required | Observed | |
  |---|---|---|---|
  | 1 | `vmtest run local` **exits 0** | `exit=0` | **met** |
  | 2 | `N1 PASS` with a non-zero exit for each of `cargo`, `rustc`, `rustup` | `N1 PASS (cargo=1 rustc=1 rustup=1)` — three tools, three non-zero codes | **met** |
  | 3 | a provisioning block ending with `rustc_version 1.91.1` | the block's last two lines are `rustc_version	1.91.1` (the TSV dump) and `provisioning OK (rustc_version 1.91.1)` | **met** |
  | 4 | a streamed byte count **> 80,000,000** | `streamed 96952320 bytes` — 96,952,320 > 80,000,000 | **met** |
  | 5 | a teardown after which `tart list` contains **no** `vmtest-*` entry | `teardown: deleted vmtest-p3ckpt2`; the `tart list` above has three rows, none `vmtest-*` | **met** |
  | 6 | `$VMTEST_RUNDIR` is removed | `ls .../runs/p3ckpt2` → `No such file or directory` | **met** |
  | 7 | `ls "${VMTEST_STATE_DIR:-$HOME/.local/state/vmtest-harness}/runs/"` is empty | `0` entries | **met** |

  **On the "in order" qualifier.** The log's landmarks appear in the pass
  condition's stated order: N1 before provisioning (§6.3's pinned position),
  provisioning before the stream, the stream before teardown.

  ---

  **The three carried obligations, each with its observed result.**

  **(1) P3-T4's THREE required checks — directory deleted, exemption deleted in
  the same commit, `-w` surviving.** All three, verbatim, at tree `f181a44e`:

  ```
  $ ls vmtest-harness/spike ; echo "exit=$?"
  ls: vmtest-harness/spike: No such file or directory
  exit=1

  $ grep -rlnw 'tart' vmtest-harness --include='*.sh' --include='vmtest'
  vmtest-harness/lib/vm.sh

  $ git log --stat --format='%h %s' -1 345e5b12
  345e5b12 feat(vmtest-harness): Phase 3 guest bring-up — N1, provisioning, toolchain hand-off, source delivery
   vmtest-harness/lib/provision.sh              | 245 ++++++++
   vmtest-harness/lib/source.sh                 | 269 +++++++++
   vmtest-harness/lib/verify.sh                 |  88 +++
   vmtest-harness/lib/vm.sh                     |  30 +-
   vmtest-harness/scenarios/install-local.sh    |  34 ++
   vmtest-harness/spike/dirty-check-fixture.txt |  11 -
   vmtest-harness/spike/spike-transport.sh      | 869 ---------------------------
   vmtest-harness/tests/dirty-check-fixture.txt |  21 +
   vmtest-harness/vmtest                        | 151 ++++-
   9 files changed, 822 insertions(+), 896 deletions(-)
  ```

  The grep lists **`lib/vm.sh` and nothing else**, unscoped. The
  `--exclude-dir=spike` argument is gone from `lib/vm.sh`'s header — deleted in
  `345e5b12`, the same commit that deletes the directory and adds
  `lib/source.sh`. **`-w` survives that deletion**, and the reason is recorded
  at the check itself so it is not simplified away: without it, `grep -rln`
  still lists the driver, on one line that is DOC-2 §4.3's mandated registry
  filename `started`, not an invocation —

  ```
  $ grep -rln 'tart' vmtest-harness --include='*.sh' --include='vmtest'
  vmtest-harness/vmtest
  vmtest-harness/lib/vm.sh
  $ grep -rn 'tart' vmtest-harness/vmtest        # the driver's ONLY hit, in full
  vmtest-harness/vmtest:396:    date -u '+%Y-%m-%dT%H:%M:%SZ'   > "$VMTEST_RUNDIR/started"
  ```

  **(2) The three dirty-worktree assertions, ported and OBSERVED PASSING.**
  `VMTEST_DIRTY_CHECK=1 vmtest run local --runid p3dirty`, exit **0**:

  ```
  vmtest: --- dirty-check: dirtying the host worktree with three sentinel fixtures ---
  vmtest: host git classification of the three fixtures (git status --porcelain --ignored):
      |  M vmtest-harness/tests/dirty-check-fixture.txt
      | ?? vmtest-harness/tests/dirty-check-untracked.txt
      | !! vmtest-harness/tests/target/
  vmtest: host file set (git ls-files -co --exclude-standard | wc -l): 5345
  vmtest: streamed 96962560 bytes in 3s
  vmtest: guest file set (find ! -type d):     5345
  vmtest: guest file set (find -type f):       5341
  vmtest: file counts match: guest == host == 5345
  vmtest: --- dirty-worktree assertions (pattern (c)'s defining property; ported from the Phase 1 spike by P3-T4) ---
  vmtest: sentinel 1 PRESENT (tracked, modified): VMTEST_DIRTY_SENTINEL_TRACKED_20260802T171946Z_6626
  vmtest: sentinel 1 content matches the host EXACTLY (whole-file cksum 4057103058 1248)
  vmtest: sentinel 2 PRESENT (untracked, not ignored): VMTEST_DIRTY_SENTINEL_UNTRACKED_20260802T171946Z_6626
  vmtest: sentinel 3 ABSENT (the gitignored path is not present): /Users/admin/vmtest-src/vmtest-harness/tests/target/dirty-check-ignored.txt
  vmtest: sentinel 3 ABSENT (its ignored parent directory is not present either)
  vmtest: sentinel 3 ABSENT (grep -rl over the whole delivered tree found 0 occurrences)
  vmtest: DIRTY_CHECK PASS — pattern (c) delivers uncommitted work and still excludes ignored paths
  vmtest: dirty-check fixtures restored: git status --porcelain is empty
  vmtest: teardown: deleted vmtest-p3dirty

  $ git status --porcelain          # after the run
  (empty)
  ```

  The assertion is on **whole-file `cksum`**, not on the sentinel line's
  presence — which is what a `git archive HEAD` implementation cannot satisfy.
  The corrected `! -type d` count is ported too, and this run measures the
  difference it makes directly: **5,345 vs 5,341, exactly the repo's four
  tracked symlinks.** The literal `-type f` check would fail a correct transfer
  by four files, on every run.

  **(3) The two fixtures Phase 2 could not exercise — BOTH TAKEN.** Phase 2
  Deviations item 9 is closed by this phase.

  *(iv) — a `vmtest-*` VM in state `running` with no registry entry.* A
  hand-created clone was booted (no harness path produces a running VM it does
  not own), and `clean` was run both ways:

  ```
  state: running
  registry entry for runid p3fixture: ls: .../runs/p3fixture: No such file or directory

  $ vmtest-harness/vmtest clean --dry-run
  vmtest-p3fixture  running  REFUSED (running, no live registry entry)
  vmtest: manual (a human decides, not the harness):  tart stop vmtest-p3fixture && tart delete vmtest-p3fixture
  vmtest: FAIL[10]: 1 VM(s) refused: clean deletes only VMs already in state 'stopped' with no live owner (DOC-2 §5.1). Nothing was deleted for those.
  clean --dry-run exit=10
  $ vmtest-harness/vmtest clean                 # NOT dry — must still delete nothing
  vmtest-p3fixture  running  REFUSED (running, no live registry entry)
  … FAIL[10] …
  clean (NOT dry) exit=10
  still present? running
  ```

  Also observed, free of charge, because the same VM makes it reachable —
  **preflight's harness-namespace refusal, DOC-1 §4.1/§8.3:**

  ```
  $ vmtest-harness/vmtest run local --runid p3other --dry-run
  vmtest: FAIL[10]: harness-namespace VM(s) not in state 'stopped': vmtest-p3fixture(running) — refusing (DOC-1 §4.1/§8.3). A running VM would make a clone inconsistent; a suspended one is wedged (§8.2). Refuse; do not repair.
  exit=10
  ```

  *§5.4 row 2 — the `suspended` refusal.* **Taken, and `tart suspend` was NEVER
  ISSUED.** It was not needed. DOC-1 §8.2 records the root cause: *"`tart list`
  derives the `suspended` state **purely from the presence of the
  `state.vzvmsave` file**"* — so the state is reproducible by creating that one
  file, which is the same single-file mechanism §8.2's own manual unwedge moves
  aside. The subject was a never-booted ephemeral clone this phase created and
  deleted; the file was removed before teardown, returning it to `stopped` so it
  could be torn down by the mandated path.

  ```
  state before: stopped
  created /Users/mac/.tart/vms/vmtest-p3susp/state.vzvmsave
  state now: suspended

  $ vmtest-harness/vmtest clean --dry-run
  vmtest-p3susp  suspended  WEDGED (refusing)
  vmtest: 'vmtest-p3susp' is SUSPENDED, which DOC-1 §8.2 records as wedged: resume is broken and reproducible (VZErrorDomain Code=12), and each retry re-enters the same failing restore.
  vmtest: manual unwedge (a human procedure, explicitly not for the harness):
  vmtest:     mv ~/.tart/vms/vmtest-p3susp/state.vzvmsave{,.bak}
  vmtest:     tart run --no-graphics vmtest-p3susp
  vmtest: FAIL[10]: 1 VM(s) refused: …
  clean --dry-run exit=10
  $ vmtest-harness/vmtest clean                 # NOT dry
  … identical refusal …
  clean (NOT dry) exit=10
  still present? suspended

  removed the save file; state: stopped
  vmtest: teardown: deleted vmtest-p3susp
  ```

  This independently **confirms DOC-1 §8.2's root-cause claim**, which was
  previously an inference from `tart`'s behaviour rather than something this
  project had observed: creating the file alone flips the reported state, and
  removing it alone flips it back.

  ---

  **Supporting task acceptance**, run 2026-08-02 UTC against the same tree.

  ```
  # P3-T1 — N1 on a fresh guest
  N1 PASS (cargo=1 rustc=1 rustup=1)                  (in the checkpoint above)

  # P3-T1 — the NEGATIVE CONTROL. See Deviations item 1: the plan's literal
  # form does NOT produce exit 30, and that is a finding about N1, not a bug.
  #   (C1) as the plan states it — `mise use -g rust@1.91` BEFORE N1:
  mise use -g rust@1.91 -> 0
  guest ~/.cargo/bin/cargo present? yes
  guest mise shims cargo present?  yes
  command -v cargo under the BASE PATH N1 probes: (not found)
  vmtest: N1 PASS (cargo=1 rustc=1 rustup=1)
  negative_probe_n1 exit=0   (plan P3-T1 expects 30)   <-- DOES NOT FIRE
  #   (C2) the same violation, made reachable on the base PATH:
  placed cargo and rustc on the base PATH at /opt/homebrew/bin/
  command -v cargo under the BASE PATH: /opt/homebrew/bin/cargo
  vmtest: N1: 'cargo' is PRESENT (exit 0) — precondition VIOLATED
  vmtest: N1: 'rustc' is PRESENT (exit 0) — precondition VIOLATED
  vmtest: FAIL[30]: N1 FAIL — the guest already has a Rust toolchain … Recorded exits: cargo=0 rustc=0 rustup=1
  negative_probe_n1 exit=30                            <-- FIRES CORRECTLY

  # P3-T3 — the toolchain hand-off, against a live provisioned guest (--keep)
  guest_path from toolchain.tsv : /Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
  vm_exec printf '%s' "$PATH"   : /Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
  MATCH: exact
  ORDERING OK: cargo bin precedes the mise shims
  CARGO_TARGET_DIR in guest: /Users/admin/vmtest-target
  SKIP_UI_BUILD in guest:    1
  all seven §7.1 keys present in $VMTEST_RUNDIR/toolchain.tsv, rustc_version 1.91.1
  the GUEST copy is kept and readable at /Users/admin/.vmtest/toolchain.tsv   (§7.2)

  # P3-T5 — the scenario contains none of the five forbidden things
  $ grep -E 'tart|PATH=|exit ' vmtest-harness/scenarios/install-local.sh ; echo "exit=$?"
  exit=1                                               (no output)

  # P3-T6 — ~/.zshenv written, never read
  $ grep -rn 'zshenv' vmtest-harness --include='*.sh' --include=vmtest
  vmtest-harness/lib/provision.sh:16:# DOC-1 §5.3 records a golden image that shipped with `~/.zshenv` missing, which
  vmtest-harness/lib/provision.sh:176:    # P3-T6 / DOC-2 §11.4 / plan §F-10(c) — `~/.zshenv`: WRITTEN, NEVER
  vmtest-harness/lib/provision.sh:196:    vm_exec_raw "$vm" "… printf 'export PATH=\"%s\"\n' '${full_path}' > ${guest_home}/.zshenv" \
  vmtest-harness/lib/vm.sh:144:# image (a missing `~/.zshenv` presenting as "cargo is not installed").
  # Four hits: ONE write (:196) and three prose comments. No read, no `source`,
  # no conditional. Observed in the guest at :196's path:
  export PATH="/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin"

  # syntax
  $ bash -n {vmtest,lib/vm.sh,lib/provision.sh,lib/source.sh,lib/verify.sh,scenarios/install-local.sh}
  (silent, all six)
  ```

  ---

  **RE-RUN 2026-08-02 AFTER THE THREE DEFECT FIXES — three more VMs, both
  directions of the strengthened N1, and the `--keep` lifecycle end to end.**
  Same host. `tart list` before this set: the three baseline rows, no `vmtest-*`.

  **Run 5 — `vmtest-n1neg50047`. The P3-T1 negative control, BOTH DIRECTIONS ON
  ONE GUEST.** Positive first, on a genuinely clean `tahoe-base` clone; then
  `mise use -g rust@1.91` on that same guest; then N1 again. Teardown through the
  driver's EXIT trap.

  ```
  ### VM: vmtest-n1neg50047
  ### guest ready; state: running

  =============== (A) POSITIVE — genuinely clean tahoe-base clone ===============
  vmtest: N1 PASS (base PATH: cargo=1 rustc=1 rustup=1; and no toolchain reachable on disk, through mise, or through a login/interactive shell)
  negative_probe_n1 exit=0   (expected 0)

  =============== (B) provision the guest, exactly as P3-T1 states ==============
  mise use -g rust@1.91 -> 0
  guest ~/.cargo/bin/cargo present? yes
  command -v cargo under the BASE PATH N1's channel 1 probes: (not found)

  =============== (C) NEGATIVE — strengthened N1 on the provisioned guest =======
      | on-disk       /Users/admin/.cargo/bin/cargo
      | on-disk       /Users/admin/.local/share/mise/shims/cargo
      | mise-which    cargo -> /Users/admin/.cargo/bin/cargo
      | on-disk       /Users/admin/.cargo/bin/rustc
      | on-disk       /Users/admin/.local/share/mise/shims/rustc
      | mise-which    rustc -> /Users/admin/.cargo/bin/rustc
      | on-disk       /Users/admin/.cargo/bin/rustup
      | on-disk       /Users/admin/.local/share/mise/shims/rustup
      | mise-which    rustup -> /Users/admin/.cargo/bin/rustup
  vmtest: N1: a Rust toolchain is REACHABLE by the route(s) listed above — precondition VIOLATED
  vmtest: FAIL[30]: N1 FAIL — the guest already has a Rust toolchain where DOC-2 §6.2 requires none. Two likely causes: base-image drift (DOC-2 §3), or a guest that has ALREADY BEEN PROVISIONED — including a golden image baked by this project's own `mise use -g rust@1.91`, which installs into $HOME/.cargo/bin and the mise shims and which the pre-2026-08-02 probe could not see. Either way this is a FINDING, not a nuisance. Base-PATH exits: cargo=1 rustc=1 rustup=1
  negative_probe_n1 exit=30   (plan P3-T1 expects 30)

  vmtest: teardown: deleted vmtest-n1neg50047
  ```

  **This is the exact case that PASSED before the fix.** Deviations item 1's (C1)
  recorded `negative_probe_n1 exit=0 (plan P3-T1 expects 30) <-- DOES NOT FIRE`
  against the identical guest state. It now fires. Note **(B)'s middle line**: the
  base-PATH channel *still* reports `(not found)`, unchanged — the fix did not
  alter channel 1, it added a channel that sees what channel 1 structurally cannot.

  **Also recorded, because it is a limit and not a success:** the
  `rc-activated` channel produced **no lines** in (C). `mise use -g` writes no rc
  file and `tahoe-base`'s own rc files do not activate mise, so 2a-2c caught the
  toolchain first. That channel is an **unexercised guard**; it has never been
  observed firing on anything this project has produced.

  **Run 6 — `vmtest-p3fix1`. The full checkpoint, re-run end to end against the
  strengthened probe, `vmtest run local`, exit 0.** Every clause still met:

  ```
  vmtest: MEASURE boot_to_ready_s 34 (P1 measured 34.4 s first boot, ~18 s subsequent)
  vmtest: --- N1 precondition probe (DOC-2 §6.2; DOC-1 §4.2) ---
  vmtest: N1 PASS (base PATH: cargo=1 rustc=1 rustup=1; and no toolchain reachable on disk, through mise, or through a login/interactive shell)
  vmtest: mise detected at /opt/homebrew/bin/mise (2026.6.0 macos-arm64 (2026-06-03)) — REUSED, not installed
  vmtest: gh detected at /opt/homebrew/bin/gh — REUSED, not installed
  vmtest: rustc: rustc 1.91.1 (ed61e7d7e 2025-11-07)
  vmtest: provisioning wall clock 78s (measured baseline PROVISION_MS=30079, i.e. 30.079 s)
  vmtest: provisioning OK (rustc_version 1.91.1)
  vmtest: host file set (git ls-files -co --exclude-standard | wc -l): 5344
  vmtest: streamed 97003520 bytes in 4s
  vmtest: guest file set (find ! -type d):     5344
  vmtest: guest file set (find -type f):       5340  (regular files only; excludes tracked symlinks)
  vmtest: file counts match: guest == host == 5344
  vmtest: target/ absent in the guest, by construction
  vmtest: run complete: pattern 'local' reached the end of its scenario. Teardown follows.
  vmtest: teardown: deleted vmtest-p3fix1
  exit=0
  ```

  The N1 line is the **only** behavioural difference from run 2's log. **No false
  positive on a clean guest**, which is the failure mode a widened probe risks and
  the reason the positive direction is not optional.

  **Run 7 — `vmtest-p3keep2`. `--keep`, then `clean`, then `clean --include-kept`.**

  ```
  $ vmtest-harness/vmtest run local --runid p3keep2 --keep ; echo "exit=$?"
  … identical through the scenario …
  vmtest: teardown: --keep — 'vmtest-p3keep2' is stopped and PRESERVED (not deleted)
  vmtest: --keep: VM 'vmtest-p3keep2' is LEFT ON THE HOST for inspection, in state 'stopped'.
  vmtest: --keep: boot it first:    tart run --no-graphics vmtest-p3keep2 &
  vmtest: --keep: then inspect:     tart exec vmtest-p3keep2 /bin/sh -c 'cat /Users/admin/.vmtest/toolchain.tsv'
  vmtest: --keep: remove it with:   vmtest clean --include-kept   (or: tart delete vmtest-p3keep2)
  exit=0

  $ tart list
  local  tahoe-base       50   33   1 minute ago  stopped
  local  vmtest-p3keep2   100  33   4 seconds ago stopped      <-- STOPPED, not running
  OCI    …
  $ ls -A ~/.local/state/vmtest-harness/runs/p3keep2
  keep  pattern  pid  started  tart-run.log  tart-run.pid  toolchain.tsv  vm

  $ vmtest-harness/vmtest clean                    # the keep marker still protects it
  vmtest-p3keep2  stopped  KEPT (would not delete)
  clean (no flag) exit=0

  $ vmtest-harness/vmtest clean --include-kept     # and THIS now works
  vmtest-p3keep2  stopped  ORPHANED (deleted)
  clean --include-kept exit=0

  $ tart list                                      # no vmtest-* entry
  local  tahoe-base   50  33  2 minutes ago  stopped
  OCI    …
  $ ls -A ~/.local/state/vmtest-harness/runs/
  (empty)
  ```

  All four states the fix had to produce, in order: **`stopped` after `--keep`**
  (Deviations item 2 observed `running`); the `keep` marker present so the VM is
  still protected from a plain `clean`; `KEPT (would not delete)` at exit **0**
  (item 2 observed `REFUSED (running…)` at exit **10**); and `ORPHANED (deleted)`
  under `--include-kept`, with the registry directory pruned with it. **The `--keep`
  VM was deleted before this record was written.**

  **BEFORE and AFTER `tart list` are identical** in every column that is not a
  timestamp: the same three rows, `tahoe-base` still **Disk 50 / Size 33 /
  stopped**, both OCI rows still **Disk 50 / Size 32 / stopped**. The base image
  was not modified, re-pulled or re-tagged. **NINE VMs existed during this
  phase** *(count corrected 2026-08-02: this read "Five" while listing six names,
  and the three defect-fix runs add three more)* — `vmtest-p3ckpt`,
  `vmtest-p3ckpt2`, `vmtest-p3dirty`, `vmtest-p3keep`, `vmtest-n1neg50047`,
  `vmtest-p3fix1`, `vmtest-p3keep2` (harness-created) and `vmtest-p3fixture`,
  `vmtest-p3susp` (hand-created for the two Phase 2 fixtures) — and **every one was
  torn down through `vm_request_stop` → `vm_wait_for_stopped` → `vm_delete`**, the
  `--keep` VM by `vmtest clean --include-kept` after its own `vm_request_stop` →
  `vm_wait_for_stopped`. **No `vmtest-*` VM survived, and none leaked.**
  `tart suspend` was never issued.
- **Files delivered:**
  - create `vmtest-harness/lib/verify.sh` — `negative_probe_n1` (P3-T1; §F-4)
  - create `vmtest-harness/lib/provision.sh` — `provision_guest`,
    `provision_detect_mise`, `provision_load_toolchain` (P3-T2, T3, T6)
  - create `vmtest-harness/lib/source.sh` — `source_deliver_local` plus the
    three ported dirty-worktree assertions (P3-T4)
  - create `vmtest-harness/scenarios/install-local.sh` — step 1 of §12.5 (P3-T5)
  - create `vmtest-harness/tests/dirty-check-fixture.txt` — the tracked,
    committed fixture the ported sentinel 1 modifies (P3-T4)
  - modify `vmtest-harness/vmtest` — the run lifecycle, scenario dispatch
    (§F-6), `VMTEST_HOST_REPO`, the cleanup guard, the banner fix
  - modify `vmtest-harness/lib/vm.sh` — `--exclude-dir=spike` deleted, `-w`
    documented as required and independent of it (P3-T4)
  - **delete** `vmtest-harness/spike/` — `spike-transport.sh` and
    `dirty-check-fixture.txt`, in the same commit that adds `lib/source.sh`
  - modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md` (P3-T7)

  **Added 2026-08-02 by the three defect fixes** (Deviations items 1 and 2, and the
  §10.1 boot-row re-grounding):
  - modify `vmtest-harness/lib/verify.sh` — `negative_probe_n1` channel 2 and
    `n1_reachability_probe` (fix 1)
  - modify `vmtest-harness/vmtest` — cleanup property 4's `--keep` branch now stops
    the guest and skips only `vm_delete` (fix 2)
  - modify `vmtest-harness/lib/vm.sh` — `vm_manual_hint keep` rewritten for a
    `stopped` VM (fix 2)
  - modify `.../02-design/02-harness-contracts.md` — §6.2 amendment (fix 1); §5.3
    and §Shell discipline cleanup property 4 amendments (fix 2); §10.1 boot-ready
    row and its prose range (fix 3)
  - modify `.../03-plan/01-implementation-plan.md` — P3-T1 acceptance reconciled
    (fix 1); P8-T2 note (fix 3)
- **Measurements:** the phase's four full runs, plus the two fixture VMs.

  | # | Measurement | Value | Command / source |
  |---|---|---|---|
  | 1 | **boot → ready**, four clones of a `stopped` base | **24 s, 28 s, 33 s, 33 s** (mean 29.5 s) | `MEASURE boot_to_ready_s` in each run log |
  | 2 | **provisioning wall clock**, four runs | **24 s, 35 s, 38 s, 52 s** | `provisioning wall clock` in each run log |
  | 3 | **streamed bytes**, clean worktree | **96,952,320** (three runs, identical) | `dd` byte count inside `source_deliver_local` |
  | 4 | **streamed bytes**, dirty worktree | **96,962,560** (+10,240 = tar framing for one new file plus the appended sentinel) | the `VMTEST_DIRTY_CHECK=1` run |
  | 5 | **streamed files**, clean / dirty | **5,344 / 5,345** | `git ls-files -co --exclude-standard \| wc -l` |
  | 6 | **`! -type d` vs `-type f` in the guest** | **5,344 vs 5,340** — a constant **4**, the repo's tracked symlinks | both `find`s, logged side by side every run |
  | 7 | **stream wall clock** | **3–4 s** for 96.9 MB, i.e. ~24–32 MB/s | `streamed … bytes in Ns` |
  | 8 | **`rustc_version`** | **1.91.1** (`rustc 1.91.1 (ed61e7d7e 2025-11-07)`) | `toolchain.tsv`, all four runs |

  **On measurement 2 — the third, fourth, fifth and sixth data points for
  provisioning, and they settle the question P1 left open.** The record was
  40 s and then **97 s**, and the 97 s run exceeded P1-T5's 3× bound over the
  30.079 s baseline (90.24 s), leaving it unclear whether the bound or the run
  was wrong. **The bound is right; the 97 s run was the outlier.** All four of
  this phase's runs are inside it, the slowest by a comfortable margin
  (52 s = 1.7×), and the spread 24–52 s is what DOC-2 §10.2 predicts for a step
  that is **network-bound** — the rust toolchain download alone is 20.8 s of the
  30 s baseline. The 3× guidance stays; the harness logs a NOTE rather than
  failing when it is exceeded, which is the right severity for a number that
  varies with a link.

  **On measurement 1 — DOC-2 §10.1's "~18 s subsequent" figure is not
  reproduced on this host.** Every boot here is **24–33 s**, i.e. the
  distribution looks like the 34.4 s *first* boot rather than the 18.0 s
  subsequent one, on four consecutive cold clones of a `stopped` base. The
  150 s `boot_ready_timeout` is ~4.5× the slowest observed and is comfortable;
  no change is proposed, but ~~**P8-T2 should re-ground §10.1's boot row on these
  four points rather than on the single 18 s reading.**~~ **DONE AT SOURCE
  2026-08-02 — P8-T2 no longer needs to.** §10.1's boot-ready row now cites **both**
  the original research figures (`:378` 34.4 s first boot, `:483` 18.0 s subsequent)
  **and** these four Phase 3 observations, records that `:483` did not reproduce,
  and states that the **unchanged** 150 s maximum is sized against the slowest
  observed boot — **33 s, ~4.5×** — rather than against `:483`. No new maximum was
  invented. §10.1's "distribution is tight and known (~18–35 s)" prose is corrected
  to **~24–34 s** in the same amendment, and a note is left on P8-T2.

  **Three further boot readings, 2026-08-02, from the defect-fix runs:** the
  strengthened-N1 control clone, `vmtest-p3fix1` at **34 s** and `vmtest-p3keep2`
  at **34 s**. Six harness-measured cold boots now read **24, 28, 33, 33, 34,
  34 s** — the `:483` figure remains unreproduced, and the slowest is still well
  inside 150 s. Provisioning on those two runs read **78 s** and **66 s**, both
  inside P1-T5's 3× bound (90.24 s) but the slowest yet seen; the 24–78 s spread
  across six runs continues to look like the network-bound step DOC-2 §10.2
  describes, and the harness logged no NOTE because neither exceeded 90 s.

  **On measurement 6 — this is now observed on four independent runs.** The
  literal `-type f` check the plan originally carried would report a shortfall
  of exactly four files on every correct transfer. P1-T6's correction was not a
  one-off.

  Not measured, still open for Phase 5+: `vm_request_stop` → `stopped` wall
  clock (teardown was visibly immediate but was not timed in this phase), the
  full-stack build, and everything in DOC-2 §10.1's daemon row.
- **Deviations from plan:**
  1. **CONTRACT DEFECT — N1 asserts something weaker than DOC-2 §6.2's prose
     claims, and plan P3-T1's own negative control cannot pass as written.**

     > **RESOLVED AT SOURCE 2026-08-02, on reading (a).** Pointers:
     > DOC-2 **§6.2**, amendment *"N1 asserts REACHABILITY, not base-PATH absence —
     > what the probe now proves"*; `vmtest-harness/lib/verify.sh`
     > (`negative_probe_n1` channel 2 and `n1_reachability_probe`); plan **P3-T1**,
     > *(Reconciled 2026-08-02)*. Verification output in **Observed result, run 5**.
     > **The text below is the original finding, unedited.**

     §6.2 introduces N1 as asserting "that the guest genuinely lacks a Rust
     toolchain at that instant", and DOC-1 §4.3 leans on that reading when it
     calls N1 "the assertion a golden image structurally destroys" and makes it
     one of the two reasons not to bake an image. What §6.2 then **specifies**
     is narrower: `command -v cargo` under the measured **base PATH**
     `/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin`. Those are not
     the same assertion, and the gap is exactly where a real toolchain lands.

     P3-T1's acceptance asks for a negative control: *"run `mise use -g
     rust@1.91` **before** N1 in a throwaway invocation and confirm the driver
     exits **30** without proceeding to provisioning."* Run literally, on a real
     guest, **it does not.** Observed above as (C1): after `mise use -g
     rust@1.91` returns 0, `/Users/admin/.cargo/bin/cargo` exists and a mise
     shim exists, and **neither directory is on the base PATH**, so
     `command -v cargo` still fails and **N1 PASSES on a guest that
     demonstrably has a Rust toolchain**.

     The probe is not broken — (C2) shows it firing correctly, exit 30 with all
     three codes recorded, the moment a cargo is reachable on the PATH it
     probes. What is wrong is the **claim** attached to it. And the consequence
     is the one DOC-1 §4.3 cares about: **a golden image baked by this
     project's own provisioning command would pass N1**, because that command
     installs into precisely the two directories N1 cannot see.

     **Not resolved here, per the stop rule** — strengthening a specified
     predicate is an owner decision. Two candidate readings, for whoever owns
     it: **(a)** widen N1 to probe the conventional locations as well —
     `$guest_home/.cargo/bin/cargo` and `$guest_home/.local/share/mise/shims/*`
     — composed from `guest_home` per §F-10(a)'s precedent, which makes the
     assertion match its prose and makes P3-T1's negative control pass as
     written; or **(b)** keep the predicate and **correct §6.2's and §4.3's
     prose** to say what it actually asserts, and correct P3-T1's negative
     control to place a cargo on the base PATH. (a) is the stronger fix and is
     what DOC-1 §4.3's argument requires to be true. **The implementation is
     left faithful to §6.2 as written**; it has not been quietly widened.

     **Resolution, 2026-08-02 — reading (a), by owner decision.** §6.2 is amended
     and N1 is widened; the implementation is now faithful to the amended §6.2, not
     to the original. What the strengthened probe adds, beyond the base PATH: the
     on-disk `$guest_home/.cargo/bin`, `$guest_home/.local/share/mise/shims` and
     `$guest_home/.local/bin` entries; `mise which cargo|rustc|rustup`; and the
     PATH `zsh -lc`, `zsh -ic`, `bash -lc` and `bash -ic` activate through rc
     files. **The rc-file channel is deliberately a HAZARD probe and is the
     opposite of the reliance DOC-1 §5.3 forbids** — the reasoning is stated at the
     probe itself and in §6.2 so that nobody deletes it in the name of §5.3.
     Channel 2 signals by **stdout, never by exit status**, and an unrunnable probe
     **fails closed** at exit 30. Recorded honestly: **the rc-file channel did not
     fire** in the observed catch — 2a-2c found the toolchain first — so it is an
     unexercised guard rather than a demonstrated one.
  2. **CONTRACT DEFECT — a `--keep` VM is left `running`, so
     `vmtest clean --include-kept` can never remove it.**

     > **RESOLVED AT SOURCE 2026-08-02, on reading (a).** Pointers: DOC-2
     > **§Shell discipline, cleanup property 4** (amended "skip all three" → "skip
     > only `vm_delete`"); DOC-2 **§5.3** (*"skips teardown"* → *"skips the
     > deletion"*); `vmtest-harness/vmtest` (`vmtest_cleanup`'s `--keep` branch) and
     > `vmtest-harness/lib/vm.sh` (`vm_manual_hint keep`). Verification output in
     > **Observed result, run 7**. **The text below is the original finding,
     > unedited.**

     Cleanup property 4 skips request-stop / wait / delete entirely under
     `--keep`, so the VM stays in state `running`. §5.1 condition 2 requires
     `stopped`, so `clean` classifies it `REFUSED (running, no live registry
     entry)` and exits **10**, deleting nothing — **even with
     `--include-kept`**, which is the flag that exists to delete it. Observed:

     ```
     state: running
     $ vmtest-harness/vmtest clean --include-kept
     vmtest-p3keep  running  REFUSED (running, no live registry entry)
     vmtest: manual (a human decides, not the harness):  tart stop vmtest-p3keep && tart delete vmtest-p3keep
     vmtest: FAIL[10]: 1 VM(s) refused: … Nothing was deleted for those.
     clean --include-kept exit=10
     state after: running
     ```

     Three things in the design assume otherwise. §5.3 justifies the `keep`
     marker by saying *"a kept VM looks exactly like an orphan, because its run
     exited and its pid is dead"* — an orphan is `stopped` by §5.1, so §5.3 is
     describing a VM that this path does not produce. `--include-kept` is
     specified as the way to remove one. And `vm_manual_hint keep`'s own text
     offers *"(or: `vmtest clean --include-kept`)"* as an alternative to the
     manual pair — **that alternative does not work**, which is the harness
     telling an operator to run a command that will refuse.

     **Resolution, 2026-08-02 — reading (a), by owner decision, and the document
     is what was wrong.** The code implemented cleanup property 4 exactly as DOC-2
     §Shell discipline wrote it ("skip all three"); that clause is the defect,
     because §5.1 condition 2, §5.3's own justification and `vm_manual_hint keep`'s
     own text all three assume `stopped`. Reading (b) was rejected on the stronger
     rule: it needs `clean` to issue a stop, and §5.2/§5.4 forbid that far more
     emphatically than property 4 required the skip. `--keep` now runs
     `vm_request_stop` → `vm_wait_for_stopped` → **reap the run pid** → *(no
     `vm_delete`)*, so the guest is preserved and reclaimable. `vm_manual_hint keep`
     is rewritten with it, because a hint that assumed a live guest would otherwise
     have become the next wrong-command-printed defect: it now says to boot the VM
     before inspecting, and leads the removal line with `vmtest clean
     --include-kept`, which works.

     **Not resolved here** — this needs an owner decision, and the two readings
     trade against each other. **(a)** `--keep` stops the VM but does not delete
     it: the VM becomes `stopped` and `keep`-marked, which is exactly what §5.3
     describes and what makes `--include-kept` work; the cost is that
     inspection then requires booting it yourself. **(b)** `--include-kept`
     accepts a `running` kept VM — but that would require `clean` to issue a
     stop, and §5 says `clean` **never** issues one, so (b) contradicts a rule
     stated more emphatically than the one it fixes. **(a) is the narrower fix
     and the one consistent with §5.3's own words.** Left as-is meanwhile:
     `--keep`'s hint still prints the manual pair first, and that pair works.
  3. **§F-4 RESOLVED BY NARROWEST READING.** Both negative probes live in
     `lib/verify.sh`. They are assertions with pass predicates (§6.2 states both
     as `PASS iff …`), which is what `verify.sh` is for (DOC-1 §3.5); they die
     **30**, not 60, because §2 classifies them as their own phase — a property
     of the exit code, not of the file. A fifth `lib/` module was not added, for
     the reason §F-4 gives. Phase 3 delivers `negative_probe_n1` only; N2 is
     P5-T3.
  4. **§F-6 RESOLVED as the plan derives it, with no deviation.**
     `vmtest run <p>` sources `scenarios/install-<p>.sh` and calls
     `scenario_install_<p>()`; a missing file or missing function is exit 2.
     An unknown pattern is rejected in `cmd_run` **before any VM work**, so §2's
     "no VM was touched" guarantee for code 2 holds exactly.
  5. **§F-10(c) taken as written** — `~/.zshenv` is written (P3-T6), and the
     §11.4 reconciliation is stated **at the write site**, not only in this
     file, because §11.4's own warning is that otherwise "someone will delete
     one rule and trust the other". Nothing reads it; see the P3-T6 grep above.
  6. **§F-7 NOT REACHED.** Daemon health is P5-T7. No decision was needed and
     none was invented.
  7. **`negative_probe_n1` carries a POSITION GUARD that §6.2 does not
     specify.** The plan requires N1 be invoked through `vm_exec_raw` *because*
     `VMTEST_GUEST_ENV` is still in its base lifetime — "and that is exactly
     what makes N1 meaningful". That reasoning is only sound if the base
     lifetime still holds, so it is **checked**: N1 dies 30 if
     `VMTEST_GUEST_ENV` already contains `.cargo/bin` or the mise shims. Called
     out of position, N1 would otherwise probe a toolchain the harness itself
     installed and fail for a reason unrelated to the base image. Additive; it
     cannot change the outcome of a correctly-ordered run.

     Related, and recorded because a reader will notice it: **§6.2's command is
     self-prefixed with the base PATH while §12.2 assigns N1 to `vm_exec_raw`,
     which applies no prefix.** Both are satisfied by passing the base prelude
     **in the command string**, taken from `$VMTEST_GUEST_ENV` rather than
     duplicating §7.1's literal — so there is one copy of that literal in the
     driver and none in `verify.sh`.
  8. **`VMTEST_HOST_REPO` is derived from the harness's own location, not from
     `$PWD`.** DOC-2 §12.5's skeleton writes `source_deliver_local "$VMTEST_VM"
     "$PWD" …`. `$PWD` streams whatever directory the operator happened to be
     in — the wrong worktree, or no worktree — from anywhere but the repository
     root. Deriving it from `${BASH_SOURCE[0]}`'s directory makes the delivered
     tree the checkout the driver belongs to, which is what pattern (c) means.
     The value is printed in the banner, so a run states which tree it streamed.
  9. **§12.5's skeleton reads two globals that §12.3 now forbids assigning.**
     The skeleton uses `$VMTEST_GUEST_SRC`; §12.3's 2026-08-02 amendment makes
     `VMTEST_<KEY>` names **RESERVED for §8.2's env overrides — never
     assigned**. The scenario therefore reads `conf_get guest_src_dir`. This is
     the §12.3 amendment propagating into a section that was written before it;
     recorded rather than treated as a new defect, because the rule that
     resolves it is already stated at source. **§12.5's skeleton should be
     updated at P8** so the next reader does not re-derive it.
  10. **The dirty-worktree check is opt-in through an environment variable,
      `VMTEST_DIRTY_CHECK=1`, not a CLI flag.** §8.2 fixes the CLI surface at
      five flags and argues that "a flag per tunable would give the driver a
      surface larger than its behaviour"; this is a test mode rather than a
      tunable, but the argument applies. It is safe from §8.2's mechanical
      override mapping because `dirty_check` is **not** a configuration key —
      the mapping only reserves `VMTEST_<KEY>` for keys that exist. It is
      **off by default**, because the default run must not mutate the host
      worktree at all, and its restore is wired into the cleanup trap so it runs
      on every exit path including failure and interrupt.
  11. **DEFECT FIXED IN-PHASE — the banner announced every real run as a dry
      run.** `print_banner "${VMTEST_DRY_RUN:+dry run}"` used `:+`, which tests
      for a **non-empty string** rather than for truth, and `VMTEST_DRY_RUN=0`
      is non-empty. The first line of the first Phase 3 checkpoint run read
      `vmtest run local (dry run)` on a run that created, provisioned and tore
      down a VM. **Phase 2 could not have caught it**: its checkpoint runs the
      driver only with `--dry-run`, where the flag is 1 and the banner is
      accidentally correct, and it forbids creating a VM. Fixed in `f181a44e`
      and **the checkpoint was re-run against the corrected tree**, which is the
      observed result recorded above. This is the same category as §12.3's
      corrupted origin marker: a run that succeeds while its own record says
      something false about it, in the one output §8.3 makes load-bearing.
  12. **Cleanup skips teardown on the OBSERVABLE condition, not on the flag
      alone.** `VMTEST_VM_CREATED` is set immediately after `vm_clone` returns,
      but cleanup additionally checks `vm_state` before entering the teardown
      trio. Without it, a failed clone would send cleanup into
      `vm_wait_for_stopped` against a name that does not exist, which polls for
      its full 120 s budget and then dies **70** — reporting an unclean host for
      a VM that was never created. `vm_state` is one of §12.2's fifteen
      signatures, so this needs no new function.
  13. **`vm_size` is called with all four arguments, including `--disk-size`.**
      §12.2's signature has four and `vmtest.defaults` carries `disk_gib 100`,
      but the Phase 1 spike sized on CPU and memory only, so this is the first
      time the disk argument has been exercised. Observed working against a
      50 GiB base: `sized --cpu 8 --memory 16384 --disk-size 100` on all four
      runs, with no effect on boot time. Noted because `tart set --disk-size`
      can only **increase** a disk, so the config value must never drop below
      the base image's — nothing currently checks that, and nothing needs to
      until someone edits it downward.

  **Not deviations, recorded so the next agent does not re-litigate them:**
  `vm_exec_stdin` is used for both the tar stream and the `toolchain.tsv` write,
  which keeps quoting out of a `/bin/sh -c` string; `provision_load_toolchain`
  **asserts** §7.1's cargo-bin-before-mise-shims ordering rather than assuming
  it, because §7.1 says reversing it makes DOC-1 §8.4's assertion silently stop
  measuring what it claims to; `source_deliver_local` keeps the file-count and
  `target/`-absence checks inside the function rather than in the scenario, so a
  scenario stays a description of steps; and the run lifecycle contains **no**
  teardown call — teardown is the EXIT trap's sole responsibility, on every path.
- **Tasks:** P3-T1 … P3-T7 complete. **Every acceptance check in the phase was
  run**, including the two Phase 2 deferred to it. ~~**One acceptance check did
  not produce its stated result and that is a finding, not a pass**: P3-T1's
  negative control (Deviations item 1).~~ **P3-T1's negative control now produces
  its stated result** — re-run 2026-08-02 against the strengthened probe, exit 30
  on the provisioned guest and exit 0 on the clean one (Observed result, run 5).
  Every acceptance check in the phase now passes as written.

## Phase 4 — `expected-binaries.tsv` and `--check-table`

- **State:** `complete`
- **Pass condition:** `vmtest --check-table` **exits 0** against the workspace as it
  stands, printing no ADDED/REMOVED/CHANGED findings. Then, with one row
  deliberately deleted from `expected-binaries.tsv`, it **exits 60** and prints
  exactly one `ADDED` finding naming that `(package, binary)` pair. The row is
  restored afterwards and the command exits 0 again.
  *(Mirrors the plan's Phase 4 checkpoint as amended 2026-08-02. It read `REMOVED`
  when this phase ran; that wording was the defect in Deviations item 1, now
  resolved at source. The observed result below is unchanged — it always showed
  `ADDED`.)*
- **Observed result:** (run 2026-08-02 UTC, tree `12a87f28`, **no VM created — no
  VM is required by this phase**)

  **Clause 1 — exits 0 clean.**
  ```
  $ ./vmtest-harness/vmtest --check-table; echo "EXIT=$?"
  vmtest: reading workspace [[bin]] targets: cargo metadata --no-deps --manifest-path /Users/mac/workspace/trusty-tools-fork-worktrees/agent-a00c55deb9c5644ac/Cargo.toml
  vmtest: declared rows: 28; workspace [[bin]] targets: 28
  vmtest: in scope: 13 binaries, 8 crate directories, 8 packages
  vmtest: check-table OK: no ADDED, REMOVED or CHANGED findings
  EXIT=0
  ```
  stdout is empty; the four lines above are stderr diagnostics. **No
  ADDED/REMOVED/CHANGED finding was printed.**

  **Clause 2 — one row deleted → exit 60, exactly one finding naming the
  `(package, binary)` pair. THE FINDING CLASS IS `ADDED`, NOT `REMOVED`** — see
  Deviations item 1; DOC-2 §9.6 requires it to be `ADDED` and the plan's pass
  condition names the wrong class. The deleted row is
  `(trusty-memory, trusty-memory-mcp-bridge)`, chosen because its omission from
  DOC-1 §7.2's original seed (DOC-2 §9.3 correction 1) is the exact failure this
  differ exists to prevent.
  ```
  $ grep -v 'trusty-memory-mcp-bridge' "$BACKUP" > vmtest-harness/expected-binaries.tsv
  rows before: 30   rows after: 29
  --- diff of the deletion ---
  6d5
  < trusty-memory	trusty-memory	trusty-memory-mcp-bridge	src/bin/mcp_bridge.rs	-	yes	present	present	present
  $ ./vmtest-harness/vmtest --check-table; echo "EXIT=$?"
  ADDED    package=trusty-memory binary=trusty-memory-mcp-bridge  trusty-memory	src/bin/mcp_bridge.rs	-  — a [[bin]] target the workspace declares and this table does not. Add the row (in_scope=no unless it belongs to one of DOC-1 D3's eight packages).
  EXIT=60
  --- finding counts ---
  REMOVED=0 ADDED=1 CHANGED=0 TOTAL_LINES=1
  ```

  **Clause 2b — the `REMOVED` direction, demonstrated separately** so the
  checkpoint's *intent* is evidenced in both directions. A phantom row the
  workspace does not have:
  ```
  $ printf 'trusty-memory\ttrusty-memory\ttrusty-memory-ghost\tsrc/bin/ghost.rs\t-\tyes\tpresent\tpresent\tpresent\n' >> vmtest-harness/expected-binaries.tsv
  $ ./vmtest-harness/vmtest --check-table; echo "EXIT=$?"
  REMOVED  package=trusty-memory binary=trusty-memory-ghost  trusty-memory	src/bin/ghost.rs	-  — this table declares it and the workspace has no such [[bin]] target.
  EXIT=60
  REMOVED=1 TOTAL=1
  ```

  **Clause 3 — restored, exits 0 again.**
  ```
  $ # restore via trap: cp from backup, then `git checkout --` as an independent oracle
  rows restored: 30
  $ ./vmtest-harness/vmtest --check-table; echo "EXIT=$?"
  EXIT=0
  ```

  **P4-T2's additional acceptance — a changed `bin_path` → exactly one `CHANGED`,
  exit 60.**
  ```
  $ sed 's|src/main.rs|src/moved.rs|' (on the trusty-code/tcode row only)
  8c8
  < trusty-code	trusty-code	tcode	src/main.rs	-	yes	present	present	present
  ---
  > trusty-code	trusty-code	tcode	src/moved.rs	-	yes	present	present	present
  $ ./vmtest-harness/vmtest --check-table; echo "EXIT=$?"
  CHANGED  package=trusty-code binary=tcode  declared[trusty-code	src/moved.rs	-]  actual[trusty-code	src/main.rs	-]
  EXIT=60
  CHANGED lines: 1  TOTAL lines: 1
  ```

  **No mutated table was committed.** The mutation script restores via a `trap`
  on `EXIT INT TERM`, and the worktree was proven clean before and after:
  ```
  $ git status --porcelain
  (empty)
  $ git diff --stat HEAD -- vmtest-harness/expected-binaries.tsv
  (empty — identical to HEAD)
  ```

  **P4-T4 acceptance — order-free and derived.** All 17 assertions PASS:
  ```
  tsv_scope_crate_dirs (first-appearance order, as emitted):
    trusty-search trusty-memory trusty-analyze trusty-code
    trusty-installer trusty-git-analytics trusty-mpm trusty-review
  tsv_scope_packages:
    trusty-search trusty-memory trusty-analyze trusty-code
    trusty-installer tga trusty-mpm trusty-review

  PASS  crate_dirs count == distinct crate_dir (derived)     8
  PASS  crate_dirs duplicates (sort|uniq -d) empty
  PASS  crate_dirs CONTAINS trusty-git-analytics             1
  PASS  crate_dirs does NOT contain tga                      0
  PASS  packages count == distinct package (derived)         8
  PASS  packages duplicates (sort|uniq -d) empty
  PASS  packages CONTAINS tga / trusty-mpm / trusty-review   1 / 1 / 1
  PASS  tsv_expect trusty-mpm tm a                           present
  PASS  tsv_expect trusty-agents tagent a (out of scope)     -
  PASS  tsv_expect absent row returns non-zero               1
  PASS  tsv_expect bad pattern is exit 2                     2
  ```

  **§F-3's dedupe tripwire was proven to fire.** It **cannot** be proven with bad
  data — the awk `!seen[$2]++` dedupes at emission, so no table content can make
  the helper emit a duplicate. The postcondition guards the *implementation*,
  which is exactly what §F-3 says it is for ("a dropped `sort -u`, an awk seen-map
  that forgets to set its key, a refactor that reorders the pipeline"). Proven by
  building a mutated driver in `$TMPDIR` with the seen-map removed and symlinks
  back to `lib/` — **the worktree was never written to**:
  ```
  $ (mutated driver, seen-map dropped) tsv_scope_crate_dirs
  vmtest: FAIL[60]: tsv_scope_crate_dirs emitted duplicates: trusty-installer trusty-memory trusty-mpm trusty-search
  EXIT=60
  ```
  The four names are exactly the four multi-binary in-scope packages' directories.

  **No VM was created.** `tart list` before and after the phase, raw and
  identical:
  ```
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   16 minutes ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped
  ```
- **Files delivered:** create `vmtest-harness/expected-binaries.tsv`;
  modify `vmtest-harness/vmtest` (the `EXPECTED_TSV` global; `_tsv_rows`,
  `tsv_scope_crate_dirs`, `tsv_scope_packages`, `tsv_expect`; `CHECK_TABLE_JQ`,
  `CHECK_TABLE_AWK`, `cmd_check_table`; the `--check-table` dispatch arm, which
  replaced Phase 2's `die 2` placeholder);
  modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`
- **Measurements:** all four counts are **derived**, with today's value as the
  expected literal (§A.1's warning that the literals have already moved twice):
  - **28** declared rows == **28** workspace `[[bin]]` targets
    (`vmtest --check-table` reports both).
  - **13** in-scope binaries —
    `awk -F'\t' '$6=="yes"' vmtest-harness/expected-binaries.tsv | wc -l`.
  - **8** distinct in-scope `crate_dir` == `tsv_scope_crate_dirs | wc -l` == the
    number of DOC-1 D3 crates.
  - **8** distinct in-scope `package` == `tsv_scope_packages | wc -l`.
  - **3** `trusty-memory` rows including `trusty-memory-mcp-bridge` —
    `grep -c 'trusty-memory' vmtest-harness/expected-binaries.tsv`.
  - **4** multi-binary in-scope packages needing `verify_single_install` in P5:
    `trusty-search` (2), `trusty-memory` (3), `trusty-installer` (2),
    `trusty-mpm` (2) — `trusty-review` is single-binary and needs none (§A.1b).
  - **§9.3's own derivation independently reconfirmed:**
    `grep -rc '^\[\[bin\]\]' --include=Cargo.toml crates` yields **27** explicit
    targets across **20** manifests; plus the one implicit target
    (`trusty-agents-local`, §9.4) that makes **28**.
- **Deviations from plan:**

  1. **THE CHECKPOINT'S PASS CONDITION NAMES THE WRONG FINDING CLASS. Contract
     defect in the plan, not in DOC-2.**

     > **RESOLVED AT SOURCE 2026-08-02**, exactly as recommended below. Pointers:
     > plan **[01-implementation-plan.md](./01-implementation-plan.md)**, Phase 4
     > checkpoint — now reads *"prints exactly one `ADDED` finding"* and carries a
     > dated **Correction, 2026-08-02 (UTC)** note stating the set direction (table
     > deletion → `ADDED`; workspace `[[bin]]` deletion → `REMOVED`) so it is not
     > "corrected" back; plan **P4-T5** acceptance, corrected identically; this
     > file's **Pass condition** above, mirrored. **DOC-2 §9.6 was NOT amended** —
     > it was right all along and remains authoritative. No code changed: the
     > implementation already followed §9.6 and is unmodified.
     > **The text below is the original finding, unedited.**

     The pass condition requires that deleting
     one row from `expected-binaries.tsv` print *"exactly one `REMOVED` finding"*.
     **DOC-2 §9.6 defines the opposite**, unambiguously:
     ```
     ADDED   := keys in actual  \ declared   -> a new binary nobody declared
     REMOVED := keys in declared \ actual    -> a binary that vanished
     ```
     Deleting a row from the **table** leaves that `(package, binary)` key present
     in `actual` and absent from `declared` — which is **`ADDED` by definition**.
     `REMOVED` is produced by the opposite operation: a row the table declares that
     the workspace does not have.

     **The implementation follows §9.6 and NOT the checkpoint wording, deliberately.**
     Inverting the labels to satisfy the plan's sentence would make the differ
     report *"a binary that vanished"* when a binary had in fact been **added**, and
     the direction is load-bearing: it is what tells an operator which side to fix,
     and P4-T2's own *Contract* line cites §9.6 as its authority. §9.6 wins.

     Every **operative** clause of the pass condition is met by the deletion:
     exit **60**, **exactly one** finding, and it **names the `(package, binary)`
     pair**. Only the class label differs, and it differs because the plan is wrong.
     Clause 2b above additionally demonstrates the genuine `REMOVED` path, so the
     checkpoint's intent is evidenced in both directions.

     **Recommended fix at source** (owner decision; not applied here): amend the
     plan's Phase 4 checkpoint to read *"prints exactly one `ADDED` finding naming
     that `(package, binary)` pair"*, and mirror it in this file's Pass condition.
     Deleting a table row is the cheapest way to exercise the differ and is worth
     keeping as the checkpoint's action — only the expected class is wrong.

  2. **P4-T3 — NO DRIFT.** `--check-table` exits 0 on the unmodified workspace, so
     DOC-2 §9.3's seed is **exactly correct as of 2026-08-02**: 28 rows, 28
     `[[bin]]` targets, no ADDED, no REMOVED, no CHANGED, on all five compared
     columns. §9.3's own derivation was independently reconfirmed (see
     Measurements). **No edit to the table was required and none was made.** DOC-1
     D3's scope was not widened.

  3. **Two integrity checks precede the diff, beyond §9.6's six steps.** Each
     enforces something §9 already states rather than inventing a rule, and each
     dies **60**:
     - every data row has **nine fields** (§9.1) — P4-T1's acceptance made
       permanent instead of one-shot;
     - **no duplicate `(package, binary)` key** (§9.2 makes it the composite
       primary key). Without this a duplicated key lets one row shadow another in
       the diff, so the table could disagree with the workspace and still exit 0 —
       silent, which is the one outcome worse than a false finding.

  4. **`--check-table` does not call `preflight_host_deps`.** That function begins
     with `vm_require_cli`, and §9.6 scopes this command's host dependencies to
     *"only `cargo` and `jq`"* on a host with **no VM**. Requiring the
     virtualisation CLI would make the expectation table uncheckable on any machine
     that cannot run guests — most CI. `cargo`, `jq`, and jq's functional smoke
     test are checked directly instead.

  5. **§F-5 applied unchanged from Phase 2.** `tsv_scope_crate_dirs`,
     `tsv_scope_packages` and `tsv_expect` are defined in the **driver**, beside
     the existing TSV reader, not in `lib/verify.sh`. P4-T4 permits either; the
     driver is where §F-5's narrowest reading already put `conf_get`, `tsv_*`,
     `log` and `die`, and no fifth `lib/tsv.sh` was created, so no new deviation
     arises.

  6. **`EXPECTED_TSV` is deliberately not named `VMTEST_EXPECTED_TSV`.** §12.3
     rule 2 (amended 2026-08-02) reserves every `VMTEST_<KEY>` name for §8.2's
     mechanical override channel. `expected_tsv` is not a config key today, but an
     un-prefixed global cannot collide with one added later — the same reasoning
     that already names `CONF_EFFECTIVE`, `REGISTRY_ROOT` and `PIN_*`.

  7. **§F items:** **§F-3** applied as specified and **not** recorded as a
     deviation (its decision was always right; the tripwire enforces it, and it was
     verified to fire — see Observed result). **§F-4, §F-6, §F-7, §F-10** did not
     arise: Phase 4 touches no probe, no scenario dispatch, no daemon and no
     install ordering. **§F-1, §F-2, §F-8, §F-9** are resolved and unaffected.

  8. **The `binary` column was kept off the install path, as §12.2 requires.**
     Nothing in Phase 4 installs anything, but `tsv_scope_crate_dirs` and
     `tsv_scope_packages` are the only scope helpers delivered — there is
     deliberately **no** helper that emits `(crate_dir, binary)` pairs, because
     that is the shape that invites the prohibited `cargo install --path <dir>
     --bin <binary>`. The `binary` column is reachable only through `tsv_expect`,
     which is the oracle's accessor.
- **Tasks:** P4-T1 … P4-T5 complete

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
   > **2026-08-01 — §F-1 and §F-5 are now DECIDED, in the implementation rather
   > than in the plan.** Phase 2 took the narrowest reading of each and recorded
   > both in Phase 2 Deviations items 1 and 2. They are **not** "resolved at
   > source" the way §F-2 / §F-8 / §F-9 were — the plan's §F text still reads as
   > it did — so do not re-decide them from §F alone; the record of what was
   > chosen, and of why the alternative §F-5 explicitly permits was rejected,
   > is in this file. **Four of the six remain undecided: §F-4, §F-6, §F-7,
   > §F-10.**
3. Read this file's summary table. Start at the first phase that is not `complete`.
   If any phase is `blocked`, resolve that first — the plan does not route around a
   blocked phase.
4. Verify before trusting. This file records what was true when it was written; the
   repo records what is true now. If they disagree, the repo wins, and this file
   gets a dated correction.
