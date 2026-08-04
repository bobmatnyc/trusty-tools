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
| **P5** — Pattern (c) complete: installs, N2, oracle | `complete` | 2026-08-03 | `298a02c7`, `462f6d5c`, `2bf453bc` |
| **P6** — Pattern (b): branch | `complete` | 2026-08-04 | `d9f09253` |
| **P7** — Pattern (a): released | `complete` | 2026-08-04 | `4f81bb37`, `b6017459` |
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
item 1.

**Phase 5 is `blocked`, 2026-08-02 — and it is blocked on a CONTRACT, not on the
harness.** Everything Phase 5 was built to do, it does: `vmtest run local` streams
97 MB of worktree into a clean guest, installs **all eight in-scope crates with
eight package-granular `cargo install --path` commands**, lands **all thirteen
in-scope binaries**, and passes **all four Single-Install Convention gates** —
including the three-sidecar `trusty-memory` case that DOC-1's original seed table
had omitted. Measurement K5 reproduced: `trusty-git-analytics` resolves rustc
**1.97.1** against the workspace's **1.91.1**. **The real full-stack wall clock is
656 s, 722 s and 919 s across three runs (11–15 min), which SUPERSEDES DOC-1 §9's
4–8 minute extrapolation** — the measured value is 1.4×–3.8× that upper bound, and
`install_timeout` is tightened 2700 → 1800 s on it.

> **PHASE 5 IS NOW `complete`, 2026-08-03.** The paragraphs below describe the
> `blocked` state as it stood on 2026-08-02 and are **retained** per this file's
> record-reversals rule. Both blocking contract defects were resolved — DOC-2
> §1.1a scopes the `stack doctor` predicate (and its own two mis-stated causes
> were corrected on 2026-08-03), and §6.2 closes RC-2 as
> *unreachable-by-design*. **Run C re-ran the checkpoint and exited 0 with all six
> clauses satisfied**, in 656 s — the fastest of the three runs *and* the only one
> to complete the entire oracle. **Nothing was weakened to get there.** RC-2 stays
> **OPEN** as a product-side item.

**Two contract defects stopped the checkpoint, and both were found by executing
predicates nobody had run before.**

