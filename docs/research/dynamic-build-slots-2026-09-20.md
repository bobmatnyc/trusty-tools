# Dynamic build slots — implementation design for #8261+#8298 (2026-09-20)

Owner ruling, verbatim, 2026-09-20: "Merge 8261 and 8298. The concept is that
there should be N set of build slots, set dynamically by machine capacity.
Prioritize this." A ticketing agent is folding #8261 (persistent
build-output-directory pool) and #8298 (load/memory-derived admission, closed
as duplicate into #8261) into one issue; this report is that issue's
implementation design.

## 1. The builder cap today

**Config.** `builders.max_concurrent` lives under `[builders]` in
`~/.trusty-mpm/config.toml`, read only by `resolve_max_concurrent`
(`crates/trusty-mpm/src/core/builders.rs:125-129`), which calls
`MpmConfig::load_default()` — the host-root loader only, never an
effective-config loader that folds a project's `.trusty-mpm.toml` on top
(`builders.rs:15-23`, enforced by source-text guard test `:198-228`).
`.trusty-mpm.toml` parses `deny_unknown_fields`, so a project cannot declare
`[builders]` at all (`builders.rs:234-242`). Absent config, the cap defaults
from the host's `MemoryTier` (`crates/trusty-common/src/machine_tier/tier.rs:34-47`):
Degraded(<16 GB)→1, Medium(16–31)→2, Large(32–63)→3, XLarge(≥64)→4
(`builders.rs:102-109`). This host: 128 GB RAM, 16 logical cores → XLarge →
cap 4.

**Who counts.** `agent_is_builder`
(`crates/trusty-mpm/src/core/dispatch_isolation.rs:416-428`) is true for
`BUILDER_NAMES = ["local-ops"]` (`:240`, checked first so a bundle rename
cannot drop it) or any bundled agent with `role: engineer`
(`BUILDER_ROLES`, `:223`) — a **name/role classifier, not a
"does-this-invoke-a-compiler" classifier**. #8298's evidence: a `local-ops`
dispatch doing only read-only `gh api` calls was refused as "builder 4"
alongside two `engineer` holders and one `rust-engineer`.

**Enforcement.** `tm hook --pm-guard`'s `PreToolUse` hook on `Agent`/`Task`.
The cheap local predicate `dispatch_claims_a_builder_slot`
(`crates/trusty-mpm/src/bin/tm/commands/pm_guard_builder_cap.rs:76-78`) runs
before any network call, so a non-builder dispatch never reaches the daemon,
bounding a daemon outage's blast radius to builder dispatches
(`:25-30`). Three call sites in `pm_guard.rs` ask the cap — the
worktree-grant Rewrite arm (`:730`), the no-rewrite arm (`:767`), and the
plain-dispatch path (`:822`) — via `emit_builder_cap_or` (`:1251-1268`).

**Atomicity.** `DaemonState::claim_builder_slot`
(`crates/trusty-mpm/src/daemon/state/builder_slots.rs:283-303`) takes a
mutex, computes `builder_slot_holders`, and only if
`eligible && holders.len() < cap` runs the caller's `record` closure inside
the same critical section — two dispatches issued in one PM turn cannot both
see a free slot.

**Refusal text**, pinned by test
`deny_reason_names_every_holder_the_cap_and_the_config_key`
(`pm_guard_builder_cap.rs:96-115,500-515`): names every holder
(`agent (running Nm)`), the cap and its config key, and says
`isolation: "worktree"` "buys a separate directory, not separate RAM" —
already correctly distinguishing directory-scope from RAM-scope, which this
design must preserve. `unverifiable_deny_reason` covers the daemon-
unreachable fail-closed case; neither offers "wait" as a remedy (a lease can
run to the 45-minute TTL).

**Slot leak, #8012 — fixed, `status:merged`.** A deny that fires *after* the
shared-tree/worktree-grant claim already recorded a `Running` delegation left
that record alive (and its slot held) for the full six-hour
`RUNNING_STALE_AFTER_SECS`. `release_denied_builder_dispatch`
(`builder_slots.rs:332-347`) now closes the record — marked `Cancelled`, not
deleted, so the tracker's late `matcher: "*"` hook cannot resurrect it —
inside the same critical section as the refusal.

**Ordinary release**, three independent signals, first to fire wins
(`BuilderLease`, `builder_slots.rs:107-172`): `ReleasedByStatus` (terminal/
stale delegation), `ReleasedByDeadOwner` (dispatching session's PID confirmed
dead), `ReleasedByTtl` (45 min, `BUILDER_LEASE_TTL_SECS`, `:72`, the backstop
when neither other signal answers — an *unknown* PID is inconclusive, not
dead, and falls through to the TTL rather than releasing on a guess).

