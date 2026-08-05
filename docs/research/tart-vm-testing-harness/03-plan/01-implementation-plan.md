# DOC-3 — `vmtest-harness/` Implementation Plan

**Status:** Plan — sequencing only, **no implementation**
**Implements:** [DOC-1](../02-design/01-vm-install-harness.md) and [DOC-2](../02-design/02-harness-contracts.md), in that order of authority.
**Creates:** nothing. `vmtest-harness/` does not exist and is **not** created by this PR. This document describes how a future engineer creates it.
**Progress record:** [MANIFEST.md](./MANIFEST.md) — the only durable state between execution sessions.

## Purpose

DOC-1 settles **what** the harness does and **why**. DOC-2 settles **every
interface**. Neither settles **order**, and order is the whole risk: the harness's
headline transport — host `git ls-files` → `tar` → `tart exec -i` → guest unpack →
build — has **never been measured end-to-end** (DOC-1 §14, DOC-2 open items,
devil's-advocate critique #9). A plan that builds `lib/` first and reaches that
transport in week two commits the entire module layout to a mechanism nobody has
seen work.

This document sequences the build so the unverified thing is verified **first**,
against the real payload, before anything is designed around it. It targets an
engineer with **zero codebase context**, executing **autonomously**, in sessions
that may be interrupted at any point.

It does **not** re-open a settled decision. Where DOC-1 or DOC-2 states a rule,
this plan cites it and moves on. Where DOC-2 leaves something genuinely
under-specified, this plan **flags it in §F** rather than inventing a contract —
that is the established register of this doc set, and a plan that quietly filled
the gaps would be the least honest document in the set.

---

## How to execute this plan

1. **Read DOC-1 and DOC-2 in full before starting.** They are ~2,700 lines
   combined. There is no shortcut; every task in this plan cites a DOC-2 section
   number, and a task executed without reading its contract will be wrong in a way
   the acceptance check may not catch.
2. **Read [MANIFEST.md](./MANIFEST.md) next.** It states which phases are done,
   what was observed, and what deviated. If it disagrees with this plan, the
   MANIFEST wins for *history*; this plan wins for *what to do next*.
3. **Execute phases in order.** A phase may not begin until the previous phase's
   checkpoint has been observed to pass and the MANIFEST records the observation.
   Phase 1 is the one exception to nothing: it is the risk retirement, and if it
   fails, §P1-T10 says exactly what happens instead.
4. **Within a phase, execute tasks in ID order** unless a task's `Depends` line
   says otherwise.
5. **Commit after every task**, conventional commits, scope `vmtest-harness`.
   Example: `feat(vmtest-harness): add lib/vm.sh tart boundary (P2-T4)`.
6. **The last numbered task of every phase updates the MANIFEST.** It is a task,
   not a habit. It has an acceptance check like any other.

### Scope guardrails — things that are *not* in this plan, by decision

Do not build them, do not "while I'm here" them. Each is an explicit DOC-1 §13
non-goal: golden image or bake script; Cirrus CLI or `.cirrus.yml`; any GitHub
Actions workflow; cargo registry pre-warming; `--dir` mounts in either direction;
a Rust crate or any `cargo test` integration; `install.sh` or prebuilt-tarball
coverage; upgrade scenarios (DOC-1 §12.1 — designed *for*, not built); Linux
support (DOC-1 §12.2 — same).

Two invariants are worth restating because violating either silently destroys the
harness's claim:

- **`lib/vm.sh` is the only file that may contain the string `tart`** (DOC-1 §3.2,
  DOC-2 §12.2). This is mechanically checkable and P2-T4 checks it.
- **The host repo is never mounted, in either direction** (DOC-1 §6.4). Source is
  always copied to guest-local disk. This single rule is what closes the isolation
  hole (DOC-1 §11).

### The stop rule

If a task requires you to *decide* something this plan and DOC-2 do not settle:
**stop, record it in the MANIFEST's Deviations field for the current phase, and
flag it.** Do not invent a contract to keep moving. Every under-specification
already known at planning time is listed in §F with a decision rule attached; a
gap not in §F is a new finding and is worth more than the hour it costs.

---

## A. Ordering and its rationale

**Implementation order is (c) local source → (b) branch → (a) released.** This is
DOC-1 **D4**, as corrected on 2026-07-31, and the corrected justification is the
opposite of the one D4 originally gave.

**(c) is first because its transport is the unverified one.** What the research
measured is a *generic channel property* — 200,000 lines through `tart exec`
untruncated, exit codes propagating exactly through `-i`
([`vm-install-probe-findings.md:179`](../01-research/vm-install-probe-findings.md),
DOC-1 §5.1). What it did **not** measure is the sequence pattern (c) needs. The
112s `trusty-search` build of measurement K3 reached its source tree by a
**guest-side `git clone`** (`GIT_CLONE_MS=50131`, `:942`) — that is **pattern
(b)'s** transport, not (c)'s. The tar pipeline has no end-to-end run anywhere in
the record.

> **Recorded product-owner decision, 2026-07-31 (DOC-1 D4).** *Building pattern (c)
> IS the measurement.* The alternative — write a standalone tar-transport probe,
> measure it, then build (c) — was considered and rejected: the probe would be most
> of `lib/source.sh` with none of its value, and a second artifact to keep honest.

That decision is what shapes **Phase 1**. Phase 1 is a **thin vertical slice**: a
single disposable script that boots a guest, streams the tracked worktree, unpacks
it, and builds **one** crate. It exists to make the transport fail **before**
`lib/` is built around it, not after. Nothing in Phase 1 survives except two
measurements, one pinned digest, and the knowledge that the pipeline works.

**(b) reuses (c)'s scaffolding with no new mechanism** — guest-side `git clone`
(the repo is public, DOC-1 §6.2) plus the same `cargo install --path` install step
(DOC-2 §12.2 `source_deliver_branch` + `install_from_path`). Its transport is the one
that *was* measured, which is also why DOC-1 D4 names it the fallback if (c)'s
transport does not work.

**(a) adds only a pattern-aware oracle path** — `cargo install <package> --locked`
from crates.io (DOC-1 §6.3), no delivery step at all
(`source_deliver_released` is a no-op that exists so scenarios stay symmetric,
DOC-2 §12.2). No new infrastructure.

### A.1 The D2/D3 reversal — carry this correctly or the plan is wrong

**`trusty-mpm` is published at v1.0.2.** `crates/trusty-mpm/Cargo.toml` has **no
`publish` key**, so cargo defaults to `publish = true`, and `cargo search
trusty-mpm --limit 5` returned `trusty-mpm = "1.0.2"` on 2026-07-31 (DOC-1 D2 as
amended; DOC-2 §9.5).

Consequences this plan carries end-to-end:

- Pattern (a) covers **all eight crates** (DOC-1 D3), not six and not seven.
- `tm` and `trusty-mpm` are asserted **PRESENT** under (a), (b) and (c) alike
  (DOC-1 §7.5 as amended). `expect_a = present` on both rows (DOC-2 §9.3).
- **A pattern-(a) run that does not find `tm` is a FAILURE**, where under the
  superseded D2 it was the expected result.
- All **thirteen** in-scope binaries are expected present under all three patterns.
  Eight crates produce thirteen binaries (DOC-2 §9.3) — that is not a typo, and
  §7.4's Single-Install gate is why the count matters.
- The `expect_*` columns and the pattern-aware oracle **stay** (DOC-2 §9.5). No
  in-scope row diverges across patterns *today*; the columns are the recording
  mechanism for the next divergence, and collapsing them would mean re-inventing
  them.

Any text you encounter — in this repo or in a stale doc — implying a six-crate
pattern-(a) scope, or a known-absent `tm`, is **wrong**. One such text is known and
is fixed in P8-T5.

### A.1b The D3 scope widening — `trusty-review` is IN

**Owner decision, 2026-07-31, recorded as a dated amendment to DOC-1 D3.** This is
**separate from and later than** the D2 reversal above. D2 corrected a false
premise; this widens a scope that was never wrong, only narrower than the owner
wanted. Do not merge the two in your head — a doc that says "seven" may be
faithfully recording the state between the two amendments.

- **DOC-1 D3 is now eight crates.** `trusty-review` is row 8. DOC-2 §9.3 note 2 had
  carried it `in_scope=no` and asked that the question be *"decided knowingly rather
  than by omission"*; it has been.
- **It is published.** `crates/trusty-review/Cargo.toml` declares `publish = true`
  **explicitly**, and `cargo search trusty-review --limit 5` returned
  `trusty-review = "0.10.1"` on 2026-07-31 and **`0.11.0` on 2026-08-04**. Pattern
  (a) installs it with `cargo install trusty-review --locked`.
- ~~**The published version is 0.10.1; the working tree is 0.11.0.**~~ **STALE —
  rewritten 2026-08-04 at plan P8-T2/P8-T5. The example no longer illustrated
  anything, because the pair converged.** The original text read: *"The published
  version is 0.10.1; the working tree is 0.11.0. Pattern (a) will therefore install
  a different version from (b) and (c)."* As of 2026-08-04 `trusty-review` is
  **0.11.0 in both places**, so it demonstrates the exemption by *not* exercising
  it. (`trusty-mpm` moved too, 1.0.2 → **1.3.4**, and is likewise equal on both
  sides now.)

  **The exemption still earns its place, and here is the pair that earns it —
  the one Phase 7's run actually skipped:**

  | Crate | Published (crates.io) | Working tree | Pattern (a) behaviour |
  |---|---|---|---|
  | **`trusty-installer`** | **0.4.10** | **0.5.0** | `tool_version` **0.4.10** ≠ `source_tree_version` **0.5.0** → **the equality is SKIPPED** |

  DOC-2 §1.2's `tool_version == source_tree_version(trusty-installer)` cross-check
  applies only to patterns (b) and (c) — under (a) the binary comes from crates.io
  and the tree it would be compared against is not the tree it was built from. That
  is exactly the situation above, and Phase 7 observed it: a released `tctl` at
  0.4.10 against a working tree at 0.5.0. Under (b)/(c) the same comparison is
  meaningful and is asserted. **This is expected, not drift.**

  **Read the table as illustrative, not as a fixture.** It is a snapshot of
  2026-08-04 and the whole point of this rewrite is that such snapshots rot: the
  previous example was written on 2026-07-31 and was stale within four days.
  Nothing in the harness reads these numbers — `verify_versions` computes both
  sides at run time and skips the comparison **by pattern**, never by version. If
  `trusty-installer` is published at 0.5.0 tomorrow, the example stops
  illustrating and the contract does not change.
- **It is single-binary** — one `[[bin]]`, `name = "trusty-review"`,
  `path = "src/main.rs"`, `required-features = []`. So it adds **one** in-scope TSV
  row (twelve → thirteen), **one** `crate_dir` (seven → eight), and **no**
  `verify_single_install` call: a single-binary crate has no Single-Install
  Convention to gate. The multi-binary in-scope packages remain **four**.
- **Its `/health` comes into the oracle's scope**, under the INTERIM liveness
  predicate only. DOC-2 §1.3 records why liveness-only is still sufficient: the
  known MCP-vs-HTTP drift in `review_health` is **off the assertion path** (the
  oracle reads the HTTP handler, never the MCP tool), and the predicate touches only
  `.status`, which all four daemon shapes carry. **RC-1's status is unchanged** — it
  becomes neither more nor less urgent.

### A.2 Phase map

| Phase | Delivers | VM? | Retires |
|---|---|---|---|
| **P1** | Disposable transport spike; real base-image digest | yes | **The transport risk.** DOC-1 §14's headline gap |
| **P2** | Driver, config, exit codes, run registry, `lib/vm.sh`, preflight, `clean` | no | Host-side contract risk |
| **P3** | N1 probe, provisioning, toolchain hand-off, `lib/source.sh` (local), delivery-only scenario | yes | Guest bring-up risk |
| **P4** | `expected-binaries.tsv`, `--check-table` | no | Expectation-table drift |
| **P5** | Pattern (c) installs + the full oracle; RC-2 **closed as unreachable-by-design**, not pinned (2026-08-03, DOC-2 §6.2) | yes | Oracle risk; **first full-stack timing** |
| **P6** | Pattern (b) | yes | — |
| **P7** | Pattern (a) | yes | — |
| **P8** | Hardening, docs, measurement write-back | mixed | Doc drift |

---

## B. Global task format

Every task states, without exception:

- **Files** — create/modify, full repo-relative paths.
- **Contract** — the DOC-2 section (and DOC-1 section where relevant) that defines
  it. Every task traces to one.
- **Do** — what to build.
- **Acceptance** — a command and its observable result. Not "it works".
- **Depends** — task IDs required first.

Task IDs are stable. `P3-T4` means phase 3, task 4, forever, and the MANIFEST
references them by that ID.

---

## PHASE 1 — Transport spike (thin vertical slice)

**Goal:** stream the tracked worktree of this repo into a clean Tart guest and
build **one** crate from the unpacked tree, end-to-end, in one disposable script.

**Why this shape.** DOC-1 D4 accepts an unverified transport as the first
implementation target. This phase honours that while limiting the blast radius: if
the pipeline does not work, it fails here, in a 200-line script, before `lib/`
exists to be rewritten. Nothing built in Phase 1 is production code; P3-T4 promotes
the one function that survives and deletes the rest.

**Checkpoint — PASS CONDITION.**

> `bash vmtest-harness/spike/spike-transport.sh` **exits 0** and its final three
> log lines report: (i) a streamed byte count greater than 80,000,000; (ii) the
> guest's `trusty-search --version` output on stdout; (iii) `tart list` containing
> **no** `vmtest-spike-*` entry after teardown.

### P1-T1 — Verify the host dependency set

- **Files:** none (record in MANIFEST).
- **Contract:** DOC-2 §JSON parsing dependency ("the complete host dependency set
  is `tart`, `git`, `jq`, `cargo`, and bash ≥ 3.2"); DOC-1 §4.1.
- **Do:** confirm each tool is present and record its version. Run DOC-2's exact
  functional `jq` smoke test — a `jq` on `PATH` that is a broken symlink or a
  differently-named tool passes `command -v` and fails several minutes into a run.
- **Acceptance:**
  ```sh
  tart --version && git --version && jq --version && cargo --version
  printf '{"a":1}' | jq -e '.a == 1' >/dev/null && echo JQ_OK
  echo "${BASH_VERSINFO[0]}"        # >= 3
  ```
  All exit 0; `JQ_OK` printed; bash major ≥ 3.
- **Depends:** —

### P1-T2 — Spike scaffold: clone, size, boot, poll ready

- **Files:** create `vmtest-harness/spike/spike-transport.sh`.
- **Contract:** DOC-1 §4.3 (full sequence), §8.5 (`--cpu 8 --memory 16384`);
  DOC-2 §10.1 boot-ready poll (**2 s interval, 150 s maximum**), §10.4 (**there is
  no `timeout(1)` on macOS** — background, record PID, poll `kill -0`).
- **Do:** `tart clone` the base image to `vmtest-spike-<utc>-<pid>`, `tart set
  --cpu 8 --memory 16384`, background `tart run --no-graphics`, then poll
  `tart exec <vm> /bin/sh -c 'exit 0'` every 2 s until it returns 0 or 150 s
  elapse. **Poll for the observable condition; never sleep a fixed interval**
  (DOC-1 §4.3). Log elapsed seconds to ready.
- **Acceptance:** script prints `READY after <n>s` with `n` between 10 and 150
  (measured baseline: 34.4 s first boot, 18.0 s subsequent — DOC-2 §10.1), and
  `tart list` shows the VM `running`.
- **Depends:** P1-T1

### P1-T3 — Capture the real base-image digest and write `base-image.pin`

**This task is a hard prerequisite for every later phase that depends on a pinned
base — P2-T5 (preflight), and transitively P3, P5, P6, P7.** It is placed here
because capturing the true digest **requires a live Tart run**, and this is the
first phase that boots a VM.

- **Files:** create `vmtest-harness/base-image.pin`.
- **Contract:** DOC-2 §3.1 (a checked-in file, not a shell constant), §3.2
  (`key<TAB>value`, unknown keys are a preflight error), §3.3 (comparison), §3.4
  (roll procedure — **not** performed here).
- **Do:**
  1. Determine whether `tart` exposes an **untruncated 64-hex digest**. Try, in
     order: `tart list --format json`, `tart list --quiet`, `tart list`. The
     research recorded the digest only **truncated** — `sha256:a8e1...`
     (`vm-install-probe-findings.md:652`, `:685`) — and the full value **was never
     recorded anywhere**. DOC-2 §3.3 flags the introspection invocation as a
     genuine unknown.
  2. **If a full digest is obtainable:** write the pin file in DOC-2 §3.2's exact
     format with the real `digest`, today's `pinned_on`, your handle as
     `pinned_by`, and a `note` naming the `tart` version.
  3. **If it is not obtainable:** do **not** guess and do **not** ship the
     placeholder as if it were a pin. Adopt DOC-2 §3.3's **by-construction
     variant** — clone the pinned OCI reference directly, so the pin is enforced by
     construction with no comparison to get wrong — record `digest` as the value
     you *can* obtain, add `enforcement<TAB>by-construction` to the file, and
     record the finding verbatim in the MANIFEST. Both branches are specified;
     neither requires you to invent anything.
- **Acceptance:**
  ```sh
  grep -Eq '^digest'$'\t''sha256:[0-9a-f]{64}$' vmtest-harness/base-image.pin && echo PIN_REAL
  ```
  prints `PIN_REAL`; **or** the file carries `enforcement<TAB>by-construction` and
  the MANIFEST records why, with the verbatim `tart list` output that proves the
  digest is not retrievable.
- **Depends:** P1-T2

### P1-T4 — N1 precondition probe, inside the spike

- **Files:** modify `vmtest-harness/spike/spike-transport.sh`.
- **Contract:** DOC-2 §6.2 **N1**; DOC-1 §4.2 (position: boot-before-provision).
- **Do:** for each of `cargo`, `rustc`, `rustup`, run `command -v <tool>` under the
  **measured base PATH** literal
  `/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin`
  (`vm-install-probe-findings.md:213`). Assert **non-zero exit and empty stdout**
  for all three. Log which exit code each produced. Assert non-zero rather than a
  specific code: 127 was measured for *invoking* `cargo`, not for `command -v
  cargo`, and pinning a code measured for a different command is exactly the false
  precision this doc set avoids (DOC-2 §6.2).
- **Acceptance:** script prints `N1 PASS` and, for each tool, the observed
  non-zero exit code. A base image that already has cargo fails here — which is a
  **finding** (image drift), not a nuisance.
- **Depends:** P1-T2

### P1-T5 — Provision the spike guest

- **Files:** modify `vmtest-harness/spike/spike-transport.sh`.
- **Contract:** DOC-2 §11.2 (per-tool strategy table), §11.1 (what is actually
  preinstalled), §11.3 (fail, do not repair).
- **Do:** **`mise` and `gh` are PREINSTALLED and must be reused, never installed**
  — DOC-1 §3.3's phrasing is wrong on both and DOC-2 §11.5 amends it. Detect mise
  under `/opt/homebrew/`, assert **no second mise at `$HOME/.local/bin/mise`**, and
  assert `mise --version` returns 0. Then `mise use -g rust@1.91` and
  `mise use -g uv@latest`. **Never run `curl https://mise.run | sh`** (creates a
  second, conflicting mise) and **never `mise self-update`** (hard-fails on a
  Homebrew-managed mise).
- **Acceptance:** the guest reports `rustc 1.91.1` when queried from
  `/Users/admin` under the full guest PATH; total provisioning wall clock is
  logged and is within 3× the measured 30.079 s (`PROVISION_MS=30079`).
- **Depends:** P1-T4

### P1-T6 — **THE SLICE**: stream the worktree and unpack it

This is the task the whole phase exists for.

- **Files:** modify `vmtest-harness/spike/spike-transport.sh`.
- **Contract:** DOC-1 §6.1 (file set, payload, "log the actual streamed byte
  count"); DOC-2 §12.2 `source_deliver_local`; DOC-2 §Shell discipline
  (`pipefail` — without it, a `tar` that fails mid-stream is invisible if
  `tart exec` exits 0, giving a silently truncated tree that then fails to build
  for an unrelated-looking reason).
- **Do:** on the host, enumerate with `git ls-files -co --exclude-standard`, pipe
  through `tar`, pipe through `tart exec -i` to a guest-side unpack into
  `/Users/admin/vmtest-src`. Count the bytes crossing the pipe and log the count.
  The file set is right for two reasons: it **includes uncommitted work** (the
  entire point of pattern (c)) and it **excludes `target/` by construction**,
  because `target/` is gitignored and `--exclude-standard` honours that — not a
  hand-maintained exclude list that can rot.
- **Acceptance:** all four hold —
  ```sh
  # host
  git ls-files -co --exclude-standard | wc -l          # -> H
  # guest, via tart exec
  find /Users/admin/vmtest-src ! -type d | wc -l       # -> G
  ```
  (1) `G == H`; (2) the logged byte count is > 80,000,000 (DOC-1 §6.1 measured
  ~81 MiB across 5,306 files by `git archive`, a **lower bound** since `-o` adds
  untracked-but-not-ignored files); (3) `test -d /Users/admin/vmtest-src/target`
  is **false**; (4) the pipeline's exit status is 0 with `pipefail` set.

  > **Correction, 2026-08-01 (UTC) — the guest-side count is `! -type d`, not
  > `-type f`. Do not revert it.** As originally written this acceptance check
  > **failed on a correct transfer**. This repo carries **4 tracked symlinks**
  > (`git ls-files -s | awk '$1=="120000"' | wc -l` → 4). `git ls-files` counts
  > them, `tar` transfers them correctly *as symlinks*, and `find -type f` does
  > **not** count them — so the literal check reported `G = H − 4` and would have
  > condemned a transfer that was byte-for-byte right. `! -type d` counts regular
  > files **and** symlinks, which is the set comparable to `git ls-files`'s output.
  > This is not a loosening: it is the same equality over the correct set, and it
  > is what the Phase 1 run of 2026-07-31 actually asserted (`5337 == 5337`, with
  > `-type f` logged alongside at `5333` precisely so the 4-file gap stays visible).
  > **P3-T4 must carry `! -type d` into `lib/source.sh`.** Recorded in
  > [MANIFEST.md](./MANIFEST.md) Phase 1, Deviations item 1.

### P1-T7 — Build one crate from the unpacked tree

- **Files:** modify `vmtest-harness/spike/spike-transport.sh`.
- **Contract:** DOC-2 §7.3 (guest environment prelude: `PATH`,
  `CARGO_TARGET_DIR`, `SKIP_UI_BUILD=1`), §7.4 (worked `tart exec` invocation);
  DOC-1 §8.4 (assert `rustc --version` in the crate directory immediately before
  the build), §8.6 (shared `CARGO_TARGET_DIR`), §7.3 (installs go through cargo
  only — **never `cp`**, for cdhash reasons).
- **Do:** build **`trusty-search`** with `cargo install --path
  /Users/admin/vmtest-src/crates/trusty-search`, under the full prelude, using
  `/bin/sh -c` and **never `-lc`** (a login shell reads rc files, which DOC-1 §5.3
  forbids depending on).
  > **Judgment call, labelled.** `trusty-search` is chosen over `tga` because it is
  > the crate whose in-guest source build was actually measured — 112 s, 409
  > crates, 8 vCPU, under `SKIP_UI_BUILD=1` (`vm-install-probe-findings.md:934`).
  > A failure is therefore attributable to the transport rather than to an
  > unmeasured build. `tga` would add a confounder: its
  > `rust-toolchain.toml` pins `channel = "stable"`, resolving to rustc **1.97.1**
  > inside the crate directory versus the workspace-pinned **1.91.1** at the root
  > (DOC-1 §8.4, measurement K5), so a `tga` spike would also be downloading a
  > second toolchain. That drift is real and P5-T1 asserts it; it does not belong
  > in the risk-retirement slice.
- **Acceptance:** `cargo install --path` exits 0; then in the guest
  `command -v trusty-search` prints a path under `/Users/admin/.cargo/bin`, and
  `trusty-search --version` exits 0. Log the build wall clock and compare it to
  the measured 112 s.
- **Depends:** P1-T6

### P1-T8 — Teardown and host-cleanliness assertion

- **Files:** modify `vmtest-harness/spike/spike-transport.sh`.
- **Contract:** DOC-1 §8.1 (**never** bare-`tart stop` treated as completion —
  write loss reproduced **4 of 5 attempts**, the confirmed root cause of a golden
  image shipping broken), §8.2 (never `tart suspend`); DOC-2 §10.1
  `wait_for_stopped` (**1 s interval, 120 s maximum**), §12.2 `vm_request_stop`,
  §Shell discipline (trap rule, cleanup properties 4–5).
- **Do:** call the shutdown initiator DOC-2 §12.2 now names — **`vm_request_stop`**:
  `sync; sync` in the guest over `tart exec` (non-fatal), then `tart stop` with its
  **exit code discarded**. Then **poll `tart list` for the observable `stopped`
  state**, then `tart delete`. Do not trust the stop's return, and do not kill the
  `tart run` process on failure.
  - **§F-9 was RESOLVED at source on 2026-07-31** — the initiator is specified, not
    left to you, and a guest-side `shutdown -h now` is **forbidden** (DOC-2 §12.2,
    amended 2026-07-31). Do not substitute one, and do not read this task as
    validating the choice of initiator: that question is closed.
- **Acceptance:** after the script exits, `tart list | grep vmtest-spike` produces
  **no output**, and the script's exit status is 0. Record the wall clock between
  `vm_request_stop` returning and `tart list` first reporting `stopped` — that
  interval is the only unmeasured number in the teardown path, and it is worth
  measuring on its own account: DOC-2 §10.1's 120 s maximum is a judgment call
  standing in for a worst-case flush duration nobody has observed.
- **Depends:** P1-T7

### P1-T9 — Record the two measurements this phase produces

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`
  (Measurements field of Phase 1).
- **Contract:** DOC-1 §6.1 ("the implementation should log the actual streamed
  byte count so this stops being an estimate"), DOC-1 §14 (the transport gap),
  DOC-2 open items (full base-image digest).
- **Do:** record verbatim — streamed byte count and file count; boot-to-ready
  seconds; provisioning seconds; `trusty-search` build seconds; the
  `vm_request_stop`-to-`stopped` interval from P1-T8 (§F-9); the base-image
  digest and how it was obtained. These replace three estimates in the doc set and
  are written back to DOC-1/DOC-2 in P8-T4.
- **Acceptance:** MANIFEST Phase 1 `Measurements` contains six numeric values and
  the digest, each with the command that produced it.
- **Depends:** P1-T8

### P1-T10 — Contingency: what happens if the transport does not work

- **Files:** modify MANIFEST (Deviations for Phase 1).
- **Contract:** DOC-1 D4's recorded product-owner decision of 2026-07-31; DOC-1
  §14.
- **Do:** if P1-T6 or P1-T7 fails and the cause is the transport (not a host
  misconfiguration): **do not repair, do not retry, do not invent a workaround.**
  Record the failure verbatim, mark Phase 1 `blocked`, and **stop**. DOC-1 D4
  names the fallback explicitly — pattern (b), whose transport *was* measured at
  `GIT_CLONE_MS=50131`, becomes the first implemented pattern and the order becomes
  (b) → (c) → (a). That re-ordering **changes a settled decision** and therefore
  requires product-owner sign-off before Phase 2 begins; it is not yours to make.
- **Acceptance:** either this task is recorded `N/A — transport verified`, or the
  MANIFEST carries the verbatim failure output, the phase state `blocked`, and an
  explicit note that sign-off is pending.
- **Depends:** P1-T6, P1-T7

### P1-T11 — Update the MANIFEST

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** set Phase 1 state, paste the **observed result** of the checkpoint
  (actual terminal output, not a claim), list files delivered, record deviations.
- **Acceptance:** `git diff --stat` shows MANIFEST.md modified; the Phase 1
  `Observed result` field contains pasted output including the byte count and the
  post-teardown `tart list`; state is one of `complete` / `blocked`.
- **Depends:** P1-T9, P1-T10

---

## PHASE 2 — Host-side skeleton: driver, config, registry, `lib/vm.sh`, preflight, `clean`

**Goal:** everything the harness does **before it touches a guest**, plus the
complete `tart` boundary module. No provisioning, no install, no oracle.

**Why here.** Phase 1 proved the transport with a script that cheats on every
contract — no exit codes, no registry, no config, no trap. Phase 2 builds the
contracts that the rest of the harness is allowed to assume. It is entirely
host-side and therefore fast to iterate: no phase after this one should be
debugging argument parsing while a VM boots.

**Checkpoint — PASS CONDITION.**

> All three hold, in one session:
> 1. `vmtest run local --dry-run` **exits 0**, prints an effective-configuration
>    banner in which every key carries an origin marker (`default` / `env` /
>    `flag`), and `tart list` afterwards shows **no new VM**.
> 2. `VMTEST_CPU=4 vmtest run local --dry-run` prints `cpu 4 (env)`, and
>    `vmtest run local --cpu 2 --dry-run` prints `cpu 2 (flag)`.
> 3. `vmtest clean --dry-run` correctly classifies a hand-created stopped
>    `vmtest-*` VM as `ORPHANED (would delete)` and a `keep`-marked one as
>    `KEPT (would not delete)`, deleting neither.

### P2-T1 — Driver skeleton, `die()`, traps, cleanup

- **Files:** create `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §2 (exit-code table), §12.4 (`die`, write-once
  `VMTEST_EXIT`, "first classified failure wins"), §Shell discipline (bash **3.2**
  target, `set -euo pipefail` set **once** at the top of the driver before sourcing
  any `lib/` file, the trap/cleanup rule and its five properties).
- **Do:** shebang `#!/usr/bin/env bash`; assert `[ "${BASH_VERSINFO[0]}" -ge 3 ]`;
  `set -euo pipefail`; implement `die()` exactly as DOC-2 §12.4 gives it; install
  the three traps (`EXIT`, `INT`→130, `TERM`→143 — the explicit `exit` in the
  signal traps is **not decoration**: after a trap handler returns, bash may resume
  the interrupted command); implement `vmtest_cleanup` satisfying all five listed
  properties, including capturing `$?` on its **very first line** and using
  `${VAR:-}` for every variable it touches (`set -u` and traps interact badly).
  Subcommand dispatch for `run` / `clean` / `--check-table`; unknown → **exit 2**.
  - **bash 3.2 is the target and it shapes the code.** No `declare -A`, no
    namerefs, no `mapfile`, no `${var,,}`, no `globstar`, no `wait -n`. This is
    *why* §3, §8 and §9 all use the same `key<TAB>value` TSV — the TSV files are
    the substitute for a hash, not a stylistic preference.
  - Two gotchas DOC-2 states so you do not rediscover them: `set -e` is
    **suppressed inside a condition**, so a lib function whose failure must abort
    must never be called in `if`/`&&`/`||`/`!` context; and `local x=$(cmd)`
    **swallows** `cmd`'s status — declare first, assign second.
- **Acceptance:** `bash -n vmtest-harness/vmtest` is silent; `vmtest-harness/vmtest`
  with no arguments exits **2** and prints usage on **stderr**;
  `vmtest-harness/vmtest bogus` exits **2**.
- **Depends:** —

### P2-T2 — Configuration: `vmtest.defaults`, TSV reader, three-tier precedence

- **Files:** create `vmtest-harness/vmtest.defaults`; modify
  `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §8.1 (three tiers), §8.2 (**the complete example file is
  given verbatim — copy it**), §8.3 (precedence and origin reporting), §3.2 (TSV
  format rules: `key<TAB>value`, `#` comments, blank lines ignored, exactly one
  line per key, **unknown keys are an error** so a typo cannot silently become
  "unpinned").
- **Do:** copy DOC-2 §8.2's file verbatim. Implement one `awk`-based reader used
  by all three TSV files (§3.1's "one parser, three files"). Implement the
  **mechanical** override mapping — uppercase the key, prefix `VMTEST_`; there is
  no table to maintain and no key that is overridable-in-principle but forgotten in
  practice. CLI flags exist **only** for `--cpu`, `--memory`, `--runid`, `--keep`,
  `--dry-run` (§8.2); adding a flag per tunable would give the driver a surface
  larger than its behaviour.
  - See **§F-5**: DOC-2 assigns no module to the TSV reader. The decision rule is
    there.
- **Acceptance:**
  ```sh
  bash -c '. vmtest-harness/vmtest --source-only 2>/dev/null; conf_get cpu'   # -> 8
  ```
  or equivalent direct invocation returns `8` for `cpu`, `16384` for `memory_mib`,
  `2700` for `install_timeout`; a defaults file with an injected unknown key makes
  the driver exit **10**.
- **Depends:** P2-T1

### P2-T3 — `--runid` generation, validation, and the atomic run registry

- **Files:** modify `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §4.1 (optional, auto-generated when omitted), §4.2 (format
  `YYYYMMDDThhmmssZ-<pid>`; validation regex `^[A-Za-z0-9][A-Za-z0-9-]{0,31}$`;
  violation is **exit 2** before any VM work), §4.3 (registry, run-directory
  contents, ~~concurrency warning~~ — **§4.3a single-run refusal**, see the
  retraction note below).
- **Do:** acquire the run by **`mkdir "<registry root>/<runid>"`**. `mkdir` either
  creates or fails and two concurrent callers cannot both succeed — **a
  test-then-create sequence (`[ -d ... ] || mkdir ...`) is a race and must not be
  used.** Registry root is
  `${VMTEST_STATE_DIR:-$HOME/.local/state/vmtest-harness}/runs/`. Write `pid`,
  `vm`, `pattern`, `started` immediately on acquisition. ~~**Warn — do not fail —
  when another run directory holds a live PID** (§4.3: the harness cannot know the
  operator's host, and refusing a legitimate second run on a large machine would be
  worse than a warning ignored on a small one).~~ **RETRACTED — see below.**
- **Acceptance:** `vmtest run local --runid 'a b' --dry-run` exits **2**;
  `vmtest run local --runid $(printf 'x%.0s' $(seq 40)) --dry-run` exits **2**;
  running two `--runid dup` invocations where the first holds the lock makes the
  second exit **10** naming the conflicting run; an auto-generated id matches
  `^[0-9]{8}T[0-9]{6}Z-[0-9]+$`.
- **Depends:** P2-T2

  > **RETRACTED 2026-08-04 (issue #15) — this task's warn-don't-fail instruction
  > is withdrawn, and the §4.3 clause it cites is withdrawn with it.** The
  > harness supports **exactly one run at a time**: a second run is **refused**
  > with exit **10**, not warned about. The replacement contract is
  > [DOC-2 §4.3a](../02-design/02-harness-contracts.md), and the implementation
  > is `preflight_single_run`, which reads the refusal from the **run registry**
  > before the VM-state scan and distinguishes *a peer run is live — wait for
  > it* from *a crashed run left a VM — clean it up*. `registry_warn_live_peers`,
  > written for the retracted clause, is deleted.
  >
  > **This file was the FIFTH artifact, and the count in issue #15 was wrong.**
  > The issue named four (DOC-2 §4.3, `registry_warn_live_peers`, the
  > runid/registry mechanisms, and `README.md`); this task was missed because it
  > lives in the plan rather than the design set. Leaving it would have
  > re-created exactly the design-vs-implementation conflict #15 exists to
  > close — a future reader implementing P2-T3 from the plan would have restored
  > the warn path. Found by the two adversarial review passes over commit
  > `7f00f24a`, both independently.
  >
  > **What in this task still stands, unchanged:** the `mkdir` acquisition, the
  > prohibition on test-then-create, the registry root, the four immediate
  > files, the validation regex, and every acceptance clause above. Only the
  > warn-don't-fail sentence is withdrawn.

### P2-T4 — `lib/vm.sh` — the OS boundary

- **Files:** create `vmtest-harness/lib/vm.sh`.
- **Contract:** DOC-2 §12.2 (`lib/vm.sh` surface — **fifteen** signatures, given
  in full; *twelve* until the 2026-08-02 amendment added `vm_require_cli`,
  `vm_list` and `vm_manual_hint`), §12.1 (calling conventions),
  §10.1/§10.2 (poll and watchdog parameters),
  §10.4 (**no `timeout(1)` on macOS**); DOC-1 §3.2 (the designed extension seam for
  Linux — §12.2), §8.1, §8.2.
- **Do:** implement `vm_require_cli`, `vm_list`, `vm_clone`, `vm_size`, `vm_boot`,
  `vm_wait_ready`, `vm_state`, `vm_exec`, `vm_exec_raw`, `vm_exec_stdin`,
  `vm_request_stop`, `vm_wait_for_stopped`, `vm_assert_stopped`, `vm_delete`,
  `vm_manual_hint`, exactly per §12.2's return/emit column.
  - **`vm_require_cli`, `vm_list` and `vm_manual_hint` were added to §12.2 on
    2026-08-02** — the original twelve covered one VM's *lifecycle* completely and
    covered *enumeration* and *operator guidance* not at all. All three exist to
    **preserve** the DOC-1 §3.2 invariant: the preflight `tart`-on-`PATH` check,
    the VM enumeration §5.1/§4.1 need, and the manual-command text for three
    driver sites all name the OS tool, and the driver may not. See §12.2's
    amendment for the per-function reasoning.
  - **`vm_exec` deliberately does not die on non-zero** — it returns the guest's
    status verbatim so a caller can distinguish "the command failed" from "the
    harness failed", which is precisely what N1 needs, since N1's *expected* result
    is a non-zero exit. Callers requiring success wrap with `|| die 50 "..."`.
  - **`vm_request_stop` always returns 0 and discards `tart stop`'s status**
    (§12.2, added by the 2026-07-31 §F-9 amendment). The guest-side `sync; sync`
    that precedes the stop is logged-but-not-fatal, because cleanup runs on paths
    where the guest is already unreachable. The completion signal is
    `vm_wait_for_stopped`, never the stop's return. A guest-side `shutdown -h now`
    is **forbidden** as the initiator (§12.2, amended 2026-07-31) — do not reach
    for one here.
  - Build the watchdog from shell primitives: background the command, record the
    PID, poll `kill -0 <pid>` at the site's interval until the deadline, then kill
    and reap. **Do not reach for `timeout`/`gtimeout`** — that adds a Homebrew
    dependency to a harness whose host requirements are otherwise `tart`, `git`,
    `jq`, `cargo`, and would fail on a clean machine in a way that looks like a
    harness bug.
  - `vm_wait_ready` polls at a **fixed** 2 s interval, **not** exponential backoff:
    the distribution is tight and known (~18–35 s), so backoff's only effect is to
    overshoot a ready guest, in exchange for saving `tart exec` calls whose cost was
    measured as negligible (K1d).
  - **Timeout behaviour is uniform (§10.3): no retry, ever.** A retry that succeeds
    converts a reproducible failure into an intermittent one, and DOC-1 §8.2 shows
    a case where retrying is structurally incapable of helping. Classify by phase,
    report the budget *and* the `vmtest.defaults` key that changes it, and let the
    cleanup trap still run.
- **Acceptance:** two mechanical checks —
  ```sh
  grep -rlnw 'tart' vmtest-harness --include='*.sh' --include='vmtest' \
      --exclude-dir=spike          # exemption EXPIRES at P3-T4 — see below
  ```
  (`-w` is load-bearing, not decorative — see the second correction below)
  lists **only** `vmtest-harness/lib/vm.sh` (this is the DOC-1 §3.2 invariant and
  it must stay true for the life of the harness); and `bash -n
  vmtest-harness/lib/vm.sh` is silent. `lib/` files define **functions and nothing
  else** — no top-level statements, no `set`, no side effects at source time
  (§12.1); a stray `set +e` in a library would silently disarm the driver.

  > **Correction, 2026-08-01 (UTC) — the grep is scoped to the production tree;
  > `spike/` is exempt until P3-T4. Owner decision.** As originally written this
  > check was **unsatisfiable**: it required the grep to list *only* `lib/vm.sh`,
  > but `vmtest-harness/spike/spike-transport.sh` necessarily contains `tart` (it
  > *is* the tart boundary during Phase 1) and is not deleted until **P3-T4**, one
  > phase later. P2-T4 and P3-T4 could not both be satisfied. Opened as a forward
  > conflict by Phase 1 ([MANIFEST.md](./MANIFEST.md) Phase 1, Deviations item 4)
  > and decided here.
  >
  > **The invariant itself is NOT weakened.** It is DOC-1 §3.2 and it still says
  > exactly one file in the production tree may name the OS: `lib/vm.sh`. What is
  > scoped is the *search path*, not the rule — `spike/` is disposable Phase 1
  > scaffolding that was never production code (see the spike's own header).
  >
  > **The exemption EXPIRES at P3-T4**, which deletes `vmtest-harness/spike/`
  > outright (P3-T4 *Files*: "**delete** `vmtest-harness/spike/`"; its acceptance
  > requires `ls vmtest-harness/spike` to **fail**). Once that lands,
  > `--exclude-dir=spike` matches nothing and the scoped grep and the unscoped
  > grep are the same command. **P3-T4 must delete the `--exclude-dir=spike`
  > argument in the same commit that deletes the directory** — an exemption whose
  > subject no longer exists is how a temporary exception becomes a permanent one.
  > A reviewer of P3-T4 should check for both deletions.

  > **Correction, 2026-08-02 (UTC) — the grep is `-w`, because the English word
  > `started` contains the four letters `tart`. Owner decision.** As written
  > without `-w` this check was **unsatisfiable for a second, independent
  > reason**, and unlike the `spike/` conflict above this one never expires.
  > `grep -rln 'tart'` is a **substring** match, and DOC-2 §4.3 mandates
  > `started` as one of the four run-registry filenames — so the driver's
  > `date -u '+%Y-%m-%dT%H:%M:%SZ' > "$VMTEST_RUNDIR/started"` puts
  > `vmtest-harness/vmtest` in the output on a line that is a **mandated
  > filename, not an invocation of the OS tool**. Opened by Phase 2
  > ([MANIFEST.md](./MANIFEST.md) Phase 2, Deviations item 4) and decided here on
  > reading (a), the narrower of the two candidates.
  >
  > **The invariant itself is NOT weakened; it is measured correctly for the
  > first time.** DOC-1 §3.2 says exactly one production file may name the OS,
  > and exactly one does — `grep -rlnw` lists only `lib/vm.sh`, and the driver
  > contains **zero** invocations. An invocation is always word-delimited, so
  > `-w` is strictly the semantics the check always intended. It still matches
  > `tart-run.pid` and `tart-run.log`, because `-` is not a word character, so
  > nothing that *should* be caught stops being caught.
  >
  > **Do not "simplify" the `-w` away.** It looks redundant and it is not: drop
  > it and this check fails on correct work, which is the failure mode that
  > wastes a reviewer's afternoon and then gets "fixed" by deleting the check.
  > Rejected out of hand: renaming the registry file (§4.3 mandates the name) or
  > obfuscating the literal in the driver, which would defeat a review rather
  > than pass one. **P3-T4 inherits the identical check and is corrected in the
  > same way** — it would otherwise hit this same line the moment
  > `--exclude-dir=spike` is deleted.
- **Depends:** P2-T1

### P2-T5 — Preflight

- **Files:** modify `vmtest-harness/vmtest` (or `vmtest-harness/lib/vm.sh` for the
  VM-state checks only).
- **Contract:** DOC-1 §4.1 (the check table and the **stopped-state refusal**),
  §8.3; DOC-2 §2 (**exit 10** for every preflight refusal), §3.3 (digest
  comparison), §8.4 (host-capacity table), §JSON parsing dependency (the `jq`
  functional smoke test).
- **Do:** in order — `tart` on `PATH`; `jq` present **and functional**; base-image
  digest matches `base-image.pin` (or is enforced by construction per P1-T3);
  **every** existing VM the harness would touch is `stopped`; no runid collision;
  host capacity per §8.4's four rows (total physical memory **hard-fails**,
  available memory and core counts **warn**).
  - **Refuse; do not repair.** DOC-1 §4.1 is exact: *do not attempt to stop it, do
    not attempt to resume it, do not retry.* Both §8 failure modes are
    unrecoverable-by-retry, and an automated "fix it up and carry on" path is
    exactly how a broken image shipped once already.
  - Core count uses `hw.physicalcpu`, **not** `hw.ncpu`: on Apple silicon `hw.ncpu`
    counts efficiency cores, which do not contribute to a build the way the
    measured 8-vCPU guest's cores did, so counting the wrong cores produces a
    reassuring warning-free run on a machine that will be slow.
  - The **24 GiB** `host_min_memory_gib` default is a labelled judgment call in
    DOC-2 §8.4 (16 GiB guest + 8 GiB host), deliberately conservative and tunable
    *because* it is a guess. Do not "fix" it.
- **Acceptance:** temporarily corrupt the pin's `digest` value → `vmtest run local
  --dry-run` exits **10** and prints **both** the pinned digest and what was
  actually found; rename `jq` out of `PATH` → exits **10** with the host-dependency
  message; with the pin restored, preflight passes.
- **Depends:** P2-T2, P2-T3, P2-T4, **P1-T3**

### P2-T6 — `vmtest clean`

- **Files:** modify `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §5.1 (the four-condition definition of *orphaned* — **all
  four**), §5.2 (how in-progress runs are distinguished; the PID-reuse edge),
  §5.3 (`--keep` and the `keep` marker), §5.4 (the four cases and `--dry-run`).
- **Do:** implement the classifier. **`clean` never issues `tart stop`, never
  issues `tart suspend`, and never deletes a VM that is not already `stopped`** —
  it inherits DOC-1 §8.1/§8.3 wholesale. Implement `--dry-run` (full
  classification, prints the verdict for every candidate, deletes nothing) and
  `--include-kept`.
  - The **PID-reuse edge** is accepted deliberately and stated plainly: a recycled
    PID makes `clean` skip a genuine orphan, leaving a VM for a human. The
    opposite error — deleting a VM out from under a live run — **cannot occur by
    this mechanism**. Accepting a conservative false negative to make the dangerous
    false positive impossible is the trade; do not try to eliminate it with
    start-time comparisons.
- **Acceptance:** construct four fixtures and run `vmtest clean --dry-run` —
  (i) stopped `vmtest-*` with no registry entry → `ORPHANED (would delete)`;
  (ii) same, with a `keep` marker → `KEPT (would not delete)`;
  (iii) a registry directory with no matching VM → `PRUNE (bookkeeping)`;
  (iv) a `vmtest-*` VM in state `running` with no registry entry → the command
  **refuses**, prints the VM and its state, and exits **10**. Nothing is deleted in
  any of the four.
- **Depends:** P2-T4, P2-T5

### P2-T7 — Wire the checkpoint: `vmtest run <pattern> --dry-run`

- **Files:** modify `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §8.2 (lists `--dry-run` among the five CLI flags), §8.3
  (effective-configuration banner with origins). **See §F-1 — DOC-2 defines
  `clean --dry-run` but never defines `run --dry-run`.** The decision rule in §F-1
  is binding; do not extend it.
- **Do:** `run --dry-run` performs preflight, prints the effective configuration
  with origin markers plus the bash version (§Shell discipline: so a bug report
  says which bash produced it), acquires and immediately releases the run
  registry entry, and **stops before `tart clone`**. It creates no VM.
- **Acceptance:** the phase checkpoint's three conditions, verbatim. Note
  specifically that *"a run whose log does not state its own sizing cannot be
  compared against DOC-1 §9's cost baseline, and comparing against that baseline is
  most of what the numbers are for"* (§8.3) — the banner is load-bearing, not
  decoration.
- **Depends:** P2-T5, P2-T6

### P2-T8 — Update the MANIFEST

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** state, observed result (paste all three checkpoint commands and their
  output), files delivered, deviations.
- **Acceptance:** MANIFEST Phase 2 `Observed result` contains pasted terminal
  output for all three checkpoint conditions, including the `tart list` that shows
  no VM was created.
- **Depends:** P2-T7

---

## PHASE 3 — Guest bring-up: N1, provisioning, toolchain hand-off, source delivery

**Goal:** a `vmtest run local` that boots a guest, proves it has no toolchain,
provisions it, streams the source in, and tears down — with **no installs and no
oracle yet**.

**Why this shape.** The scenario file grows across phases. At the end of Phase 3
`scenarios/install-local.sh` contains step 1 of DOC-2 §12.5's skeleton and nothing
else. This is deliberate: it gives Phase 3 a runnable checkpoint without inventing
a driver flag to stop early, and it keeps the scenario honest — a scenario is *a
sequence of install steps plus the expectations that follow from them* (DOC-1
§3.6), and at this point there are no install steps, so there are no expectations.

**Checkpoint — PASS CONDITION.**

> `vmtest run local` **exits 0**, and its log shows, in order: `N1 PASS` with a
> non-zero exit recorded for each of `cargo`, `rustc`, `rustup`; a provisioning
> block ending with `rustc_version 1.91.1`; a streamed byte count > 80,000,000;
> and a teardown after which `tart list` contains **no** `vmtest-*` entry.
> `$VMTEST_RUNDIR` is removed, and `ls "${VMTEST_STATE_DIR:-$HOME/.local/state/vmtest-harness}/runs/"`
> is empty.

### P3-T1 — N1 precondition probe

- **Files:** create `vmtest-harness/lib/verify.sh` (see §F-4 for why the probes
  live here); modify `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §6.2 **N1** (exact command, expected exit, expected output
  shape, predicate, **exit 30** on failure), §6.3 (**pinned** lifecycle position),
  §6.1 (why the probe had to be split at all); DOC-1 §4.2.
- **Do:** implement `negative_probe_n1`, invoked through `vm_exec_raw` — the
  **raw** variant, because at this point `VMTEST_GUEST_ENV` is still in its
  **base** lifetime (§7.3: base path only, no cargo, no mise, no cargo variables)
  and that is exactly what makes N1 meaningful.
  - Position it at `boot → vm_wait_ready → [N1] → provision` and nowhere else.
    This is the only window in which the guest genuinely lacks cargo, and it is
    the assertion a golden image structurally destroys — one of the two stated
    reasons the harness does not bake one (DOC-1 §4.3).
- **Acceptance:** on a fresh guest, `N1 PASS` with three recorded non-zero exits
  **on the base-PATH channel and no reachable toolchain on the second channel**;
  then, as a deliberate negative control, run `mise use -g rust@1.91` **before**
  N1 in a throwaway invocation and confirm the driver exits **30** without
  proceeding to provisioning.
  - *(Reconciled 2026-08-02.)* **This negative control did not pass as written at
    Phase 3, and that was a finding about N1 rather than about the check.**
    `mise use -g rust@1.91` installs to `~/.cargo/bin` and the mise shims, neither
    of which is on the base PATH the probe then examined, so N1 **passed on a
    guest that demonstrably had a toolchain** — see MANIFEST Phase 3, Deviations
    item 1. Resolved by owner decision on reading (a): **DOC-2 §6.2 is amended and
    N1 is strengthened to assert REACHABILITY**, so this acceptance check is now
    satisfied by the wording above, unchanged. **Re-run 2026-08-02 on one guest,
    both directions: clean clone → `N1 PASS`, exit 0; same guest after
    `mise use -g rust@1.91` → `FAIL[30]`, exit 30.** Output in MANIFEST Phase 3.
- **Depends:** P2-T8

### P3-T2 — Provisioning

- **Files:** create `vmtest-harness/lib/provision.sh`.
- **Contract:** DOC-2 §11.1 (**verified** preinstall state), §11.2 (per-tool
  strategy and the three-assertion mise detection command), §11.3 (**fail, do not
  repair** — exit 40), §11.5 (the amendment to DOC-1 §3.3); §12.2
  (`provision_guest`, `provision_detect_mise`, `provision_load_toolchain`).
- **Do:** implement the three functions with §12.2's exact signatures. Detection
  asserts all three of: `mise` resolves **under `/opt/homebrew/`**; **no second
  mise at `$HOME/.local/bin/mise`** — the exact artefact `mise.run` would create,
  so asserting its absence turns "somebody ran the forbidden command" from a
  mystery into a named failure; and `mise --version` returns 0. Install only
  `rust@1.91` and `uv@latest`.
  - **If detection fails, exit 40. Do not fall back to installing mise.** A
    `tahoe-base` without a Homebrew mise at `/opt/homebrew/bin/mise` **is not the
    base image this harness is pinned to** — it is a drift signal, and §3's whole
    purpose is to catch drift. This is not hypothetical: DOC-1 §5.3 records a
    golden image that shipped with `~/.zshenv` missing, which made `cargo` return
    **127** under both `/bin/sh` and `/bin/zsh` and presented as "cargo is not
    installed". A missing dotfile and a duplicated toolchain manager are the same
    category of failure.
- **Acceptance:** `vmtest run local` logs a provisioning block whose total wall
  clock is within 3× of the measured 30.079 s, with `gh` detected as already
  present (measured 616 ms — a no-op); a fixture where `$HOME/.local/bin/mise` is
  created before provisioning makes the run exit **40** with the second-mise
  message.
- **Depends:** P3-T1

### P3-T3 — Toolchain hand-off: `toolchain.tsv` and `VMTEST_GUEST_ENV`

- **Files:** modify `vmtest-harness/lib/provision.sh`, `vmtest-harness/lib/vm.sh`.
- **Contract:** DOC-2 §7.1 (what provisioning writes, where, and its **measured**
  seven values), §7.2 (why a guest file *and* a host copy), §7.3 (composition
  happens in **exactly one place**: `vm_exec`), §7.4 (the worked invocation and its
  four deliberate details); DOC-1 §5.2, §5.3, §3.3, §8.6.
- **Do:** provisioning writes `/Users/admin/.vmtest/toolchain.tsv`; the driver
  reads it back over `tart exec` into `$VMTEST_RUNDIR/toolchain.tsv`; the guest
  copy is **kept**, because it is what makes a `--keep` VM inspectable by a human
  reproducing a failing command by hand. Compose `VMTEST_GUEST_ENV` — `PATH`,
  `CARGO_TARGET_DIR`, `SKIP_UI_BUILD=1`, each followed by `export` — inside
  `vm_exec` and nowhere else. Scenarios never build a prefix and never see one.
  - **Ordering is load-bearing, not cosmetic:** `~/.cargo/bin` **must precede** the
    mise shims directory. mise's rust backend delegates to rustup, so putting the
    real rustup shims first is what allows rustup's *directory-based*
    `rust-toolchain.toml` resolution to work — which is precisely the mechanism
    DOC-1 §8.4 depends on. Reverse the order and §8.4's assertion silently stops
    measuring what it claims to measure.
  - **`VMTEST_GUEST_ENV` has two lifetimes** (§7.3): base before provisioning,
    full after. It is the only global that changes after preflight (§12.3).
- **Acceptance:** `$VMTEST_RUNDIR/toolchain.tsv` contains all seven keys of §7.1
  with `rustc_version 1.91.1`; `guest_path` begins with
  `/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:`; a `vm_exec` of
  `printf '%s' "$PATH"` returns that exact string.
- **Depends:** P3-T2

### P3-T4 — Promote the spike into `lib/source.sh`; delete the spike

- **Files:** create `vmtest-harness/lib/source.sh`; **delete**
  `vmtest-harness/spike/`.
- **Contract:** DOC-2 §12.2 `source_deliver_local` (signature, and "**emits the
  streamed byte count**, which DOC-1 §6.1 explicitly asks be logged"), §12.1
  (positional string arguments; the value channel is stdout and carries **at most
  one value**; diagnostics **always** to stderr because §1's oracle parses stdout);
  DOC-1 §6.1.
- **Do:** port P1-T6's pipeline into `source_deliver_local <vm_name> <host_repo>
  <guest_dir>` through `vm_exec_stdin`. Emit **only** the byte count on stdout;
  everything else goes to stderr. Then delete the spike directory — its job was to
  fail fast, and it has either done that or been superseded.
  - **Naming tension, recorded (§12.2):** DOC-1 §3.4 calls `source.sh` "source
    delivery" while DOC-1 §12.1 wants reusable **install-step** functions, so
    `install_from_path` / `install_from_registry` (P5-T1, P7-T1) also live here.
    Read `source.sh` as *"source acquisition and installation"*. A later split into
    `lib/install.sh` is permitted and would change no scenario, because scenarios
    call the functions, not the file.
- **Acceptance:** `vmtest run local` logs the byte count; `ls vmtest-harness/spike`
  fails; `git log --stat` shows the spike deleted in the same commit that adds
  `lib/source.sh`. **Plus, added 2026-08-01: P2-T4's `--exclude-dir=spike`
  exemption is deleted in that same commit, and the unscoped
  `grep -rlnw 'tart' vmtest-harness --include='*.sh' --include='vmtest'` lists only
  `lib/vm.sh`.**

  > **Correction, 2026-08-01 (UTC) — two things this task must carry.** Both are
  > consequences of Phase 1's findings; neither changes what P3-T4 does, only what
  > it must be checked for.
  >
  > 1. **Port `find … ! -type d`, not `find … -type f`.** P1-T6's acceptance was
  >    corrected on this date because `-type f` misses this repo's 4 tracked
  >    symlinks and therefore fails a correct transfer. `lib/source.sh` inherits
  >    the corrected form.
  > 2. **Retire P2-T4's `spike/` exemption here.** P2-T4's grep carries
  >    `--exclude-dir=spike` because this task is what removes its subject. Deleting
  >    `vmtest-harness/spike/` without also deleting that argument leaves a
  >    permanent exemption for a directory that no longer exists — the DOC-1 §3.2
  >    invariant would then be enforced by a command that has quietly stopped being
  >    able to catch a violation in a re-created `spike/`.

  > **Correction, 2026-08-02 (UTC) — the unscoped grep above carries `-w`, for
  > the same reason P2-T4's does.** This task inherits P2-T4's check verbatim, so
  > it inherits its defect verbatim: without `-w`, `grep -rln 'tart'` matches the
  > substring inside `started`, DOC-2 §4.3's mandated run-registry filename, and
  > lists `vmtest-harness/vmtest` on a line that is a filename rather than an
  > invocation. See P2-T4's 2026-08-02 correction for the full reasoning. **The
  > two corrections are independent of each other**: deleting `--exclude-dir=spike`
  > here (item 2 above) removes the *first* reason this grep could not pass and
  > does nothing about the second, so `-w` must survive that deletion. A reviewer
  > of P3-T4 now checks for **three** things: the directory deleted, the
  > `--exclude-dir=spike` argument deleted, and `-w` still present.
- **Depends:** P3-T3, P1-T6

### P3-T5 — `scenarios/install-local.sh` (delivery only) and scenario dispatch

- **Files:** create `vmtest-harness/scenarios/install-local.sh`; modify
  `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §12.5 (the worked skeleton — implement **step 1 only** at
  this phase), §12.1, §12.4 (scenarios do **not** call `die` with a code of their
  own — they call lib functions, which die with their own phase code, so a scenario
  stays a description of steps and expectations and never encodes the exit-code
  table); DOC-1 §3.6. **See §F-6** — the driver's pattern→file→function dispatch is
  unspecified; the decision rule is there.
- **Do:** the scenario contains `scenario_install_local()` with step 1 of §12.5 and
  a `log` of the byte count. Note what the skeleton must **not** contain: no
  `tart`, no `PATH`, no timeout, no exit code, no `if` around a lib call.
- **Acceptance:** `grep -E 'tart|PATH=|exit ' vmtest-harness/scenarios/install-local.sh`
  produces **no output**; `vmtest run local` reaches teardown and exits 0.
- **Depends:** P3-T4

### P3-T6 — `~/.zshenv`: written, never depended on

- **Files:** modify `vmtest-harness/lib/provision.sh`.
- **Contract:** DOC-2 §11.4 (the reconciliation, stated as a blockquote rule);
  DOC-1 §5.3.
- **Do:** provisioning **may** write `~/.zshenv` as a convenience for a human
  inspecting a `--keep` VM. **No harness logic may read it, source it, or depend on
  it having been written.** The measured step exists (`STEP_ZSHENV_MS=617`), so
  writing it costs nothing. The reconciliation must be explicit in a comment **or
  someone will delete one rule and trust the other**.
- **Acceptance:** the file is written in the guest, and `grep -rn 'zshenv'
  vmtest-harness --include='*.sh' --include=vmtest` shows it referenced **only** in
  the writing step — never in a read, source, or conditional. The deliberate
  deletion drill that proves this is P8-T1.
- **Depends:** P3-T2

### P3-T7 — Run the checkpoint and update the MANIFEST

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** run `vmtest run local` to completion; paste the observed log; record
  files delivered and deviations. Also record the second boot-to-ready measurement
  (subsequent boots measured ~18 s) for comparison against P1.
- **Acceptance:** MANIFEST Phase 3 `Observed result` contains the four log
  landmarks named in the checkpoint plus the empty-registry `ls`.
- **Depends:** P3-T5, P3-T6

---

## PHASE 4 — `expected-binaries.tsv` and `--check-table`

**Goal:** the authoritative expectation table, and the self-diff that keeps it
honest. Host-only, no VM.

**Why before the oracle.** The oracle consumes this table. DOC-1 §7.4 is blunt
about the stakes: the Single-Install gate *"is only ever as good as §7.2's table.
It cannot detect the loss of a binary it has never heard of — an omitted row is
not a weaker assertion, it is **no** assertion, and it fails silently and
permanently."* That is not hypothetical either: the `trusty-memory-mcp-bridge`
omission (DOC-2 §9.3) would have produced exactly that blindness. Build the table
and its differ before anything asserts against it.

**Checkpoint — PASS CONDITION.**

> `vmtest --check-table` **exits 0** against the workspace as it stands, printing
> no ADDED/REMOVED/CHANGED findings. Then, with one row deliberately deleted from
> `expected-binaries.tsv`, it **exits 60** and prints exactly one `ADDED` finding
> naming that `(package, binary)` pair. The row is restored afterwards and the
> command exits 0 again.

> **Correction, 2026-08-02 (UTC) — the class is `ADDED`, not `REMOVED`. Do not
> "correct" it back.** As originally written this checkpoint named the finding
> class produced by the *opposite* operation, and no implementation could satisfy
> both it and its own cited contract. DOC-2 §9.6 defines the two sets by direction:
>
> ```
> ADDED   := keys in actual  \ declared   -> a new binary nobody declared
> REMOVED := keys in declared \ actual    -> a binary that vanished
> ```
>
> `actual` is the workspace's `[[bin]]` targets; `declared` is the rows of
> `expected-binaries.tsv`. **Deleting a row from the TABLE removes the key from
> `declared` while the workspace still declares that `[[bin]]`** — the key is in
> `actual \ declared`, which is `ADDED` by definition. `REMOVED` is produced by
> deleting a `[[bin]]` from the **workspace** (or adding a phantom row the
> workspace has no target for): the key is then in `declared \ actual`. Table and
> workspace are the two sides of the diff and the drill touches the table, so the
> original wording named the wrong side.
>
> **The direction is load-bearing, not cosmetic.** It is what tells an operator
> which side to fix. Inverting the labels to satisfy the original sentence would
> make the differ announce *"a binary that vanished"* when a binary had in fact
> been added, and would send whoever reads it to repair a workspace that is
> correct. **DOC-2 §9.6 is authoritative and is NOT amended** — it was right all
> along; only this checkpoint was wrong.
>
> Deleting a table row remains the checkpoint's action: it is the cheapest way to
> exercise the differ, and every other clause — exit **60**, **exactly one**
> finding, naming the `(package, binary)` pair — was already correct and is
> unchanged. Opened by Phase 4 ([MANIFEST.md](./MANIFEST.md) Phase 4, Deviations
> item 1, where the implementation is recorded as following §9.6 deliberately) and
> decided here. **P4-T5's acceptance is corrected in the same way**, and P4-T2's
> acceptance inherits this checkpoint verbatim.

### P4-T1 — Seed `expected-binaries.tsv`

- **Files:** create `vmtest-harness/expected-binaries.tsv`.
- **Contract:** DOC-2 §9.1 (**nine** columns, tab-separated, one header row, `#`
  comments, `LF` endings), §9.2 (`[package] name` is the key; `package` + `binary`
  is the composite primary key), §9.3 (**the seed content is given verbatim —
  copy it**), §9.4 (`req_features` and the implicit target); DOC-1 §7.2, §7.5, D3.
- **Do:** copy §9.3's block verbatim, including the out-of-scope rows. Do not
  re-derive it by hand.
  - **`in_scope` exists rather than two files** because `--check-table` must diff
    against **every** `[[bin]]` in the workspace or it cannot detect a newly added
    binary: a binary absent from a scope-only file is indistinguishable from one
    that was never in scope.
  - **Thirteen in-scope rows, eight packages.** Both `trusty-mpm` rows carry
    `expect_a = present` (§A.1). `tga`'s `package` is `tga` while its `crate_dir`
    is `trusty-git-analytics` — the discontinuity DOC-1 D3 warns about.
  - `req_features` is carried because four in-scope binaries are gated behind
    `required-features` that are *currently* in their crate's `default` set. If a
    future change drops one, `cargo install` **succeeds and silently produces no
    binary** — a green install with a missing daemon, exactly DOC-1 §7.4's failure
    mode.
- **Acceptance:**
  ```sh
  awk -F'\t' 'NR>1 && $1 !~ /^#/ && NF!=9 {print NR": "NF}' vmtest-harness/expected-binaries.tsv
  ```
  prints nothing (every row has nine fields). Then, **stated as derivations, with
  today's value as the expected literal** — the literals have already moved twice
  (D2's reversal, then D3's `trusty-review` addition), so assert the derivation and
  read the number as a checksum, not as the contract:
  - `awk -F'\t' '$6=="yes"' | wc -l` equals **the number of in-scope binaries**,
    i.e. one row per `[[bin]]` of a D3 package (**13** today).
  - `awk -F'\t' '$6=="yes" {print $2}' | sort -u | wc -l` equals **the number of
    D3 crates** (**8** today) — this is the same number P4-T4's helper must emit.
  - `grep -c 'trusty-memory'` shows the **three** `trusty-memory` binary rows
    including `trusty-memory-mcp-bridge` — three because the manifest declares three
    `[[bin]]` targets, not because three is a magic number.
- **Depends:** P2-T8

### P4-T2 — `--check-table` self-diff

- **Files:** modify `vmtest-harness/vmtest`.
- **Contract:** DOC-2 §9.6 (source of truth **confirmed**; the six-step algorithm;
  exit **60**; **no auto-fix**), §9.4 (implicit targets); DOC-1 §7.2.
- **Do:** read actual targets via **`cargo metadata --no-deps --format-version 1`**
  and `jq`, not by parsing `Cargo.toml` files. Three real defects avoided:
  `cargo metadata` reports **implicit** targets (`crates/trusty-agents-local` has a
  `src/main.rs` and no `[[bin]]` section, so manifest-parsing misses it entirely
  and would report a spurious deletion); it resolves workspace-inherited fields;
  and it enumerates the non-`crates/*` path member
  `crates/trusty-agents/ui/src-tauri` that a `crates/*/Cargo.toml` glob would skip.
  Implement ADDED / REMOVED / CHANGED, and RENAMED as a **suggestion only, never
  applied automatically**.
  - **It does not auto-fix.** A table that rewrites itself to match reality asserts
    nothing — the human edit *is* the review step, and removing it would turn the
    authoritative expectation source into a mirror.
  - Compare **columns 1–5 only**. `in_scope` and the three `expect_*` columns are
    human judgments about scope, not facts about the workspace, and nothing can
    derive them.
- **Acceptance:** the phase checkpoint, verbatim. Additionally, changing a
  `bin_path` value in the TSV produces exactly one `CHANGED` finding and exit 60.
- **Depends:** P4-T1

### P4-T3 — Reconcile the seed against today's workspace

- **Files:** possibly modify `vmtest-harness/expected-binaries.tsv`; modify
  MANIFEST.
- **Contract:** DOC-2 §9.3, §9.6.
- **Do:** DOC-2's seed was enumerated on 2026-07-31 (**27** explicit `[[bin]]`
  targets across 20 manifests, plus one implicit — **28** rows; the count was
  corrected from 26 in DOC-2 §9.3 on the same date, the table itself having always
  been right). If `--check-table` reports findings on
  the unmodified workspace, the workspace has moved since. **Record every finding
  verbatim in the MANIFEST**, then apply the human edit — adding a genuinely new
  binary with `in_scope=no` unless it belongs to one of D3's **eight** packages, in
  which case it is `in_scope=yes` with `present` in all three `expect_*` columns.
  - **Do not silently widen D3's scope.** The worked precedent is `trusty-review`:
    DOC-2 §9.3 note 2 carried it `in_scope=no` faithfully to D3 while recording that
    *"whether D3's scope should include it is a design question this document does
    not decide, but it should be decided knowingly rather than by omission."* It was
    then decided knowingly — by the owner, on 2026-07-31, as a **dated amendment to
    DOC-1 D3** (§A.1b) — and only then did its row flip to `in_scope=yes`. That is
    the shape the rule requires: flag it here, decide it in a design amendment, and
    let the table follow. Same rule for anything new: knowingly, in a PR, not as a
    side effect of this task.
- **Acceptance:** `vmtest --check-table` exits 0; the MANIFEST records either
  "no drift since DOC-2 §9.3" or the exact findings and the edit made.
- **Depends:** P4-T2

### P4-T4 — Scope helpers, including the deduplication the oracle needs

- **Files:** modify `vmtest-harness/vmtest` (or `lib/verify.sh`, per §F-5).
- **Contract:** DOC-2 §12.5 (calls `tsv_scope_crate_dirs`, "column 2 where
  in_scope=yes"), §9.1, §9.3. **See §F-3 (RESOLVED)** — the thirteen in-scope rows
  contain only **eight** distinct `crate_dir` values, so the helper must
  deduplicate. Read §F-3 for *why*: not because an undeduped loop would invalidate
  the Single-Install gate (it would not — that claim was false and is corrected
  there), but because the P5 checkpoint counts installs and repeats are waste.
- **Do:** implement `tsv_scope_crate_dirs` (unique `crate_dir`, in first-appearance
  order), `tsv_scope_packages` (unique `package`), and `tsv_expect <package>
  <binary> <pattern>`. Apply §F-3's decision rule.
  - **`tsv_scope_crate_dirs` asserts its own postcondition before emitting.** Two
    lines of bash, and they turn every possible undedupe variant — a dropped
    `sort -u`, an `awk` seen-map that forgets to set its key, a refactor that
    reorders the pipeline — from a silent behaviour change into a classified
    failure at the point of the defect:

    ```sh
    _dups=$(printf '%s\n' "$_dirs" | sort | uniq -d)
    [ -z "$_dups" ] || die 60 "tsv_scope_crate_dirs emitted duplicates: $(printf '%s' "$_dups" | tr '\n' ' ')"
    ```

    It dies **60** (verification, DOC-2 §2) and **names the duplicated values**,
    because "a directory appeared twice" is unactionable without knowing which. A
    function whose entire contract is *unique* values should not rely on a caller
    to notice when it stops delivering them — and this one runs on the host, with
    no VM, so the failure arrives seconds into a run rather than minutes.
- **Acceptance — order-free, and derived.** Every count below is stated as a
  derivation with today's value as the expected literal:
  - `tsv_scope_crate_dirs | wc -l` equals the number of **distinct `crate_dir`
    values among `in_scope=yes` rows** (**8** today), and equals the number of D3
    crates.
  - `tsv_scope_crate_dirs | sort | uniq -d` is **empty** — no directory emitted
    twice. This is the property §F-3 actually needs; the count alone does not
    establish it.
  - `tsv_scope_crate_dirs` **contains `trusty-git-analytics` and does NOT contain
    `tga`** — `tga` is the package name and `--path` takes the directory, which is
    exactly the discontinuity DOC-1 D3 warns about.
  - `tsv_scope_packages | wc -l` equals the number of **distinct `package` values
    among `in_scope=yes` rows** (**8** today), includes `tga`, `trusty-mpm` and
    `trusty-review`, and likewise emits no duplicates.
  - `tsv_expect trusty-mpm tm a` returns `present`.
  - **Dropped, 2026-07-31: the "beginning `trusty-search`" assertion.** This
    acceptance previously required `tsv_scope_crate_dirs` to *begin* with
    `trusty-search`. That is a **file-layout assertion masquerading as a
    behavioural one**, and it had to go: DOC-2 §9.1 mandates **no sort order** for
    the TSV, and §9.6's `--check-table` algorithm is **set-based** (ADDED / REMOVED
    / CHANGED over key sets). Re-sorting the file — which nothing forbids and
    §9.6 would not even notice — would turn a green test red while nothing real
    had changed. First-appearance order remains the helper's specified *behaviour*
    (§F-3, and §F-10(b) relies on row order for install sequencing); it is simply
    no longer *asserted against a specific file's current layout*. The checks above
    are order-free and test the properties that matter: right set, no duplicates,
    right count, directory-not-package.
- **Depends:** P4-T1

### P4-T5 — Update the MANIFEST

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** state, observed result (paste the three `--check-table` invocations of
  the checkpoint), files delivered, deviations — including any reconciliation from
  P4-T3.
- **Acceptance:** Phase 4 `Observed result` shows exit 0, then exit 60 with the
  `ADDED` finding, then exit 0 again. (`ADDED`, not `REMOVED` — deleting a row
  from the *table* puts the key in `actual \ declared`; see the phase
  checkpoint's 2026-08-02 correction.)
- **Depends:** P4-T3, P4-T4

---

## PHASE 5 — Pattern (c) complete: install steps, N2, and the full oracle

**Goal:** `vmtest run local` installs all eight crates from the streamed tree and
asserts all thirteen binaries, `tctl stack doctor --json`, `tctl version --json`, the
Single-Install Convention, and interim daemon liveness.

**Why this is the largest phase.** Everything before it was infrastructure.
This is where the harness first makes its actual claim — *"a clean install of this
stack works today"* — and it is also where the first full-stack run is timed,
which DOC-1 §9 and DOC-2 §10.2 both explicitly request as a replacement
measurement.

**Checkpoint — PASS CONDITION.**

> `vmtest run local` **exits 0**, and the run log shows:
> Counts below are **derived**, with today's value as the expected literal; if the
> TSV has changed, the derivation is the condition and the literal follows it.
> (i) one `cargo install --path` per value of `tsv_scope_crate_dirs`
> (**8** today), and no directory installed twice, each preceded by a
> `rustc --version` line emitted from inside that crate's directory;
> (ii) `verify_binaries` reporting **N/N in-scope binaries present**, where N is the
> count of `in_scope=yes` rows (**13** today);
> (iii) `tctl stack doctor --json` parsed, with every in-scope package **that
> `doctor` reports as a member** satisfying `on_path == true`, `version != null`,
> and `health ∈ H_c` — where, per **DOC-2 §1.1 as amended 2026-08-03 (§1.1a)**,
> `H_c` is `{healthy, stale}`, plus `unknown` for a member with
> `plist_installed == null` (the product deliberately declines to probe it,
> `#4246`), plus `down` for a member with `plist_installed == false` (no plist,
> because `tctl install`'s service step is banned from pattern (c) by DOC-1 §6.5).
> In-scope packages `doctor` does not report — `trusty-code`, `trusty-installer`
> and `tga`, which are not daemon members — carry **no health obligation**; their
> coverage is clause (ii) and clause (iv);
> (iv) `verify_single_install` passing for `trusty-search` (2 binaries),
> `trusty-memory` (**3**), `trusty-installer` (2), and `trusty-mpm` (2);
> (v) N2 recorded with its observed exit code and stderr — **and, per DOC-2 §6.2
> as amended 2026-08-03, an N2 recorded `BLOCKED` SATISFIES THIS CLAUSE.** N2's
> guide-and-abort is unreachable through `tctl install` from a guest for two
> structural reasons (§6.2's RC-2 closure); the clause asks for the observation to
> be recorded, and a BLOCKED record with its exit code and stderr is that
> observation. Every other N2 failure shape still dies 30 and still fails the run;
> (vi) a total wall clock, logged, which is recorded in the MANIFEST as the
> **first full-stack measurement**.

> **CORRECTED 2026-08-03 — clause (iii), by owner decision. The previous text is
> not restored, and this note is why.** Phase 5 ran this checkpoint twice on real
> guests and clause (iii) could not be met, for reasons that were **in the
> checkpoint rather than in the harness or the product** (MANIFEST Phase 5,
> Deviations item 1).
>
> It previously required all **8** `tsv_scope_packages` values — "**including
> `trusty-mpm`**" — to satisfy `health ∈ {healthy, stale}`. Three of the eight are
> **not daemon members and are structurally absent from `stack doctor`'s output**
> (`doctor.rs:151` filters `stable_set()` to `m.daemon`), so no run of any pattern
> could ever satisfy it; and the "including `trusty-mpm`" emphasis named **the one
> member the product guarantees will fail the predicate** — `#4246` reports it
> `unknown` by deliberate design. That emphasis was **not in DOC-2**; it entered
> through this plan's own D2/D3 reversal, and it is **removed**, because a
> checkpoint must not single out the member it is least able to assert. DOC-2
> **§1.1a** now scopes the predicate and clause (iii) above tracks it.
>
> **This does not weaken an assertion the scenario can actually make.** All 13
> in-scope binaries are still asserted present, all 4 Single-Install gates still
> run, and `on_path`/`version` are still asserted for every member `doctor`
> reports. **Nothing under `crates/` was changed** — the harness adapts to the
> product, never the reverse.

> **CORRECTED 2026-08-03 — clause (v), by owner decision.** It previously read only
> "N2 recorded with its observed exit code and stderr", which P5-T2 read as
> requiring a PASS. RC-2's behaviour is **unreachable through `tctl install` from a
> guest** (DOC-2 §6.2, closed 2026-08-03 as *unreachable-by-design*), so the clause
> could never go green while being read that way — the checkpoint would have been
> permanently unmeetable for a reason that has nothing to do with whether the
> install worked. It now says explicitly that a **BLOCKED record satisfies it**.
> Every N2 failure shape other than the one proven unreachable still dies 30 and
> still fails the run.

### P5-T1 — `install_from_path` and the per-build-step `rustc` assertion

- **Files:** modify `vmtest-harness/lib/source.sh`; modify
  `vmtest-harness/lib/verify.sh`.
- **Contract:** DOC-2 §12.2 (`install_from_path`, `verify_rustc` — "called from
  `install_from_path`", dies **50**), §7.4 (the worked invocation and its four
  deliberate details); DOC-1 §8.4, §8.6, §7.3, §6.5.
- **Do:** `install_from_path <vm_name> <guest_dir> <crate_dir>` asserts `rustc
  --version` **first**, then runs `cargo install --path`.
  - **`cd` into the crate directory, and use `&&` not `;`.** rustup resolves by
    current directory; the assertion is worthless run anywhere else, which is
    exactly why DOC-1 requires it adjacent to the build rather than once at
    provisioning time. A failed `cd` must not run the command in the wrong
    directory.
  - **Install the package, never the binary — no `--bin`, no filtered `--bins`**
    (DOC-2 §12.2, amended 2026-07-31). The TSV's `binary` column is the
    **oracle's** input, not the installer's; do not make the loop "row-faithful"
    by installing each row's binary by name. DOC-1 §7.4's gate asserts that *one*
    package-granular install yields *every* sidecar, so a per-binary install
    satisfies `verify_binaries` and every `verify_single_install` call while
    proving nothing — and a crate that stopped shipping a sidecar would still show
    green. Unlike a missing table row, `--check-table` cannot catch this, because
    the table is not what would be wrong.
  - **Toolchain drift is confirmed real in this repository, in-guest, under mise.**
    `crates/trusty-git-analytics/rust-toolchain.toml` specifies `channel =
    "stable"`, resolving to rustc **1.97.1** inside that crate versus the
    workspace-pinned **1.91.1** at the root (measurement K5, on the mise-provisioned
    VM `probe-k2`). So `verify_rustc` must take the **expected** version as an
    argument rather than assuming 1.91.1 everywhere — DOC-2 §12.2's signature
    `verify_rustc <vm_name> <guest_dir> <expected>` already says so.
  - **`tctl install` MUST NOT be used here** (DOC-1 §6.5). `install_one()` in
    `crates/trusty-installer/src/commands/install.rs` is prebuilt-tarball-first with
    a crates.io `cargo install --locked` fallback and has **no `--path` code path**,
    so invoking it during a source-based scenario would silently overwrite the
    source-built binaries under test — a false pass, the worst possible harness
    failure mode.
  - **Never `cp` a binary into a `PATH` directory** (DOC-1 §7.3): copying a Mach-O
    binary is not equivalent to installing it, and cdhash-dependent behaviour (TCC
    attribution, keychain ACLs, notarisation) does not survive an arbitrary copy.
- **Acceptance:** the run log contains one `rustc --version` line per value of
  `tsv_scope_crate_dirs` (**8** today), each immediately preceding its `cargo
  install --path`, and the one emitted from `crates/trusty-git-analytics` reports a
  **different** version from all the others
  — reproducing K5. If it does not, that is a finding to record, not to smooth over.
- **Depends:** P4-T5

### P5-T2 — **Pin RC-2**: the `tctl install` cargo-absent exit code

DOC-2 §6.2 deliberately leaves N2's predicate weak because the code at
`install.rs:829` was never read out to a verified value. This task closes that.

- **Files:** modify MANIFEST (Measurements); modify `vmtest-harness/lib/verify.sh`
  (predicate).
- **Contract:** DOC-2 §6.2 **RC-2** ("must exit with a stable, documented, non-zero
  code distinct from 1, and must emit actionable guidance on stderr, leaving stdout
  clean… Until that code is fixed and documented, N2 asserts only: exit != 0,
  stdout empty, stderr non-empty and containing a cargo-related token. That weaker
  predicate is stated as weak on purpose."), §2 (exit-code table).
- **Do:** three steps, in order, no decision required at any of them.
  1. **Read the guard.** `crates/trusty-installer/src/commands/install.rs:829` —
     `which::which("cargo").map_err(...)` inside the `Outcome::Fallback` arm,
     producing `anyhow!("no Rust toolchain found on PATH (cargo not available);
     cannot fall back to \`cargo install {}\`")`. The same guard exists at
     `upgrade.rs:502` and `self_update.rs:295`.
  2. **Trace it to a process exit code.** `install::run()` at `install.rs:102-110`
     returns `i32`; the error can reach `return 1` at `install.rs:148`, `:250`,
     `:273`, `:311`, or the roll-up `report.exit_code()` at `:321`. That `i32` is
     handed to `std::process::exit` at
     `crates/trusty-installer/src/main.rs:133`. Determine **which path** the
     cargo-absent error takes by reading, then **confirm by observation** — N2
     itself is the experiment.
  3. **Record and branch.**
     - **If the observed code is non-zero and distinct from 1:** RC-2 is satisfied
       in practice. Tighten N2 to assert that **exact** code, record it in the
       MANIFEST with the verbatim stderr, and note in the MANIFEST that RC-2
       remains formally open until the code is *documented* in `trusty-installer`
       (observing a code does not make it a contract).
     - **If the observed code is 1** (or varies between runs): RC-2 is **not**
       satisfiable today. Leave N2's weak predicate exactly as DOC-2 wrote it,
       record the observed value verbatim, and record RC-2 as still-open with the
       precise reason. **Do not change `trusty-installer` to make the harness
       happier** — that is a separate PR against a shipping crate, out of this
       plan's scope, and doing it here would mean the harness and the thing it
       tests were changed in the same breath.
- **Acceptance:** the MANIFEST Phase 5 `Measurements` field contains the literal
  observed exit code and the first line of stderr, and `lib/verify.sh` contains
  either the pinned code or a comment citing `DOC-2 §6.2 RC-2` explaining why the
  weak predicate stands.
- **Depends:** P5-T1

> **RECONCILED 2026-08-03 — step 3's branches were both wrong, and the outcome was
> a third thing neither anticipated.** The observed code is **3**: non-zero and
> distinct from 1, so step 3 routes to the first branch — "RC-2 is satisfied in
> practice; tighten N2 to assert that **exact** code". **Doing that would have been
> a defect.** `3` is `decide_install_gate`'s **consent-gate** code
> (`install_gate.rs:77-85` → `install.rs:266-278`), returned before `install_one`
> is ever called; the cargo guard at `install.rs:829` was never reached, so
> pinning `3` would have recorded a *different guard's* code as RC-2's and made
> N2 assert something it had not tested. Step 2 asked which path the cargo-absent
> error takes and assumed one of them was taken; **none was.**
>
> **RC-2 is now CLOSED as *unreachable-by-design* (DOC-2 §6.2), not pinned and not
> left open.** Two structural causes, both read from the source and confirmed by
> observation: the consent gate returns 3 before `install_one` whenever `--yes` is
> absent and stdin is not a TTY (a guest exec channel is not); and `--yes` would
> reach a **prebuilt-tarball-first** path whose cargo guard only fires when the
> download *fails*, so on a networked guest it would install **released** binaries
> over the source-built ones under test — the false pass DOC-1 §6.5 exists to
> prevent.
>
> **What this task actually delivered stands.** P5-T2's real rule — an observed
> code is not a documented contract, and **`crates/trusty-installer` is not to be
> changed to make the harness happier** — was correct and was followed; nothing
> under `crates/` was changed. The acceptance is met by the second of its two
> alternatives: `lib/verify.sh` carries a comment block citing `DOC-2 §6.2 RC-2`,
> and the MANIFEST carries the literal exit code and verbatim stderr. **This task
> needs no re-execution**; it is reconciled here so a future reader does not
> re-run it expecting branch one to apply.

### P5-T3 — N2 guide-and-abort probe

- **Files:** modify `vmtest-harness/lib/verify.sh`; modify
  `vmtest-harness/scenarios/install-local.sh`.
- **Contract:** DOC-2 §6.2 **N2** (the two-step capture-then-reinvoke command),
  §6.3 (position: **after** the scenario's install steps — the earliest point at
  which its subject exists), §6.1 (the circularity that forced the split).
- **Do:** step 1 captures `TCTL_PATH` under the **installed** environment; step 2
  re-invokes that **absolute path** under a PATH that excludes `~/.cargo/bin` and
  the mise shims. The capture is the load-bearing step: `tctl` is reached in step 2
  by absolute path *precisely because* it is not on the PATH step 2 constructs.
  - **An empty `TCTL_PATH` is a harness error, not an RC-2 observation** — it means
    step 1 failed to find the binary the scenario claims to have installed. Raise it
    as such.
  - Failure is **exit 30**, the same phase code as N1: both are the negative probe,
    and an operator reading the code should not have to know which half fired. The
    message says which.
  - **Departure from DOC-1, already recorded (§6.3):** DOC-1 §4.2 describes one
    probe; DOC-2 specifies two, because the single-probe formulation is not
    executable — at DOC-1's position the subject of the probe does not yet exist.
    DOC-1's actual *requirement* is fully preserved. Do not "simplify" this back.
- **Acceptance:** the run log records N2 with a non-zero exit, **empty stdout**,
  and non-empty stderr containing a cargo-related token; `TCTL_PATH` is logged and
  is non-empty.
- **Depends:** P5-T2

### P5-T4 — `verify_binaries` and `verify_single_install`

- **Files:** modify `vmtest-harness/lib/verify.sh`.
- **Contract:** DOC-2 §12.2 (both signatures, both die **60**), §9.3
  (`expect_<pattern>` columns); DOC-1 §7.4 (the Single-Install Convention gate),
  §7.5 (pattern-aware by construction).
- **Do:** `verify_binaries` iterates `in_scope=yes` rows and asserts
  present/absent per the pattern column. `verify_single_install <package>` asserts
  that **every** binary of that package is present, not merely one.
  - The two functions are separate **on purpose**: §7.4's gate is specifically that
    installing a parent yields *all* its sidecars, and stating it as its own
    function makes the failure message say *"trusty-memory installed but sidecar
    trusty-memory-mcp-bridge is missing"* rather than *"a binary is missing"*.
    Given that the third `trusty-memory` sidecar was dropped from DOC-1's original
    seed table, this gate earns its separate existence.
  - `trusty-mpm` gets a `verify_single_install` call too — it ships **two**
    binaries, `tm` and `trusty-mpm`, and under the D2 reversal both are expected
    present under every pattern (§A.1). **This is no longer a plan-level judgment
    call: DOC-2 §12.5's skeleton was amended at source on 2026-07-31 and now
    carries the fourth call itself.** The rule the amendment states is the one to
    implement — *every multi-binary in-scope package gets a call*, and there are
    four of them.
- **Acceptance:** `verify_binaries` logs `N/N present` where N is the count of
  `in_scope=yes` rows (**13** today); one `verify_single_install` call passes per
  **multi-binary** in-scope package (**4** today — `trusty-search`,
  `trusty-memory`, `trusty-installer`, `trusty-mpm`); deliberately renaming
  `~/.cargo/bin/trusty-memory-mcp-bridge` in the guest makes the run exit **60**
  with the sidecar named in the message.
- **Depends:** P5-T3, P4-T4

### P5-T5 — `verify_stack_doctor`

- **Files:** modify `vmtest-harness/lib/verify.sh`.
- **Contract:** DOC-2 §1.1 (the full JSON shape, the field table, and the
  **pass predicate**), §12.2; DOC-1 §7.1 (**JSON only — never scrape
  human-readable text**).
- **Do:** run `tctl stack doctor --json` through `vm_exec`, parse **host-side**
  with `jq`, apply §1.1's per-member predicate. There are no `rename` or
  `skip_serializing_if` attributes on the struct, so field names serialise verbatim
  and `None` serialises as `null` — **not** as an absent key — and the oracle may
  address every field unconditionally.
  - **`stale` is accepted, `down` is not.** On a freshly installed VM where daemons
    have just been bootstrapped, a stale heartbeat is expected timing, not a
    packaging defect; the harness's claim is that *installation* succeeded. `down`
    and `unknown` do refute that and fail. (§1.1, a labelled judgment call.)
  - **Do not use `tctl stack doctor`'s own exit code as the assertion.** It exits 0
    on `ok`, 2 on `degraded`, 3 on unknown member, 1 on JSON write failure. Read the
    JSON on stdout and apply the predicate. This is the same principle as DOC-1
    §8.1's "a tart exit code is not a completion signal", applied to a different
    tool. Log `verdict` for the human; do not assert on it.
  - **Do not reach for `tctl stack health --json`** because the name reads better:
    it has a narrower shape and a **different verdict vocabulary** (`ready` |
    `degraded` versus doctor's `ok` | `degraded`).
- **Acceptance:** the run log shows the parsed member list with eight packages, all
  satisfying the predicate, `trusty-mpm` among them; and the `verdict` value logged
  but not asserted.
- **Depends:** P5-T4

### P5-T6 — `verify_versions`

- **Files:** modify `vmtest-harness/lib/verify.sh`.
- **Contract:** DOC-2 §1.2 (the literal shape, the three properties, the pass
  predicate **and its 2026-07-31 amendment**), §9.1's matching note, §12.2.
  **§F-2 was RESOLVED at source:** the predicate's last clause no longer names
  `tsv_version(...)` — it compares against `source_tree_version(trusty-installer)`,
  read with `cargo metadata --no-deps --format-version 1` in the guest at
  `$VMTEST_GUEST_SRC` and parsed host-side with `jq`. There is no version column in
  `expected-binaries.tsv` and none is to be added.
- **Do:** assert `tool_version` non-empty, `stack_version` present and non-empty
  **only**, and `contract_floor`/`contract_target` integers with `floor <= target`.
  - `tool` is hardcoded `"trusty-installer"` **even when the binary is invoked as
    `tctl`** — asserting `tool == "tctl"` would fail always.
  - `stack_version` is a Phase-0 placeholder constant `PHASE0_STACK_VERSION =
    "0.0.0-scaffold"` (`crates/trusty-installer/src/commands/version.rs:28`). The
    field is stable; **its value is a stub.** Do not compare it against a release
    label until the real `stack_version` lands.
- **Acceptance:** the predicate passes under pattern (c); an injected
  `contract_floor > contract_target` fixture makes the run exit **60**.
- **Depends:** P5-T5

### P5-T7 — `verify_daemon_liveness` — and the RC-1 scoping statement

**RC-1 is a scoped-around dependency, not a blocker.** This task states that
explicitly, in code and in the MANIFEST.

- **Files:** modify `vmtest-harness/lib/verify.sh`.
- **Contract:** DOC-2 §1.3 (**RC-1**, the four independently-evolved shapes, and
  the **INTERIM** predicate), §1.4 (the contract-availability table), §10.1
  (daemon-health poll: 1 s interval, 60 s maximum, **wholly unmeasured**); DOC-1
  §8.7 (`launchctl bootstrap gui/$(id -u)` works under `tart exec` — no SSH, no GUI
  login), §7.1.
- **Do:** implement DOC-2 §1.3's **INTERIM** predicate and nothing stronger: HTTP
  200, body parses as JSON (`jq -e . >/dev/null`), `.status` a non-empty string,
  `.status` not one of `{"down","error","unhealthy"}`.
  - **State the scoping in a header comment on the function**, in these terms: the
    oracle asserts **liveness only** for daemon health because there is **no shared
    type in `trusty-common`** and no unified schema — four daemons, four shapes,
    no field in common beyond `status` and `version`; `trusty-mpm` has **two
    different `/health` endpoints on two different ports** (the supervisor's at
    `crates/trusty-mpm/src/supervisor/http.rs:50-53` returns `{"status":"ok"}` and
    is **not** the daemon health surface); and `trusty-review`'s MCP `review_health`
    is **not byte-identical** to its HTTP handler — the MCP tool rebuilds the
    payload as a hand-written `json!` literal
    (`crates/trusty-review/src/mcp/tools.rs:331-351`) that emits `detail`
    unconditionally where the HTTP struct omits it via `skip_serializing_if`, and
    nothing enforces they stay in sync. A strong assertion here would have to be
    **invented**, and DOC-1 §7.1's whole argument for JSON-only is that the oracle
    must not depend on surfaces free to change underneath it.
  - **What would change if RC-1 is ever resolved.** RC-1 asks for one type — the
    natural home is `trusty-common`, consumed by every daemon's `/health` handler —
    guaranteeing at minimum `{"status": "ok"|"degraded"|"down", "version":
    "<semver>", "daemon": "<crate-name>"}`, with per-daemon extras **under a nested
    object** so the envelope stays stable. On the day that lands, exactly three
    things change here and nothing else: the INTERIM predicate is replaced by an
    envelope assertion (`status ∈ {ok, degraded}`, `version` matching the installed
    version, `daemon` matching the expected crate name); DOC-2 §1.4's third row
    flips from "**No** — liveness only" to "Yes"; and the 60 s poll maximum stops
    being a guess because a real time-to-ready can be measured. **No scenario, no
    transport, and no other oracle function is affected.** That containment is why
    RC-1 is scoped around rather than waited on.
  - **See §F-7** — DOC-2 names neither the daemon start mechanism nor how the
    oracle discovers each daemon's port. The decision rule is there, and it
    includes an explicit *record-as-blocked-and-skip* branch, so this task cannot
    strand the phase.
- **Acceptance:** either the run log shows a liveness result per in-scope daemon
  (HTTP 200 + parseable JSON + acceptable `.status`), **or** the MANIFEST records
  `verify_daemon_liveness` as **BLOCKED** with the verbatim reason from §F-7 and
  the function present but returning 0 with a loud `SKIPPED (RC-1 / §F-7)` log
  line. A silent skip is not acceptable in either branch.
- **Depends:** P5-T6

### P5-T8 — Full pattern (c) run; record the first full-stack measurement

- **Files:** modify `vmtest-harness/scenarios/install-local.sh` (complete it to
  DOC-2 §12.5's full skeleton); modify `vmtest-harness/vmtest.defaults` (timeouts).
- **Contract:** DOC-2 §12.5 (the complete worked skeleton), §10.2 (watchdog table;
  "**It should be tightened once the first pattern-(c) full-stack run is timed**");
  DOC-1 §9 ("The first pattern-(c) full-stack run should be recorded as the
  replacement measurement"), D4's recorded decision ("the first successful
  pattern-(c) run must be recorded as the replacement measurement").
- **Do:** complete the scenario to §12.5 exactly — deliver, install each in-scope
  crate, N2, then the six verifications. Run it. Time it.
  - **The 4–8 minute full-stack figure is an extrapolation, computed for six
    crates against what is now an eight-crate scope, and explicitly lower-confidence
    since the D2 and D3 amendments each widened it without re-deriving it.** Do not treat your measured number as a confirmation
    or a refutation of it — it *replaces* it.
  - **Assert the install loop ran exactly once per emitted `crate_dir`.** The
    scenario counts its own canonical `install_from_path` log lines and requires
    equality with the helper's output — the run-level counterpart to P4-T4's
    host-level tripwire, catching an undedupe that enters *between* the helper and
    the loop (a `for` rewritten over rows instead of directories, a retry that
    re-enters, a second install block added for an upgrade scenario and left in):

    ```sh
    _expected=$(tsv_scope_crate_dirs | wc -l | tr -d ' ')
    _actual=$(grep -c '^vmtest: install_from_path ' "$VMTEST_RUNDIR/run.log")
    [ "$_actual" = "$_expected" ] || die 60 "install ran $_actual times, expected $_expected (one per crate_dir)"
    printf '%s\n' "$_installed_dirs" | sort | uniq -d | grep -q . && die 60 "a crate_dir was installed twice"
    ```

    Two lines of substance on top of P4-T4's two. Note what this does **not** claim
    to be: it is a **loudness** guarantee, not a correctness one. Per §F-3 as
    corrected, an undeduped loop does not produce a wrong end state — it produces a
    count mismatch, which is precisely what this makes fail fast and by name instead
    of hiding in minutes of duplicate build output.
  - Then tighten the `install_timeout` (currently 2700 s, **~5.6× a low-confidence
    estimate**) to a value grounded in your measurement. DOC-2 §10.2's reasoning is
    the constraint: a tight timeout over a low-confidence estimate does not enforce
    a budget, it manufactures flaky failures that get "fixed" by raising the
    timeout. Leave generous headroom and say what multiple you chose.
- **Acceptance:** the phase checkpoint's six conditions, verbatim, plus a logged
  total wall clock recorded in the MANIFEST.
- **Depends:** P5-T7

### P5-T9 — Update the MANIFEST

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** paste the full checkpoint output; record the full-stack wall clock, the
  RC-2 observation from P5-T2, and the RC-1 disposition from P5-T7 as
  Measurements; record every deviation.
- **Acceptance:** Phase 5 `Observed result` contains the parsed `stack doctor`
  member list showing `trusty-mpm` present, and `Measurements` contains the
  full-stack wall clock and the RC-2 exit code.
- **Depends:** P5-T8

---

## PHASE 6 — Pattern (b): branch

**Goal:** `vmtest run branch` — guest-side `git clone`, checkout, `cargo install
--path`. **No new infrastructure**, per DOC-1 §10 step 2.

**Checkpoint — PASS CONDITION.**

> `vmtest run branch` **exits 0** with the **same derived binary and package
> assertions as Phase 5** — N/N where N is the count of `in_scope=yes` rows
> (**13** today), over `tsv_scope_packages`' values (**8** today) — and the run log
> shows a
> guest-side `git clone` (no host→guest byte stream) and the checked-out branch
> name.

### P6-T1 — `source_deliver_branch`

- **Files:** modify `vmtest-harness/lib/source.sh`.
- **Contract:** DOC-2 §12.2 (`source_deliver_branch <vm_name> <repo_url> <branch>
  <guest_dir>`, dies 50), §10.2 (guest `git clone` watchdog **300 s**, grounded in
  the measured `GIT_CLONE_MS=50131`), §8.2 (`repo_url`, `default_branch`); DOC-1
  §6.2.
- **Do:** the guest clones directly — **the repo is public, so no credential
  plumbing is needed**, and **no host→guest source transfer occurs; the host
  repository is not read at all** under this pattern. Check out the target branch.
- **Acceptance:** the run log shows the clone duration and the resolved commit SHA;
  `grep -c 'streamed' ` in the log is **0** (nothing was streamed); the guest tree
  exists at `guest_src_dir`.
- **Depends:** P5-T9

### P6-T2 — `scenarios/install-branch.sh`

- **Files:** create `vmtest-harness/scenarios/install-branch.sh`.
- **Contract:** DOC-2 §12.5 (same shape, different step 1), §12.1, §12.4; DOC-1
  §3.6, §6.5 (**`tctl install` MUST NOT be used in patterns (b) or (c)** — the
  prohibition applies here identically).
- **Do:** copy the pattern-(c) scenario, replace step 1 with
  `source_deliver_branch`, and pass `b` to every `verify_*` call. **This is the
  proof that the scenario abstraction holds:** if this file needs anything other
  than a different step 1 and a different pattern letter, the abstraction leaked
  and that is a finding to record.
- **Acceptance:** `diff` between the two scenario files shows differences **only**
  in the function name, step 1, and the pattern letter.
- **Depends:** P6-T1

### P6-T3 — Branch selection

- **Files:** none (documentation of the mechanism in the scenario header comment).
- **Contract:** DOC-2 §8.2 (mechanical override mapping: uppercase the key, prefix
  `VMTEST_`; **CLI flags exist only for the five listed**).
- **Do:** the branch under test is selected by `VMTEST_DEFAULT_BRANCH=<branch>`,
  derived mechanically from the `default_branch` key. **Do not add a `--branch`
  flag** — §8.2 is explicit that adding a flag per tunable would give the driver a
  surface larger than its behaviour, and the mechanical mapping already covers this
  case without a table to maintain.
- **Acceptance:** `VMTEST_DEFAULT_BRANCH=main vmtest run branch --dry-run` reports
  `default_branch main (env)` in the effective-configuration banner.
- **Depends:** P6-T2

### P6-T4 — Run the checkpoint

- **Files:** none.
- **Contract:** DOC-2 §1.1, §9.3; DOC-1 §7.5.
- **Do:** run `vmtest run branch` end to end. Record the wall clock and compare it
  to Phase 5's — the delta is approximately the difference between the streamed
  transport and a guest-side clone (measured 50.131 s), which is itself worth
  recording since it is the first side-by-side comparison of the two transports.
- **Acceptance:** the phase checkpoint, verbatim.
- **Depends:** P6-T3

### P6-T5 — Update the MANIFEST

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** state, observed result, files delivered, deviations, and the
  transport-comparison measurement.
- **Acceptance:** Phase 6 `Observed result` includes the clone SHA and the total
  wall clock alongside Phase 5's for comparison.
- **Depends:** P6-T4

---

## PHASE 7 — Pattern (a): released

**Goal:** `vmtest run released` — `cargo install <package> --locked` from
crates.io for **all eight** crates. **Adds a scenario only**, per DOC-1 §10 step 3
as amended.

**This is where the D2/D3 reversal is proved.** Under the superseded D2 this
pattern covered six crates and asserted `tm` known-absent. It now covers **eight** —
seven after the D2 reversal, eight after the D3 amendment added `trusty-review` —
and asserts `tm` **present**. A run that does not find `tm` is a **failure**.

**Checkpoint — PASS CONDITION.**

> `vmtest run released` **exits 0**, and the run log shows one
> `cargo install <pkg> --locked` invocation per value of `tsv_scope_packages`
> (**8** today) — including **`cargo install tga --locked`**, **`cargo install
> trusty-mpm --locked`** and **`cargo install trusty-review --locked`** — followed
> by `verify_binaries` reporting **N/N present**, where N is the count of
> `in_scope=yes` rows (**13** today), with `tm` and `trusty-mpm` explicitly among
> them, and `tctl stack doctor --json` reporting `trusty-mpm` as installed.

**Carried into Phase 7 from DOC-2 §1.1a — two assertion candidates, NEITHER
implemented before Phase 7 runs.** Both are recorded so they are not lost and not
smuggled in early; asserting either before a pattern-(a) run has been observed
would be inventing a contract, which is what §1.1a exists to stop.

1. **Pattern (a) may assert daemon health more strictly.** Under (a) the harness
   is permitted `tctl install`, whose service-bootstrap step **actually starts the
   daemons**, so a real `healthy`/`stale` is reachable and §1.1a's cause (c) does
   not apply. `H_P` is already pattern-gated to `{b, c}`, so (a) inherits the
   strict form with no edit. Confirm against the first observed pattern-(a) run
   before tightening anything further.
2. **NEW (logged 2026-08-03) — assert `plist_installed == false` DIRECTLY under
   patterns (b) and (c).** Under those patterns it is a **derivable invariant**:
   DOC-1 §6.5 bans `plans_service_bootstrap` (`install.rs:528`), no bootstrap
   runs, therefore no plist is written. Asserting it directly would **fail closed
   if `tctl install` ever leaked into a source-install scenario** — precisely the
   false pass §6.5 bans that step to prevent, and which nothing in today's oracle
   detects.
   - **This is a NEW assertion, NOT a widening of the health predicate.** It does
     not touch `H_P`, does not relax any clause, and is independent of the
     `down`-acceptance. Do not implement it by editing the health predicate.
   - **Why it belongs here and not in Phase 5:** DOC-2 §1.1a Consequence 1 shows
     the `plist_installed == false` guard is **inert** under (b)/(c) as currently
     used — it can never be `true`, so the fail-closed branch it promises never
     fires. This assertion is the productive use of that otherwise-dead signal,
     and it is a scope addition, so it waits for an owner decision rather than
     riding in on a re-run.

### P7-T1 — `install_from_registry`

- **Files:** modify `vmtest-harness/lib/source.sh`.
- **Contract:** DOC-2 §12.2 (`install_from_registry <vm_name> <package>
  [version]`, dies 50), §9.2 (`[package] name` is the key, and it is *"what `cargo
  install <name> --locked` takes, which is pattern (a)'s entire interface"*);
  DOC-1 §6.3, D1, D3.
- **Do:** `cargo install <package> --locked`. **`--locked` is mandatory** — it is
  what makes the run reproducible against the published lockfile rather than
  against whatever the resolver feels like today.
  - **`trusty-git-analytics` publishes as `tga`.** The install command is `cargo
    install tga --locked`. Drive the package list from
    `tsv_scope_packages` (P4-T4), **not** from directory names — that is exactly
    the discontinuity DOC-1 D3 warns about, and keying on package name is why the
    TSV is shaped the way it is.
  - **Pattern (a) means crates.io and nothing else** (DOC-1 D1). `install.sh` and
    prebuilt release tarballs are out of scope; the crates.io path is the only one
    grounded in measurement (`cargo install tga --locked`, 131 s, 211 deps, 4 vCPU).
- **Acceptance:** the run log shows one `cargo install <pkg> --locked` line per
  value of `tsv_scope_packages` (**8** today), and the set of package names is
  **exactly** `tsv_scope_packages` — no more, no fewer, none repeated — with `tga`
  among them. Assert against the helper's output, not against a literal list: the
  scope has changed twice already (§A.1, §A.1b), and a hardcoded list is the thing
  that silently fails to change with it.
- **Depends:** P6-T5

### P7-T2 — `scenarios/install-released.sh`

- **Files:** create `vmtest-harness/scenarios/install-released.sh`.
- **Contract:** DOC-2 §12.2 (`source_deliver_released` — "no-op returning 0;
  pattern (a) has no delivery step; **exists so scenarios stay symmetric**"),
  §12.5; DOC-1 §3.6, §6.3.
- **Do:** step 1 calls the no-op; steps 2–4 install from the registry and verify
  with pattern letter `a`. Keep the call to `source_deliver_released` even though
  it does nothing — symmetry across the three scenario files is what makes the
  upgrade-testing extension (DOC-1 §12.1) *"two install steps in one scenario file,
  and not a new mechanism"*.
  - Even though `tctl install` would, in pattern (a) alone, do roughly what this
    pattern specifies, **the harness invokes `cargo install` directly** so that all
    three patterns share one install mechanism and differ **only in source**
    (DOC-1 §6.5).
- **Acceptance:** the three scenario files differ only in name, step 1, and pattern
  letter; `vmtest run released` dispatches to this file.
- **Depends:** P7-T1

### P7-T3 — Pattern-(a) relaxation in `verify_versions`

- **Files:** modify `vmtest-harness/lib/verify.sh`.
- **Contract:** DOC-2 §1.2 (`tool_version` asserted equal to
  `source_tree_version(trusty-installer)` under patterns (b)/(c); **asserted merely
  present under pattern (a), where the published version legitimately differs from
  the working tree**). §1.2's 2026-07-31 amendment states the pattern-(a) case
  directly: there is no source tree to compare against, so there is no comparison.
- **Do:** gate the equality clause on `pattern ∈ {b, c}`. This is the one place
  the oracle is genuinely pattern-aware today, and it is a real difference, not a
  vestige.
- **Acceptance:** with a working tree whose `trusty-installer` version differs from
  the published one, `vmtest run released` still passes `verify_versions`, while
  `vmtest run local` would fail if the equality clause were applied.
- **Depends:** P7-T2

### P7-T4 — Run the checkpoint and prove the reversal

- **Files:** none.
- **Contract:** DOC-1 D2 (as amended), D3, §7.5 (as amended); DOC-2 §9.5.
- **Do:** run `vmtest run released`. Explicitly confirm in the log that `tm` and
  `trusty-mpm` are **present**, and that `tctl stack doctor --json` reports
  `trusty-mpm` with `on_path == true` and `version != null`.
  - If `cargo install trusty-mpm --locked` fails because the crate is **not** on
    crates.io, that contradicts two independent pieces of evidence (`cargo search
    trusty-mpm` returning `1.0.2`, and a manifest with no `publish` key). **Record
    it verbatim and stop** — it would mean D2 was reversed on a bad reading, which
    is a design-level finding, not a harness bug to work around.
- **Acceptance:** the phase checkpoint, verbatim.
- **Depends:** P7-T3

### P7-T5 — Update the MANIFEST

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** paste the checkpoint output, with the `tm` / `trusty-mpm` lines called
  out as the evidence that closes the D2 reversal loop.
- **Acceptance:** Phase 7 `Observed result` contains the `cargo install trusty-mpm
  --locked` line and the `stack doctor` member entry for `trusty-mpm`.
- **Depends:** P7-T4

---

## PHASE 8 — Hardening, documentation, and measurement write-back

**Goal:** close the loop — prove the isolation discipline holds, tighten what the
new measurements allow, document the harness for a human, and write the
measurements back into DOC-1/DOC-2 so the doc set stops carrying estimates it can
now replace.

**Checkpoint — PASS CONDITION.**

> All four hold: (i) the `~/.zshenv` deletion drill passes — every assertion still
> passes with the file removed mid-run; (ii) `vmtest.defaults` timeouts are
> grounded in Phase 5–7 measurements, each with a comment naming the measurement;
> (iii) `vmtest-harness/README.md` exists and a reader who has never seen the doc
> set can run `vmtest run local` from it alone; (iv) `git grep -n 'publish = false'
> docs/research/tart-vm-testing-harness/` returns **no** claim that `trusty-mpm` is
> unpublished.

### P8-T1 — The `~/.zshenv` deletion drill

- **Files:** none (a deliberate one-off run; record the result).
- **Contract:** DOC-2 §11.4 (*"If `~/.zshenv` were deleted from the guest mid-run,
  every harness assertion must still pass — that is the test of whether §7 was
  implemented correctly, and it is worth running once deliberately"*); DOC-1 §5.3.
- **Do:** run `vmtest run local` with a deliberate `rm ~/.zshenv` in the guest
  after provisioning and before the install steps.
- **Acceptance:** the run exits **0** with all assertions passing. If it does not,
  something reads an rc file, and §7's self-prefixing was **not** implemented
  correctly — fix that before anything else in this phase. This is the single
  highest-value hardening check in the plan, because the failure it catches (a
  missing dotfile presenting as "cargo is not installed", exit 127) is documented
  as having already broken a golden image once.
- **Depends:** P7-T5

### P8-T2 — Ground the timeouts in measurement

- **Files:** modify `vmtest-harness/vmtest.defaults`.
- **Contract:** DOC-2 §10.2 (watchdog table and its stated multiples), §10.1 (poll
  table), open items ("**Full-stack watchdog is 5.6× a low-confidence estimate** —
  tighten once the first pattern-(c) full-stack run is timed"; "**Daemon
  time-to-ready** — wholly unmeasured; the 60 s maximum is a guess").
- **Do:** replace estimate-derived values with measurement-derived ones, and put
  the measurement in a comment beside each. Where a value is still a guess — and
  the daemon-health 60 s maximum will still be one unless P5-T7 produced a real
  number — **say so in the comment**. A tunable whose comment claims a grounding it
  does not have is worse than an unlabelled guess.
- **Do NOT re-ground §10.1's boot-ready row — it is already done.**
  *(Noted 2026-08-02.)* MANIFEST Phase 3 recommended that P8-T2 re-ground that row
  on Phase 3's four boot measurements rather than on the single 18.0 s reading at
  `vm-install-probe-findings.md:483`, which did not reproduce. **That amendment
  was made at source instead**, in this PR: §10.1's boot row now cites both the
  original research figures and the four Phase 3 observations (24 s, 28 s, 33 s,
  33 s) and states that the 150 s maximum is sized against the slowest observed
  boot. The maximum is **unchanged** and no new one was invented. This task's
  remaining scope is the watchdog tier and the daemon-health row.
- **Acceptance:** every timeout key in `vmtest.defaults` carries a comment that is
  either a `file:line` measurement citation or the literal word `judgment call`.
- **Depends:** P8-T1

### P8-T3 — `vmtest-harness/README.md`

- **Files:** create `vmtest-harness/README.md`.
- **Contract:** DOC-1 §3.1 (driver surface), §2 (placement rationale), §11
  (isolation guarantee), §13 (non-goals); DOC-2 §2 (exit codes), §3.4 (**the
  pin-roll procedure — reproduce its six steps**), §5 (`clean`).
- **Do:** document the three subcommands, the exit-code table, the config tiers,
  the pin-roll procedure, and — prominently — the two rules a future contributor is
  most likely to break: **`lib/vm.sh` is the only file that may contain `tart`**,
  and **the host repo is never mounted in either direction**.
  - Reproduce §3.4's rule that **a pin roll is a deliberate act with its own PR and
    is never a repair step inside a failing run**, and that **all three scenarios
    must be green against the candidate before the PR opens** — a roll validated
    against one pattern is not validated.
  - Include §3.4 step 5: re-verify §11's preinstalled-tool assumptions explicitly.
    A new base image is precisely where a preinstalled `mise` could move,
    disappear, or gain a second copy.
  - Record the **microphone TCC caveat** (DOC-1 §14): `kTCCServiceAudioCapture`
    fires **on VM start even with `--no-graphics`** — a property of
    Virtualization.framework, not of Tart and not of this harness — and **all** TCC
    observations in the research are conditional on having been run from iTerm2, by
    one user, on one machine. A LaunchAgent, a cron job, or a different terminal is
    a **different responsible process and may prompt**. The harness cannot promise
    unattended operation in a launch context that has not previously been granted.
- **Acceptance:** a reader following only the README can run `vmtest run local`
  successfully on a clean machine (test this by following it literally, not from
  memory).
- **Depends:** P8-T2

### P8-T4 — Write the measurements back into the doc set

- **Files:** modify `docs/research/tart-vm-testing-harness/02-design/01-vm-install-harness.md`
  (§9, §14) and `.../02-design/02-harness-contracts.md` (open items, §10.2).
- **Contract:** DOC-1 §9 (*"The first pattern-(c) full-stack run should be recorded
  as the replacement measurement"*), §14 (the transport gap); DOC-2 open items.
- **Do:** as **amendments in the doc set's established style** — dated, stating
  what changed and why, never a silent edit (*"a design whose decisions quietly
  change is a design nobody can audit"*): replace the 4–8 min extrapolation with
  the measured full-stack time; close DOC-1 §14's tar-transport gap with the
  Phase 1 and Phase 5 evidence; close the "full base-image digest" open item with
  P1-T3's value; update RC-2's status with P5-T2's observation. **Leave RC-1
  open** — it is not this plan's to close.
- **Acceptance:** `git diff` on the two design docs shows dated amendment
  blockquotes, and no measurement in DOC-1 §9 is labelled EXTRAPOLATION that now
  has a real number.
- **Depends:** P8-T3

### P8-T5 — Verify the `02-design/README.md` summary is still correct

- **Files:** none expected.
- **Contract:** DOC-1 D2 (as amended), D3; DOC-2 §9.5.
- **Do:** **this task was completed at source on 2026-07-31 (§F-8) and is now a
  verification check, not an edit.** `02-design/README.md` ("The short version")
  previously read *"Seven crates in scope; `trusty-mpm` is a documented gap in
  pattern (a) only (`publish = false`)"* — the superseded premise, surviving in the
  index because the reversal amended DOC-1 and DOC-2 but not their README. It now
  states that all three patterns cover all **eight** crates (D3 was widened to
  include `trusty-review` on 2026-07-31, §A.1b) and that `trusty-mpm` is published at
  v1.0.2 and `trusty-review` at v0.10.1. Confirm that is still what it says, and that nothing added
  during Phases 1–8 reintroduced the old claim anywhere in the doc set. **If it is
  already correct, this task delivers no diff — that is the expected outcome, not a
  skipped task.**
- **Acceptance:** `git grep -n 'publish = false'
  docs/research/tart-vm-testing-harness/` returns no line claiming `trusty-mpm` is
  unpublished; the README's short version says **eight** crates in all three
  patterns, and names both `trusty-mpm` and `trusty-review`.
- **Depends:** —

### P8-T6 — Update the MANIFEST (final)

- **Files:** modify `docs/research/tart-vm-testing-harness/03-plan/MANIFEST.md`.
- **Contract:** MANIFEST.md §Schema.
- **Do:** record Phase 8, then add a closing `Plan status` line stating whether all
  eight phases are complete and listing every item still open (RC-1 at minimum, and
  RC-2 if P5-T2 left it open).
- **Acceptance:** every phase row in the summary table has a non-`not-started`
  state; the `Plan status` line names RC-1 explicitly.
- **Depends:** P8-T4, P8-T5

---

## F. FLAGGED — where DOC-2 is under-specified

DOC-2 is unusually complete: twelve numbered contracts, a traceability table back
to DOC-1, and its own open-items list. The items below are what it nonetheless
leaves an implementing engineer to decide, found by walking every task in this
plan and asking *"could a zero-context engineer execute this from DOC-2 alone?"*.

**None of these is filled in with an invented contract.** Each gets a **decision
rule** — a rule that resolves the gap by *observation* or by *the narrowest reading
of what DOC-2 already says*, never by taste — plus a requirement to record the
resolution in the MANIFEST. Where a gap cannot be resolved by observation, the
decision rule says **stop and record**, not **choose something**.

> Honest uncertainty is this doc set's established register. A plan that silently
> filled these gaps would read more confident and be worth less.

> ### §F RECONCILIATION — the audit of all ten, 2026-08-04 (plan P8-T6)
>
> **The count below is STALE and is corrected here.** It says *"Four are RESOLVED …
> Six remain open"*, which was true on 2026-07-31 and describes only the four that
> were resolved **by amending a document**. It has counted nothing since. Phases 1–7
> then resolved the rest **by executing them** — which was always the intended
> mechanism for the six that carry a *decision rule* rather than an amendment; a
> decision rule is resolved when the code lands and the MANIFEST records it, not
> when someone edits §F.
>
> **All ten are now settled. Nothing in §F is outstanding.** One of the ten was
> settled *and found to contain its own factual error*, which is recorded rather
> than smoothed over.
>
> | Item | Status | Settled where | Note |
> |---|---|---|---|
> | **§F-1** `run --dry-run` undefined | **RESOLVED** | **P2-T7**, by the narrowest reading, implemented | The driver runs preflight + banner + acquire/release, then halts **before the clone**, and logs `plan §F-1` by name. Phase 2's checkpoint is that path. |
> | **§F-2** §1.2 predicate cited a TSV column §9.1 does not define | **RESOLVED** | **at source, 2026-07-31**, by amending DOC-2 | Already marked below. |
> | **§F-3** thirteen in-scope rows, eight crate directories | **RESOLVED**; decision unchanged, **rationale corrected** 2026-07-31 | **at source**, then **empirically confirmed at P4-T3** | P4-T3 found **NO DRIFT** (28 rows == 28 targets; 13 in scope; 8 directories; 8 packages) and the run-level tripwire `install_assert_install_count` now fails closed if the dedupe is ever lost between the helper and the loop. |
> | **§F-4** negative probes have no assigned module | **RESOLVED** | **P3-T1 / P5-T3**, by the narrowest reading, implemented | Both `negative_probe_n1` and `negative_probe_n2` live in `lib/verify.sh` and die **30**, not 60. No fifth `lib/` module was added. MANIFEST Phase 3 Deviations. |
> | **§F-5** no module owns the TSV reader | **RESOLVED** | **P2-T2**, by the narrowest reading, implemented | `tsv_field` / `tsv_get` / `tsv_validate_keys` live in the **driver**, above the point where `lib/` is sourced, so they are shell-global to every module. One parser, three files, as §3.1 promised. `lib/vm.sh`'s header records it. |
> | **§F-6** scenario dispatch unspecified | **RESOLVED** | **P3-T5**, by the only mapping consistent with both docs | `vmtest run <p>` → `scenarios/install-<p>.sh` :: `scenario_install_<p>()`; unknown pattern is **exit 2** before any VM work. `scenario_dispatch` cites `plan §F-6`. Proved three times over: (c), (b) and (a) each needed **only** a new scenario file. |
> | **§F-7** daemon start and port discovery | **RESOLVED by step 2** — and **§F-7's own transcription of the product was WRONG** | **P5-T7**; the error corrected at source 2026-08-03 | Both machine-readable surfaces exist (`tctl start --json`, `tctl port <m> --json-port`), so the **BLOCKED branch was not taken**. But §F-7 recorded `--json-port` as emitting `{"addr":"host:port","port":N}` and **it does not**: `.addr` is the **host alone**. An oracle built on that reading composed `http://127.0.0.1/health` with no port and got HTTP 000 from all four daemons. The address is composed from **both** fields. The harness adapted; `port.rs` is correct and unchanged. |
> | **§F-8** design README carried the superseded D2 premise | **RESOLVED** | **at source, 2026-07-31**; **re-verified at P8-T5, 2026-08-04** | Still correct — eight crates, all three patterns, both crates named. P8-T5 found **three further stale claims in the same file** (see Phase 8 Deviations) and fixed them; the D2 premise was not among them. |
> | **§F-9** nothing specified what initiates guest shutdown | **RESOLVED** | **at source, 2026-07-31**, by amending DOC-2 §12.2 | `vm_request_stop` is the only permitted initiator; a guest-side `shutdown -h now` is **forbidden**, not pending. |
> | **§F-10** five smaller gaps, (a)–(e) | **RESOLVED**, all five | (a),(b),(e) at P4/P5; (c) at P3-T6; (d) at P1 | (a) guest paths composed from `guest_home`; (b) install order is TSV row order; (c) `~/.zshenv` written, never depended on — **and drilled at P8-T1**; (d) `tart-run.pid` **reaped** after `vm_wait_for_stopped`, never killed; (e) an unexpected `stack doctor` member (`trusty-console`) is **LOGGED, NOT ASSERTED**, and prints on every run. |
>
> **What the audit says about §F as a device.** Ten flagged gaps, ten settled, and
> **not one was filled in with an invented contract** — §F's own rule. Six were
> settled by a decision rule that survived contact with a real run; four needed the
> document amended. The one that went wrong (§F-7) went wrong in exactly the way
> this doc set keeps finding: a fact was **transcribed from source by reading**
> rather than by running it, and reading got the shape wrong. That is §F-3's
> lesson, restated by a different item, one phase later.

**Ten items were flagged. Four — §F-2, §F-3, §F-8, §F-9 — are RESOLVED, each on
2026-07-31, by amending DOC-2 and the design README rather than leaving them for the
executing engineer, because each was a defect with a determinable answer rather than
a genuine unknown. ~~Six remain open.~~** *(Count superseded 2026-08-04 — see the
reconciliation above. **All ten are settled**; the six not listed in this sentence
were resolved by execution across Phases 1–7, which is how a decision rule is meant
to be resolved.)* §F-3 is the newest of the four and the odd one
out: its *decision* was right from the start and is unchanged, but its *rationale*
was factually false, and the correction is what closes it — see §F-3 for both the
false claim and the empirical result that disproves it. The resolved three are retained below with
their original statement of the problem and the amendment that closed it: this doc
set records reversals rather than making silent edits, and an engineer who reads a
stale copy of DOC-2 needs to be able to tell which is which.

---

### §F-1 — `run --dry-run` is listed but never defined

- **Where:** DOC-2 §8.2 lists `--dry-run` among the five CLI flags the driver
  accepts. DOC-2 §5.4 defines `clean --dry-run` precisely. **Nothing anywhere
  defines what `vmtest run <pattern> --dry-run` does.**
- **Why it matters:** this plan uses it as Phase 2's checkpoint, because it is the
  only way to exercise the entire host-side path without a guest.
- **Decision rule (narrowest reading):** `run --dry-run` performs **preflight**,
  prints the **effective-configuration banner** (§8.3), acquires and releases the
  run-registry entry (§4.3), and **stops before `tart clone`**. It creates no VM
  and touches no guest. This is the largest prefix of the run lifecycle that
  involves no VM, which is the only reading consistent with `clean --dry-run`
  ("full classification, no destruction"). **Do not extend it further** — a
  `--dry-run` that clones and boots is not a dry run.
- **Record:** MANIFEST Phase 2 Deviations, as `§F-1 resolved by narrowest reading`.

### §F-2 — §1.2's predicate referenced a TSV column that §9.1 does not define — **RESOLVED at source, 2026-07-31**

- **Where:** DOC-2 §1.2's pass predicate ended `(pattern ∈ {b,c}) → tool_version ==
  tsv_version(trusty-installer)`, and §1.2's prose said `tool_version` is *"asserted
  equal to the crate version in `expected-binaries.tsv`"*. But **§9.1's schema has
  nine columns and none of them is a version**, and §9.3's seed rows carry no
  version value. `tsv_version()` had no source.
- **Why it mattered:** it was a direct contradiction between two contract sections,
  not an omission — following either one literally broke the other.
- **Resolution — DOC-2 §1.2 amended, no tenth column.** The clause is restated as
  `tool_version == source_tree_version(trusty-installer)`, where the expected
  version is read with `cargo metadata --no-deps --format-version 1` **in the guest
  at `$VMTEST_GUEST_SRC`** and parsed host-side with `jq`. Reading the guest's tree
  rather than the host's is what makes the clause correct under pattern (b), whose
  clone is of `default_branch` and need not match the working tree. §9.1 carries a
  matching note so the column cannot be re-proposed. See **DOC-2 §1.2** (amendment
  block after the pass predicate) and **DOC-2 §9.1**.
- **What the engineer does now:** implement the amended predicate. There is no
  decision left to make and nothing to record as a deviation.

### §F-3 — The thirteen in-scope rows contain only eight distinct crate directories — **decision unchanged; rationale CORRECTED 2026-07-31**

- **Where:** DOC-2 §12.5's skeleton loops `for _dir in $(tsv_scope_crate_dirs)` —
  "column 2 where in_scope=yes" — and calls `install_from_path` once per value.
  There are **thirteen** such rows and **eight** distinct directories:
  `trusty-search` appears twice, `trusty-memory` three times, `trusty-installer`
  twice, `trusty-mpm` twice, and four crates once each.
- **On "DOC-2 never says to deduplicate" — technically true, materially
  misleading.** *(Corrected 2026-07-31.)* DOC-2 never uses the word, but it implies
  the requirement in **four** separate places, and an engineer reading any of them
  would arrive at it: §12.5's own loop comment says "Install each in-scope
  **crate**" (not *each row*, not *each binary*); §9.2 declares the composite key to
  be `(package, binary)` and `crate_dir` explicitly **not** a key, which is what
  makes repeats in that column expected rather than anomalous; §9.1 ties `crate_dir`
  to `cargo install --path`, which takes a directory; and DOC-1 §7.4 states the
  convention in terms of one install per crate. The gap is that no single sentence
  says it imperatively — worth flagging, but this is a **weak** flag, not a hole.
- **Why it matters — CORRECTED. The original reasoning here was false.** This entry
  previously claimed the undeduped loop "makes the Single-Install Convention gate
  (DOC-1 §7.4) meaningless — installing a crate once per sidecar cannot prove that
  installing it *once* yields all of them." **That is wrong, and it was disproven
  empirically.** Repeated `cargo install --path <dir>` reinstalls the package's
  **full binary set** every time — cargo prints `Replacing …` for **every** binary
  the package declares, exits 0, and on a freshness hit completes in ~0.02 s. The
  end state after three `trusty-memory` installs is **identical** to the end state
  after one: all three sidecars present, installed by a package-granular command.
  The gate's claim therefore **survives** an undeduped loop intact. Reasoning from
  a plausible-sounding mechanism instead of running the command is how the wrong
  rationale got written, and it is recorded rather than quietly swapped because a
  correct decision resting on a false premise is one refactor away from being
  reversed for the wrong reason.
- **What the undedupe actually costs.** Two things, both **loud**:
  1. **A P5-checkpoint mismatch.** The checkpoint requires one `cargo install
     --path` per `tsv_scope_crate_dirs` value; an undeduped loop emits thirteen
     install lines against eight directories and **fails the checkpoint by count**.
     P5-T8's tripwire makes that failure immediate and named.
  2. **Redundant `tart exec` round-trips** and minutes of confusing duplicate log
     output.
  A loud smell and a wasted round-trip — **not** a silent failure, and **not** a
  false pass. It is a real defect worth preventing; it is not the catastrophe the
  original text described.
- **The genuine hazard this flag should point at is a *per-binary* install.** The
  gate **is** defeated — silently, and with a green result — by `cargo install
  --path <dir> --bin <binary>`, which is what a "row-faithful" reading of the TSV
  invites. That installs each sidecar *by name*, so `verify_binaries` and every
  `verify_single_install` pass while nothing has tested the convention at all.
  **That is now explicitly prohibited** in DOC-2 §12.2 (amended 2026-07-31) and
  mirrored in P5-T1. Unlike the undedupe, it produces no count mismatch and no
  smell, and `--check-table` cannot catch it because the table is not what is wrong.
- **Decision rule (unchanged — it was correct):** deduplicate.
  `tsv_scope_crate_dirs` emits **unique** `crate_dir` values in first-appearance
  order, and each in-scope crate is installed exactly once. Only the justification
  changes: dedupe because the checkpoint counts installs and because thirteen
  installs of eight crates is waste and noise — **not** because the gate would
  otherwise be invalid.
- **Why this is no longer open.** Three things closed it, and none of them is a
  judgment the executing engineer has to make: DOC-2's four implicit statements are
  now catalogued above; the **real** bypass is prohibited at source (§12.2); and
  P4-T4's acceptance already catches an undeduped helper **on the host, before any
  VM boots**, with P5-T8 catching it again at run level. There is no decision left
  and nothing to record as a deviation.
- **Record:** nothing. *(Previously "MANIFEST Phase 4 Deviations" — no longer a
  deviation, because dedupe is what the plan specifies and the tripwires enforce
  it.)*

### §F-4 — The negative-probe functions have no assigned module

- **Where:** DOC-2 §12.5 calls `negative_probe_n2`, and §6.2 specifies both N1 and
  N2 in full. **§12.2's module surfaces list `vm.sh`, `provision.sh`, `source.sh`,
  `verify.sh` — and neither probe appears in any of the four tables.** DOC-1 §3's
  component tree has no fifth module.
- **Decision rule (narrowest reading):** put both in **`lib/verify.sh`**. They are
  assertions with pass predicates (§6.2 states both as `PASS iff …`), which is
  exactly what `verify.sh` is for (DOC-1 §3.5, "assertion oracle"). They die with
  **30**, not 60, because §2 classifies them as their own phase — that is a
  property of the exit code, not of the file. Adding a fifth `lib/` module would
  depart from DOC-1 §3's component tree, which DOC-2 §12.2 explicitly declines to
  do unilaterally in an analogous case (the `install.sh` naming tension).
- **Record:** MANIFEST Phase 3 Deviations.

### §F-5 — No module owns the TSV reader, and three files need it

- **Where:** DOC-2 §3.1 justifies the shared TSV format with *"one parser, three
  files… all read by the same handful of `awk` lines"*, and §8.1 repeats it. But
  §12.2's four module surfaces contain **no TSV or config function**, and §12.5's
  skeleton calls `tsv_scope_crate_dirs` and `log` without saying where either is
  defined.
- **Why it matters:** under bash 3.2 this parser **is** the harness's data
  structure layer (§Shell discipline: "the substitute for a hash"). It is not a
  detail.
- **Decision rule (narrowest reading):** define `conf_get`, `tsv_*`, `log`, and
  `die` in the **`vmtest` driver itself**, above the `lib/` sourcing. They are
  driver infrastructure, not OS-boundary, provisioning, transport, or assertion
  logic, so none of the four modules is their home; and DOC-2 §12.4 already places
  `die` in the driver by showing it outside any module table. `lib/` files may call
  them, since `set` and function definitions are shell-global by the time `lib/` is
  sourced.
- **Alternative, permitted, must be recorded:** a fifth `lib/tsv.sh`. It is a
  departure from DOC-1 §3's component tree and therefore needs a MANIFEST deviation
  entry, but it changes no scenario, for the same reason DOC-2 §12.2 gives about a
  future `lib/install.sh`: scenarios call functions, not files.
- **Record:** MANIFEST Phase 2 Deviations.

### §F-6 — Scenario dispatch is unspecified

- **Where:** DOC-2 §12.5 says the scenario file is *"sourced by the driver after
  provisioning"* and defines `scenario_install_local()`. **Nothing specifies how
  the driver maps the pattern argument (`local` | `branch` | `released`) to a file
  path and a function name.**
- **Decision rule (derivable, low stakes):** `vmtest run <p>` sources
  `vmtest-harness/scenarios/install-<p>.sh` and calls
  `scenario_install_<p>()`. Both names are already fixed by DOC-1 §3 (the file
  names) and DOC-2 §12.5 (the function name for `local`); the mapping is the only
  one consistent with both. An unknown pattern is **exit 2** (§2: "unknown scenario
  name").
- **Record:** noted here; a MANIFEST entry is not required unless you deviate.

### §F-7 — Daemon start and port discovery are unspecified (interacts with RC-1)

- **Where:** DOC-2 §1.3's INTERIM predicate says *"for each in-scope daemon d
  expected present under pattern P: `GET /health` returns HTTP 200"*. It does not
  say **which** daemons are in scope as daemons (the TSV's `in_scope` column marks
  *binaries*, not daemons), **how they are started**, **on what host/port**, or
  **how the port is discovered**. §10.1 gives a poll interval and maximum for
  "daemon health" and labels the maximum *"wholly unmeasured"*.
- **What the repo offers, as facts to read — not as a contract to assume:**
  `tctl stack doctor --json` reports a boolean `port_recorded` per member (§1.1)
  but **not the port value**; `tctl` has stack lifecycle subcommands dispatched at
  `crates/trusty-installer/src/main.rs:143` (`lifecycle::run_start`), `:147`
  (`run_stop`), `:151` (`run_restart`), and a `port` subcommand at `:171`
  (`port::run(member, addr, json_port, json)`); DOC-1 §8.7 confirms `launchctl
  bootstrap gui/$(id -u)` works under `tart exec`, which is what makes any of this
  viable at all.
- **Decision rule (observation, with an explicit stop branch):**
  1. Read `crates/trusty-installer/src/commands/port.rs` and `lifecycle.rs`.
     Determine whether a **machine-readable** (`--json`) start command and a
     **machine-readable** per-member port exist.
  2. **If both exist:** use them. Start via the JSON lifecycle command, discover
     each port via the JSON port command, and apply §1.3's INTERIM predicate.
     Record the exact commands in the MANIFEST.
  3. **If either does not:** **stop, and record `verify_daemon_liveness` as
     BLOCKED** with the verbatim reason. The function stays present, logs
     `SKIPPED (RC-1 / §F-7)` loudly, and returns 0. **Do not hardcode a port map**
     — that would be inventing the contract RC-1 exists to request, in the one
     place DOC-2 is most emphatic that the oracle must not depend on a surface free
     to change underneath it.
  4. Either way, the phase is **not** blocked: RC-1 is a scoped-around dependency
     (P5-T7), and DOC-1's headline claim — installation succeeds, thirteen binaries
     land, `stack doctor` is healthy — does not rest on daemon health.
- **Record:** MANIFEST Phase 5 Deviations **and** Measurements.

### §F-8 — `02-design/README.md` carried the superseded D2 premise — **RESOLVED at source, 2026-07-31**

- **Where:** `docs/research/tart-vm-testing-harness/02-design/README.md`, "The
  short version": *"Seven crates in scope; `trusty-mpm` is a documented gap in
  pattern (a) only (`publish = false`)."*
- **Why it mattered:** the 2026-07-31 reversal amended DOC-1 and DOC-2 but not their
  index. The index is the **first** thing a zero-context engineer reads, and it
  stated as fact the exact premise DOC-1 D2 calls *"wrong"* in both halves.
- **Resolution — the README bullet is corrected.** It read, at the time of this
  resolution, that all three patterns cover all **seven** crates, that `trusty-mpm`
  is published at **v1.0.2**, and that the "documented gap" is dissolved. **The same
  day's D3 amendment then took the scope to eight** (`trusty-review`, §A.1b), and
  the README was updated again to match. See **`../02-design/README.md`**, "The
  short version" — which now says **eight**.
- **What the engineer does now:** nothing. **P8-T5** is reduced to a verification
  check.

### §F-9 — Nothing specified what *initiates* guest shutdown — **RESOLVED at source, 2026-07-31**

- **Where:** DOC-2 §Shell discipline's cleanup rule step 5 said *"`vm_wait_for_stopped`
  then `vm_delete`, in that order, always. Never a bare `tart stop`."* §12.2's
  `vm_wait_for_stopped` **polls** `tart list` for state `stopped`. DOC-1 §4.3's
  sequence shows `wait_for_stopped()` then `tart delete`. **No function in the
  contract issued a shutdown**, so a poll for `stopped` on a guest nobody asked to
  stop would run out its 120 s budget and exit **70** — on every run, including
  successful ones.
- **Resolution — DOC-2 §12.2 gains `vm_request_stop <vm_name>`.** It lives in
  `lib/vm.sh` (the only file permitted to contain `tart`), flushes the guest with
  `sync; sync` over `tart exec` — non-fatal on failure — then issues `tart stop`
  and **discards its exit code entirely**, always returning 0. That is **steps 1
  and 3** of the research's four-step procedure
  (`../01-research/vm-install-probe-findings.md:820-831`); the `echo FLUSHED`
  confirmation, the 10 s settle, and the clone→boot→assert verification are
  deliberately dropped, and DOC-2 §12.2 records why each is safe to drop here.
  The cleanup ordering is now **`vm_request_stop` → `vm_wait_for_stopped` →
  `vm_delete`**, all three skipped under `--keep`. DOC-1 §8.1 is not violated: it
  forbids issuing a bare `tart stop` *and treating its return as completion*, and
  the poll remains the completion signal. On failure there is **no escalation** —
  one attempt, then exit 70 and leave the VM for a human, because force-killing
  `tart run` is repairing, which DOC-1 §4.1 forbids. See **DOC-2 §12.2**
  (`vm_request_stop`, and the note beneath the module tables) and **§Shell
  discipline**, cleanup properties 4 and 5.
- **The guest-side alternative is forbidden, not pending.** *(Product-owner
  decision, 2026-07-31.)* A *guest-side* `shutdown -h now` over `tart exec` looks
  like the more obviously graceful initiator, and DOC-2 originally flagged the
  choice as a judgment call to validate on the first real run. DOC-2 §12.2 now
  **prohibits it outright**: its only appearance in the corpus is the superseded
  Track A script (`../01-research/vm-install-testing-trackA-fable.md:299`), issued
  over **SSH**, which DOC-1 §5.1 excludes as a transport, and it requires
  passwordless `sudo` in the guest, which was never measured. `vm_request_stop` is
  the only permitted initiator. **Nothing is left open here for the engineer to
  decide.**
- **Record:** MANIFEST Phase 1 Measurements — the observed teardown behaviour from
  the first real run, pasted verbatim. This is now an observation to capture, not a
  decision to make.

### §F-10 — Smaller gaps, resolved by narrowest reading; recorded for completeness

| # | Gap | DOC-2 § | Decision rule |
|---|---|---|---|
| a | N2's example hardcodes `/Users/admin/.cargo/bin` while §8.2 makes `guest_home` a tunable | §6.2 vs §8.2 | Compose the probe's PATH from `guest_home`, not the literal. The literal is illustrative; the tunable is normative. |
| b | Install **order** across the eight crates is unstated | §12.5 | Use TSV row order. Under a shared `CARGO_TARGET_DIR` order is performance-neutral, but `tctl` must exist before N2, and `trusty-installer` precedes N2 in row order already. |
| c | `provision.sh` "may" write `~/.zshenv` — optional, no rule for choosing | §11.4 | Write it. It is measured at 617 ms, it makes a `--keep` VM inspectable, and P8-T1's drill proves nothing depends on it. |
| d | `vm_boot` writes `tart-run.pid`; nothing says who reaps it | §12.2 | Cleanup does not kill it. The VM stopping is what ends `tart run`; killing the host process is the write-loss hazard by another route. Reap after `vm_wait_for_stopped` returns. |
| e | Which daemons `stack doctor` will list under a fresh install is not stated | §1.1 | Assert only over `tsv_scope_packages`' values (eight today). A member `stack doctor` reports that the TSV does not know about is **logged, not asserted** — it is a `--check-table` finding, not a run failure. |
| f | Pattern (b) branch selection has no flag | §8.2 | `VMTEST_DEFAULT_BRANCH`, via the mechanical override mapping. No new flag (P6-T3). |

---

## G. Traceability — plan phase to contract

Every phase, and the DOC-1/DOC-2 sections it implements. This is the inverse of
DOC-2's own traceability table: that one maps contracts to the design; this one
maps work to contracts.

| Phase | DOC-1 § | DOC-2 § | Retires / proves |
|---|---|---|---|
| **P1** | D4, §4.3, §5.1, §6.1, §8.4, §8.5, §14 | §3, §6.2 (N1), §7.3, §10.1, §11.2 | The unmeasured tar transport; the placeholder digest |
| **P2** | §3.1, §3.2, §4.1, §8.1, §8.2, §8.3 | §2, §3, §4, §5, §8, §10, §12.2, §12.4, Shell discipline, JSON dependency | Host-side contracts; the `tart`-boundary invariant |
| **P3** | §3.3, §4.2, §5.2, §5.3, §6.1, §8.6 | §6.2, §6.3, §7, §11, §12.1, §12.2 | Guest bring-up; the toolchain hand-off |
| **P4** | §7.2, §7.4, D3 | §9 (all), §12.5 | Expectation-table drift |
| **P5** | §6.5, §7.1, §7.3, §7.4, §7.5, §8.4, §9 | §1 (all), §6.2 (N2), §10.2, §12.2, §12.5 | The oracle; **RC-2**; the first full-stack timing |
| **P6** | §6.2, §10 step 2, §12.1 | §10.2, §12.2, §12.5 | That the scenario abstraction holds |
| **P7** | D1, D2, D3, §6.3, §7.5, §10 step 3 | §1.2, §9.2, §9.5, §12.2 | The **D2/D3 reversal**, end-to-end |
| **P8** | §5.3, §9, §11, §12, §13, §14 | §3.4, §10.2, §11.4, open items | Isolation discipline; doc drift |

---

## H. References

- [DOC-1 — Tart VM Install Testing Harness](../02-design/01-vm-install-harness.md) — settles what and why; **the authority on every decision this plan sequences**.
- [DOC-2 — Harness Contracts & Interfaces](../02-design/02-harness-contracts.md) — settles every interface; **every task above cites a section of it**.
- [MANIFEST.md](./MANIFEST.md) — the durable progress record. Updated by the final numbered task of every phase.
- [`../02-design/README.md`](../02-design/README.md) — design index. **Corrected 2026-07-31; §F-8 is resolved and P8-T5 is now a verification check.**
- [`../01-research/vm-install-probe-findings.md`](../01-research/vm-install-probe-findings.md) — raw measurements A–K; every number cited above traces here.
- [`../01-research/devils-advocate-review.md`](../01-research/devils-advocate-review.md) — critique #9 (`:20`) is the tar-transport gap Phase 1 exists to close.

> The `01-research/` directory lands in **PR #4456**; relative links to it resolve
> once that branch is on `main`.