> **(i) DOC-2 §1.1's `stack doctor` predicate is UNSATISFIABLE for a
> source-installed stack**, for three independent reasons: `stack doctor`
> enumerates `stable_set()` filtered to daemons, so **`trusty-code`,
> `trusty-installer` and `tga` are structurally absent** from its output and can
> never satisfy a predicate quantified over `member(p)`; **`trusty-mpm` is
> deliberately left unprobed** (#4246) and always reports `unknown`, which §1.1
> rejects — and the checkpoint singles `trusty-mpm` out by name; and the four
> launchd daemons are `down` because a source install creates no plists, which
> only `tctl install`'s service bootstrap does — and **DOC-1 §6.5 bans
> `tctl install` from pattern (c)**. §1.1's own judgment call, that `stale` is
> acceptable because "daemons have just been bootstrapped", describes a state
> pattern (c) cannot reach. **The predicate was implemented exactly as written and
> the run exits 60; it was not weakened.** §1.1 needs an owner decision.
>
> **(ii) DOC-2 §6.2's N2 probe cannot reach the behaviour RC-2 describes.**
> `tctl install` with no cargo on PATH exits **3** with **no cargo-related token**:
> the non-interactive consent gate returns before `install_one`, so the guard at
> `install.rs:826` is unreachable — and `--yes` would be worse, reaching a
> prebuilt-first path that could overwrite the source-built binaries under test.
> **RC-2 is NOT pinned and remains open**; `3` is the consent-gate code, not the
> cargo guard's. N2 is recorded **BLOCKED** using §F-7's own remedy, narrowly:
> every other failure shape still dies 30.

**No `crates/*` source was changed** — the harness adapts to the product, never the
reverse. **RC-1 is unchanged**; §F-7 resolved by **step 2** (both `tctl start
--json` and `tctl port <m> --json-port` exist), so its BLOCKED branch was not
taken, though `verify_daemon_liveness` did not execute because §12.4 ends a run at
the first classified failure. Per the state rules, `blocked` **halts the plan**:
Phase 6 does not start around it.

**Phase 6 complete, 2026-08-04 — and it is the first phase that found NO contract
defect.** `vmtest run branch` exited **0 on its first run**, in **650 s**: the
guest cloned `bobmatnyc/trusty-tools@main` at `a28698c8` in **4 s**, installed all
eight in-scope crates with eight package-granular `cargo install --path` commands,
landed all thirteen binaries, passed all four Single-Install gates, and satisfied
the full oracle under pattern `b`. **No host→guest byte stream exists on this
path** and the checkpoint asserts it mechanically: `grep -c 'streamed'` over the
run log is **0**. **The scenario abstraction held** — `install-branch.sh` differs
from `install-local.sh` only in the function name, step 1, and the pattern letter,
and **no `lib/` function needed a pattern-(b) case; `verify.sh` was not touched at
all.** Two results worth carrying forward: **(1)** the plan predicted a ~50 s
transport delta between (b) and (c) and the measured delta is **~0 s** — the
research's `GIT_CLONE_MS=50131` is a **12.5× overstatement** on this host, and the
install phase remains 86 % of the run; **(2)** DOC-2 §1.2's guest-side version read
was exercised against a commit **the host does not have**, but every in-scope crate
version happened to match across the two trees, so this run proves the read is
**correct** without falsifying the host-side alternative **by result**. Both are
recorded in Phase 6 Measurements rather than claimed as more than they cover.

**Phase 7 complete, 2026-08-04 — the last implementation phase, and the D2/D3
reversal is now proved end to end by a run rather than by a `cargo search`.**
`vmtest run released` exits **0** in **511 s** — the **fastest of the six
full-stack runs**, because pattern (a) builds published sources with published
lockfiles and skips a repository entirely. Eight `cargo install <pkg> --locked`
invocations from crates.io — **including `cargo install tga --locked`, `cargo
install trusty-mpm --locked` and `cargo install trusty-review --locked`** — land
**13/13 in-scope binaries**, pass **all four Single-Install gates**, and satisfy
the full oracle under pattern `a`. **`tm` and `trusty-mpm` are PRESENT**, and
`stack doctor` reports `trusty-mpm` `on_path=true`, `version=1.3.4`. Under the
superseded D2 that outcome was impossible in both halves. **All eight in-scope
packages were independently confirmed published on crates.io before the run**, so
no assertion rested on the plan's 2026-07-31 `cargo search` reading — two of which
have since moved (`trusty-mpm` 1.0.2 → **1.3.4**, `trusty-review` 0.10.1 →
**0.11.0**).

**Phase 7 found one contract defect, and only a run could have found it.**
DOC-2 §1.1a asserted that pattern (a) *"inherits the strict form automatically"*
because `tctl install` is permitted under (a) and writes plists. **The premise is
true and the conclusion does not follow:** plan **P7-T2 itself** specifies that
the harness invokes `cargo install` directly under (a) too, *"so that all three
patterns share one install mechanism and differ ONLY in source"*. A permission
nobody exercises writes no plist. The first pattern-(a) run exited **60** with all
four launchd members `down` / `plist=false` — **identical in shape to (b)'s and
(c)'s** — while `verify_binaries` had just resolved 13/13. **The predicate was
implemented exactly as §1.1a wrote it and it failed; it was not weakened to reach
green.** Both of Phase 7's logged assertion candidates were then decided on that
observation, and **the pair is a net strengthening**: the `down` acceptance drops
its pattern gate, *and* `plist_installed == false` becomes a **direct assertion**
under all three patterns, so the previously **inert** guard (§1.1a Consequence 1)
now fails closed by name if `tctl install` ever leaks into a scenario — the false
pass DOC-1 §6.5 bans that step to prevent, and which nothing in the oracle
detected before. See Phase 7 Deviations items 1 and 2.

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
- **CLOSED 2026-08-04 by Phase 7 — assert `plist_installed == false` DIRECTLY.
  IMPLEMENTED, and widened past (b)/(c) to ALL THREE patterns.** The first
  pattern-(a) run showed the same four launchd members `down` with `plist=false`
  as (b) and (c), because plan P7-T2 has the harness install with `cargo install`
  under (a) too — so the invariant is universal here, not source-install-specific.
  The **inverse** (`plist_installed == true` under (a)) does **not** hold and is
  **not** implemented. See Phase 7 Deviations items 1 and 2 for the decision, the
  observed evidence, and the exit-60 run that forced it. Original text:
  *"**NEW, opened 2026-08-03 by the §1.1a cause corrections — assert
  `plist_installed == false` DIRECTLY under patterns (b)/(c). Deferred to Phase 7;
  NOT implemented.**"* Under (b)/(c) it is a **derivable invariant**: DOC-1 §6.5 bans
  `plans_service_bootstrap` (`install.rs:528`), so no bootstrap runs and no plist is
  written. Asserting it **directly** would fail closed if `tctl install` ever leaked
  into a source-install scenario — **the exact false pass §6.5 bans that step to
  prevent, and which nothing in today's oracle detects**. It is a **NEW assertion,
  not a widening of the health predicate**: it does not touch `H_P` and relaxes
  nothing. Its motivation is DOC-2 §1.1a Consequence 1 — as used today the
  `plist_installed == false` guard is **inert** under (b)/(c) (it can never be
  `true`, so the fail-closed branch it promises never fires), and this is the
  productive use of that otherwise-dead signal. Recorded at plan §PHASE 7
  (candidate 2) and DOC-2 §1.1a. **Scope addition — needs an owner decision, per
  the stop rule; it did not ride in on the Phase 5 re-run.**

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

- **State:** `complete`

  > **UPDATED 2026-08-03 — `blocked` → `complete`.** The checkpoint was re-run
  > after DOC-2 §1.1a's scoping and §6.2's RC-2 closure, and **run C exited 0 with
  > all six clauses satisfied**. The two conditions the previous `blocked` value
  > named as needed to unblock were both supplied: the owner decision on DOC-2 §1.1
  > (delivered as §1.1a, whose two mis-stated causes were themselves corrected on
  > 2026-08-03), and RC-2's disposition (closed *unreachable-by-design*, §6.2).
  > **Nothing was weakened to reach the green**: all 13 binaries are still asserted
  > present, all 4 Single-Install gates still run, and `on_path`/`version` are still
  > asserted for every member `doctor` reports. RC-2 itself remains **OPEN** as a
  > product-side item. The `blocked` note below is retained, not deleted.

  > **CORRECTED 2026-08-03 — this field read `not-started` while P5-T1…P5-T9 were
  > complete, two full-stack VM runs had been performed and this section's
  > `Observed result` was several hundred lines long.** The record contradicted
  > itself: the Summary table already carried `blocked` for P5, and the section —
  > which the Schema declares **authoritative** over the table — carried the
  > placeholder. The section was the wrong one. `blocked` is the correct value
  > under the State rules: the phase's tasks are done, the checkpoint was run and
  > NOT met, and the Deviations field names what was needed to unblock (an owner
  > decision on DOC-2 §1.1, and RC-2's disposition).
  >
  > **This is the second instance of this slip** — Phase 3's `State` was found
  > stale and fixed on 2026-08-02. Twice is a pattern, and the cause is structural:
  > the update rule makes the MANIFEST the **final** task of a phase (P5-T9), so
  > the `State` field is written last, at exactly the point where an autonomous
  > session is most likely to end. Phases 1, 2, 4, 6, 7 and 8 were re-checked
  > against their sections on 2026-08-03 and are correct; only P5 was stale.
  > **Recorded rather than silently fixed**, per this file's own rule that a
  > record whose history is rewritten is a record nobody can audit.
- **Pass condition:** `vmtest run local` **exits 0**, and the run log shows:
  Counts below are **derived**, with today's value as the expected literal; if the
  TSV has changed, the derivation is the condition and the literal follows it.
  (i) one `cargo install --path` per value of `tsv_scope_crate_dirs` (**8** today),
  and no directory installed twice, each preceded by a `rustc --version` line
  emitted from inside that crate's directory;
  (ii) `verify_binaries` reporting **N/N in-scope binaries present**, where N is the
  count of `in_scope=yes` rows (**13** today);
  (iii) `tctl stack doctor --json` parsed, with every in-scope package **that
  `doctor` reports as a member** satisfying `on_path == true`, `version != null`,
  and `health ∈ H_c` — where, per **DOC-2 §1.1 as amended 2026-08-03 (§1.1a)**,
  `H_c` is `{healthy, stale}`, plus `unknown` for a member with
  `plist_installed == null`, plus `down` for a member with
  `plist_installed == false`. In-scope packages `doctor` does not report —
  `trusty-code`, `trusty-installer` and `tga`, which are not daemon members —
  carry **no health obligation**; their coverage is clause (ii) and clause (iv);
  (iv) one `verify_single_install` passing per multi-binary in-scope package
  (**4** today): `trusty-search` (2 binaries), `trusty-memory` (**3**),
  `trusty-installer` (2), and `trusty-mpm` (2);
  (v) N2 recorded with its observed exit code and stderr — **and, per DOC-2 §6.2
  as amended 2026-08-03, an N2 recorded `BLOCKED` SATISFIES THIS CLAUSE**;
  (vi) a total wall clock, logged, which is recorded here as the **first full-stack
  measurement**.
- **Observed result:** **PASS CONDITION MET — all six clauses, run C.**

  **Run C, 2026-08-03 UTC, tree `2bf453bc`. `vmtest run local` exited 0**, VM
  `vmtest-20260803T234712Z-18149`, **total wall clock 656 s**. Every clause is
  evidenced below under "Run C". The two runs that preceded it are **retained in
  full**, per this file's record-reversals rule.

  | clause | run A | run B | **run C** |
  |---|---|---|---|
  | (i) 8 installs, none twice, each with in-directory `rustc` | PASS | PASS | **PASS** |
  | (ii) `verify_binaries` 13/13 | PASS | PASS | **PASS** |
  | (iii) `stack doctor` predicate | **FAIL** | **FAIL** | **PASS** |
  | (iv) 4 × `verify_single_install` | PASS | PASS | **PASS** |
  | (v) N2 recorded (BLOCKED satisfies, §6.2) | BLOCKED | BLOCKED | **BLOCKED — satisfies** |
  | (vi) total wall clock logged | PASS | PASS | **PASS** |
  | `vmtest run local` exit code | 60 | 60 | **0** |

  **Run C, clause (iii) — the clause that had never passed.** `verify_stack_doctor`
  applied §1.1a's predicate to the **5** in-scope packages `doctor` reports, and
  named the 3 it does not:
  ```
  vmtest: stack doctor verdict: degraded   [LOGGED, NOT ASSERTED — §1.1]
  vmtest:   trusty-search: health='down' accepted (plist_installed=false; H_c = {healthy,stale,down})
  vmtest:   trusty-memory: health='down' accepted (plist_installed=false; H_c = {healthy,stale,down})
  vmtest:   trusty-analyze: health='down' accepted (plist_installed=false; H_c = {healthy,stale,down})
  vmtest:   trusty-mpm: health='unknown' accepted (plist_installed=null; H_c = {healthy,stale,unknown})
  vmtest:   trusty-review: health='down' accepted (plist_installed=false; H_c = {healthy,stale,down})
  vmtest: in-scope package(s) `stack doctor` does not report as members: trusty-code trusty-installer tga  [NO HEALTH OBLIGATION — DOC-2 §1.1a(a)]
  vmtest: stack doctor reports member(s) the expectation table does not carry: trusty-console  [LOGGED, NOT ASSERTED — plan §F-10(e)]
  vmtest: verify_stack_doctor PASS: all 5 in-scope package(s) reported by doctor satisfy §1.1a's predicate under pattern c (verdict 'degraded' logged but not asserted)
  ```

  **Run C — the two oracle functions runs A and B never reached both PASSED.**
  §12.4's write-once `die` had ended both earlier runs at clause (iii):
  ```
  vmtest: verify_versions PASS: tool_version='0.5.0', stack_version='0.0.0-scaffold' (stub value, field asserted only), contract_floor <= contract_target
  vmtest: verify_daemon_liveness PASS: 4 in-scope daemon(s) live (HTTP 200 + parseable JSON + acceptable .status). LIVENESS ONLY — see RC-1.
  ```

  **Run C — §1.1a(c)'s corrected mechanism, demonstrated end to end.** The same
  four launchd members that `doctor` reported `health=down, plist=false` answered
  HTTP 200 a few steps later, once `verify_daemon_liveness` ran an explicit
  `tctl start --json`. **What moves `health` is the start, not a plist** — which is
  precisely the causal claim §1.1a(c) was corrected to state on 2026-08-03:
  ```
  vmtest:   trusty-search: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-memory: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-mpm: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-review: LIVE — HTTP 200, JSON parses, .status='degraded'
  ```

  ---

  **RUNS A AND B — RETAINED. PASS CONDITION NOT MET.** Clause (iii) failed and was
  **unsatisfiable as written**; clauses (i), (ii), (iv) and (vi) passed; clause (v)
  was recorded and is **BLOCKED**. Two full-stack runs, 2026-08-02 UTC — run A on
  tree `298a02c7`, run B on tree `462f6d5c` (run B adds read-only snapshot
  observations only; no assertion differs). **`vmtest run local` exited 60 on both.**

  **Clause (i) — one `cargo install --path` per `tsv_scope_crate_dirs` value (8),
  none twice, each preceded by a `rustc --version` from inside that directory.
  PASS.**
  ```
  vmtest: install_from_path trusty-search
  vmtest: rustc(/Users/admin/vmtest-src/crates/trusty-search): rustc 1.91.1 (ed61e7d7e 2025-11-07)   [emitted from INSIDE the crate directory; expected='1.91.1']
  vmtest: cargo install --path /Users/admin/vmtest-src/crates/trusty-search (PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-search in 117s: Installed package `trusty-search v0.40.0 (/Users/admin/vmtest-src/crates/trusty-search)` (executables `trusty-embedderd`, `trusty-search`);
  …
  vmtest: install count OK: 8 package-granular installs for 8 in-scope crate directories, none installed twice (trusty-analyze trusty-code trusty-git-analytics trusty-installer trusty-memory trusty-mpm trusty-review trusty-search )
  ```
  **P5-T1's acceptance — K5 REPRODUCED**, on both runs:
  ```
  vmtest: rustc(trusty-git-analytics): crate declares its OWN rust-toolchain.toml — it overrides the workspace pin 1.91.1 (DOC-1 §8.4, measurement K5); asserting resolution, not a literal
  vmtest: rustc(trusty-git-analytics): K5 REPRODUCED — 'rustc 1.97.1 (8bab26f4f 2026-07-14)' differs from the workspace pin 1.91.1
  ```

  **Clause (ii) — `verify_binaries` reporting N/N where N is the count of
  `in_scope=yes` rows (13). PASS.**
  ```
  vmtest:   present  trusty-search/trusty-search -> /Users/admin/.cargo/bin/trusty-search
  vmtest:   present  trusty-search/trusty-embedderd -> /Users/admin/.cargo/bin/trusty-embedderd
  vmtest:   present  trusty-memory/trusty-memory -> /Users/admin/.cargo/bin/trusty-memory
  vmtest:   present  trusty-memory/trusty-bm25-daemon -> /Users/admin/.cargo/bin/trusty-bm25-daemon
  vmtest:   present  trusty-memory/trusty-memory-mcp-bridge -> /Users/admin/.cargo/bin/trusty-memory-mcp-bridge
  vmtest:   present  trusty-analyze/trusty-analyze -> /Users/admin/.cargo/bin/trusty-analyze
  vmtest:   present  trusty-code/tcode -> /Users/admin/.cargo/bin/tcode
  vmtest:   present  trusty-installer/trusty-installer -> /Users/admin/.cargo/bin/trusty-installer
  vmtest:   present  trusty-installer/tctl -> /Users/admin/.cargo/bin/tctl
  vmtest:   present  tga/tga -> /Users/admin/.cargo/bin/tga
  vmtest:   present  trusty-mpm/tm -> /Users/admin/.cargo/bin/tm
  vmtest:   present  trusty-mpm/trusty-mpm -> /Users/admin/.cargo/bin/trusty-mpm
  vmtest:   present  trusty-review/trusty-review -> /Users/admin/.cargo/bin/trusty-review
  vmtest: verify_binaries PASS: 13/13 in-scope binaries present, 0 correctly absent (N is derived from the count of in_scope=yes rows, not hardcoded)
  ```

  **Clause (iv) — one `verify_single_install` per multi-binary in-scope package
  (4). PASS.**
  ```
  vmtest: verify_single_install PASS: trusty-search — all 2 binaries present from ONE package-granular install (trusty-search trusty-embedderd)
  vmtest: verify_single_install PASS: trusty-memory — all 3 binaries present from ONE package-granular install (trusty-memory trusty-bm25-daemon trusty-memory-mcp-bridge)
  vmtest: verify_single_install PASS: trusty-installer — all 2 binaries present from ONE package-granular install (trusty-installer tctl)
  vmtest: verify_single_install PASS: trusty-mpm — all 2 binaries present from ONE package-granular install (tm trusty-mpm)
  ```
  Cargo's own `Installed package … (executables …)` lines above are the direct
  evidence: **one** package-granular command produced **all three**
  `trusty-memory` binaries, which is exactly what DOC-1 §7.4 asks to be proved.

  **Clause (iii) — `stack doctor --json` with all 8 `tsv_scope_packages` values
  satisfying `health ∈ {healthy, stale}`, `on_path == true`, `version != null`.
  FAILED — AND THE CLAUSE IS UNSATISFIABLE. See Deviations item 1.**
  The parsed member table, verbatim:
  ```
  vmtest: stack doctor verdict: degraded   [LOGGED, NOT ASSERTED — §1.1]
  vmtest: stack doctor member table as reported:
      | trusty-search	health=down	on_path=true	plist=false	port=false	version=0.40.0
      | trusty-memory	health=down	on_path=true	plist=false	port=false	version=0.22.0
      | trusty-analyze	health=down	on_path=true	plist=false	port=false	version=0.8.0
      | trusty-review	health=down	on_path=true	plist=false	port=false	version=0.11.0
      | trusty-console	health=not_installed	on_path=false	plist=false	port=false	version=null
      | trusty-mpm	health=unknown	on_path=true	plist=null	port=false	version=1.3.0
  vmtest: stack doctor reports member(s) the expectation table does not carry: trusty-console  [LOGGED, NOT ASSERTED — plan §F-10(e)]
  vmtest: FAIL[60]: verify_stack_doctor FAILED under pattern c — §1.1's per-member predicate does not hold for the following of the 8 in-scope packages:
      trusty-search: health='down', expected one of {healthy, stale} (§1.1 accepts stale, rejects down and unknown)
      trusty-memory: health='down', expected one of {healthy, stale} (§1.1 accepts stale, rejects down and unknown)
      trusty-analyze: health='down', expected one of {healthy, stale} (§1.1 accepts stale, rejects down and unknown)
      trusty-code: expected present, but `stack doctor` REPORTS NO MEMBER BY THAT NAME
      trusty-installer: expected present, but `stack doctor` REPORTS NO MEMBER BY THAT NAME
      tga: expected present, but `stack doctor` REPORTS NO MEMBER BY THAT NAME
      trusty-mpm: health='unknown', expected one of {healthy, stale} (§1.1 accepts stale, rejects down and unknown)
      trusty-review: health='down', expected one of {healthy, stale} (§1.1 accepts stale, rejects down and unknown)
  ```
  **Every one of the eight in-scope packages fails, and not one of them fails
  because installation failed** — `verify_binaries` had just resolved all 13
  binaries and `stack doctor` itself reports `on_path=true` and a real `version`
  for every member it carries.

  **Clause (v) — N2 recorded with its observed exit code and stderr. RECORDED;
  N2 is BLOCKED. See Deviations item 2.**
  ```
  vmtest: N2 step 1: TCTL_PATH=/Users/admin/.cargo/bin/tctl (located under the installed environment)
  vmtest: N2 step 2: probe PATH is /bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin — cargo confirmed ABSENT under it
  vmtest: N2 OBSERVED exit code: 3
  vmtest: N2 OBSERVED stdout (0 bytes):
  vmtest: N2 OBSERVED stderr (204 bytes):
      | info: ✓ git Git-155) found
      | tctl install: refusing to install without confirmation in a non-interactive context; pass --yes to proceed non-interactively, or --dry-run to preview what would be installed.
  vmtest: *** N2 BLOCKED (RC-2 / DOC-2 §6.2) — NOT A PASS. ***
  ```

  **Clause (vi) — a total wall clock, logged. PASS.**
  ```
  run A: vmtest: MEASURE run_wall_clock_s 722 (exit 60; excludes teardown) — DOC-1 §9's replacement measurement
  run B: vmtest: MEASURE run_wall_clock_s 919 (exit 60; excludes teardown) — DOC-1 §9's replacement measurement
  run C: vmtest: MEASURE run_wall_clock_s 656 (exit 0; excludes teardown) — DOC-1 §9's replacement measurement
  ```

  **`verify_versions` and `verify_daemon_liveness` DID NOT EXECUTE IN RUNS A AND
  B.** §12.4's write-once `die` ends the run at the first classified failure, and
  clause (iii) fired before them. Their raw inputs were captured by the diagnostics
  snapshot (Deviations item 5) and are recorded under Measurements; **no verdict is
  claimed for either function on those two runs.** *(Superseded for run C, which
  reached both and passed both — see the run C block above.)*

  **Host cleanliness — before and after, raw.** No `vmtest-*` VM survived any of
  the three runs this phase performed.
  ```
  $ tart list                                    # before
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   48 minutes ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped

  $ tart list                                    # after
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   16 minutes ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped
  ```
  Teardown on every path, including both exit-60 runs and the exit-50 run:
  `vmtest: teardown: deleted vmtest-20260802T190434Z-67389`.

  **Run C — host cleanliness, before and after, raw (2026-08-03).** No `vmtest-*`
  survived either of the two runs performed that day.
  ```
  $ tart list                                    # before
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   1 hour ago     stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped

  $ tart list                                    # after
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   3 minutes ago  stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped
  ```
  `vmtest: teardown: deleted vmtest-20260803T234712Z-18149`.

  **A run whose log was lost is recorded rather than omitted.** An earlier
  2026-08-03 invocation also exited 0 and tore its VM down cleanly, but its
  **stderr was not captured** (the harness logs to stderr and prunes its run
  registry on success), so it produced an exit code and no per-clause evidence.
  It is **not** counted as a measurement and its wall clock is unknown; run C is
  the re-run performed specifically to capture the log. Recorded because an
  unlogged green is not evidence, and omitting it would misstate how many VMs the
  host carried that day.
- **Files delivered:** modify `vmtest-harness/vmtest`; modify
  `vmtest-harness/lib/source.sh`; modify `vmtest-harness/lib/verify.sh`; modify
  `vmtest-harness/scenarios/install-local.sh`; modify
  `vmtest-harness/vmtest.defaults`; modify
  `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`
- **Measurements:**

  **1. THE FIRST FULL-STACK WALL CLOCK — this SUPERSEDES DOC-1 §9's 4–8 minute
  extrapolation.** Three runs, 8 crates, 13 binaries, 8 vCPU / 16 GiB, shared
  `CARGO_TARGET_DIR`, `SKIP_UI_BUILD=1`:

  | | run A (`298a02c7`) | run B (`462f6d5c`) | **run C (`2bf453bc`)** |
  |---|---|---|---|
  | boot → ready | 12 s | 34 s | **17 s** |
  | provisioning | 64 s | 137 s | **17 s** |
  | source stream | 97,126,400 B / 5,345 files in 4 s | same, 4 s | 97,198,080 B / 5,346 files in **4 s** |
  | **install phase (8 crates)** | **588 s** | **614 s** | **562 s** |
  | scenario (install + probes + oracle) | ~640 s | ~748 s | **~610 s** |
  | oracle reached | through clause (iii) | through clause (iii) | **complete** |
  | **TOTAL run wall clock** | **722 s (12 min 02 s)** | **919 s (15 min 19 s)** | **656 s (10 min 56 s)** |

  DOC-1 §9 extrapolated **4–8 minutes** and labelled it low-confidence, computed
  for **six** crates against what is now an **eight**-crate scope. The measured
  totals are **1.4×–3.8× that upper bound.** Per P5-T8 this is not a refutation of
  the estimate — **it replaces it.** Runs A and B reached the same oracle failure,
  so their totals include the full install and the first four verifications but
  **not** `verify_versions` or `verify_daemon_liveness`; **run C completed the
  entire oracle and is still the fastest of the three**, so the earlier totals are
  not short for want of the missing steps — the spread is host variance in the
  install phase, not oracle cost.

  **What the three readings say about the 45-minute watchdog.** Observed range
  **656–919 s**, mean **766 s**, spread **±17 %** about that mean. The watchdog
  (`2700 s`) sits at **2.9× the slowest observed run** and **4.1× the fastest**.
  The dominant term is the install phase (**562–614 s**, i.e. **83–86 %** of each
  total), which is the term most exposed to host load; boot and provisioning
  together never exceed 171 s. For the watchdog to fire, the install phase would
  have to slow by roughly **3.4×** against the slowest run seen so far. **The
  margin is comfortable and is now grounded in three measurements rather than
  two** — but all three are from one host, so this bounds variance *on this
  machine*, not across hosts. P8-T2 should not narrow the watchdog on this
  evidence alone.

  **2. Per-crate install times** (`MEASURE install_s`), TSV row order:

  | crate_dir | run A | run B | **run C** |
  |---|---|---|---|
  | trusty-search | 117 s | 146 s | **91 s** |
  | trusty-memory | 78 s | 92 s | **80 s** |
  | trusty-analyze | 67 s | 66 s | **59 s** |
  | trusty-code | 64 s | 55 s | **49 s** |
  | trusty-installer | 21 s | 22 s | **22 s** |
  | trusty-git-analytics | 62 s | 55 s | **64 s** |
  | trusty-mpm | 121 s | 124 s | **138 s** |
  | trusty-review | 58 s | 54 s | **59 s** |
  | **total** | **588 s** | **614 s** | **562 s** |

  `trusty-search` at 117/146/91 s brackets the research's 103–112 s. The largest
  single-crate install observed across the three runs is **146 s**, so §10.2's
  built-in 900 s single-crate budget is **~6.2×** measured — grounded, and left
  unchanged. Run C's slowest crate is `trusty-mpm` at **138 s**, the only crate
  whose time rose across all three runs; nothing in the budget turns on it.

  **3. RC-2 — the observed `tctl install` cargo-absent exit code.** **The code is
  `3`, and it is NOT the cargo-absent code.** Observed twice, identically:

  - *In-guest, source-built `tctl` 0.5.0*, `PATH=/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin`
    (cargo asserted absent under it first): **exit 3**, **stdout 0 bytes**, stderr
    204 bytes:
    ```
    info: ✓ git Git-155) found
    tctl install: refusing to install without confirmation in a non-interactive context; pass --yes to proceed non-interactively, or --dry-run to preview what would be installed.
    ```
  - *On the host, released `tctl` 0.4.10*, same PATH, `stdin=/dev/null`: **exit 3**,
    stdout empty, byte-identical stderr. (Run before the first guest run, to
    sequence the phase; side-effect-free because the prereq phase only
    auto-installs under `--yes` or TTY consent, and the consent gate refuses
    before any install action.)

  **N2's predicate was NOT tightened**, and P5-T2's branch that applies is the
  second one: `3` is non-zero and distinct from 1, **but it is the consent-gate
  code, not the cargo guard's** — the guard at `install.rs:826` was never reached.
  Recording `3` as RC-2's code would be precisely the false precision DOC-2 §6.2
  refuses. **RC-2 remains OPEN.** `lib/verify.sh` carries DOC-2's weak predicate
  verbatim plus a cited comment block explaining why it stands. **No `crates/*`
  source was changed.**

  **4. RC-1 / §F-7 — daemon start and port discovery.** §F-7 step 1 was performed
  by reading the source; **both machine-readable surfaces EXIST**, so step 2
  applies and **the step-3 BLOCKED-and-skip branch was NOT taken**:
  - **start:** `tctl start [<members>] --json` — `main.rs` → `lifecycle::run_start`;
    `--json` also suppresses the confirmation, so it is non-interactive by
    construction.
  - **port:** `tctl port <member> --json-port` → `{"addr":"host:port","port":N}`
    (`port.rs`, `PortFormat::Json`), read from the member's `http_addr` discovery
    file via `trusty_common::read_daemon_addr`.

    > **CORRECTED 2026-08-03 — this reading is WRONG, and it cost a run.** `addr`
    > is the **HOST ALONE**, not `host:port`. `format_output`'s `PortFormat::Json`
    > arm splits the address on its last colon and serialises only the left side:
    > `serde_json::json!({ "addr": host, "port": port })`, pinned by the crate's
    > own unit test `format_output("127.0.0.1:7879", PortFormat::Json) ==
    > {"addr":"127.0.0.1","port":7879}`. `verify_daemon_liveness` was built on the
    > wrong reading and composed `http://127.0.0.1/health` with **no port**, which
    > cannot reach any daemon — observed as **HTTP 000 for all four members** on
    > the 2026-08-03 run, on which `tctl start --json` had just reported every one
    > of them `installed + bootstrapped`. The oracle now composes the address from
    > **both** fields and treats a response with `.addr` but no `.port` as "not yet
    > recorded" rather than building a portless URL. `port.rs` is correct and
    > unchanged; the defect was §F-7's transcription of it.

  `verify_daemon_liveness` implements §1.3's INTERIM predicate against those two
  commands and carries the RC-1 scoping statement as a header comment, as P5-T7
  requires. **It did not execute** (clause (iii) fired first). The read-only half
  of the port surface WAS observed:
  ```
  vmtest:   tctl port trusty-search --json-port -> tctl port: no address recorded for `trusty-search` (daemon not running?). Start it with `trusty-search start`.
  vmtest:   tctl port trusty-memory --json-port -> tctl port: no address recorded for `trusty-memory` (daemon not running?). Start it with `trusty-memory start`.
  vmtest:   tctl port trusty-mpm --json-port -> tctl port: no address recorded for `trusty-mpm` (daemon not running?). Start it with `trusty-mpm start`.
  vmtest:   tctl port trusty-review --json-port -> tctl port: no address recorded for `trusty-review` (daemon not running?). Start it with `trusty-review start`.
  ```
  Consistent with `stack doctor`'s `port_recorded=false` for every member and
  `plist_installed=false` for all four launchd members. **RC-1's status is
  unchanged** — this phase neither advanced nor retired it.

  **5. §1.2's inputs, observed** (`verify_versions` did not execute; no verdict is
  claimed):
  ```
  vmtest: raw `tctl version --json`:
      | { "contract_floor": 1, "contract_target": 1, "stack_version": "0.0.0-scaffold",
      |   "tool": "trusty-installer", "tool_version": "0.5.0" }
  vmtest: source_tree_version(trusty-installer) via cargo metadata at /Users/admin/vmtest-src: '0.5.0'
  ```
  The shape matches §1.2 exactly, `stack_version` is the documented stub, and
  `tool_version == source_tree_version` — the (b)/(c) cross-check's inputs agree.

  **6. `install_timeout` tightened 2700 → 1800 s** (P5-T8), ~2.4× the slower
  measured scenario (748 s). The multiple now sits over a measurement rather than
  over DOC-1 §9's low-confidence extrapolation.
- **Deviations from plan:**

  1. **CONTRACT DEFECT — DOC-2 §1.1's pass predicate and the plan's checkpoint
     clause (iii) are UNSATISFIABLE for a source-installed stack. This is the
     phase's headline finding.** Three independent reasons, all observed above and
     all confirmed by reading `crates/trusty-installer`:

     a. **`stack doctor` does not enumerate `tsv_scope_packages`.** It iterates
        `stable_set()` **filtered to daemon members** (`commands/stack/doctor.rs`,
        "for each in-scope daemon member"), which is a different set. Three of the
        eight in-scope packages — **`trusty-code`, `trusty-installer` and `tga`** —
        are structurally absent from its output and **can never satisfy a predicate
        quantified over `member(p)`**. §F-10(e) resolved the *opposite* direction (a
        doctor member the TSV does not carry → logged, not asserted, which is why
        `trusty-console` correctly did not fail the run); nothing in the doc set
        addresses this direction.

     b. **`trusty-mpm` can never report `healthy` or `stale`, and the checkpoint
        singles it out.** `probe_member_health` returns `ProbeOutcome::Unprobeable`
        → `unknown` for `ManageStrategy::OwnVerb`, and the source comment is
        explicit that mpm is **deliberately left unprobed** (#4246) even though it
        does answer `/health`. §1.1 rejects `unknown`. Clause (iii)'s emphasis —
        "**including `trusty-mpm`**" — names the one member the product guarantees
        will fail it.

     c. **The four launchd daemons are `down` because a source install creates no
        plists, and creating them is banned.** `plist_installed=false` for all
        four. Plists are bootstrapped by `tctl install`'s service-bootstrap step —
        and **DOC-1 §6.5 bans `tctl install` from pattern (c)**. So DOC-2 §1.1's
        stated judgment call, that `stale` is accepted because "on a freshly
        installed VM ... daemons have just been bootstrapped", **describes a state
        pattern (c) cannot reach**: nothing in a source-based scenario bootstraps a
        daemon.

     **The predicate was NOT weakened to reach a green checkpoint.** It is
     implemented exactly as §1.1 states it, and the run exits 60. A package the
     oracle cannot even locate is reported as its own named failure rather than
     silently skipped. **Nothing under `crates/` was changed.** §1.1 needs an
     owner decision this phase does not have standing to take; the narrowest
     candidates, recorded without choosing between them: scope the predicate to
     the packages `stack doctor` actually reports (and assert binary presence
     alone for the rest, which `verify_binaries` already does); accept `unknown`
     for members the product declines to probe; and either accept `down` under
     source-install patterns or give the scenario a daemon-bootstrap step that
     does not route through the banned `tctl install`.

  2. **CONTRACT DEFECT — DOC-2 §6.2's N2 probe cannot reach the behaviour RC-2
     describes; N2 is recorded BLOCKED.** Observed exit **3** with **no
     cargo-related token** on stderr, so §6.2's weak predicate is not satisfied.
     Two independent structural causes:
     - `decide_install_gate`'s `InstallGate::Refuse` arm returns **3** whenever
       `--yes` is absent and stdin is not a TTY — the guest exec channel is not a
       TTY — and it returns **before `install_one` is ever called**, so the cargo
       guard at `install.rs:826` is unreachable.
     - Adding `--yes` would be **worse, not better**: `install_one` is
       **prebuilt-tarball-first**, and the cargo guard sits in the
       `Outcome::Fallback` arm reached only when the prebuilt download *fails*. On
       a networked guest the download succeeds — and would install **released**
       binaries over the source-built ones the run exists to test, which is exactly
       the false pass DOC-1 §6.5 bans `tctl install` from pattern (c) to prevent.

     **The plan has no branch for this.** P5-T2 anticipated only that the observed
     code might be `1`; it did not anticipate the predicate being unreachable.
     `negative_probe_n2` therefore applies **§F-7's own established remedy** —
     record BLOCKED, log loudly, return 0 — which §F-7 created so a
     required-contract gap "cannot strand the phase". **This is narrow, not a
     weakening:** exit 0, non-empty stdout and empty stderr all still die 30, and a
     stderr that *does* carry a cargo token still takes the normal PASS path. Only
     the one shape proven unreachable is recorded instead of asserted, and it is
     printed as `*** N2 BLOCKED … NOT A PASS ***` on every run.

  3. **P5-T8's tripwire greps `$VMTEST_RUNDIR/run.log`, which does not exist.** The
     harness as merged through Phase 4 writes every diagnostic to **stderr** (§12.1)
     and keeps no run log, so the snippet would grep a missing path. Rather than
     invent a run-log facility for one `grep -c`, `install_from_path` appends each
     crate directory to `$VMTEST_RUNDIR/installs.log` and
     `install_assert_install_count` counts that. **The ledger is strictly stronger
     than the log grep**: it is written by `install_from_path` itself, so it records
     an install issued from *anywhere* — a second install block, a retry, a future
     `install-upgrade.sh` — whereas a scenario counting its own log lines can only
     see the loop it wrote. The canonical `vmtest: install_from_path <dir>` line is
     still emitted for the human and for clause (i).

  4. **P5-T8's tripwire calls `die 60` from the scenario, which §12.4 forbids.**
     §12.4: "scenarios do NOT call `die` with a code of their own … so a scenario
     stays a description of steps and expectations and never encodes the exit-code
     table." The identical logic lives behind the lib function
     `install_assert_install_count`, satisfying both.

  5. **`verify_snapshot_inputs` is not in §12.5's skeleton.** It logs the oracle's
     raw JSON inputs verbatim before any assertion, asserts nothing, and always
     returns 0. §12.4's first-failure unwind is correct for a harness and costly for
     the one phase whose purpose is the oracle's first contact with reality: without
     it, clause (iii)'s failure would have cost the record every observation
     downstream, and a 12-minute build would have to be repeated to read a value
     that was already on screen. It reads only what the assertions read; the one
     side-effecting daemon command (`tctl start --json`) is deliberately **not** in
     it, so it cannot change a verdict.

  6. **The full-stack budget is enforced as a deadline, not a watchdog.**
     `run_watchdog` backgrounds its command (§10.4 — no `timeout(1)` on macOS), so a
     scenario wrapped in one would run in a **subshell**: `die` would exit the
     subshell instead of unwinding the driver, the §12.4 chain would never fire, and
     the write-once `VMTEST_EXIT` would not survive — §2's "first classified failure
     wins" would report the wrong code. The deadline is re-checked before each
     install and combines with the per-crate 900 s watchdog to bound the scenario at
     `install_timeout + 900 s`.

  7. **`verify_rustc`'s `expected` is empty for a crate with its own
     `rust-toolchain.toml`.** `crates/trusty-git-analytics` pins `channel =
     "stable"` — a *channel*, not a version — so no host-side literal can predict
     it. The expectation is the workspace pin from `toolchain.tsv` everywhere else
     (asserted for equality) and empty there, where resolution is asserted and the
     K5 comparison is logged. Inventing `1.97.1` would pin a number the harness
     cannot derive; asserting `1.91.1` would fail the run on the crate's declared
     intent. **K5 reproduced on both runs.**

  8. **§1.3 does not enumerate `trusty-analyze`, which is an in-scope daemon.**
     `stable_set` marks it `daemon: true` and it exposes `/health` behind its
     default `http-server` feature, but §1.3's four-shape table does not carry it,
     so `verify_daemon_liveness` has no described shape for it and does not probe
     it. Logged loudly every run rather than silently decided.

  9. **Two implementation defects found by running, both fixed in `298a02c7`:**
     `install_from_path` joined `guest_src_dir` to `crate_dir` directly, but §9.1
     defines `crate_dir` as a directory **under `crates/`** (as `--check-table`'s
     own derivation and §7.4's worked invocation both show). The failure was clean —
     `cd` failed and the `&&`, never `;`, stopped cargo running in the wrong
     directory, which is precisely why §7.4 requires `&&` there. Separately, the run
     wall clock logged `exit 0` for a run that died 50, because `on_exit` runs
     `local rc=$?` before cleanup samples `$?`; it now reports `VMTEST_EXIT`.

  10. **§F items:** **§F-4, §F-5, §F-6, §F-10(a)/(b)/(e)** applied as previously
      resolved, no new deviation. **§F-7** resolved by **step 2** (both surfaces
      exist); the BLOCKED branch was not taken. **§F-3** applied as specified and
      verified by both tripwires (8 installs, 8 directories, none twice). **§F-1,
      §F-2, §F-8, §F-9** unaffected. The fourth `verify_single_install` call is not
      a deviation (§12.5 was amended at source on 2026-07-31); it is **derived**
      from the table's multi-binary in-scope packages rather than listed.
- **Tasks:** P5-T1 … P5-T9 complete. **The phase checkpoint is NOT MET** — clause
  (iii) is unsatisfiable pending an owner decision on DOC-2 §1.1 (Deviations item
  1), and clause (v) is BLOCKED pending RC-2 (Deviations item 2). Every other
  clause passed with observed output.

## Phase 6 — Pattern (b): branch

- **State:** `complete`

  > **2026-08-04 — `not-started` → `complete` in one pass.** The checkpoint was
  > run once and met on the first attempt. Per the State rules this phase passed
  > through `in-progress` at its first commit (`afae5648`, P6-T1); it is recorded
  > `complete` here because the checkpoint has been **run** and its output is
  > pasted below, which is the gate the rules make it.
- **Pass condition:** `vmtest run branch` **exits 0** with the **same derived binary
  and package assertions as Phase 5** — N/N where N is the count of `in_scope=yes`
  rows (**13** today), over `tsv_scope_packages`' values (**8** today) — and the run
  log shows a
  guest-side `git clone` (no host→guest byte stream) and the checked-out branch
  name.
- **Observed result:** **PASS CONDITION MET — every clause, on the first run.**

  **2026-08-04 UTC, harness tree `d9f09253`. `vmtest run branch` exited 0**, VM
  `vmtest-20260804T004933Z-7781`, **total wall clock 650 s**. Run started
  `00:49:32Z`, ended `01:00:27Z`. The guest cloned
  `https://github.com/bobmatnyc/trusty-tools.git` at branch `main`, commit
  **`a28698c8`**.

  | clause | result |
  |---|---|
  | `vmtest run branch` exits 0 | **PASS** — `EXIT_CODE=0` |
  | same derived binary assertions as Phase 5 — N/N, N = count of `in_scope=yes` rows (**13**) | **PASS** — 13/13 |
  | over `tsv_scope_packages`' values (**8**) | **PASS** — 8 installs / 8 directories; 5 of the 8 packages carry a health obligation, 3 do not (§1.1a(a)) |
  | run log shows a guest-side `git clone` | **PASS** |
  | **no host→guest byte stream** | **PASS** — `grep -c 'streamed'` = **0** |
  | run log shows the checked-out branch name | **PASS** — `checked-out branch: main` |

  **Guest-side clone, and the absence of a host→guest stream.** The clone is the
  only step that differs from pattern (c):
  ```
  vmtest: guest-side clone (NO host->guest byte stream; the host repository is not read at all — DOC-1 §6.2, §11): https://github.com/bobmatnyc/trusty-tools.git branch main
  vmtest: MEASURE git_clone_s 4 (measured baseline GIT_CLONE_MS=50131, i.e. 50.131 s)
  vmtest: checked-out branch: main   [the branch under test; select it with VMTEST_DEFAULT_BRANCH — DOC-2 §8.2's mechanical override mapping, NOT a --branch flag]
  vmtest: resolved commit SHA: a28698c85d09efed4ddd0d27bda68937d5ee2cd7
  vmtest: guest working tree at /Users/admin/vmtest-src: 5540 files (excluding .git)
  vmtest: THE HOST REPOSITORY WAS NOT READ: pattern (b) has no host path argument and no host->guest transfer (DOC-1 §6.2).
  ```
  The checkpoint's "no host→guest byte stream" clause is asserted **mechanically**,
  not by inspection — P6-T1's acceptance is `grep -c 'streamed'` over the run log,
  and it is **0**. The word appears only on `source_deliver_local`'s path, which
  pattern (b) does not enter.

  **Installs are package-granular: 8 installs for 13 binaries, never `--bin`.**
  ```
  vmtest: install_from_path trusty-search
  vmtest: install_from_path trusty-memory
  vmtest: install_from_path trusty-analyze
  vmtest: install_from_path trusty-code
  vmtest: install_from_path trusty-installer
  vmtest: install_from_path trusty-git-analytics
  vmtest: install_from_path trusty-mpm
  vmtest: install_from_path trusty-review
  vmtest: install count OK: 8 package-granular installs for 8 in-scope crate directories, none installed twice (trusty-analyze trusty-code trusty-git-analytics trusty-installer trusty-memory trusty-mpm trusty-review trusty-search )
  ```
  Each was preceded by a `rustc --version` emitted from inside the crate
  directory, and **K5 reproduced again under (b)** — the same toolchain override
  pattern (c) found:
  ```
  vmtest: rustc(trusty-git-analytics): K5 REPRODUCED — 'rustc 1.97.1 (8bab26f4f 2026-07-14)' differs from the workspace pin 1.91.1
  ```

  **13/13 in-scope binaries present.**
  ```
  vmtest: verify_binaries PASS: 13/13 in-scope binaries present, 0 correctly absent (N is derived from the count of in_scope=yes rows, not hardcoded)
  ```
  **All four Single-Install Convention gates pass**, including the three-sidecar
  `trusty-memory` case:
  ```
  vmtest: verify_single_install PASS: trusty-search — all 2 binaries present from ONE package-granular install (trusty-search trusty-embedderd)
  vmtest: verify_single_install PASS: trusty-memory — all 3 binaries present from ONE package-granular install (trusty-memory trusty-bm25-daemon trusty-memory-mcp-bridge)
  vmtest: verify_single_install PASS: trusty-installer — all 2 binaries present from ONE package-granular install (trusty-installer tctl)
  vmtest: verify_single_install PASS: trusty-mpm — all 2 binaries present from ONE package-granular install (tm trusty-mpm)
  ```

  **§1.1a's health predicate under `H_b`, exercised for the first time.** The
  `down`-acceptance is gated on pattern `b|c`; this is the first run to take the
  `b` arm, and it behaved exactly as (c)'s did — nothing bootstraps a daemon under
  (b), so `down` with `plist_installed=false` is the expected state:
  ```
  vmtest:   trusty-search: health='down' accepted (plist_installed=false; H_b = {healthy,stale,down})
  vmtest:   trusty-memory: health='down' accepted (plist_installed=false; H_b = {healthy,stale,down})
  vmtest:   trusty-analyze: health='down' accepted (plist_installed=false; H_b = {healthy,stale,down})
  vmtest:   trusty-mpm: health='unknown' accepted (plist_installed=null; H_b = {healthy,stale,unknown})
  vmtest:   trusty-review: health='down' accepted (plist_installed=false; H_b = {healthy,stale,down})
  vmtest: in-scope package(s) `stack doctor` does not report as members: trusty-code trusty-installer tga  [NO HEALTH OBLIGATION — DOC-2 §1.1a(a)]
  vmtest: verify_stack_doctor PASS: all 5 in-scope package(s) reported by doctor satisfy §1.1a's predicate under pattern b (verdict 'degraded' logged but not asserted)
  ```
  §1.1a(c)'s corrected mechanism reproduced end to end again: the same four
  members reported `down` by `doctor` answered HTTP 200 once
  `verify_daemon_liveness` ran `tctl start --json`.
  ```
  vmtest:   trusty-search: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-memory: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-mpm: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-review: LIVE — HTTP 200, JSON parses, .status='degraded'
  vmtest: verify_daemon_liveness PASS: 4 in-scope daemon(s) live (HTTP 200 + parseable JSON + acceptable .status). LIVENESS ONLY — see RC-1.
  ```

  **§1.2's guest-side version cross-check — the clause pattern (b) exists to
  exercise. See Measurements item 2 for what it proved and what it did not.**
  ```
  vmtest:   source_tree_version(trusty-installer) = 0.5.0 (cargo metadata, in the guest at /Users/admin/vmtest-src)
  vmtest: verify_versions PASS: tool_version='0.5.0', stack_version='0.0.0-scaffold' (stub value, field asserted only), contract_floor <= contract_target
  ```

  **N2 — recorded BLOCKED, identically to Phase 5.** RC-2 is unchanged by this
  phase; the observed shape is byte-identical to (c)'s, which is itself the
  expected result since the probe's subject is the same source-built `tctl`.
  ```
  vmtest: N2 step 1: TCTL_PATH=/Users/admin/.cargo/bin/tctl (located under the installed environment)
  vmtest: N2 step 2: probe PATH is /bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin — cargo confirmed ABSENT under it
  vmtest: N2 OBSERVED exit code: 3
  vmtest: N2 OBSERVED stdout (0 bytes):
  vmtest: N2 OBSERVED stderr (204 bytes):
  vmtest: *** N2 BLOCKED (RC-2 / DOC-2 §6.2) — NOT A PASS. ***
  ```

  **P6-T3's acceptance, run separately with no VM created:**
  ```
  $ VMTEST_DEFAULT_BRANCH=main vmtest run branch --dry-run
  vmtest run branch (dry run)
    repo_url https://github.com/bobmatnyc/trusty-tools.git (default)
    default_branch main (env)
  vmtest: dry run: preflight passed and the run registry entry was acquired; halting before the clone (plan §F-1). NO VM WAS CREATED.

  $ vmtest run branch --dry-run          # control, no override
    default_branch main (default)
  ```

  **Host cleanliness — before and after, raw. No `vmtest-*` survived.**
  ```
  $ tart list                                    # before
  Source Name                                                                                                        Disk Size Accessed    State
  local  tahoe-base                                                                                                  50   33   1 hour ago  stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago stopped

  $ tart list                                    # after
  Source Name                                                                                                        Disk Size Accessed       State
  local  tahoe-base                                                                                                  50   33   10 minutes ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago    stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago    stopped
  ```
  `vmtest: teardown: deleted vmtest-20260804T004933Z-7781`.
- **Files delivered:** modify `vmtest-harness/lib/source.sh`; create
  `vmtest-harness/scenarios/install-branch.sh`; modify
  `docs/research/tart-vm-testing-harness/02-design/02-harness-contracts.md`;
  modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`
- **Measurements:**

  **1. THE FIRST SIDE-BY-SIDE COMPARISON OF THE TWO TRANSPORTS — and the
  transport is not the cost.** P6-T4 expected the (b)−(c) delta to be
  "approximately the difference between the streamed transport and a guest-side
  clone (measured 50.131 s)". **It is not: the delta is ~0 s, because the clone
  took 4 s, not 50.**

  | | pattern (c), run C (2026-08-03) | **pattern (b) (2026-08-04)** |
  |---|---|---|
  | source acquisition | 97,198,080 B / 5,346 files streamed in **4 s** | `git clone` + checkout, 5,540 files in **4 s** |
  | boot → ready | 17 s | **17 s** |
  | provisioning | 17 s | **19 s** |
  | install phase (8 crates) | 562 s | **557 s** |
  | **TOTAL wall clock** | **656 s** | **650 s** |

  Beside the full Phase 5 series: **722 s / 919 s / 656 s (c)** and **650 s (b)**
  — the fastest of the four, by 6 s over run C, which is well inside the ±17 %
  host variance Phase 5 measured. **The two transports are indistinguishable at
  this repository's size**, and the install phase remains the dominant term
  (**86 %** of the total, unchanged from (c)'s 83–86 %).

  **`GIT_CLONE_MS=50131` is superseded for this host: measured 4 s, a 12.5×
  overstatement.** The research figure is retained as the *provenance* of §10.2's
  300 s budget, which is now **75×** the measured value rather than ~6×. The
  budget is **left unchanged** — §10.2's own reasoning applies unaltered ("a tight
  timeout over a low-confidence estimate does not enforce a budget, it
  manufactures flaky failures"), a clone is network-bound, and one reading on one
  host is not grounds to tighten. **Flagged for P8-T2**, which is the task that
  owns grounding the timeouts.

  **2. §1.2's guest-side read under (b) — what it proved, and what it did NOT.**
  This is the clause DOC-2 §1.2 reads guest-side *precisely because* host-side
  reading is "equivalent under pattern (c) by construction, and simply wrong under
  pattern (b)". Phase 6 is its first real exercise, and the honest result is
  **structural, not numerical**:

  - **The trees genuinely differ.** The guest built commit **`a28698c8`**
    (`bobmatnyc/trusty-tools@main`). That commit **does not exist in the host
    repository at all** — `git cat-file -e a28698c8` fails there, and it is not an
    ancestor of the host HEAD. A host-side `cargo metadata` would have been
    reading a different artifact, which is exactly the hazard §1.2 names.
  - **The two versions nonetheless agreed**, so the cross-check passed on equal
    values: `tool_version = 0.5.0` and `source_tree_version(trusty-installer) =
    0.5.0`, the latter read by `cargo metadata --no-deps` **in the guest at
    `/Users/admin/vmtest-src`**.
  - **That agreement is a coincidence of today's trees, and it was checked rather
    than assumed.** All eight in-scope crates carry identical versions on the host
    worktree and on `bobmatnyc/main` as of 2026-08-04 (`trusty-search` 0.40.0,
    `trusty-memory` 0.22.0, `trusty-analyze` 0.8.0, `trusty-code` 0.3.0,
    `trusty-installer` 0.5.0, `tga` 2.11.0, `trusty-mpm` 1.3.4, `trusty-review`
    0.11.0). **No in-scope version differs**, so this run could not have
    distinguished a guest-side read from a host-side one by its result.
  - **Conclusion, stated so it is not over-read:** the guest-side read is
    **correct** and was **exercised against a tree the host does not have**, but
    this run is **not** a falsification test of the host-side alternative. A run
    against a branch whose crate versions differ from the working tree's would be
    the sharper test. **Recorded as an open item, not as a pass claimed for more
    than it covers.** Pattern (a) exercises the complementary case in Phase 7,
    where the published version legitimately differs (`trusty-review` 0.10.1 vs
    0.11.0, §A.1b) and the comparison is skipped entirely.

  **3. Per-crate install times** (`MEASURE install_s`), TSV row order, beside
  pattern (c)'s three runs:

  | crate_dir | (c) run A | (c) run B | (c) run C | **(b)** |
  |---|---|---|---|---|
  | trusty-search | 117 s | 146 s | 91 s | **95 s** |
  | trusty-memory | 78 s | 92 s | 80 s | **85 s** |
  | trusty-analyze | 67 s | 66 s | 59 s | **59 s** |
  | trusty-code | 64 s | 55 s | 49 s | **47 s** |
  | trusty-installer | 21 s | 22 s | 22 s | **21 s** |
  | trusty-git-analytics | 62 s | 55 s | 64 s | **60 s** |
  | trusty-mpm | 121 s | 124 s | 138 s | **132 s** |
  | trusty-review | 58 s | 54 s | 59 s | **58 s** |
  | **total** | **588 s** | **614 s** | **562 s** | **557 s** |

  Every crate lands inside the pattern-(c) spread. **The install step is
  transport-agnostic**, which is the expected result — `install_from_path` is
  shared and does not know which pattern delivered the tree.

  **4. `trusty-mpm` reports 1.3.4, where Phase 5 run C observed 1.3.0.** Both
  readings are correct: the trees are three weeks apart. Recorded because a reader
  comparing the two `stack doctor` member tables would otherwise see it as drift
  between patterns rather than between dates.
- **Deviations from plan:**

  1. **P6-T2's acceptance holds for executable code, and the scenario headers
     differ BY P6-T3'S OWN INSTRUCTION.** P6-T2 asks that the diff between the two
     scenario files show differences "**only** in the function name, step 1, and
     the pattern letter". P6-T3 asks that the branch-selection mechanism be
     documented "in the scenario header comment" and delivers **no other file**.
     The two cannot both hold literally: satisfying P6-T3 puts a block of prose in
     `install-branch.sh` that `install-local.sh` does not carry.

     **Resolved by reading P6-T2's acceptance as a statement about the code**,
     which is what makes it a test of the abstraction. Comments and blank lines
     stripped, the diff is **exactly** the three permitted differences:
     ```
     -scenario_install_local() {
     -    local _bytes _guest_src _dir _pkg
     +scenario_install_branch() {
     +    local _sha _guest_src _dir _pkg
          _guest_src=$(conf_get guest_src_dir)
     -    _bytes=$(source_deliver_local "$VMTEST_VM" "$VMTEST_HOST_REPO" "$_guest_src")
     -    log "streamed ${_bytes} bytes of git-tracked + untracked-unignored source"
     +    _sha=$(source_deliver_branch "$VMTEST_VM" "$(conf_get repo_url)" \
     +                                 "$(conf_get default_branch)" "$_guest_src")
     +    log "guest cloned $(conf_get repo_url) at branch $(conf_get default_branch), commit ${_sha}"
          for _dir in $(tsv_scope_crate_dirs); do
              install_from_path "$VMTEST_VM" "$_guest_src" "$_dir"
          done
          install_assert_install_count
          negative_probe_n2 "$VMTEST_VM"
     -    verify_snapshot_inputs "$VMTEST_VM" c
     -    verify_binaries "$VMTEST_VM" c
     +    verify_snapshot_inputs "$VMTEST_VM" b
     +    verify_binaries "$VMTEST_VM" b
          for _pkg in $(tsv_scope_multibin_packages); do
              verify_single_install "$VMTEST_VM" "$_pkg"
          done
     -    verify_stack_doctor    "$VMTEST_VM" c
     -    verify_versions        "$VMTEST_VM" c
     -    verify_daemon_liveness "$VMTEST_VM" c
     +    verify_stack_doctor    "$VMTEST_VM" b
     +    verify_versions        "$VMTEST_VM" b
     +    verify_daemon_liveness "$VMTEST_VM" b
      }
     ```
     **THE SCENARIO ABSTRACTION DID NOT LEAK.** Steps 2, 2b, 3, 3b and 4 are the
     same calls in the same order; the install step, both tripwires, N2 and all
     six oracle functions were reused with no edit. **No `lib/` function needed a
     pattern-(b) special case, and `verify.sh` was not touched at all.**

  2. **`source_deliver_branch` emits the resolved commit SHA on stdout; DOC-2
     §12.2 specifies only "0 or dies 50".** §12.1 permits a single value on the
     value channel and P6-T1's acceptance requires the SHA to reach the run log,
     so the function emits it and the scenario logs it — the same shape
     `source_deliver_local` uses for its byte count. Nothing asserts on it; it is
     a value the scenario prints.

  3. **`git clone` then `git checkout` as two stages, rather than
     `git clone --branch`.** DOC-1 §6.2 describes both actions and the checkpoint
     requires the run log to show "the checked-out branch name". An explicit
     checkout, followed by reading `git rev-parse --abbrev-ref HEAD` back and
     asserting it equals the requested branch, makes the branch an **observed**
     state of the guest tree rather than an argument the harness passed and never
     confirmed. It also leaves every remote branch fetched, so
     `VMTEST_DEFAULT_BRANCH` can name a branch that is not the remote HEAD with no
     code change.

  4. **No §F item was newly resolved, and none was newly opened.** §F-1, §F-3,
     §F-4, §F-5, §F-6, §F-10(a)/(b)/(e) applied exactly as previously resolved.
     **§F-7 unchanged** — `verify_daemon_liveness` ran and passed under (b) using
     the same two machine-readable surfaces. **RC-1 and RC-2 are both unchanged by
     this phase:** RC-1 was neither advanced nor retired, and RC-2's N2 shape is
     byte-identical to (c)'s, which is expected because the probe's subject is the
     same source-built `tctl`.

  5. **NO CONTRACT DEFECT WAS FOUND.** Every phase from 1 to 5 found at least one
     contract that was wrong when executed; **Phase 6 found none**, and the
     checkpoint passed on its first run with nothing weakened. That is recorded as
     a result rather than passed over, and it is the expected shape of a phase the
     plan describes as reusing existing scaffolding with **no new mechanism**
     (plan §A): the contracts pattern (b) depends on had already been executed and
     corrected by pattern (c). The one plan expectation that did **not** hold is a
     measurement, not a contract — see Measurements item 1, where the predicted
     ~50 s transport delta was measured at ~0 s.

  6. **One open item is carried forward, and it is a limit on the evidence rather
     than a defect:** §1.2's guest-side read was exercised against a tree the host
     does not have, but every in-scope crate version happened to match, so this run
     cannot distinguish the guest-side read from a host-side one **by result**. See
     Measurements item 2.
- **Tasks:** P6-T1 … P6-T5 complete. **The phase checkpoint is MET** — `vmtest run
  branch` exited 0 with all clauses satisfied on the first run, and no assertion
  was weakened to reach it.

## Phase 7 — Pattern (a): released

- **State:** `complete`

  > **2026-08-04 — `not-started` → `complete` in one pass, over TWO runs.** The
  > checkpoint was run twice: the first exited **60** on a contract defect in
  > DOC-2 §1.1a (Deviations item 1), the second exited **0** with every clause
  > satisfied. Per the State rules this phase passed through `in-progress` at its
  > first commit (`4f81bb37`, P7-T1/P7-T2); it is recorded `complete` here because
  > the checkpoint has been **run** and its output is pasted below, which is the
  > gate the rules make it. **The failing run's output is retained in Deviations
  > item 1 rather than replaced** — this file records reversals.
- **Pass condition:** `vmtest run released` **exits 0**, and the run log shows one
  `cargo install <pkg> --locked` invocation per value of `tsv_scope_packages`
  (**8** today) — including **`cargo install tga --locked`**, **`cargo install
  trusty-mpm --locked`** and **`cargo install trusty-review --locked`** — followed
  by `verify_binaries` reporting **N/N present**, where N is the count of
  `in_scope=yes` rows (**13** today), with `tm` and `trusty-mpm` explicitly among
  them, and `tctl stack doctor --json` reporting `trusty-mpm` as installed.
- **Observed result:** **PASS CONDITION MET — every clause.** Met on the **second**
  run; the first exited **60** on a contract defect (Deviations item 1).

  **2026-08-04 UTC, harness tree `b6017459`. `vmtest run released` exited 0**, VM
  `vmtest-20260804T023953Z-50439`, **total wall clock 511 s**. Run started
  `02:39:52Z`, ended `02:48:27Z` (515 s including teardown).

  | clause | result |
  |---|---|
  | `vmtest run released` exits 0 | **PASS** — `EXIT_CODE=0` |
  | one `cargo install <pkg> --locked` per value of `tsv_scope_packages` (**8**) | **PASS** — 8 invocations, set asserted equal to the accessor's |
  | including `cargo install tga --locked` | **PASS** — line 84 of the run log |
  | including `cargo install trusty-mpm --locked` | **PASS** — line 89 |
  | including `cargo install trusty-review --locked` | **PASS** — line 94 |
  | `verify_binaries` reporting N/N, N = count of `in_scope=yes` rows (**13**) | **PASS** — 13/13 |
  | with `tm` explicitly among them | **PASS** — `present trusty-mpm/tm -> /Users/admin/.cargo/bin/tm` |
  | with `trusty-mpm` explicitly among them | **PASS** — `present trusty-mpm/trusty-mpm -> /Users/admin/.cargo/bin/trusty-mpm` |
  | `tctl stack doctor --json` reporting `trusty-mpm` as installed | **PASS** — `on_path=true`, `version=1.3.4`, `health=unknown` (§1.1a(b), #4246) |

  **The eight registry installs — the clause the D2/D3 reversal turns on.** Every
  package name comes from `tsv_scope_packages`; none is spelled out in the
  scenario.
  ```
  vmtest: cargo install trusty-search --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-search in 80s: Installed package `trusty-search v0.39.1` (executables `trusty-embedderd`, `trusty-search`);
  vmtest: cargo install trusty-memory --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-memory in 71s: Installed package `trusty-memory v0.21.2` (executables `trusty-bm25-daemon`, `trusty-memory`, `trusty-memory-mcp-bridge`);
  vmtest: cargo install trusty-analyze --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-analyze in 54s: Installed package `trusty-analyze v0.7.4` (executable `trusty-analyze`);
  vmtest: cargo install trusty-code --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-code in 37s: Installed package `trusty-code v0.2.0` (executable `tcode`);
  vmtest: cargo install trusty-installer --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-installer in 13s: Installed package `trusty-installer v0.4.10` (executables `tctl`, `trusty-installer`);
  vmtest: cargo install tga --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed tga in 32s: Installed package `tga v2.11.0` (executable `tga`);
  vmtest: cargo install trusty-mpm --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-mpm in 106s: Installed package `trusty-mpm v1.3.4` (executables `tm`, `trusty-mpm`);
  vmtest: cargo install trusty-review --locked (from crates.io; PACKAGE granularity — no --bin, no filtered --bins; DOC-2 §12.2)
  vmtest: installed trusty-review in 44s: Installed package `trusty-review v0.11.0` (executable `trusty-review`);
  vmtest: install count OK: 8 package-granular installs, one per package name from `tsv_scope_packages` (8), none installed twice, set matches exactly (tga trusty-analyze trusty-code trusty-installer trusty-memory trusty-mpm trusty-review trusty-search )
  ```
  **`cargo install trusty-mpm --locked` SUCCEEDED and produced BOTH `tm` and
  `trusty-mpm`.** That is the D2 reversal closed by execution: under the superseded
  D2 this command did not exist and `tm` was asserted **known-absent**. **`cargo
  install tga --locked`** closes the D3 discontinuity — the directory is
  `crates/trusty-git-analytics/` and the only name crates.io answers to is `tga`.
  **`cargo install trusty-review --locked`** closes §A.1b's scope widening.

  **13/13 in-scope binaries present, `tm` and `trusty-mpm` among them.**
  ```
  vmtest:   present  trusty-search/trusty-search -> /Users/admin/.cargo/bin/trusty-search
  vmtest:   present  trusty-search/trusty-embedderd -> /Users/admin/.cargo/bin/trusty-embedderd
  vmtest:   present  trusty-memory/trusty-memory -> /Users/admin/.cargo/bin/trusty-memory
  vmtest:   present  trusty-memory/trusty-bm25-daemon -> /Users/admin/.cargo/bin/trusty-bm25-daemon
  vmtest:   present  trusty-memory/trusty-memory-mcp-bridge -> /Users/admin/.cargo/bin/trusty-memory-mcp-bridge
  vmtest:   present  trusty-analyze/trusty-analyze -> /Users/admin/.cargo/bin/trusty-analyze
  vmtest:   present  trusty-code/tcode -> /Users/admin/.cargo/bin/tcode
  vmtest:   present  trusty-installer/trusty-installer -> /Users/admin/.cargo/bin/trusty-installer
  vmtest:   present  trusty-installer/tctl -> /Users/admin/.cargo/bin/tctl
  vmtest:   present  tga/tga -> /Users/admin/.cargo/bin/tga
  vmtest:   present  trusty-mpm/tm -> /Users/admin/.cargo/bin/tm
  vmtest:   present  trusty-mpm/trusty-mpm -> /Users/admin/.cargo/bin/trusty-mpm
  vmtest:   present  trusty-review/trusty-review -> /Users/admin/.cargo/bin/trusty-review
  vmtest: verify_binaries PASS: 13/13 in-scope binaries present, 0 correctly absent (N is derived from the count of in_scope=yes rows, not hardcoded)
  ```
  **All four Single-Install Convention gates pass — against PUBLISHED packages,
  which is a claim patterns (b) and (c) cannot make.** A `[[bin]]` that stopped
  shipping in a crate's published form (behind a feature no longer default, say)
  builds fine from source and fails here.
  ```
  vmtest: verify_single_install PASS: trusty-search — all 2 binaries present from ONE package-granular install (trusty-search trusty-embedderd)
  vmtest: verify_single_install PASS: trusty-memory — all 3 binaries present from ONE package-granular install (trusty-memory trusty-bm25-daemon trusty-memory-mcp-bridge)
  vmtest: verify_single_install PASS: trusty-installer — all 2 binaries present from ONE package-granular install (trusty-installer tctl)
  vmtest: verify_single_install PASS: trusty-mpm — all 2 binaries present from ONE package-granular install (tm trusty-mpm)
  ```
  **`stack doctor` reports `trusty-mpm` as installed** — the checkpoint's last
  clause, and the member table it comes from:
  ```
  vmtest: stack doctor member table as reported:
      | trusty-search	health=down	on_path=true	plist=false	port=true	version=0.39.1
      | trusty-memory	health=down	on_path=true	plist=false	port=false	version=0.21.2
      | trusty-analyze	health=down	on_path=true	plist=false	port=false	version=0.7.4
      | trusty-review	health=down	on_path=true	plist=false	port=false	version=0.11.0
      | trusty-console	health=not_installed	on_path=false	plist=false	port=false	version=null
      | trusty-mpm	health=unknown	on_path=true	plist=null	port=false	version=1.3.4
  vmtest:   trusty-search: health='down' accepted (plist_installed=false; H_a = {healthy,stale,down})
  vmtest:   trusty-memory: health='down' accepted (plist_installed=false; H_a = {healthy,stale,down})
  vmtest:   trusty-analyze: health='down' accepted (plist_installed=false; H_a = {healthy,stale,down})
  vmtest:   trusty-mpm: health='unknown' accepted (plist_installed=null; H_a = {healthy,stale,unknown})
  vmtest:   trusty-review: health='down' accepted (plist_installed=false; H_a = {healthy,stale,down})
  vmtest: verify_stack_doctor PASS: all 5 in-scope package(s) reported by doctor satisfy §1.1a's predicate under pattern a, AND every launchd member among them is plist_installed=false — asserted directly since 2026-08-04, not inferred (verdict 'degraded' logged but not asserted)
  ```
  **§1.2's cross-check is SKIPPED under (a), and this run is the case that shows
  why the skip is correct rather than convenient.** The installed `tctl` is the
  **published 0.4.10**; the host working tree carries **0.5.0**. An equality clause
  would have failed on a difference that is the whole point of pattern (a).
  ```
  vmtest: tctl version --json: {"contract_floor":1,"contract_target":1,"stack_version":"0.0.0-scaffold","tool":"trusty-installer","tool_version":"0.4.10"}
  vmtest:   source-tree cross-check SKIPPED: pattern (a) installs from the registry, where the published version legitimately differs from any working tree (§1.2, §A.1b)
  vmtest: verify_versions PASS: tool_version='0.4.10', stack_version='0.0.0-scaffold' (stub value, field asserted only), contract_floor <= contract_target
  ```
  **All four in-scope daemons live**, from published binaries:
  ```
  vmtest:   trusty-search: address 127.0.0.1:7878 (tctl port trusty-search --json-port)
  vmtest:   trusty-search: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-memory: address 127.0.0.1:7070 (tctl port trusty-memory --json-port)
  vmtest:   trusty-memory: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-mpm: address 127.0.0.1:7880 (tctl port trusty-mpm --json-port)
  vmtest:   trusty-mpm: LIVE — HTTP 200, JSON parses, .status='ok'
  vmtest:   trusty-review: address 127.0.0.1:7891 (tctl port trusty-review --json-port)
  vmtest:   trusty-review: LIVE — HTTP 200, JSON parses, .status='degraded'
  vmtest: verify_daemon_liveness PASS: 4 in-scope daemon(s) live (HTTP 200 + parseable JSON + acceptable .status). LIVENESS ONLY — see RC-1.
  ```
  **N2 — recorded BLOCKED, byte-identical to (b)'s and (c)'s, and that is itself a
  result.** The `tctl` under probe here is the **published 0.4.10**, not a source
  build, so this is the first run in which RC-2's shape is observed against a
  released artefact. It is the same shape.
  ```
  vmtest: N2 step 1: TCTL_PATH=/Users/admin/.cargo/bin/tctl (located under the installed environment)
  vmtest: N2 OBSERVED exit code: 3
  vmtest: N2 OBSERVED stdout (0 bytes):
  vmtest: N2 OBSERVED stderr (204 bytes):
      | info: ✓ git Git-155) found
      | tctl install: refusing to install without confirmation in a non-interactive context; pass --yes to proceed non-interactively, or --dry-run to preview what would be installed.
  vmtest: *** N2 BLOCKED (RC-2 / DOC-2 §6.2) — NOT A PASS. ***
  ```
  **Host isolation — `tart list` before and after, verbatim.** No `vmtest-*` VM
  survived either run; teardown ran on both the exit-60 path and the exit-0 one.
  ```
  === tart list BEFORE ===
  Source Name                                                                                                        Disk Size Accessed    State
  local  tahoe-base                                                                                                  50   33   1 hour ago  stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago stopped

  === tart list AFTER ===
  Source Name                                                                                                        Disk Size Accessed      State
  local  tahoe-base                                                                                                  50   33   8 minutes ago stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base:latest                                                                  50   32   2 weeks ago   stopped
  OCI    ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c 50   32   2 weeks ago   stopped

  vmtest: teardown: deleted vmtest-20260804T022437Z-28772   (run 1, exit 60)
  vmtest: teardown: deleted vmtest-20260804T023953Z-50439   (run 2, exit 0)
  ```
  Baseline held: `~/.tart` **62G**, `tahoe-base` **Disk 50 / Size 33**, unchanged.
  `git diff --stat origin/main..HEAD -- crates/` is **empty** — no product source
  was changed to reach any of this.

- **Files delivered:**
  - modify `vmtest-harness/lib/source.sh` — `source_deliver_released` (no-op, P7-T1),
    `install_from_registry` (P7-T1), `install_assert_install_count` gains an
    optional accessor argument and SET equality
  - create `vmtest-harness/scenarios/install-released.sh` (P7-T2)
  - modify `vmtest-harness/lib/verify.sh` — `verify_stack_doctor`'s §1.1a
    correction (Deviations 1 and 2); `verify_rustc` log-text fix
  - modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md` (P7-T5)
  - **`vmtest-harness/expected-binaries.tsv` NOT modified** — `expect_a` already
    read `present` on all thirteen in-scope rows, so the D2/D3 reversal needed no
    table edit, only a run.

- **Measurements:**

  **1. Publishability of all eight in-scope packages, confirmed BEFORE the run.**
  Queried directly against the crates.io API (`GET
  https://crates.io/api/v1/crates/<pkg>`), read-only, host `~/.cargo` untouched:

  | package | crates.io `max_version` | version the run actually installed |
  |---|---|---|
  | `trusty-search` | 0.39.1 | 0.39.1 |
  | `trusty-memory` | 0.21.2 | 0.21.2 |
  | `trusty-analyze` | 0.7.4 | 0.7.4 |
  | `trusty-code` | 0.2.0 | 0.2.0 |
  | `trusty-installer` | 0.4.10 | 0.4.10 |
  | `tga` | 2.11.0 | 2.11.0 |
  | `trusty-mpm` | **1.3.4** | 1.3.4 |
  | `trusty-review` | **0.11.0** | 0.11.0 |

  **All eight are published. No in-scope package had to be dropped from the
  assertion set.** `trusty-code` and `trusty-installer` — the two the plan singled
  out as worth checking first — are both live.

  **Two of the plan's 2026-07-31 readings have moved, and neither changes a
  decision.** `trusty-mpm` is **1.3.4**, not the 1.0.2 §A.1 records; `trusty-review`
  is **0.11.0**, not the 0.10.1 §A.1b records. Recorded because §A.1b reasons
  explicitly from "published 0.10.1 vs working tree 0.11.0" to justify §1.2's
  pattern-(a) exemption, and **that particular example is now stale — the two are
  equal**. The exemption is unaffected and is still doing work, on a different
  crate: `trusty-installer` published **0.4.10** against the working tree's
  **0.5.0**, which is the pair this run's `verify_versions` actually skipped.

  **2. Total wall clock, beside the four prior readings.**

  | pattern | run | wall clock |
  |---|---|---|
  | (c) local | run A | 722 s |
  | (c) local | run B | 919 s |
  | (c) local | run C | 656 s |
  | (b) branch | first run | 650 s |
  | **(a) released** | **run 2 (exit 0)** | **511 s** |

  **Pattern (a) is the fastest full-stack run of the six, by 139 s over the next
  fastest.** Two contributions, both structural rather than lucky: there is **no
  acquisition step at all** (no 97 MB stream, no clone), and cargo builds each
  published crate against **its own published lockfile** rather than the workspace
  graph. The exit-60 first run measured **521 s** to the same point, so the figure
  is stable across two runs.

  **3. Per-package install times** (`MEASURE install_s`), TSV row order. Note the
  key changes from `crate_dir` to `package` on this path — `tga`, not
  `trusty-git-analytics`.

  | package | (a) run 1 | (a) run 2 | (b) same crate | (c) run C same crate |
  |---|---|---|---|---|
  | trusty-search | 83 s | **80 s** | 95 s | 91 s |
  | trusty-memory | 79 s | **71 s** | 85 s | 80 s |
  | trusty-analyze | 55 s | **54 s** | 59 s | 59 s |
  | trusty-code | 38 s | **37 s** | 47 s | 49 s |
  | trusty-installer | 14 s | **13 s** | 21 s | 22 s |
  | tga | 33 s | **32 s** | 60 s | 64 s |
  | trusty-mpm | 108 s | **106 s** | 132 s | 138 s |
  | trusty-review | 45 s | **44 s** | 58 s | 59 s |
  | **total** | **455 s** | **437 s** | **557 s** | **562 s** |

  **Every package installs faster from the registry than from source — by 22 % in
  aggregate**, and `tga` is the outlier at **~1.9×** (32 s vs 60 s). The measured
  `cargo install tga --locked` baseline in the research is **131 s at 4 vCPU**;
  this run is **32 s at 8 vCPU**. **K5 does NOT reproduce under pattern (a), and
  that is expected rather than drift**: `crates/trusty-git-analytics/rust-toolchain.toml`
  governs builds run *inside that directory*, and a published `tga` is built from
  cargo's own temporary unpack directory. Every registry install resolved
  **rustc 1.91.1**, the workspace pin, asserted per install:
  ```
  vmtest: rustc(/Users/admin): rustc 1.91.1 (ed61e7d7e 2025-11-07)   [emitted from INSIDE /Users/admin, because rustup resolves by directory; expected='1.91.1']
  ```

  **4. Boot and provisioning, run 2:** `MEASURE boot_to_ready_s 17`; provisioning
  wall clock **16 s** (measured baseline `PROVISION_MS=30079`). Both inside the
  existing spread; neither prompts a timeout change.

  **5. What `verify_versions` does under pattern (a).** It asserts **five** things
  and skips exactly **one**. Asserted: `tool_version` non-empty; `stack_version`
  non-empty (the FIELD only — `0.0.0-scaffold` is a known Phase-0 stub, §1.2);
  `contract_floor` an integer; `contract_target` an integer; `contract_floor <=
  contract_target`. Logged but never asserted: `tool`, which is hardcoded
  `trusty-installer` even when the binary is invoked as `tctl`. **Skipped under (a)
  alone:** the `tool_version == source_tree_version(trusty-installer)` equality.
  It does **not** substitute a comparison against crates.io or against the host
  tree — it makes **no** version comparison at all, because under (a) there is no
  source tree in the guest to compare against and the published version
  legitimately differs from any working tree.

- **Deviations from plan:**

  1. **CONTRACT DEFECT, FOUND BY EXECUTION — DOC-2 §1.1a's pattern-(a) strictness
     claim is FALSE, and the first run exited 60 on it.** §1.1a read: *"Under (a)
     `tctl install` is permitted and its service step DOES write plists, so
     `plist_installed == true` and a real `healthy`/`stale` are reachable — cause
     (c) does not apply there, which is why the `down` acceptance below is GATED ON
     PATTERN b|c and (a) inherits the strict form automatically."* The plan carried
     the same claim as Phase 7's logged candidate 1.

     **The premise is true; the conclusion does not follow.** `tctl install` is
     permitted under (a) — and **the harness does not use it**, by plan **P7-T2's
     own instruction**: *"Even though `tctl install` would, in pattern (a) alone,
     do roughly what this pattern specifies, the harness invokes `cargo install`
     directly so that all three patterns share one install mechanism and differ
     only in source."* A permission nobody exercises writes no plist. The `b|c`
     gate was a proxy for *"no service bootstrap ran"*, and under this harness's
     three scenarios that condition is **universal**.

     **Observed, run 1, `vmtest run released`, exit 60**, after eight successful
     `cargo install --locked` invocations and a passing `verify_binaries`:
     ```
     vmtest: FAIL[60]: verify_stack_doctor FAILED under pattern a — §1.1's per-member predicate (as amended 2026-08-03, §1.1a) does not hold for the following of the 5 in-scope packages doctor reports:
         trusty-search: health='down', expected one of {healthy,stale} for plist_installed=false under pattern a (DOC-2 §1.1a)
         trusty-memory: health='down', expected one of {healthy,stale} for plist_installed=false under pattern a (DOC-2 §1.1a)
         trusty-analyze: health='down', expected one of {healthy,stale} for plist_installed=false under pattern a (DOC-2 §1.1a)
         trusty-review: health='down', expected one of {healthy,stale} for plist_installed=false under pattern a (DOC-2 §1.1a)
     ```
     The member table was **identical in shape to (b)'s and (c)'s** — not one
     member reached `healthy` or `stale`, not one plist existed. **The predicate
     was implemented exactly as §1.1a wrote it and it failed. It was not weakened
     to reach green** — see item 2 for what replaced it and why that is a
     strengthening.

     **DECISION on logged candidate 1 (`H_a` and `down`): `H_a` does NOT exclude
     `down`.** The acceptance is now conditioned on `plist_installed == false`
     alone and the pattern gate is removed. The candidate asked for the decision to
     rest on observed evidence rather than assumption; it does — one run, quoted
     above. **`H_a` was not left as-is**, because the branch that instruction
     covers ("if the scenario does not in fact bootstrap, leave `H_a` as-is") is
     unreachable: leaving it as-is means asserting a state the specified scenario
     structurally cannot produce, which is exactly the defect §1.1a was written to
     correct one pattern earlier. This item records that departure rather than
     burying it. **Nothing under `crates/` was changed.**

  2. **SCOPE ADDITION IMPLEMENTED — `plist_installed == false` is now asserted
     DIRECTLY, under ALL THREE patterns (logged candidate 2, opened 2026-08-03).**

     **DECISION: implement it, and widen it past (b)/(c) to (a).** The candidate
     asked two questions and this answers both:
     - *Implement it now for (b)/(c)?* **Yes.** Under (b)/(c) it is a derivable
       invariant — DOC-1 §6.5 bans `plans_service_bootstrap` (`install.rs:528`), so
       no bootstrap runs and no plist is written — but **derivable is not
       asserted**. §1.1a Consequence 1 recorded that the guard was **inert**: it
       could never be `true`, so the fail-closed branch it promised never fired.
       Asserting it directly makes the run **fail closed by name** if `tctl
       install` ever leaks into a source-install scenario, which is the false pass
       §6.5 bans that step to prevent and **which nothing in the previous oracle
       detected**.
     - *Does an inverse hold under (a)?* **No, and it is NOT implemented.** An
       inverse (`plist_installed == true` under (a)) would require the scenario to
       run `tctl install`, which P7-T2 forbids. Asserting it would invent a
       contract for a code path the harness deliberately does not take — precisely
       the error the strict `H_a` made. What **does** hold under (a) is the *same*
       invariant as under (b)/(c), and run 2 observed it on all four launchd
       members.

     **This is why item 1 is a net strengthening rather than a relaxation, and the
     two are not separable.** Before: the oracle asserted **nothing** about plists.
     After: every in-scope launchd member is asserted `plist_installed == false`
     every run, and the `down` acceptance is **derived from an invariant the run
     asserts** rather than inferred from a ban stated in a document. It is a **new
     assertion, not a widening of `H_P`** — it lives outside the health predicate,
     relaxes nothing, and is evaluated independently of the health value. `null`
     (a non-launchd member, §1.1's field table) carries no obligation.

     Observed, run 2:
     ```
     vmtest: verify_stack_doctor PASS: all 5 in-scope package(s) reported by doctor satisfy §1.1a's predicate under pattern a, AND every launchd member among them is plist_installed=false — asserted directly since 2026-08-04, not inferred (verdict 'degraded' logged but not asserted)
     ```

  3. **P7-T3 was ALREADY DONE before Phase 7 began — no code was written for it.**
     The task asks to gate §1.2's equality clause on `pattern ∈ {b, c}`.
     `verify_versions` has carried that gate, plus an explicit `a` arm that logs
     the skip, **since Phase 5** (`lib/verify.sh`, the `case "$pattern" in b|c) …
     a) …` block). Phase 7 verified it rather than re-implementing it, and run 2
     exercised it for the first time with a real difference to skip over —
     published `trusty-installer` **0.4.10** against the working tree's **0.5.0**.
     P7-T3's acceptance is therefore met by observation, and its stated example is
     recorded as stale in Measurements item 1.

  4. **`install_assert_install_count` gained an optional accessor argument and a
     SET-equality assertion — a departure from P5-T8's shape, made because pattern
     (a) needs a different key.** (b)/(c) install by **directory**
     (`tsv_scope_crate_dirs`); (a) installs by **package name**
     (`tsv_scope_packages`), because that is what `cargo install` takes (DOC-2
     §9.2). **Both accessors emit eight values today**, so the pre-existing
     count-only check would have passed pattern (a) even if the loop had been
     driven off the wrong accessor and tried `cargo install trusty-git-analytics`
     — the exact discontinuity DOC-1 D3 warns about, and one that fails on the
     **last** package after seven multi-minute installs. The assertion is now on the
     **set**, which is what P7-T1's acceptance requires ("no more, no fewer, none
     repeated", asserted against the helper's output rather than a literal list).
     (b)/(c) keep the default accessor and are unchanged in behaviour.

  5. **A cosmetic log defect corrected.** `verify_rustc` logged
     `[emitted from INSIDE the crate directory]` unconditionally. On pattern (a)'s
     path the directory is the **guest home** — cargo builds a published crate in
     its own temporary unpack directory, so there is no crate directory in the
     guest to be inside. The line now names the directory it actually ran in.

  6. **§F items:** none newly resolved and none re-opened by this phase. §F-1
     (Phase 2), §F-3 (closed at source), §F-4 (Phase 3), §F-5 (Phase 2), §F-6
     (dispatch — `install-released.sh` / `scenario_install_released()` follows the
     recorded mapping with no new deviation), §F-7 (Phase 5, resolved by step 2),
     §F-10(b) (install order = TSV row order, followed) and §F-10(e) (`trusty-console`
     logged, not asserted — observed again here) all stand as recorded.

  7. **`--locked` held on every one of the eight installs. The E0063 hazard did
     not reproduce, and that is the expected result rather than a lucky one** — the
     incident it comes from (`cargo install trusty-analyze` pairing old published
     source with a newer `trusty-common`) is caused by cargo *re-resolving* and
     ignoring the published lockfile, which is precisely what `--locked` prevents.
     No install failed for any reason. **No workaround was applied and none was
     needed.**

- **Tasks:** P7-T1 … P7-T5 complete.

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