**`tm doctor`.** The `builder_cap` row
(`crates/trusty-mpm/src/bin/tm/commands/doctor_builder_cap.rs:51-91`): `Ok`
naming `N/cap` held; `Warn` when any lease is past the TTL unreaped; `Unknown`
— never `Ok` — when the census cannot be read.

**Cross-session correctness today.** Leases live only in the daemon's
in-memory delegation map, which is **rebuilt empty at every daemon restart**
(#8257, quoted verbatim in #8261's own body) — relevant to §F below.

## 2. Build directories today

**`CARGO_TARGET_DIR`** is resolved per machine by `resolve_build_env`
(`crates/trusty-mpm/src/core/build_env.rs:228-253`) from the `build:` section
of `~/.trusty-tools/trusty-mpm/config.yaml`. Absent config, it defaults to
`<home>/.trusty-tools/cargo-target/<owner>/<repo>` (`:202-207`,
`CARGO_TARGET_SUBDIR`, `:41`) — keyed by `<owner>/<repo>` so the main
checkout and *every* worktree of the same repo share one warm directory,
confirmed here by `.cargo/config.toml:2`
(`…/cargo-target/bobmatnyc/trusty-tools`, itself machine-local/gitignored,
`.gitignore:3-4`, #8251). The daemon never applies this automatically; a PM
pastes `paste_line` (`build_env.rs:267-278`, e.g. `CARGO_TARGET_DIR=…
CARGO_BUILD_JOBS=8 [RUSTC_WRAPPER=sccache] SKIP_UI_BUILD=1`) into each brief
because an agent's shell env does not persist between Bash calls.

**`CARGO_BUILD_JOBS`** defaults to half the host's logical cores, floored at
`MIN_BUILD_JOBS = 2` (`:49,180-192`) — deliberately not the full count, since
handing every dispatch the full core count is what produced the 2026-08-08
crash at load average 36. This host resolves to 8.

**sccache** defaults off (`:89-91,238`) — an operator decision `tm` never
takes on its own.

**Why one shared directory.** `build_env.rs`'s own module doc (`:1-24`, #6868,
16-core host, 2026-09-16) has the numbers: **~200 s** cold in a fresh
worktree, **103 s** with `CARGO_TARGET_DIR` pointed at a warm shared
directory from another worktree, **17 s** the second time from that same
path. **sccache was neutral** on the same tree — "a path-crate-heavy
workspace builds incrementally and incremental artifacts are not cacheable"
— so the doc's own conclusion is that a shared warm directory is the lever
that pays, not a compilation cache. #8251's incident (2026-09-17) measured 65
worktrees each with an independent `target/` (~850 GB aggregate, ~13 GB/
worktree average) before the shared directory existed, and one cold
`cargo test -p trusty-mpm` at 32 concurrent rustc jobs running **2h22m**.
The shared directory today measures **207 GB** (`du -sh`), on a volume with
1.3 TiB free of 3.6 TiB.

**Cargo's own lock.** Per the module doc (`:16-18`): "Cargo's own target lock
serialises concurrent builds sharing the directory, which is the DESIRED
behaviour here." `check`, `build`, `test`, `clippy`, `doc` all take Cargo's
build-directory lock (`Blocking waiting for file lock on build directory`);
`fmt` touches no build output and takes none. With one shared directory and
a cap of 4 on this host, a fourth admitted-but-lock-serialized build queues
invisibly behind the lock — invisible to the cap, to `tm doctor`, and to the
waiting agent's own turn budget. This is the direct cause of the contention
named in the brief (engineer rounds losing ~half a 2.5–4h box to the lock; a
49-minute stall after a `HOME` change invalidated fingerprints; a 30-minute
cold `cargo check`).

## 3. Host metrics available

`crates/trusty-common/src/host_metrics.rs` (feature `host-metrics`) exposes
`HostSampler::sample()` (`:446`): `CpuMetrics` (`:161-168`, `usage_pct` =
`sysinfo`'s global CPU usage averaged across logical cores **at the moment of
the call**, not a load average) and `MemoryMetrics` (`:181-188`,
`available_bytes` — the OS "available" figure, reclaimable included, called
out as "a better headroom signal than `total - used`"). **No averaging
window exists today** for either — each `sample()` is one instantaneous
read; only disk sampling has a warm-cache concept. A 1-minute *load average*
— what the owner and #8298 both ask for — is a different primitive
(`getloadavg`/`sysctl vm.loadavg`/`/proc/loadavg`) this crate does not
currently read. This host right now: `uptime` reports `20.33 15.26 14.10`
(1/5/15-min), already above 16 logical cores — this machine runs loaded well
past 1.0×cores under ordinary PM/agent traffic, so any load threshold needs
calibrating against that baseline, not an idle-machine assumption.
`sys_metrics` is the per-process sibling (RSS+CPU for one PID) and is not
what admission needs. Sampling cost is sub-millisecond (syscall-bound); the
cost that matters is decision cadence, not the read itself.

## 4. Design questions

### A. What is a slot?

**Recommend:** a slot = one persistent `CARGO_TARGET_DIR` under a pool root
(`~/.trusty-tools/cargo-target-pool/<owner>/<repo>/slot-<n>/`) plus one
admission token from `DaemonState`. A compiling agent holds exactly one; a
non-compiling agent holds none — the #8261 closure condition verbatim.

**Warming.** Seed each new slot with a one-time APFS `cp -c` clone
(copy-on-write, near-zero disk at creation) from the existing warm shared
directory, tracked by a marker file so it happens once per slot, not on
every daemon restart or N-shrink-then-grow cycle. This turns "N-1 of N start
cold" into "N of N start warm" for the cost of a clone (seconds) instead of
a cold build (~200 s, §2).

**Disk.** The current shared directory is 207 GB. An APFS clone costs near-
zero extra at creation and grows only as slots' incremental artifacts
diverge under different concurrent branches — plausibly between 207 GB
(heavily shared, unlikely once diverged) and 207 GB×N (fully independent,
matching the pre-#6868 ~13 GB/worktree-average shape at much larger scale).
With 1.3 TiB free, N=4 fully diverged (~828 GB) fits comfortably; the
owner's prior N=8 number (~1.66 TB) should be watched. The disk ceiling is
therefore a soft input to N's operator ceiling in increment one, not a hard
admission signal (see §G).

### B. How N is derived

**Inputs:** operator ceiling (`builders.max_concurrent`, unchanged key,
now the hard maximum N can reach — this host's tier default 4); operator
floor 1 (a host that runs `trusty-mpm` can run one builder); logical
CPUs/total memory, read once at daemon start to size the *default* ceiling
when unconfigured (unchanged `MemoryTier` table); a new measured 1-minute
load average; free memory (`MemoryMetrics.available_bytes`) against a
configured floor. sccache hit rate is **excluded** — not cheap to read
without a subprocess call, and §2 already shows sccache is neutral on this
workspace's build shape, so reading it would not change the answer.

**Formula:**

```
ceiling    = builders.max_concurrent            # operator; default tier_default (this host: 4)
floor      = 1
load_ok    = load_avg_1min <= logical_cores * load_factor   # load_factor default 1.5
mem_ok     = available_bytes >= free_mem_floor_bytes        # default 4 GiB
N_capacity = ceiling if (load_ok && mem_ok) else max(floor, held_count)
N_effective = clamp(N_capacity, floor, ceiling)
admit      = eligible(agent) && compiling(dispatch) && holders.len() < N_effective
```

Worked for this host: `logical_cores=16`, `load_factor=1.5` → threshold
24.0; measured `load_avg_1min=20.33` → `load_ok=true`, though close enough
to illustrate the factor needs owner tuning — at `load_factor=1.0` (threshold
16.0) this exact reading would already refuse. Free memory was not sampled
live in this read-only pass, but `vm_stat`'s free+speculative pages (≈17 GB)
suggest headroom over a 4 GiB floor. With both OK, `N_effective = ceiling =
4` — identical to today, the correct degenerate case for a quiet machine.

**Cadence:** once per admission decision (matches today's shape — the cap is
already resolved fresh per request, `builder_slot_routes.rs:128-134`), not on
a background timer.

**Hysteresis:** N may **drop immediately** (under-admitting is the safe
direction — the same fail-closed framing as §D); N may **rise only after a
quiet window** — recommend both readings OK for 3 consecutive admission
decisions spaced ≥30 s apart, or 60 s wall-clock since the last bad reading.
**An in-flight lease is never revoked when N drops** — matches the owner's
own framing and the existing invariant that a lease, once granted, ends only
by status/death/TTL.

### C. Where admission is enforced

**Recommend both, phased:** dispatch-time reservation (today's mechanism,
extended) plus first-cargo-invocation confirmation — but ship dispatch-time
only in increment one (§G).

Dispatch-time alone false-refuses (today's local-ops case, #8298) and is
evadable by mislabelling (an agent declared `engineer` that never compiles,
or a `research` agent that runs an ad hoc `cargo check`). Bash-time alone
misses worktree provisioning — a fresh worktree needs `CARGO_TARGET_DIR` in
its brief *before* the agent's first Bash call, and cannot bound a daemon
outage's blast radius without a round trip on every Bash call. **Both
together:** dispatch reserves a provisional slot (cheap, bounds blast
radius, gets the directory into the brief up front); the first actual
`cargo` invocation — a Bash `PreToolUse` hook recognizing
`cargo check|build|test|clippy|doc|install`, mirroring the existing
`pm_guard_bash` command-classification idiom — confirms it, converting an
unused reservation (an `engineer` dispatch that never compiles) into an
early release instead of a 45-minute TTL wait.

**The orphaned-cargo case** (a stopped agent's `cargo` child holding the lock
1h45m, seen twice in one day per the brief) is not an admission-site
question at all — neither enforcement point prevents an orphan *process*
from holding Cargo's file lock after its delegation is gone. It belongs in
the lease lifecycle (§D).

### D. Lease lifecycle

**Acquire/liveness** extend `claim_builder_slot` and `session_owner_alive`
unchanged (`builder_slots.rs:283-303,373`): under the existing mutex, compute
`N_effective`, pick a free slot (LRU among the pool's fixed paths, favoring
the build most likely to still be incrementally similar), record the lease
with its slot id.

**Reclaim** rides the existing staleness sweep (extending it, not adding a
new timer): for every lease whose delegation is no longer live, check
whether the slot's `.cargo-lock` is still held by a live PID (`lsof`,
rate-limited to the sweep cadence, never per-admission). **On finding a live
orphaned PID, the sweep kills nothing by default — it reports**, as a
`tm doctor` finding (`orphan_cargo_process`, naming slot/PID/age) and a new
verb to reclaim with an *explicit* kill (`tm builders reclaim <slot>
--kill`). An automatic kill risks destroying in-progress compiled work on a
racy `lsof` read, to save at most the TTL window a report-and-wait already
bounds.

**Fail-Open surfaces, both must hold:** (1) an unobtainable load or memory
reading fails closed to the fixed ceiling, named in the refusal
(`builder-cap-load-read-failure` / `-memory-read-failure`, names from
#8298's own acceptance criteria) so "unreadable" is never confused with
"exceeded"; (2) a lease that cannot be released never silently leaks — #8012
already fixed this for the denied path, and the sweep + explicit-reclaim
pairing above extends the same invariant to the granted-then-abandoned path.

### E. Operator surface

New keys under the existing `[builders]` section, beside `max_concurrent`
(unchanged key/meaning, now the hard ceiling rather than the count):
`load_factor` (`f32`, default 1.5), `free_memory_floor_mb` (`u64`, default
4096), `slot_pool_root` (`String`, default
`~/.trusty-tools/cargo-target-pool`, leading `~` expands as
`cargo_target_dir` already does).

`tm doctor`'s `builder_cap` row extends rather than adds a new row: reports
`N_effective` alongside `ceiling`, the load/memory readings behind it, each
slot's holder (agent, session, elapsed, slot id), and any
`orphan_cargo_process` finding. The refusal message names which condition
failed (pool exhausted / load / memory), the measured reading, and the
configured limit, or which reading was unreadable.

**`tm builders` verb family:** `list` (holders, slot assignments,
`N_effective` and its inputs, JSON-able); `reclaim <slot> [--kill]` (§D);
`set-ceiling <n>` (thin wrapper over the existing config key — no new
daemon state, defer if time-boxed).

### F. Cross-session correctness

Leases stay in `DaemonState`, unchanged home. **Daemon restart:** today's
delegation map, and therefore builder leases, do not survive a restart
(#8257). For a slot pool specifically this matters more, because the pool
has state beyond "who holds what" — directories on disk that outlive the
daemon. Recommend: slot *assignment* (which directory is `slot-N`) persists
in a small on-disk manifest under `slot_pool_root`, written on pool
creation/resize, read at daemon start; *leases* need no separate
persistence beyond what #8257's restart-reconciliation (if it lands first)
already gives every delegation. If #8257 has not landed, a restart forgets
in-flight leases and any still-running `cargo` process becomes exactly the
orphan case §D already handles — not a new failure mode.

**Daemon unreachable (#8026):** fail closed to "no slot", unchanged from
today. #8026's own ask (a documented safe recovery verb for a hung daemon)
stays out of this issue's scope.

## G. First increment (one PR, rung 5) and what to defer

Rung 5 justification: persisted state (slot manifest), process lifecycle
(reclaim sweep, orphan detection), cross-session contract (daemon-held
leases) — matches the Rust Test Ladder's rung-5 row exactly.

**In scope:** (1) extend `BuildersConfig` with `load_factor` and
`free_memory_floor_mb`; (2) add a 1-minute load-average reader to
`trusty-common`, gated behind `host-metrics` or `machine-tier`; (3) replace
`resolve_max_concurrent`'s fixed answer with the §B formula, fail-closed on
either reading's absence; (4) a fixed-size slot pool under `slot_pool_root`,
sized to `max_concurrent` (the ceiling, not `N_effective` — the pool needs
room for every slot the ceiling could ever grant), created lazily,
one-time APFS-clone seeding; (5) `claim_builder_slot` extended to assign and
return a slot path; (6) `deny_reason`/`tm doctor` per §E; (7)
`tm builders list`/`reclaim --kill`; (8) the reclaim sweep and
`orphan_cargo_process` finding (report-only, no auto-kill) — the part that
most directly answers the owner's two same-day orphan incidents.

**Deferred:** Bash-time lease confirmation (§C's second half — real value,
but a second hook with its own tests; dispatch-time-plus-slot-directory
already fixes the shared-lock contention that chiefly motivates this
design); hysteresis tuning beyond the fixed window; a disk-aware admission
signal (ship pool sizing off the ceiling and today's 207 GB baseline; defer
an active disk-usage refusal until multi-slot growth is measured);
`set-ceiling` and any dashboard surfacing beyond `tm doctor`; sccache
hit-rate reads.

**Regression tests, named:** `admission_refused_at_high_load`;
`admission_admitted_at_low_load_with_more_non_compiling_agents_than_the_old_cap`
(proves the #8298 evasion-by-agent-type case is closed); `admission_
refused_when_memory_under_floor_at_low_load`; `admission_fails_closed_on_
unreadable_metric` (two variants, one per named Fail-Open Check surface);
`lease_reclaimed_after_holder_pid_gone`; `n_never_revoked_mid_lease`;
`no_leak_after_a_stopped_agent` (extends #8012's coverage to the
slot-assignment path); `orphan_cargo_case_reported_not_killed`.

## Open questions for the owner

1. **Free-memory floor default (4 GiB)** is an unmeasured placeholder — no
   real `cargo build` link-step peak RSS was captured on this workspace.
2. **`load_factor` default (1.5×cores)** is a starting guess; this host's
   own current reading (load 20.33 on 16 cores, ratio 1.27) suggests the
   "quiet machine" baseline already runs above 1.0×cores under normal
   traffic — worth a short empirical pass before shipping.
3. **Pool sized to the ceiling vs. lazy growth:** sizing to the ceiling
   wastes disk on a machine that rarely reaches it; lazy growth adds
   complexity (a slot creation can itself take ~200 s on a cold seed miss).
   The owner's disk tolerance for N×207 GB is worth a direct answer rather
   than an inferred default.
4. **Authorization bar for `tm builders reclaim --kill`:** this design
   assumed any agent may run it, but the worktree-removal precedent
   (PM-only, ADR-0056/0057) argues it should be PM-only too.

## Recommendation, formula, scope — summary

Replace the fixed builder-slot count with a daemon-computed `N_effective`
clamped between an operator floor (1) and ceiling (`builders.max_concurrent`,
unchanged meaning), lowered from the ceiling when 1-minute load average
exceeds `logical_cores × load_factor` or free memory drops below a
configured floor, re-evaluated on every admission decision, allowed to drop
immediately but rise only after a quiet window, never revoking a granted
lease. Pair this with a pool of N persistent, APFS-clone-seeded
`CARGO_TARGET_DIR` directories so concurrent builders stop serializing on
one shared Cargo lock, each slot leased atomically alongside admission
through the same `DaemonState` mutex the cap already uses. On this host
(16 cores, 128 GB RAM, current load 20.33) the formula reduces to today's
fixed cap of 4 when quiet — the correct degenerate case. First increment:
extend `BuildersConfig`, add a load-average reader, rewrite
`resolve_max_concurrent`'s formula, stand up the fixed-size slot pool with
one-time clone seeding, extend claim/deny/doctor surfaces, ship a
report-only orphan-cargo reclaim sweep with an explicit-kill `tm builders
reclaim` verb — deferring Bash-time lease confirmation, disk-aware
admission, and hysteresis tuning to follow-ups once the pool is proven.

Report: `docs/research/dynamic-build-slots-2026-09-20.md`.
