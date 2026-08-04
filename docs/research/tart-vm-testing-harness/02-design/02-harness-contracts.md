# DOC-2 — Harness Contracts & Interfaces

**Status:** Draft — design specification only, **no implementation**
**Completes:** [DOC-1 — Tart VM Install Testing Harness](./01-vm-install-harness.md)
**Source research:** [conclusions-post-measurement.md](../01-research/conclusions-post-measurement.md), [vm-install-probe-findings.md](../01-research/vm-install-probe-findings.md) (raw measurements A–K), [devils-advocate-review.md](../01-research/devils-advocate-review.md)
**Implements:** nothing. `vmtest-harness/` does not exist and is not created by this PR.

## Purpose

DOC-1 settles **what** the harness does and **why**. It does not settle the
**interfaces**. A dozen of its sections name a mechanism — "JSON-only assertions",
"poll for the observable condition", "self-prefix `PATH`", "`--check-table`
self-diff" — without naming the JSON, the interval, the file, or the diff source.
An engineer with zero context could not build from DOC-1 alone; they would have to
invent twelve contracts and hope.

This document defines those twelve contracts. It is subordinate to DOC-1: where
DOC-1 states a rule, this document states the exact interface that satisfies it,
and cross-references the DOC-1 section by number. Where this document departs from
DOC-1, or discovers that a DOC-1 premise does not hold against the current
workspace, it says so explicitly and in the same register DOC-1 uses — see §1.4,
§6.3, and §9.5.

Every number here either traces to a measurement with a `file:line` citation, or is
labelled a **judgment call** with the reasoning stated. There are no unattributed
numbers in this document. Honest uncertainty is the established register of this
doc set; manufactured confidence is not.

---

## CONTRACTS

### 1. Assertion oracle JSON schema

DOC-1 §7.1 mandates that the oracle read **only** machine-readable output, and
names three sources — `tctl stack doctor --json`, `tctl version --json`, and
"daemon health JSON" — but specifies no field of any of them. This section defines
what `lib/verify.sh` reads and what predicate it applies.

> **Path correction inherited by this document.** DOC-1 §6.5 cites
> `crates/trusty-installer/src/commands/install.rs`, which is correct.
> `crates/trusty-controller/` **does not exist** — the crate was renamed to
> `trusty-installer` per [ADR-0013](../../../adr/0013-rename-trusty-controller-to-trusty-installer.md).
> Any future reference to a `trusty-controller` path is stale.

#### 1.1 `tctl stack doctor --json` — **EXISTS TODAY**

`tctl` is a real declared binary sharing `src/main.rs` with `trusty-installer`
(`crates/trusty-installer/Cargo.toml:26-28`). `--json` is a **global** flag on the
root parser (`crates/trusty-installer/src/cli.rs:54-55`), so both `tctl stack
doctor --json` and `tctl --json stack doctor` parse. The report is a typed serde
struct at `crates/trusty-installer/src/commands/stack/doctor.rs:32-74`:

```json
{
  "command": "stack doctor",
  "members": [
    {
      "member": "trusty-search",
      "health": "healthy",
      "on_path": true,
      "plist_installed": true,
      "port_recorded": true,
      "version": "0.40.0"
    }
  ],
  "verdict": "ok"
}
```

| Field | Type | Domain |
|---|---|---|
| `command` | string | always `"stack doctor"` |
| `members[].member` | string | crate name |
| `members[].health` | string | `healthy` \| `stale` \| `down` \| `not_installed` \| `unknown` |
| `members[].on_path` | bool | |
| `members[].plist_installed` | bool \| null | `null` for non-launchd members |
| `members[].port_recorded` | bool | |
| `members[].version` | string \| null | `null` when the binary is absent |
| `verdict` | string | `ok` \| `degraded` |

There are no `rename` or `skip_serializing_if` attributes on this struct, so field
names serialise verbatim and `None` serialises as `null` — **not** as an absent
key. The oracle may therefore address every field unconditionally.

**Fields `verify.sh` reads:** `verdict`, and per member `member`, `health`,
`on_path`, `version`.

**Pass predicate** (pattern-aware per DOC-1 §7.5), for pattern *P* and the
in-scope members of `expected-binaries.tsv` (§9):

```
PASS  iff  for every in-scope package p:
             if expect_P(p) == present  then  member(p).health   ∈ H_P(p)
                                        and   member(p).on_path  == true
                                        and   member(p).version  != null
             if expect_P(p) == absent   then  member(p).health   == "not_installed"
                                        and   member(p).on_path  == false

           where every clause above is quantified over
             health_scope := tsv_scope_packages ∩ { m.member : m ∈ report.members }
           and H_P(p), the accepted health set for p under pattern P, is

             H_P(p) := {healthy, stale}
                     ∪ {unknown}  if member(p).plist_installed == null
                     ∪ {down}     if P ∈ {b, c} and member(p).plist_installed == false
```

#### 1.1a Health is quantified over doctor's daemon-member set, not over the TSV

*(Amended 2026-08-03 — owner decision. Supersedes the two bullets that follow it
wherever they disagree.)*

**The predicate above was unsatisfiable for a source-installed stack, and Phase 5
proved it on a real guest with every one of the eight in-scope packages failing —
not one of them because installation had failed.** `verify_binaries` had just
resolved all 13 binaries and `doctor` itself reported `on_path=true` and a real
`version` for every member it carries. A predicate that fails on a stack it agrees
is correctly installed is testing the wrong thing. **The oracle stops asserting
what the scenario structurally cannot produce.** Three confirmed causes, each
narrowing the predicate in exactly one way:

**(a) `stack doctor` does not enumerate `tsv_scope_packages`, so health is
quantified over the members it actually reports.** **`trusty-code`,
`trusty-installer` and `tga` are structurally absent from doctor's output** and
can never satisfy a predicate quantified over `member(p)`.

> **CAUSE CORRECTED 2026-08-03.** This bullet previously attributed all three
> absences to a single mechanism — `stack doctor` filtering `stable_set()` to
> `m.daemon`. **That explains exactly one of the three.** There are **two
> distinct causes**, and a future reader who acts on the single-cause version
> will reason wrongly about what happens when the scope changes.

- **`trusty-code` and `trusty-installer` are not in `stable_set()` at all.**
  `stable_set()` (`crates/trusty-installer/src/commands/stable_set.rs:173-183`)
  has exactly **seven** members — `trusty-search`, `trusty-memory`,
  `trusty-analyze`, `trusty-review`, `tga`, `trusty-console`, `trusty-mpm`.
  Neither package appears in it. **They would be absent even if the `m.daemon`
  filter were deleted outright**; their absence is upstream of any filtering.
- **`tga` IS in `stable_set()`, and it is the filter that removes it.** It is
  declared `StableMember::new("tga", "tga", false, false)`
  (`stable_set.rs:179`); the third argument of `StableMember::new` is `daemon`
  (`stable_set.rs:109`), so `tga` carries **`daemon: false`** and is dropped by
  `doctor.rs:151`, which resolves doctor's member set as `stable_set()`
  **filtered to `m.daemon`**. `tga` is the *only* one of the three this
  mechanism accounts for.

**The two causes reach one conclusion**, which is why the original single-cause
text could be wrong about the mechanism and still land on the correct predicate:
none of the three is in doctor's member set, so no health obligation can attach
to any of them. **This correction has zero runtime effect.** `verify.sh:659`
tests membership **dynamically** —
`jq -e --arg m "$pkg" 'any(.members[]; .member == $m)'` — so the oracle asks the
report what it actually contains rather than deriving it from a stated cause, and
**already handles both cases identically**. A package later added to
`stable_set()`, or flipped to `daemon: true`, is picked up with no edit to the
oracle; that is the property the dynamic test buys, and it is unaffected by which
of the two causes applied.

They are not exempted from verification:
their **presence is asserted by `verify_binaries`** (all 13 in-scope binaries,
including `tcode`, `trusty-installer`, `tctl` and `tga`) **and by
`verify_single_install`** for the multi-binary ones, both of which are unaffected
by this amendment and both of which are stronger evidence of a correct install
than a daemon health field a non-daemon package does not have. §F-10(e) resolved
the *opposite* direction — a doctor member the TSV does not carry is logged, not
asserted, which is why `trusty-console` correctly does not fail a run. This is
that rule's missing counterpart.

**(b) `unknown` is accepted for members the product deliberately declines to
probe.** `probe_member_health` (`commands/probe.rs:141-158`) returns
`ProbeOutcome::Unprobeable` for `ManageStrategy::OwnVerb`, and
`probe_http.rs:211` maps `Unprobeable` to `unknown`. The source comment is
explicit that this is a decision, not a gap:

> `#4246`: trusty-mpm (`OwnVerb`) is DELIBERATELY left unprobed and reported
> `unknown`, even though it does answer `/health` on 7880. […] probing it would
> flip `tctl status` to exit 2 and `tctl install` to NOT VERIFIED for every user
> who simply has not started mpm. Enabling it is a separate, user-visible policy
> change, tracked separately.

Rejecting `unknown` therefore asserts against a documented product decision.
**The condition is written as `plist_installed == null`, not as a member name.**
`null` means "not a launchd member" (§1.1's own field table) which is exactly the
`OwnVerb`/`None` set that `probe_member_health` returns `Unprobeable` for, so the
acceptance is **derived from the JSON** and follows the product automatically if
another member ever changes strategy. Naming `trusty-mpm` in the predicate would
hardcode today's `stable_set` into the oracle.

**(c) `down` is accepted under the source-install patterns (b) and (c) when
`plist_installed == false`.** The acceptance is correct and `verify.sh` implements
it correctly. **The cause previously recorded for it was not.**

> **CAUSE FALSIFIED AND CORRECTED 2026-08-03.** This bullet previously asserted:
> *"A launchd member reports `down` because it has no plist."* **That is false**,
> and this repo's own `#4246` fix (`6078b21c`) is what made it false. It is
> corrected here rather than deleted, per this doc set's record-reversals
> convention. **The conclusion is unchanged and `verify.sh`'s predicate is NOT
> modified by this correction** — only the reasoning a future reader would act on.

**Mechanism — `health` is a pure HTTP fact, and `plist_installed` is not an input
to it.** For a `Launchd` member, `probe_member_health` delegates to
`probe_member_http_blocking` (`commands/probe.rs:146`), a blocking `GET /health`
against the member's recorded address. `probe_http.rs:213-218` maps
`NoAddress | Refused | Timeout | HttpError | BadEnvelope | ProbeFailed` to
`down` — **every one of those is a transport fact about the request**.
`plist_installed` is computed **only** at `doctor.rs:117-124`, solely to populate
the report field of the same name, and is **read by nothing on the probe path**.
No value of `plist_installed` can move `health`.

**Observed refutation, in both directions, on the development host (2026-08-03).**
The investigation that produced this correction recorded four launchd members with
**no plist** reporting **`healthy`** — `trusty-search`, `trusty-analyze`,
`trusty-review` and `trusty-console`, each with `"plist_installed": false`. A
second reading the same day, taken while re-verifying this amendment, records the
complementary case:

```
{"member":"trusty-search","health":"down","plist_installed":false}
{"member":"trusty-memory","health":"down","plist_installed":true}
{"member":"trusty-analyze","health":"down","plist_installed":false}
{"member":"trusty-review","health":"down","plist_installed":false}
{"member":"trusty-console","health":"down","plist_installed":false}
{"member":"trusty-mpm","health":"unknown","plist_installed":null}
```

`trusty-memory` **has** a plist, its `trusty-memory serve` process was running,
and it still reports `down`. Between them the two readings refute the causal claim
from both sides: members without a plist have been observed `healthy`, and a
member with a plist has been observed `down`. In both readings health tracked the
**probe**, never the plist.

> **The host reading is one half of a natural experiment, and it was recorded
> without its other half.** *(Amended 2026-08-04.)* The four launchd members above
> that reported `healthy` with `"plist_installed": false` **had been started
> manually by the operator** — a fact the original entry omitted. Read cold, that
> observation alone invites the conclusion that `healthy` is simply the default
> state of a member with no plist. The complementary reading is the Phase 5
> pattern-(c) **guest**, where nothing had started anything:
>
> | | plists installed | daemon started | `health` |
> |---|---|---|---|
> | **host**, 2026-08-03 | none | **yes — manually, by the operator** | `healthy` |
> | **guest**, Phase 5 run C | none | **no** | `down` |
>
> Same `plist_installed: false` on both sides, **opposite** health, and exactly
> **one** variable between them: whether the daemon was started. That pairing is
> what isolates the cause — either observation on its own is still consistent with
> the falsified plist explanation, and only the pair rules it out. The guest half
> is recorded verbatim in [MANIFEST](../03-plan/MANIFEST.md) Phase 5, **"Run C,
> clause (iii)"**, where `trusty-search`, `trusty-memory`, `trusty-analyze` and
> `trusty-review` each read
> `health='down' accepted (plist_installed=false; H_c = {healthy,stale,down})`.
> This **strengthens** the correction above; it does not reopen it.

**The correct statement.** Under DOC-1 §6.5's patterns (b) and (c), **nothing has
started the daemons at the point the oracle reads `doctor`.**
`plans_service_bootstrap` (`install.rs:528`) is banned under those patterns, so
the install path starts nothing. A daemon that was never started answers nothing
on `/health`, so the probe returns `NoAddress`/`Refused` and `health_string()`
renders `down`. **That is the expected state of a correctly source-installed
stack**, not a packaging defect, and it is why the acceptance is right.
**`plist_installed == false` is a co-indicator of "no bootstrap ran", not the
cause of `down`.** Both facts descend from the same upstream cause — the banned
bootstrap step — and neither causes the other.

> **The ordering qualifier is load-bearing; do not drop it.** It is *not* true
> that nothing in a pattern-(c) scenario ever starts the daemons — the harness's
> own `verify_daemon_liveness` step runs an explicit `tctl start --json` (§1.3),
> after which the very same members answer `/health` with HTTP 200. What is true
> is that this happens **after** `verify_stack_doctor` has read and asserted the
> report. Observed in the Phase 5 pattern (c) run of 2026-08-03: `trusty-search`,
> `trusty-memory`, `trusty-analyze` and `trusty-review` were reported
> `health=down, plist=false` by `doctor`, and the same four were then reported
> **LIVE — HTTP 200** by `verify_daemon_liveness` a few steps later in the same
> run. That transition is this correction's mechanism demonstrated end to end:
> **what changes `health` is the start, not a plist.**
>
> > **REFINED 2026-08-04 BY READING `tctl start --json`'s ACTUAL OUTPUT — the
> > last sentence is true but its implication is not, and the difference makes
> > the ordering qualifier load-bearing for a SECOND assertion.** `tctl start`
> > does not merely start; it reaches a **service-install** path and **writes the
> > plists**. Observed verbatim in the pattern (c) run of 2026-08-04:
> >
> > ```
> > { "member": "trusty-search",  "ok": true, "detail": "installed + bootstrapped com.trusty.trusty-search" }
> > { "member": "trusty-memory",  "ok": true, "detail": "installed + bootstrapped com.trusty.memory" }
> > { "member": "trusty-analyze", "ok": true, "detail": "installed + bootstrapped com.trusty.analyze" }
> > { "member": "trusty-review",  "ok": true, "detail": "installed + bootstrapped com.trusty.trusty-review" }
> > { "member": "trusty-mpm",     "ok": true, "detail": "trusty-mpm start ok" }
> > ```
> >
> > It remains correct that a plist does not *cause* `health` to change — the
> > daemon running is what does. But it is **not** correct to picture the plists
> > as still absent afterwards. **They are written, by the harness's own liveness
> > step.**
> >
> > **CONSEQUENCE — §1.1a Decision 2's plist invariant is ORDERING-DEPENDENT, and
> > nothing enforces the ordering.** Since 2026-08-04 `verify_stack_doctor`
> > asserts `plist_installed == false` **directly** for every in-scope launchd
> > member. That assertion holds only because `verify_stack_doctor` runs
> > **before** `verify_daemon_liveness` in all three scenarios. **Reorder those
> > two calls and the plist invariant fails on every run** — not because anything
> > regressed, but because the harness itself bootstrapped the services one step
> > earlier. This is recorded rather than fixed: the ordering is correct today in
> > all three scenarios (`install-local.sh:93,95`, `install-branch.sh:131,133`,
> > `install-released.sh:114,116`), and inventing a guard for a hazard no
> > scenario currently exhibits would add a mechanism nobody has needed. **A
> > future editor moving these calls must read this note first.**

**Consequence 1 — the fail-closed branch this bullet promises is INERT under the
very patterns it is gated to.** The original text justified requiring
`plist_installed == false` by adding that *"a launchd member that **does** have a
plist and is still `down` is a real failure and still fails."* But under (b)/(c)
no plist is ever written, so that guard **can never be `true`** and that branch
**can never fire**. It contributes no discriminating power under (b)/(c). It is
**not wrong — it is dead**, and it is recorded as dead here so a future reader does
not count it as live protection. It becomes reachable only under a pattern where a
bootstrap actually runs, which is pattern (a) — where the `H_P` term is already
not gated in, so the strict form applies there anyway.

**Consequence 2 — do NOT reuse `plist_installed` as a "was bootstrapped" signal
under pattern (a).** It is sound as a co-indicator only under (b)/(c), where the
bootstrap is *banned* and the implication runs one way. Pattern (a) carries no
such guarantee, and this host is the standing counter-example: it has carried
launchd members answering `/health` with **no** plist, alongside `trusty-memory`
**with** one. `plist_installed` reports whether a file exists at
`plist_path_for(binary)` (`doctor.rs:117-124`) and nothing more.

**The `stale` justification below was wrong, and is corrected here rather than
deleted.** It reads "on a freshly installed VM where daemons have just been
bootstrapped, a stale heartbeat is expected timing". **Pattern (c) cannot reach
that state at all** — by (c) above, nothing in a source-based scenario bootstraps
a daemon, so there is no just-bootstrapped heartbeat to be stale. The sentence
described pattern (a)'s world and was applied to all three. `stale` remains
accepted (it is still not evidence of a packaging failure), but its stated reason
holds only where a bootstrap actually happens.

**What this does NOT narrow, stated so a future reader does not over-read it:**
`on_path == true` and `version != null` are still asserted for **every** in-scope
member doctor reports; all **13** in-scope binaries are still asserted present by
`verify_binaries`; all **4** Single-Install gates still run; §1.3's RC-1
liveness-only rule is untouched. The narrowing is to **daemon health**, and to
nothing else.

**Pattern (a) may legitimately assert more strictly, and Phase 7 should consider
it.** Under (a) the harness is permitted `tctl install`, whose service-bootstrap
step **actually starts the daemons** (and, incidentally, writes their plists), so
a member can answer `/health` and a real `healthy` or `stale` becomes reachable —
cause (c) does not apply there, and the `H_P` term above is already pattern-gated
to `{b, c}` so that (a) inherits the strict form. *(Emphasis corrected
2026-08-03 alongside cause (c): what makes `healthy` reachable under (a) is the
**start**, not the plist write. Reading it as the plist is the same falsified
causal step that (c) records.)* Causes (a) and (b) are structural and apply under
every pattern. **This is recorded for Phase 7, not implemented now**; asserting it
before a pattern-(a) run has ever been observed would be inventing a contract,
which is what this amendment exists to stop doing.

**A second, separate Phase 7 candidate — assert `plist_installed == false`
DIRECTLY under (b)/(c).** Under those patterns it is a **derivable invariant**: no
bootstrap ran, therefore no plist was written. Asserting it directly would **fail
closed** if `tctl install` ever leaked into a source-install scenario — the exact
false pass DOC-1 §6.5 bans that step to prevent, and which nothing in the oracle
currently detects. **This is a NEW assertion, not a widening of the health
predicate**: it does not touch `H_P` and does not relax anything. It is the
productive use of the signal that Consequence 1 above shows is otherwise inert
under (b)/(c). **Recorded for Phase 7, not implemented now** (plan §PHASE 7,
MANIFEST open items).

Two deliberate choices, both judgment calls with no measurement behind them:

- **`stale` is accepted, `down` is not.** *(Scoped by §1.1a, 2026-08-03: `down`
  IS accepted under patterns (b)/(c) when `plist_installed == false`, and the
  justification in this bullet describes a state those patterns cannot reach.)*
  `stale` describes a daemon whose
  heartbeat is old; on a freshly installed VM where daemons have just been
  bootstrapped, a stale heartbeat is expected timing, not a packaging defect. The
  harness's claim is that *installation* succeeded (DOC-1 Purpose), and a stale
  heartbeat does not refute that. `down` and `unknown` do refute it and fail.
- **The top-level `verdict` is recorded but not used as the pass predicate.**
  *(Amended 2026-07-31.)* The original reason was that pattern (a) expected
  `trusty-mpm` absent, driving `verdict` to `degraded` on every pattern-(a) run.
  The D2 reversal (DOC-1 D2, §7.5; §9.5 below) removes that specific case — no
  in-scope member is expected absent under any pattern today. The choice stands on
  its own merits: `verdict` is a single crate-wide roll-up whose derivation the
  harness does not control, so asserting on it couples the oracle to a summarisation
  rule that can change without any packaging regression. The per-member predicate
  above is what DOC-1 §7.5 actually asks for, and it remains the assertion; the
  oracle logs `verdict` for the human. Any future `expect_P == absent` row would
  re-create the original degraded-verdict situation, and this predicate already
  handles it.

`doctor` exits 0 on `ok`, 2 on `degraded` (`doctor.rs:100-106`), 3 on unknown
member, 1 on JSON write failure. **The harness must not treat `tctl stack
doctor`'s own exit code as the assertion.** It reads the JSON on stdout and
applies the predicate above. *(Amended 2026-07-31.)* This is a forward-looking
guard, not a description of today: post-D2 reversal no in-scope member is expected
absent under any pattern, so a passing run should currently exit 0. Both the
per-member column mechanism and this exit-code caution are retained **deliberately,
against future divergence** — the first `expect_P == absent` row, or any other
member state that drives `verdict` to `degraded` without being a packaging
regression, needs them already in place. This is
the same principle as DOC-1 §8.1's "a tart exit code is not a completion signal",
applied to a different tool.

> **Related but distinct — do not conflate.** `tctl stack health --json` also
> exists, with a narrower shape (`{command, members[{member, health}], verdict}`,
> `crates/trusty-installer/src/commands/stack/health.rs:27-47`) and a **different
> verdict vocabulary**: `ready` \| `degraded`, versus doctor's `ok` \| `degraded`.
> The oracle uses `doctor`. A future contributor reaching for `health` because the
> name reads better must not assume the verdict strings match.

#### 1.2 `tctl version --json` — **EXISTS TODAY, one value is a stub**

Dispatched at `crates/trusty-installer/src/main.rs:90-91`. Unlike doctor, this is
**not** a serde struct — it is an inline `serde_json::json!` literal at
`crates/trusty-installer/src/output.rs:76-84`:

```json
{
  "tool": "trusty-installer",
  "tool_version": "0.5.0",
  "stack_version": "0.0.0-scaffold",
  "contract_floor": 1,
  "contract_target": 1
}
```

Three properties the oracle must respect:

- `tool` is hardcoded `"trusty-installer"` **even when the binary is invoked as
  `tctl`**. Asserting `tool == "tctl"` would fail always.
- `tool_version` is `env!("CARGO_PKG_VERSION")` and is real.
- `stack_version` is a Phase-0 placeholder constant, `PHASE0_STACK_VERSION =
  "0.0.0-scaffold"` (`crates/trusty-installer/src/commands/version.rs:28`). The
  field is stable; **its value is a stub**. The oracle must assert only that the
  key is present and non-empty, and must **not** compare it against a release
  label, until the real `stack_version` lands.

**Fields `verify.sh` reads:** `tool_version` (asserted equal to the version declared
by the source tree the scenario installed from, under patterns (b)/(c); asserted
merely present under pattern (a), where the published version legitimately differs
from the working tree — see the amendment below), `contract_floor`,
`contract_target` (asserted integers, and `floor <= target`), `stack_version`
(asserted present and non-empty only).

**Pass predicate:**

```
PASS  iff  tool_version is a non-empty string
     and   stack_version is a non-empty string
     and   contract_floor and contract_target are integers with floor <= target
     and   (pattern ∈ {b,c}) → tool_version == source_tree_version(trusty-installer)
```

> **Amended 2026-07-31 — `tsv_version(...)` removed.** The last clause previously
> read `tool_version == tsv_version(trusty-installer)`, and the prose above sourced
> the expected version from `expected-binaries.tsv`. **That file has no version
> column** — §9.1 defines exactly nine and §9.3's seed rows carry no version value —
> so the clause addressed data that does not exist, and two contract sections of
> this document contradicted each other. The cross-check itself is worth keeping:
> under (b)/(c) it is what distinguishes the `tctl` the scenario just built from one
> that was somehow already there. It is therefore **restated against a source that
> genuinely carries a version**, not dropped:
>
> ```
> source_tree_version(p) := the `version` cargo reports for package p in the source
>                           tree the scenario installed from — read with
>                           `cargo metadata --no-deps --format-version 1` in the
>                           guest at $VMTEST_GUEST_SRC via vm_exec, parsed
>                           host-side with jq (§JSON parsing dependency).
> ```
>
> **A tenth column was deliberately not added.** §9.6 states the rule this follows:
> the TSV's non-derivable columns are *"human judgments about the harness's scope,
> not facts about the workspace"*. A crate version is the opposite — a fact that
> changes on every release — and hand-maintaining it in the expectation table would
> guarantee exactly the staleness DOC-1 §7.2 created that table to prevent. It is
> also the same source `--check-table` already reads (§9.6), so no new dependency
> and no second parser.
>
> **Why the guest's tree and not the host's.** The host's `cargo metadata` is
> equivalent under pattern (c) by construction, and simply wrong under pattern (b),
> whose guest-side clone is of `default_branch` (§8.2) and need not match the host
> working tree at all. Reading the tree that was actually built is correct for both,
> and under (c) it is a free integrity check on the tar transport DOC-1 §14 records
> as unmeasured. Pattern (a) is unchanged: no comparison, because there is no source
> tree to compare against.

#### 1.3 Daemon health JSON — **REQUIRED-CONTRACT, not yet implemented**

This is the one DOC-1 §7.1 source that does not exist as a contract.

Every daemon does expose `GET /health`, and it returns JSON. But there is **no
shared type in `trusty-common`** and no unified schema. There are **five**
independently-evolved shapes, with no field in common beyond `status` and
`version`:

| Daemon | Route | Response struct |
|---|---|---|
| `trusty-search` | `crates/trusty-search/src/service/server/mod.rs:245` | `HealthResponse`, `crates/trusty-search/src/service/server/health.rs:22-` — ~22 fields, several `skip_serializing_if = "Option::is_none"` (**absent, not null**) |
| `trusty-memory` | `crates/trusty-memory/src/web/mod.rs:199` | `crates/trusty-memory/src/web/health.rs:103-` — `status`, `version`, `daemon_state`, `worker{in_flight,oldest_age_secs?,wedged}`, resource fields |
| `trusty-analyze` | `crates/trusty-analyze/src/service/routes.rs:75` | `crates/trusty-analyze/src/service/routes.rs:176-180` — `status`, `version`, `search_reachable`. Handler at `routes.rs:188-201`. **The only daemon here that signals through the HTTP STATUS CODE**: 200 + `status:"ok"` when trusty-search is reachable, **503** + `status:"degraded"` when it is not. |
| `trusty-mpm` | `crates/trusty-mpm/src/daemon/api.rs:181` | `crates/trusty-mpm/src/daemon/api/types.rs:41-87` — `status`, `catalog_stale`, `catalog_unknown`, `catalog_changes`, `supervised`, `version` |
| `trusty-review` | `crates/trusty-review/src/service/mod.rs:130` | `crates/trusty-review/src/service/handlers.rs:171-186` — `status`, `version`, `dry_run`, `reviewer_model`, `inference`, `deps{…}` |

> **`trusty-analyze` WAS MISSING FROM THIS TABLE, AND THE OMISSION PROPAGATED**
> *(added 2026-08-04)*. It is an in-scope package, `stable_set()` marks it a
> daemon, and `stack doctor` has reported it on every run this harness has ever
> made — but this table did not carry it, so `verify_daemon_liveness` transcribed
> a **four-name** list from it and never probed the fifth. The harness no longer
> transcribes this table at all: it **derives** the set from `stack doctor`'s
> member table (`stable_set()` filtered to `m.daemon`, `doctor.rs:151`)
> intersected with the expectation table's in-scope packages, so a table like
> this one cannot silently narrow the oracle again. **This table is now
> documentation, not the contract's iteration set.**
>
> **The two citations corrected here were both off by a few lines and both were
> real drift**: `trusty-memory`'s route is at `web/mod.rs:**199**` (190 is a
> comment line about issue #99), not 190. `trusty-search:245`, `trusty-mpm:181`
> and `trusty-review:130` were re-checked and are correct.

Additional hazards a spec must not paper over:

- **`trusty-mpm` has two different `/health` endpoints on two different ports.**
  The supervisor's (`crates/trusty-mpm/src/supervisor/http.rs:50-53`) returns
  `{"status":"ok"}` and nothing else. It is not the daemon health surface.
- **`trusty-review`'s MCP `review_health` is not byte-identical to its HTTP
  `/health`.** The MCP tool rebuilds the payload as a hand-written `json!` literal
  (`crates/trusty-review/src/mcp/tools.rs:331-351`) that emits `detail`
  unconditionally, whereas the HTTP struct omits it via `skip_serializing_if`.
  Nothing enforces they stay in sync. This is a live drift hazard.
- Only `trusty-search` and `trusty-review` expose an MCP health tool at all.

**Therefore:** DOC-1 §7.1's "daemon health JSON" cannot be implemented as written.
It is specified here as a **REQUIRED-CONTRACT dependency on `trusty-installer` /
`trusty-common`**, clearly flagged **not yet implemented**:

> **REQUIRED-CONTRACT RC-1 — unified daemon health envelope.** For the oracle to
> assert daemon health uniformly, a single type must exist — the natural home is
> `trusty-common`, consumed by every daemon's `/health` handler — guaranteeing at
> minimum:
>
> ```json
> { "status": "ok" | "degraded" | "down", "version": "<semver>", "daemon": "<crate-name>" }
> ```
>
> with additional per-daemon fields permitted **under a nested object**, so that
> the common envelope is stable regardless of what a daemon adds. No such type
> exists today. This document does **not** invent it and does not imply it ships.

**Until RC-1 lands, the oracle must not assert on daemon health JSON.** The
interim contract is narrower and honest, and it is what an implementer should
build today:

```
INTERIM (as amended 2026-08-04 — see below; the status CODE is not the signal):
         for each in-scope daemon d expected present under pattern P:
           an address is discovered inside the budget   (`tctl port d --json-port`)
           GET /health produces an HTTP RESPONSE AT ALL  (curl code != 000)
           the body parses as JSON  (`jq -e . >/dev/null`)
           .status is a non-empty STRING
           .status is not one of {"down", "error", "unhealthy"}
         The HTTP status code is LOGGED, NOT ASSERTED.
```

> **THE ORIGINAL FIRST LINE READ `GET /health returns HTTP 200`, AND IT WAS
> WRONG — the product had already settled this and the harness was carrying the
> rule the product fixed.** *(Amended 2026-08-04.)*
>
> `trusty-analyze` answers **503** with `{"status":"degraded"}` when
> `trusty-search` is unreachable; `trusty-review` answers **200** with the *same*
> `status:"degraded"` for the identical condition. The product's conclusion,
> recorded at `crates/trusty-installer/src/commands/probe_http.rs:396-406` after
> #4246:
>
> > "A 2xx-only liveness check (`ensure::daemon::health_ok`) therefore reads one
> > healthy daemon as `degraded` and the other as `down` for the identical
> > condition — and pre-gate, would have kickstarted analyze every time search was
> > slow. **The body is the signal; the status code is not.**"
>
> `classify_response` implements exactly that: a JSON body carrying a string
> `status` field is `Serving` **regardless of `status_code`**.
>
> **THE TWO 2026-08-04 AMENDMENTS ARE COUPLED AND CANNOT BE SEPARATED.** Adding
> `trusty-analyze` to the table above under the *old* 200-only rule would have
> **failed every run in which trusty-search was not yet answering** — which, per
> §1.1a, is the normal state of a source-installed stack for as long as search
> takes to come up. The harness would have reported a packaging defect where the
> product reports a healthy daemon with a degraded dependency.
>
> **THIS IS A NARROWER CHECK ON ONE AXIS, NOT A WEAKER PREDICATE.** What was a
> *code* check is now an *envelope* check, which is `classify_response`'s own
> discipline. **Still rejected, each failing the run:** no address discovered
> inside the budget; **no HTTP response at all** (curl `000` — refused, timed out,
> unreachable: the product's `Refused`/`Timeout`); a body that does not parse as
> JSON; an absent, empty or non-string `.status` (the product's `BadEnvelope`);
> and `.status` in `{down, error, unhealthy}`. A non-2xx code carrying **no**
> parseable envelope still fails — that is the product's `HttpError` → `down`.
> **An unreachable or genuinely failed daemon still fails the run.** The only
> thing that moved is that a non-2xx code no longer overrides a well-formed body.
>
> **The envelope check is not a complete defence and is not claimed as one.** A
> squatter on a stale `http_addr` emitting a generic `{"status":"ok"}` still
> passes — the **#3364 collision class**. Closing it needs a `service`
> discriminator in each daemon's payload, which **no daemon emits today** and
> which `tctl stack health --json` does **not** carry either. It stays open, and
> **RC-1 is what would close it.**
>
> **WHY THIS IS NOT DELEGATED TO `tctl stack health --json`** *(evaluated by
> running it, 2026-08-04)*. It emits
> `{"command","members":[{"member","health"}],"verdict":"ready"|"degraded"}`
> (exit 0 ready / 2 degraded) over `stable_set()` filtered to `m.daemon`
> (`stack/health.rs:96`), classified on the body through the same
> `probe_member_health` this amendment is modelled on — so it is genuinely
> product-maintained, body-based, 503-tolerant, and sweeps the whole stable set.
> **It would nonetheless LOSE an assertion this oracle already makes.**
> `trusty-mpm` is *deliberately* left unprobed by `probe_member_health`
> (`ManageStrategy::OwnVerb` → `Unprobeable` → `unknown`, #4246) **even though it
> does answer `/health` on 7880** — which the oracle's own probe reaches and
> proves. Delegating would downgrade a daemon this harness demonstrates is serving
> into a verdict nothing can assert on. It also drops `version`, drops each
> daemon's raw body, and carries no `service` discriminator.
>
> **So the product's CLASSIFICATION RULE is adopted; the product's COMMAND is not
> substituted for the probe.** `stack health --json` is recorded verbatim in the
> oracle's input snapshot (logged, never asserted on) precisely so a divergence
> between it and the oracle's own probe is visible in any failed run.
>
> **This is NOT the swap §1.1 warns against, and §1.1 is untouched.** That warning
> is about substituting `stack health` **for `stack doctor`** in §1.1a's
> per-member predicate, whose verdict vocabularies differ (`ready|degraded` vs
> `ok|degraded`). `verify_stack_doctor` still uses `stack doctor`. This section
> considered `stack health` as an *additional* surface for the liveness probe
> specifically, and **declined it** for the `trusty-mpm` reason above.

> **`trusty-review` is now one of the daemons this covers.** *(Amended 2026-07-31,
> D3 widened to eight crates.)* It was previously in this section's four-shape table
> but out of D3's scope, so the drift hazard above was documented and not asserted
> against. Now that it is in scope, the INTERIM predicate runs against it like any
> other in-scope daemon — and **liveness-only remains sufficient**, for two reasons
> worth stating rather than assuming:
>
> 1. **The MCP/HTTP drift is not on the assertion path.** The predicate reads
>    `GET /health` — the axum handler at
>    `crates/trusty-review/src/service/handlers.rs:171-186`. The oracle never calls
>    the MCP `review_health` tool, so the hand-written `json!` literal at
>    `crates/trusty-review/src/mcp/tools.rs:331-351` and its unconditional `detail`
>    field cannot affect a result either way. The drift stays a real hazard for MCP
>    consumers; it is simply not this oracle's hazard.
> 2. **The predicate touches only fields all four shapes agree on.** It asserts
>    a response, parseable JSON, and a non-empty string `.status` outside
>    `{down, error, unhealthy}` *(the "HTTP 200" this originally read was amended
>    away on 2026-08-04 — see the status-code correction below)*. `status` is one
>    of the two fields §1.3 opens by
>    naming as common to every daemon here; `trusty-review`'s struct carries it.
>    Nothing in its `dry_run` / `reviewer_model` / `inference` / `deps{…}` tail is
>    read, so bringing it in scope adds a daemon to the loop and **no new field
>    dependency**. RC-1 is what would let the oracle assert more, and RC-1's status
>    is unchanged by this scope decision — it neither becomes more urgent nor less.

This is a **liveness** assertion, not a health-contract assertion. It is
deliberately weak because a strong assertion here would have to be invented, and
DOC-1 §7.1's whole argument for JSON-only is that the oracle should not depend on
surfaces that are free to change underneath it. An envelope that four crates have
not agreed on is exactly such a surface.

DOC-1 §8.7 records that `launchctl bootstrap gui/$(id -u)` works under `tart exec`,
so starting the daemons in order to reach `/health` is viable. That finding is what
makes even the interim assertion possible.

#### 1.4 Summary of contract availability

| DOC-1 §7.1 source | Status | Oracle may assert on it today? |
|---|---|---|
| `tctl stack doctor --json` | **EXISTS** — typed, tested, stable | **Yes** — full predicate, §1.1 |
| `tctl version --json` | **EXISTS** — inline literal; `stack_version` value is a stub | **Yes, partially** — §1.2, no comparison of `stack_version` |
| daemon health JSON | **DOES NOT EXIST as a contract** — four unrelated shapes | **No** — liveness only, pending **RC-1** |

---

### 2. Exit-code contract

DOC-1 §13 makes CI integration an explicit non-goal, and nothing here changes that.
A stable exit-code contract is still required for two reasons that have nothing to
do with CI: a maintainer scripting a repeat run needs to distinguish "my host is
not ready" from "the install is broken", and the harness's own `trap` handler
(§Shell discipline) must decide what to do on the way out. A single `exit 1` for
every failure makes both impossible.

| Code | Phase | Meaning |
|---|---|---|
| **0** | — | Success. Scenario ran, every assertion passed, teardown completed. |
| **2** | argument parsing | Usage error — unknown subcommand, bad `--runid` format (§4.2), unknown scenario name. No VM was touched. |
| **10** | preflight | Preflight refused (DOC-1 §4.1): `tart` missing, base-image digest mismatch (§3), a VM not in `stopped` state, runid collision (§4.3), host capacity assertion failed (§8.4), `jq` missing (§JSON parsing). **No VM was created.** |
| **20** | VM lifecycle | `tart clone` / `set` / `run` failed, or boot-ready polling timed out (§10.1). |
| **30** | negative probe | The cargo-absent probe did not produce its expected result (§6). |
| **40** | provisioning | `provision.sh` failed or timed out — including the mise-detection failures of §11. |
| **50** | scenario / install | The scenario's install steps failed: a build failed, `cargo install` returned non-zero, source delivery failed. |
| **60** | verification | Every install step succeeded but an oracle assertion failed (§1, §9). **This is the interesting failure** — it means the stack installed but is wrong. |
| **70** | teardown | `wait_for_stopped()` timed out or `tart delete` failed. The run's result may have been fine; **the host is not clean.** |
| **130** | user abort | SIGINT. `128 + 2`, the universal shell convention. |
| **143** | user abort | SIGTERM. `128 + 15`. |

**Resolution rule when more than one thing fails.** The **first classified failure
wins** and is what the driver exits with — a scenario failure (50) followed by a
teardown failure does **not** become 70, because the operator needs to know what
broke, not what broke last. Teardown failure is reported on stderr regardless, and
is only *returned* as 70 when nothing earlier failed. Implementation: a single
global `VMTEST_EXIT`, written once by the first `die()` and never overwritten.

**Why the gaps.** Codes are spaced by ten so a phase can gain a sub-code without
renumbering the table. Every harness code is `<= 70`, deliberately below the shell's
reserved range: `126` (found but not executable), `127` (found nothing), and
`128+n` (killed by signal *n*). Confusing a harness verdict with a `127` matters
here specifically — DOC-1 §5.3 records a golden image that shipped with a missing
`~/.zshenv`, which made `cargo` return **127** and presented as "cargo is not
installed" (`vm-install-probe-findings.md:180-181`). A harness that also used 127
for its own purposes would have made that failure harder, not easier, to read.

---

### 3. Base image digest pinning

DOC-1 §4.1 requires preflight to check that "base image `tahoe-base` exists and its
digest matches the pinned digest". It does not say where the pin lives, what it
looks like, or how the comparison is performed.

#### 3.1 The pin is a checked-in file, not a shell constant

**File:** `vmtest-harness/base-image.pin`

**Why a file rather than a constant inside `lib/vm.sh`:**

- **The diff is the point.** Rolling the pin is the single highest-consequence
  change anyone can make to this harness — it changes the thing every run is
  derived from. As a one-line diff to a data file it is visible in review and in
  `git log -p base-image.pin`. Buried in a shell function it is a diff to a script
  that also contains logic, where it is reviewed alongside unrelated changes.
- **`git blame` answers "when and why did we roll it".** A constant inside a
  function that also gets refactored loses that history to noise.
- **It is data the harness reads, not behaviour it executes.** DOC-1 already
  applies this rule to expectations (`expected-binaries.tsv`, §7.2). The digest pin
  is the same kind of thing and should not be treated differently.
- **One parser, three files.** §8 puts tunables in the same key/value TSV format,
  which means `base-image.pin`, `vmtest.defaults`, and `expected-binaries.tsv` are
  all read by the same handful of `awk` lines. This matters more than it sounds
  under a bash 3.2 constraint (§Shell discipline), where a config *format* costs
  real code.

#### 3.2 Format

Tab-separated `key<TAB>value`. Blank lines and lines whose first character is `#`
are ignored. Exactly one line per key; unknown keys are a preflight error, so a
typo cannot silently become "unpinned".

```
# vmtest-harness base image pin
# Roll this file deliberately — see §3.4. One line, one review.
oci_ref	ghcr.io/cirruslabs/macos-tahoe-base
digest	sha256:0000000000000000000000000000000000000000000000000000000000000000
local_name	tahoe-base
pinned_on	2026-07-31
pinned_by	<maintainer>
note	tart v2.32.1; see ../docs/research/tart-vm-testing-harness/01-research/vm-install-probe-findings.md:6
```

> **The `digest` value above is a placeholder and must not be copied.** The
> research recorded the base image only as
> `ghcr.io/cirruslabs/macos-tahoe-base:latest` (`vm-install-probe-findings.md:6`)
> and printed its digest **truncated** — `ghcr.io/cirruslabs/macos-tahoe-base@sha256:a8e1...`
> (`vm-install-probe-findings.md:652`, `:685`). The full 64-hex digest **was never
> recorded anywhere in the research**. The implementer must capture it from
> `tart list` on first implementation and commit the real value. Shipping the
> placeholder would make preflight fail closed, which is the correct direction to
> fail, but it is not a pin.

#### 3.3 How preflight compares

`tart list` reports OCI-sourced images by digest — the research shows exactly this
form in its own output (`vm-install-probe-findings.md:651-652`, `:684-685`), which
is the evidence that a digest is retrievable at all. Preflight:

1. Reads `oci_ref` and `digest` from the pin file.
2. Queries `tart list` for an OCI entry matching `<oci_ref>@<digest>`.
3. On match → proceed. On no match → **exit 10**, printing both the pinned digest
   and what was actually found.

> **Judgment call, flagged.** The exact `tart list` invocation and output format
> that yields a full, machine-readable digest was **not measured** in the research —
> only the truncated human-readable rendering at `:652` was captured. An
> implementer must determine whether `tart list --format json` (or equivalent)
> exposes the untruncated digest, and if it does not, fall back to the
> by-construction variant below. This is a genuine unknown and is recorded in the
> gaps list rather than guessed at.

**By-construction variant (recommended if introspection proves awkward).** Rather
than clone the local `tahoe-base` and then verify what it came from, clone the
pinned OCI reference directly:

```sh
tart clone "ghcr.io/cirruslabs/macos-tahoe-base@sha256:<digest>" "vmtest-<runid>"
```

The pin is then enforced *by construction* — there is no comparison to get wrong,
and no window in which a local image is re-pulled between check and use. The
trade-off is that the first such clone must pull the image, which is unmeasured and
certainly far more than the measured **0.31s** CoW clone
(`vm-install-probe-findings.md:875`, DOC-1 §9); subsequent clones should be CoW
again once the image is local. DOC-1 §4.1 is written in terms of comparing the
local `tahoe-base`, so that remains the specified behaviour; this variant is
recorded as the preferred resolution if the comparison cannot be made reliable.

#### 3.4 Rolling the pin forward

A pin roll is a deliberate act with its own PR. It is **never** a repair step
inside a failing run — DOC-1 §4.1's "do not attempt to fix it up and carry on" rule
applies here directly, and for the same reason: an automated fix-up is how a broken
image shipped once already (DOC-1 §4.3).

1. Pull the candidate: `tart pull ghcr.io/cirruslabs/macos-tahoe-base:latest`.
2. Record its full digest.
3. Update `base-image.pin` — `digest`, `pinned_on`, `pinned_by`, and a `note`
   saying *why* (a security update, a macOS point release, a tool the guest now
   needs).
4. Run **all three** scenarios green against the candidate before opening the PR.
   The new base is the input to every subsequent run; a roll validated against one
   pattern is not validated.
5. Re-verify §11's preinstalled-tool assumptions explicitly. A new base image is
   precisely where a preinstalled `mise` could move, disappear, or gain a second
   copy — and §11 records that this class of detail has broken a build before.
6. Open the PR with the before/after digests in the body.

---

### 4. `--runid` generation and collisions

DOC-1 §3.1 lists `--runid <id>` in the driver's surface and §4.1 requires preflight
to detect "no leftover `vmtest-*` VM with the target runid". Detection is not
prevention. This section specifies prevention.

#### 4.1 Required vs auto-generated

`--runid` is **optional**. When omitted the driver generates one. Making it
required would push a naming burden onto every invocation and would make the common
case (`vmtest run local`) worse for no benefit. Supplying one explicitly is
supported because a maintainer inspecting a `--keep` VM (DOC-1 §3.1) wants a name
they chose rather than a timestamp they have to copy.

#### 4.2 Format

```
runid   := <utc-timestamp> "-" <pid>          # auto-generated form
utc     := YYYYMMDDThhmmssZ                    # date -u +%Y%m%dT%H%M%SZ
vm name := "vmtest-" <runid>
```

Auto-generated example: `20260731T142233Z-48213` → VM `vmtest-20260731T142233Z-48213`.

**Validation, applied to user-supplied and generated ids alike:** must match
`^[A-Za-z0-9][A-Za-z0-9-]{0,31}$` — leading alphanumeric, then alphanumerics and
hyphens, 1–32 characters. A violation is **exit 2**, before any VM work. The
character class excludes anything that could be meaningful to a shell, a path, or
`tart`'s own name parsing; the 32-character cap keeps the full VM name under 40
characters so it stays readable in `tart list` output.

The `vmtest-` prefix is not decoration. It is the sole means by which `vmtest
clean` (§5) identifies a VM as belonging to this harness, and the reason the
harness will never touch the owner's `tahoe-base` (`vm-install-probe-findings.md:676`).

#### 4.3 Collision prevention

Prevention rests on two mechanisms, not on a check.

**(a) The auto-generated form cannot collide with a concurrent run.** It embeds the
driver's own PID, and a PID is unique among live processes on a host. Two `vmtest`
invocations running at the same instant necessarily have different PIDs, so they
necessarily generate different runids — regardless of whether their timestamps
agree. The timestamp handles PID *reuse* across time, since a reused PID at a later
second yields a different id. This is a guarantee from the operating system, not a
race the harness has to win.

**(b) An atomic run-registry lock covers user-supplied ids.** The above says
nothing about two invocations that both pass `--runid nightly`. For those:

```
registry root:  ${VMTEST_STATE_DIR:-$HOME/.local/state/vmtest-harness}/runs/
run directory:  <registry root>/<runid>/
```

The driver acquires the run by **`mkdir "<registry root>/<runid>"`**. `mkdir` on a
POSIX filesystem either creates the directory or fails; it cannot partially
succeed, and two concurrent callers cannot both succeed. This is the whole
guarantee — a test-then-create sequence (`[ -d ... ] || mkdir ...`) would be a
race, and must not be used. On failure, preflight reports the conflicting run and
exits **10**.

Immediately after acquiring, the driver writes into the run directory:

| File | Contents |
|---|---|
| `pid` | the driver's PID |
| `vm` | the VM name |
| `pattern` | `local` \| `branch` \| `released` |
| `started` | UTC timestamp |
| `toolchain.tsv` | written later by provisioning (§7.3) |
| `keep` | written only if `--keep` was requested (§5.3) |

The run directory is removed by the cleanup trap on normal and abnormal exit,
**except** when `keep` is present. Lock and registry are the same object: there is
no separate lock file to get out of sync with the registry entry.

**Concurrency is safe but not advised.** The mechanisms above make two simultaneous
runs collision-free. They do not make them a good idea: DOC-1 §8.5 sizes each guest
at 8 vCPU / 16 GB, and the research host was 18 logical cores / 64 GB
(`vm-install-probe-findings.md:842-843`), which accommodates one such guest with
"ample headroom" and two only by contending. Preflight therefore **warns** (does
not fail) when another run directory holds a live PID. Warning rather than failing
is a judgment call: the harness cannot know the operator's host, and refusing a
legitimate second run on a large machine would be worse than a warning ignored on
a small one.

---

### 5. `vmtest clean` semantics

DOC-1 §3.1 gives `vmtest clean` as "delete orphaned `vmtest-*` VMs" and pairs it
with `--keep`. "Orphaned" is undefined, and the hard case — telling an abandoned VM
from one a run is actively using — is not addressed.

#### 5.1 The definition

A VM is **orphaned** iff **all** of the following hold:

1. Its name matches `vmtest-*`. (Nothing else is ever a candidate. `tahoe-base` and
   the OCI base images are not in the harness's namespace and are never touched.)
2. `tart list` reports its state as **`stopped`**.
3. Its runid has **no live owner**: either no `<registry>/runs/<runid>/` directory
   exists, or the directory exists but its `pid` file names a process that is not
   alive (`kill -0 <pid>` fails).
4. The run directory does **not** contain a `keep` marker.

All four. A VM failing any one of them is not orphaned and `clean` does not delete
it.

#### 5.2 How in-progress runs are distinguished

By condition 3 — the run registry of §4.3 — and secondarily by condition 2.

An in-progress run holds a registry directory whose `pid` is its own live PID, so
`kill -0` succeeds and the VM is protected. This is the primary discriminator, and
it is why the registry exists at all rather than the harness relying on VM state
alone.

Condition 2 is the backstop. A VM in state `running` is either genuinely in use or
is wedged, and DOC-1 §8.1/§8.3 forbid the harness from stopping or repairing it
either way. `clean` inherits that rule wholesale: **`clean` never issues `tart
stop`, never issues `tart suspend`, and never deletes a VM that is not already
`stopped`.**

**The PID-reuse edge, stated plainly.** A dead run's PID may have been recycled by
an unrelated process, in which case `kill -0` succeeds and `clean` treats a genuine
orphan as live. The failure direction is *conservative* — `clean` skips something it
could have deleted, leaving a VM behind for a human. The opposite error, deleting a
VM out from under a live run, cannot occur by this mechanism. Accepting a
conservative false negative to make the dangerous false positive impossible is the
deliberate trade. It is not eliminated, and it is not worth eliminating with
start-time comparisons; the residual is one stale VM and a message.

#### 5.3 `--keep` interacts with this and must not be confused for abandonment

`vmtest run --keep` skips **the deletion** so a failed run can be inspected (DOC-1
§3.1). The run then exits, so its PID dies, so condition 3 is satisfied — a kept VM
would look exactly like an orphan. The `keep` marker (condition 4) is what prevents
`clean` from deleting the very thing `--keep` was asked to preserve.

*(Amended 2026-08-02: "skips teardown" → "skips the deletion".)* This paragraph
already assumed a **`stopped`** kept VM — condition 3 alone does not make a VM
"look exactly like an orphan"; condition 2 is the other half. The §Shell discipline
cleanup property 4 that shipped at Phase 3 skipped the stop as well, producing a
`running` kept VM that `clean` could never remove. Property 4 is now amended to
skip **only** `vm_delete`, which is what makes this paragraph, condition 2, and
`--include-kept` describe the same VM.

`clean` lists kept VMs separately, with their age, and does not remove them.
`vmtest clean --include-kept` removes them too, and is the intended way to tidy up
after an inspection session.

#### 5.4 When `clean` cannot tell

Two ambiguous cases, and the rule for each:

| Situation | What `clean` does |
|---|---|
| `vmtest-*` VM in state `running`, no registry entry at all | **Refuses.** Reports the VM, its state, and the manual commands a human may choose to run. Exits **10**. It cannot distinguish "another user's run" from "crashed run, registry wiped", and both are cases where deleting is destructive. |
| `vmtest-*` VM in state `suspended` | **Refuses and flags it as wedged.** Per DOC-1 §8.2, resume is broken and reproducible (`VZErrorDomain Code=12`); the manual unwedge (`mv ~/.tart/vms/<name>/state.vzvmsave{,.bak}`) is documented there as a human procedure explicitly *not* for the harness. `clean` prints that procedure and exits 10. |
| Registry directory with no matching VM | **Prunes the directory** and says so. There is nothing to destroy; a stale registry entry is bookkeeping, not state. |
| VM in state `stopped`, no registry entry | **Orphaned. Deletes.** This is the normal aftermath of an interrupted run and is the case `clean` exists for. |

`vmtest clean --dry-run` performs the full classification and prints the verdict for
every candidate without deleting anything. Given that `clean` is the only
destructive subcommand, this is the mode a cautious operator should reach for
first, and the one to use when reporting a `clean` bug.

---

### 6. Negative-probe mechanics

DOC-1 §4.2 requires a "cargo absent → guide-and-abort" probe running after boot and
before provisioning, and its own sequencing note flags the lifecycle position as
something the source research left unpinned. Specifying this precisely surfaces a
problem DOC-1 does not acknowledge, so this section is longer than the others.

#### 6.1 The problem with the probe as literally written

DOC-1 §4.2 says "the installer path under test must produce its guide-and-abort
behaviour". At the specified lifecycle position — after boot, before provisioning —
the guest has no Rust toolchain. But `tctl` **is built by the toolchain**: under
pattern (c) it is compiled from streamed source, under (b) from a guest-side clone,
under (a) it arrives via `cargo install`. All three require cargo.

**So at the moment the probe is supposed to run, the subject of the probe does not
exist.** The assertion as written cannot execute at the position DOC-1 assigns it.
This is not a wording problem; it is a circularity in the design, and it needs
resolving rather than restating.

The relevant code does exist and does guard on cargo — `which::which("cargo")` at
`crates/trusty-installer/src/commands/install.rs:829`, with the same guard in
`upgrade.rs:502` and `self_update.rs:295` — so the *behaviour* DOC-1 wants to test
is real. It simply cannot be reached before provisioning.

> **CITATION CORRECTED 2026-08-04: the guard is at `install.rs:829`, not
> `install.rs:826`.** Every occurrence in this document, in the plan and in the
> MANIFEST said **826**, which is the `Outcome::Fallback { reason }` match arm
> three lines above — the arm the guard lives *inside*, not the guard. The
> `which::which("cargo")` call is at **829**. The two sibling citations were
> re-checked against the same source and are **correct as written**:
> `upgrade.rs:502` and `self_update.rs:295`. Nothing about RC-2's substance
> changes — the guard is still unreachable from a guest, N2 is still `BLOCKED`,
> and the exit code and message are still unpinned by any test. Only the line
> number was wrong, and it was wrong in sixteen places.

#### 6.2 Resolution — split the probe in two

**N1 — Precondition probe. Runs at DOC-1 §4.2's position, on every run.**

Asserts that the guest genuinely lacks a Rust toolchain at that instant. This is
the assertion a golden image structurally destroys, and preserving it is one of the
two stated reasons the harness does not bake one (DOC-1 §4.3). It stays exactly
where DOC-1 puts it.

*Command* (self-prefixed per §7, base PATH form):

```sh
tart exec vmtest-<runid> /bin/sh -c \
  'PATH=/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin; export PATH; command -v cargo'
```

*Expected exit code:* **127** is what the research measured for an absent `cargo`
under both `/bin/sh` and `/bin/zsh` (`vm-install-probe-findings.md:180-181`, probes
B7/B8). `command -v` itself returns **1** on not-found; the harness asserts
**non-zero**, which both satisfy, and logs which it got. Asserting non-zero rather
than a specific code is deliberate — 127 was measured for *invoking* `cargo`, not
for `command -v cargo`, and pinning a code that was measured for a different
command would be exactly the kind of false precision this doc set avoids.

*Expected output shape:* **empty stdout.** Any non-empty stdout means a cargo was
found and the precondition is violated.

*Assertion predicate,* **channel 1** *(see the amendment below for channel 2):*

```
N1 PASS  iff  exit != 0  AND  stdout is empty,  for each of cargo, rustc, rustup
```

*On failure:* **exit 30**. Do not proceed to provisioning. A guest that already has
cargo is not the guest this harness claims to test, and the most likely cause is
that the base image drifted (§3) — which is a finding, not a nuisance.

The base PATH literal above is measured, not invented:
`vm-install-probe-findings.md:213` records the guest's non-interactive PATH as
`/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin`.

##### N1 asserts REACHABILITY, not base-PATH absence — what the probe now proves

*(Amended 2026-08-02.)*

**The claim above was wider than the predicate above it, and the gap is exactly
where a real toolchain lands.** This section opens by saying N1 "asserts that the
guest genuinely lacks a Rust toolchain at that instant", and DOC-1 §4.3 leans on
that reading when it calls N1 "the assertion a golden image structurally destroys"
and makes it one of the two reasons not to bake an image. What channel 1 actually
tests is narrower: `command -v` under **one** PATH.

Phase 3 measured the difference on a real guest. After `mise use -g rust@1.91` —
**this project's own provisioning command**, the one `provision.sh` runs — the
guest has `/Users/admin/.cargo/bin/cargo` and a mise shim, **neither directory on
the base PATH**, and N1 returned **exit 0, PASS**. A golden image baked the way
this project would bake one therefore would **not** have been caught, which is the
precise scenario DOC-1 §4.3 cites as a reason not to bake one.

**Owner decision: make the code match the claim.** The alternative — keep the
narrow predicate and weaken §6.2's and DOC-1 §4.3's prose to match it — was
rejected, because DOC-1 §4.3's argument *requires* the wide reading to be true. A
probe that cannot see the toolchain its own harness installs is not evidence about
a golden image.

**What N1 now proves.** N1 fails if a Rust toolchain is reachable by **any route a
later build step could use**, not merely by the base PATH. Channel 1 is unchanged
and still asserted; channel 2 is added:

| Channel | Route probed | Rationale |
|---|---|---|
| **1** | `command -v cargo\|rustc\|rustup` under the measured base PATH | unchanged; the original predicate, still asserted verbatim |
| **2a** | `$guest_home/.cargo/bin/{cargo,rustc,rustup}` on disk | where `rustup` — and therefore `mise use -g rust@…` — actually puts them |
| **2b** | `$guest_home/.local/share/mise/shims/{…}` and `$guest_home/.local/bin/{…}` on disk | the shims §7.1 puts second on the full PATH; `.local/bin` is where the forbidden `mise.run` installer writes |
| **2c** | `mise which cargo\|rustc\|rustup` | resolvable by the very tool `provision.sh` uses to install one |
| **2d** | `zsh -lc`, `zsh -ic`, `bash -lc`, `bash -ic` → `command -v …` | a PATH an rc file would activate |

*Predicate:* **N1 PASS iff channel 1 passes AND channel 2 finds nothing.** Channel 2
signals by **stdout content, never by exit status** — its guest-side script always
exits 0, so "the probe could not run" can never be misread as "the guest is clean".
An unrunnable channel-2 probe **fails closed**, exit 30.

**On channel 2d and DOC-1 §5.3, because it looks like a contradiction and is not.**
DOC-1 §5.3 forbids the harness from **depending on** guest shell rc files — a golden
image once shipped with `~/.zshenv` missing and `cargo` returned 127 under both
`/bin/sh` and `/bin/zsh`. That rule governs **reliance**. Channel 2d probes rc files
as a **hazard**: the question is not "does an rc file give the harness cargo" but
"could an rc file give a **later step** a cargo N1 just certified absent". Probing
something you refuse to rely on is the opposite use and is legitimate. The harness
still resolves every path it *uses* explicitly, composed in `vm_exec` and nowhere
else (§7.3). The reasoning is repeated at the probe itself so that nobody deletes it
in the name of §5.3 compliance.

*Observed, 2026-08-02, one guest, both directions* (MANIFEST Phase 3):

```
(A) clean tahoe-base clone
    N1 PASS (base PATH: cargo=1 rustc=1 rustup=1; and no toolchain reachable
             on disk, through mise, or through a login/interactive shell)
    negative_probe_n1 exit=0

(B) same guest, after `mise use -g rust@1.91` -> 0
    command -v cargo under the BASE PATH channel 1 probes: (not found)

(C) strengthened N1 on that provisioned guest
    | on-disk       /Users/admin/.cargo/bin/cargo
    | mise-which    cargo -> /Users/admin/.cargo/bin/cargo
    …
    FAIL[30]  negative_probe_n1 exit=30
```

**Honest limit, recorded rather than glossed.** Channel **2d contributed nothing**
in case (C): `mise use -g` does not write an rc file, and `tahoe-base`'s own rc
files do not activate mise, so the login/interactive shells found nothing that 2a-2c
had not already found. 2d is retained because it is the channel that would catch an
image whose *rc file* is the only thing making a toolchain reachable — a case this
project has not yet produced and therefore has not yet observed 2d firing on. It is
an unexercised guard, not a demonstrated one.

**N2 — Guide-and-abort probe. Runs after the scenario's install steps.**

Asserts the behaviour DOC-1 §4.2 actually cares about: that `tctl`, finding no
cargo, guides and aborts rather than failing incomprehensibly. It runs against the
`tctl` the scenario just installed, in a shell whose PATH contains `tctl`'s
directory but **excludes** `~/.cargo/bin` and the mise shims — reproducing "cargo
absent" *for the process under test* without needing an unprovisioned guest.

```sh
# 1. Locate it, on the host, capturing the guest's stdout. PATH here is the
#    INSTALLED environment — cargo's bin dir included — because that is where the
#    scenario just put tctl.
TCTL_PATH="$(tart exec vmtest-<runid> /bin/sh -c \
  'PATH=/Users/admin/.cargo/bin:/bin:/usr/bin; export PATH; command -v tctl')"

# 2. Re-invoke that captured absolute path under a PATH that excludes
#    ~/.cargo/bin and the mise shims. Note the double quotes: $TCTL_PATH is
#    expanded by the HOST shell before the string is handed to `tart exec`.
tart exec vmtest-<runid> /bin/sh -c \
  "PATH=/bin:/usr/bin:/usr/sbin:/opt/homebrew/bin; export PATH; $TCTL_PATH install trusty-search"
```

The capture is the load-bearing step: `tctl` is reached in step 2 by absolute path
precisely *because* it is not on the PATH that step 2 constructs. An empty
`TCTL_PATH` means step 1 failed to find the binary the scenario claims to have
installed — that is a harness error to be raised as such, not an RC-2 observation.

*Expected exit code and output shape:* **REQUIRED-CONTRACT RC-2, not yet
pinned.** `install.rs:829` maps the `which::which("cargo")` failure through
`map_err`, but this document does not assert what code that produces or what the
message says, because that was not read out to a verified value and guessing it
would be inventing a contract. What the harness needs from `trusty-installer` is:

> **REQUIRED-CONTRACT RC-2 — cargo-absent guide-and-abort.** `tctl install`, run
> with no `cargo` on `PATH`, must exit with a **stable, documented, non-zero code
> distinct from 1**, and must emit actionable guidance on **stderr** (leaving
> stdout clean, so §1's JSON discipline is unaffected). Until that code is fixed
> and documented, N2 asserts only: exit != 0, stdout empty, stderr non-empty and
> containing a cargo-related token. That weaker predicate is stated as weak on
> purpose.

*On failure:* **exit 30**, the same phase code as N1 — both are the negative probe,
and an operator reading the code should not have to know which half fired. The
message says which.

##### RC-2 CLOSED as *unreachable-by-design* — N2 records BLOCKED and the run continues

*(Amended 2026-08-03 — owner decision. RC-2 is closed here, not left unpinned.)*

**N2's guide-and-abort is not reachable through `tctl install` from a guest.** This
is not "not yet measured"; it is structural, it was read out of the source and
confirmed by observation on two real guest runs and once on the host (MANIFEST
Phase 5, Deviations item 2, Measurements item 3), and no change on the harness side
can reach it. **Both** causes must be stated, because fixing only the first makes
the situation worse:

**1. The consent gate returns before `install_one` is ever called.**
`decide_install_gate` (`commands/install_gate.rs:77-85`) returns
`InstallGate::Refuse` whenever `--yes` is absent and stdin is not a TTY — **and the
guest exec channel is not a TTY**. Its arm in `install.rs:266-278` prints the
refusal to stderr and **`return 3`**, before `install_all` on line 295. The cargo
guard at **`install.rs:829`** lives inside `install_one`, which that refusal never
reaches. Observed, identically, three times:

```
exit 3, stdout 0 bytes, stderr 204 bytes:
  info: ✓ git Git-155) found
  tctl install: refusing to install without confirmation in a non-interactive
  context; pass --yes to proceed non-interactively, or --dry-run to preview
  what would be installed.
```

`3` is non-zero and distinct from 1, so it satisfies RC-2's *shape* — but it is the
**consent-gate code, not the cargo-absent code**, and recording it as RC-2's would
be exactly the false precision this section refuses.

**2. Adding `--yes` would be worse, and is therefore forbidden here.** It reaches
`install_one`, which is **prebuilt-tarball-first**: the cargo guard sits only in the
`Outcome::Fallback` arm, reached **only when the prebuilt download fails**. On a
networked guest the download **succeeds** — and installs **released binaries over
the source-built ones under test**, before the oracle reads them. That is precisely
the false pass **DOC-1 §6.5** bans `tctl install` from patterns (b)/(c) to prevent,
and it is the worst failure mode a harness has. A probe that can corrupt its own
run's subject is not a probe.

**Status: RC-2 is CLOSED as *unreachable-by-design*, not satisfied and not
outstanding.** The distinction matters. RC-2 is not a request `trusty-installer`
has failed to answer — it is a request that **cannot be exercised through this
entry point at all**, so leaving it open would leave a permanently unresolvable
item on the register and imply work that would never be done. The behaviour it
describes is real (`which::which("cargo")` at `install.rs:829`, and the same guard
in `upgrade.rs:502` and `self_update.rs:295`); what is unreachable is **N2's route
to it**. Anything that wants to assert it must reach the guard by a route that does
not run `tctl install` on a networked guest — a unit test in
`crates/trusty-installer`, or an offline-network scenario — and **neither is this
harness's job**. Recorded so a future contributor does not re-open RC-2 and re-run
the same probe expecting a different answer.

**What N2 does now.** It records the observation and continues, loudly. This is the
doc set's **own established remedy** — §F-7's "record as BLOCKED and skip" branch,
written so a required-contract gap "cannot strand the phase". **The predicate is
NOT weakened.** Exit 0, non-empty stdout and empty stderr all still **die 30**, and
a stderr that *does* carry a cargo token still takes the **normal PASS path**. Only
the one shape proven unreachable — non-zero exit, clean stdout, guidance on stderr,
no cargo token — is recorded instead of asserted, and it prints

```
*** N2 BLOCKED (RC-2 / DOC-2 §6.2) — NOT A PASS. ***
```

on **every run**. A BLOCKED N2 therefore satisfies the Phase 5 checkpoint's clause
(v), which asks for the observation to be recorded (plan §Phase 5 checkpoint, as
corrected 2026-08-03); it never satisfies a claim that guide-and-abort was proved.

**Reconciling P5-T2.** P5-T2 anticipated only that the observed code "might be
`1`", and told the implementer not to tighten the predicate to an observed code
unless it was non-zero and distinct from 1. The observed code **is** non-zero and
distinct from 1 — and tightening to it would still have been wrong, because it is
a different guard's code. P5-T2's instinct (an observed code is not a documented
contract) was right; its enumeration of outcomes was incomplete, and it did not
anticipate the predicate being unreachable at all. **The correct action under
P5-T2 is the one taken: do not tighten, do not weaken, record.** P5-T2's rule that
nothing in `crates/trusty-installer` may be changed to make the predicate happier
stands, and nothing was.

#### 6.3 Lifecycle position — pinned

DOC-1 §4.2 flags the position as unresolved. Pinned here:

```
boot → vm_wait_ready → [N1] → provision → scenario install steps → [N2] → verify → teardown
```

**N1 keeps DOC-1's position exactly** (boot-before-provision), for DOC-1's exact
reason: it is the only window in which the guest genuinely lacks cargo, and it is
what a golden image would destroy. **N2 is new to this document**, placed after the
install steps because that is the earliest point at which its subject exists.

> **Departure from DOC-1, recorded.** DOC-1 §4.2 describes one probe; this
> specifies two. The split is a judgment call by this document, made because the
> single-probe formulation is not executable (§6.1). The *requirement* DOC-1 states
> — that the cargo-absent case is asserted on every run, never skipped, never
> lost to a pre-provisioned image — is fully preserved: N1 runs every run at the
> position DOC-1 mandates, and N2 adds the behavioural assertion DOC-1 wanted but
> could not site. If a future reader prefers a single probe, the thing to change is
> the design, not the placement, and §6.1 is the argument to answer.

---

### 7. Toolchain path hand-off

DOC-1 §5.2 records that `tart exec` has **no `--env` flag**, so every guest command
must set its own `PATH` inline; §5.3 forbids depending on guest shell rc files; and
§3.3 requires provisioning to end by "exporting the resolved absolute toolchain
paths". DOC-1 does not say where those paths are written, in what format, or how a
later command composes its prefix. This section does.

#### 7.1 What provisioning writes, and where

At the end of `provision_guest()`, provisioning writes **in the guest**:

```
/Users/admin/.vmtest/toolchain.tsv
```

Same `key<TAB>value` format as §3.2 and §8.2. Contents, with the values measured
during research:

```
guest_home	/Users/admin
cargo_bin	/Users/admin/.cargo/bin
mise_shims	/Users/admin/.local/share/mise/shims
mise_bin	/opt/homebrew/bin/mise
base_path	/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
guest_path	/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin
rustc_version	1.91.1
```

Every path above is measured, not assumed:
`vm-install-probe-findings.md:139` records the working guest PATH verbatim as
`/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin`;
`:213` records the bare non-interactive base PATH; `:51` records
`mise -> /opt/homebrew/bin/mise`; `:102` records rustc `1.91.1`.

**Ordering is load-bearing, not cosmetic.** `~/.cargo/bin` must precede the mise
shims directory. The research states the reason directly: mise's rust backend
**delegates to rustup** — it downloads `rustup-init` and mise's own rust entry is a
`(symlink)` (`vm-install-probe-findings.md:112-114`, `:146`). Putting the real
rustup shims first is what allows rustup's *directory-based* `rust-toolchain.toml`
resolution to work, which is precisely the mechanism DOC-1 §8.4 depends on for its
per-build-step `rustc --version` assertion. Reverse the order and §8.4's assertion
silently stops measuring what it claims to measure.

#### 7.2 Why a guest file *and* a host copy

Provisioning runs in the guest, so it can only write in the guest. The driver runs
on the host and must compose prefixes, so it needs the values host-side. The driver
therefore reads the file back across `tart exec` immediately after provisioning and
stores it at `$VMTEST_RUNDIR/toolchain.tsv` (§4.3):

```sh
vm_exec_raw "$VMTEST_VM" 'cat /Users/admin/.vmtest/toolchain.tsv' > "$VMTEST_RUNDIR/toolchain.tsv"
```

This round-trip is safe because DOC-1 §5.1 records that `tart exec` keeps stdout
and stderr **separate** and does not truncate at volume (200,000 lines passed
intact, `vm-install-probe-findings.md:179`). A seven-line TSV is not a stress case.

The guest copy is kept as well, because it is what makes a `--keep` VM inspectable
by a human who wants to reproduce a failing command by hand.

#### 7.3 How each subsequent call composes its prefix

**Composition happens in exactly one place: `vm_exec` in `lib/vm.sh`.** Scenarios
never build a prefix and never see one. This follows DOC-1 §3.2 — every guest-OS
assumption lives in `vm.sh` and nowhere else — and it is what stops a single
forgotten prefix from producing a silent `127` three months from now.

The prefix is not only `PATH`. Two other values must ride along on every build
command, and both are measured requirements:

- `CARGO_TARGET_DIR` — DOC-1 §8.6 mandates a single shared target directory across
  all crates, and DOC-1 §9's full-stack extrapolation is invalid without it.
- `SKIP_UI_BUILD=1` — the K3 build was measured under it
  (`vm-install-probe-findings.md:934`), and `trusty-search/build.rs:45-54` returns
  early on it (`devils-advocate-review.md:40-41`).

So the composed value is a **guest environment prelude**, held in the global
`VMTEST_GUEST_ENV`:

```sh
VMTEST_GUEST_ENV='PATH=/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin; export PATH; CARGO_TARGET_DIR=/Users/admin/vmtest-target; export CARGO_TARGET_DIR; SKIP_UI_BUILD=1; export SKIP_UI_BUILD;'
```

`VMTEST_GUEST_ENV` has **two lifetimes**:

1. **Before provisioning** it is initialised to the measured *base* form —
   `base_path` only, no cargo, no mise, no cargo variables. This is what N1 (§6.2)
   runs under, and it is what makes N1 meaningful.
2. **After provisioning** `provision_guest()` overwrites it with the full form
   above.

It is the only global that changes after preflight (§12.3).

#### 7.4 Worked example — a fully-formed `tart exec` invocation

The rustc-version assertion DOC-1 §8.4 requires immediately before a build of
`trusty-git-analytics` — the crate whose `rust-toolchain.toml` pins `channel =
"stable"` and resolves to 1.97.1 inside the crate directory versus the
workspace-pinned 1.91.1 at the root (DOC-1 §8.4, measurement K5):

```sh
tart exec vmtest-20260731T142233Z-48213 /bin/sh -c \
'PATH=/Users/admin/.cargo/bin:/Users/admin/.local/share/mise/shims:/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin; export PATH; CARGO_TARGET_DIR=/Users/admin/vmtest-target; export CARGO_TARGET_DIR; SKIP_UI_BUILD=1; export SKIP_UI_BUILD; cd /Users/admin/vmtest-src/crates/trusty-git-analytics && rustc --version'
```

Four details in that line are deliberate:

- **`/bin/sh -c`, never `-lc`.** A login shell would read rc files, and DOC-1 §5.3
  forbids depending on them. Using `-l` would make the harness pass for the wrong
  reason and hide exactly the failure mode that broke a golden image.
- **`export` after each assignment.** `VAR=x cmd` prefixing works for a single
  command; these must survive into `cargo`'s child processes, so they are exported.
- **`cd` *into the crate directory* before `rustc --version`.** rustup resolves by
  current directory (DOC-1 §8.4). The assertion is worthless run anywhere else,
  which is exactly why DOC-1 requires it adjacent to the build rather than once at
  provisioning time.
- **`&&`, not `;`, between `cd` and the command.** A failed `cd` must not run the
  command in the wrong directory. Exit codes propagate exactly through `tart exec`
  (DOC-1 §5.1), so the failure reaches the host intact.

---

### 8. Configuration and tunables

DOC-1 gives `--cpu 8 --memory 16384` (§8.5), the base image name (§4.1), and the
guest sizing rationale as prose literals. This section decides where they live.

#### 8.1 The decision

**Three tiers, in increasing precedence:**

1. **Checked-in project defaults** — `vmtest-harness/vmtest.defaults`. The values
   the project asserts are correct, reviewable in a diff.
2. **Environment-variable overrides** — `VMTEST_*`. Per-host, per-invocation, never
   checked in.
3. **CLI flags** — for the two or three values an operator changes often.

**Why not hardcoded.** DOC-1 §8.5's sizing is grounded in a host with 18 logical
cores and 64 GB (`vm-install-probe-findings.md:842-843`). A maintainer on an
8-core machine cannot run an 8-vCPU guest comfortably, and §8.4 below requires the
harness to *warn* about exactly that situation — which would be an absurd thing to
warn about while offering no way to act on it. Hardcoding makes the harness usable
on one class of machine.

**Why not env-vars alone.** Env-var-only configuration has no reviewable record.
The project's position that 8 vCPU / 16 GB is *the* measured sizing is a claim worth
holding in git, next to the digest pin that is held there for the same reason (§3.1).

**Why a file is cheap here.** The parser already exists — §3.2 and §7.1 use the
same `key<TAB>value` TSV — so the marginal cost of a third file in that format is a
few lines of `awk`, not a configuration subsystem. Under the bash 3.2 constraint
(§Shell discipline) that reuse is the difference between a config format and a
config problem.

#### 8.2 `vmtest-harness/vmtest.defaults` — complete example

```
# vmtest-harness project defaults.
# Format: key<TAB>value. '#' comments and blank lines ignored. Unknown keys are an error.
# Override per-host with the matching VMTEST_* environment variable; never edit this
# file to suit one machine.

# --- guest sizing (DOC-1 §8.5; measured at vm-install-probe-findings.md:841-843) ---
cpu	8
memory_mib	16384
disk_gib	100

# --- guest paths (measured: vm-install-probe-findings.md:139) ---
guest_home	/Users/admin
guest_src_dir	/Users/admin/vmtest-src
guest_target_dir	/Users/admin/vmtest-target

# --- host capacity preflight (DOC-1 §4.1 gap; see §8.4) ---
host_min_memory_gib	24
host_warn_physical_cores	8

# --- polling and watchdogs, seconds (see §10) ---
boot_ready_timeout	150
boot_ready_interval	2
stopped_timeout	120
stopped_interval	1
provision_timeout	300
install_timeout	2700
health_timeout	60
health_interval	1

# --- source (DOC-1 §6.2) ---
repo_url	https://github.com/bobmatnyc/trusty-tools.git
default_branch	main
```

**Override mapping is mechanical:** key `boot_ready_timeout` ← `VMTEST_BOOT_READY_TIMEOUT`.
Uppercase the key, prefix `VMTEST_`. No table to maintain and no key that can be
overridable-in-principle but forgotten in practice.

**CLI flags** exist only for `--cpu`, `--memory`, `--runid`, `--keep`, and
`--dry-run`. Everything else is file-or-env. Adding a flag per tunable would give
the driver a surface larger than its behaviour.

#### 8.3 Precedence and reporting

Defaults file → env var → CLI flag, last wins. The driver prints the **effective**
configuration at the top of every run, marking each value's origin
(`default` / `env` / `flag`). A run whose log does not state its own sizing cannot
be compared against DOC-1 §9's cost baseline, and comparing against that baseline
is most of what the numbers are for.

#### 8.4 Host-capacity preflight — closing DOC-1 §4.1's flagged gap

DOC-1 §4.1 flags host capacity as underspecified and recommends asserting available
memory and warning at ≤8 physical cores. Specified:

| Check | Command | Threshold | Outcome |
|---|---|---|---|
| Total physical memory | `sysctl -n hw.memsize` | `< host_min_memory_gib` (default 24 GiB) | **FAIL, exit 10** |
| Approx. available memory | `vm_stat` — (free + inactive + speculative) × page size | `< memory_mib` | **WARN** |
| Physical cores | `sysctl -n hw.physicalcpu` | `<= host_warn_physical_cores` (default 8) | **WARN** |
| Requested vs available cores | `sysctl -n hw.physicalcpu` | `cpu > hw.physicalcpu` | **WARN** |

**Why total memory hard-fails but available memory only warns.** Total physical
memory is an unambiguous number: a 16 GiB host cannot host a 16 GiB guest plus
macOS, full stop, and proceeding produces a failure that will look like a harness
bug. "Available" memory on macOS is not a comparably solid number — memory
compression and the unified buffer cache mean the `vm_stat` arithmetic above is an
approximation, and a host with little nominally-free memory may run the guest
perfectly well. Hard-failing on a soft number would block legitimate runs. This
split is a **judgment call**; the research measured host capacity exactly once,
incidentally, at 18 logical cores / 64 GB (`vm-install-probe-findings.md:842-843`),
and no threshold was measured at all.

The **24 GiB** default is likewise a judgment call: 16 GiB guest + 8 GiB for the
host, the smallest headroom that does not obviously thrash. It is not measured, it
is deliberately conservative, and it is a tunable precisely because it is a guess.

Core count uses `hw.physicalcpu`, not `hw.ncpu`. On Apple silicon `hw.ncpu` counts
efficiency cores, which do not contribute to a build the way the measured 8-vCPU
guest's cores did — DOC-1 §8.5 shows vCPU count dominating every other lever, so
counting the wrong cores would produce a reassuring warning-free run on a machine
that will be slow.

---

### 9. `expected-binaries.tsv` schema

DOC-1 §7.2 establishes the table as the single authoritative expectation source and
gives a seed table of eight rows (seven until D3 was widened on 2026-07-31).
DOC-1 §7.2's own note flags the `--check-table`
diff source of truth as unnamed and recommends the `[[bin]]` targets. This section
gives the real schema, the real seed content enumerated from the workspace, and
confirms the diff source.

#### 9.1 Format and columns

Tab-separated, one header row, `#` comments permitted, `LF` line endings.

| # | Column | Meaning |
|---|---|---|
| 1 | `package` | `[package] name` — **the key**, see §9.2 |
| 2 | `crate_dir` | directory under `crates/`, needed for `cargo install --path` |
| 3 | `binary` | `[[bin]] name` — what must land on `PATH` |
| 4 | `bin_path` | `[[bin]] path` — lets `--check-table` detect a moved target |
| 5 | `req_features` | `[[bin]] required-features`, `-` if none — see §9.4 |
| 6 | `in_scope` | `yes` \| `no` — is this binary in the harness's D3 scope |
| 7 | `expect_a` | `present` \| `absent` \| `-` (pattern (a), released) |
| 8 | `expect_b` | `present` \| `absent` \| `-` (pattern (b), branch) |
| 9 | `expect_c` | `present` \| `absent` \| `-` (pattern (c), local source) |

`-` in `expect_*` is only valid where `in_scope` is `no`.

> **Nine columns, and no version column — deliberately.** *(Amended 2026-07-31.)*
> §1.2's version cross-check previously named a `tsv_version(...)` accessor over
> this file. This schema has never had a version column and is not gaining one; the
> cross-check now reads `cargo metadata` from the source tree the scenario installed
> from, per §1.2's amendment. A version belongs to the class §9.6 excludes from this
> file in the other direction: it is a *fact about the workspace*, derivable and
> release-volatile, and a hand-maintained copy of it would be stale one release
> later.

**Why an `in_scope` column rather than two files.** `--check-table` must diff
against **every** `[[bin]]` in the workspace or it cannot detect a newly added
binary — a binary absent from a scope-only file is indistinguishable from one that
was never in scope. Carrying every target with a scope flag lets one file serve
both jobs: the oracle filters to `in_scope=yes`, the differ reads all rows.

#### 9.2 The `tga` key question — resolved

DOC-1 D3 flags the package-name/directory-name discontinuity on
`crates/trusty-git-analytics`, whose published package and binary are both `tga`,
and warns that "scenario and table code must not assume directory name == package
name". Confirmed against the manifest: `crates/trusty-git-analytics/Cargo.toml:2`
declares `name = "tga"`.

**The `[package] name` is the key.** `package` + `binary` together form the
composite primary key of the file. Reasons:

- It is what `cargo install <name> --locked` takes, which is pattern (a)'s entire
  interface (DOC-1 §6.3).
- It is what `cargo metadata` reports and what `tctl stack doctor --json` puts in
  `members[].member` (§1.1), so the key joins cleanly to the oracle's other input.
- The directory name is an implementation detail of the workspace layout and is
  only needed for `--path` installs — which is why it is carried as a **separate
  column** rather than being the key. Both are needed; only one can be the key.

There is a **second** directory/package mismatch the design has not previously
noted: the Tauri member at `crates/trusty-agents/ui/src-tauri` declares
`name = "trusty-agents-ui"`. It is out of scope, but `--check-table` must handle it,
which is a further argument for keying on package name.

#### 9.3 Seed content

Enumerated from the workspace manifests. **27** explicit `[[bin]]` targets across 20
manifests, plus one implicit target (§9.4) — **28** rows in total. **Thirteen** rows
are in scope; DOC-1 D3's **eight** crates produce **thirteen** binaries, not eight.
The eight in-scope `crate_dir` values are the eight D3 directories — see §12.5 and
the plan's §F-3 on why the loop over them must deduplicate.

> **Correction, 2026-07-31 — the explicit count read 26.** The prose said "26
> explicit … plus one implicit", i.e. 27, while the block below it has always had
> **28** rows. Re-enumerated: `grep -c '^\[\[bin\]\]'` over every `Cargo.toml` under
> `crates/` yields **27** across 20 manifests — 26 of them in the 19 top-level
> `crates/*/Cargo.toml` manifests, plus one in the non-glob path member
> `crates/trusty-agents/ui/src-tauri/Cargo.toml`. That twentieth manifest is the
> same one §9.2 and §9.6 both single out as the reason to key on package name and
> to read `cargo metadata` rather than a `crates/*/Cargo.toml` glob; the prose
> counted the manifest but not its target. "20 manifests" was right; only the target
> total was wrong, and the table was right all along.

```
package	crate_dir	binary	bin_path	req_features	in_scope	expect_a	expect_b	expect_c
trusty-search	trusty-search	trusty-search	src/main.rs	-	yes	present	present	present
trusty-search	trusty-search	trusty-embedderd	src/bin/trusty-embedderd.rs	-	yes	present	present	present
trusty-memory	trusty-memory	trusty-memory	src/main.rs	axum-server	yes	present	present	present
trusty-memory	trusty-memory	trusty-bm25-daemon	src/bin/bm25_daemon.rs	-	yes	present	present	present
trusty-memory	trusty-memory	trusty-memory-mcp-bridge	src/bin/mcp_bridge.rs	-	yes	present	present	present
trusty-analyze	trusty-analyze	trusty-analyze	src/main.rs	http-server	yes	present	present	present
trusty-code	trusty-code	tcode	src/main.rs	-	yes	present	present	present
trusty-installer	trusty-installer	trusty-installer	src/main.rs	-	yes	present	present	present
trusty-installer	trusty-installer	tctl	src/main.rs	-	yes	present	present	present
tga	trusty-git-analytics	tga	src/main.rs	-	yes	present	present	present
trusty-mpm	trusty-mpm	tm	src/bin/tm/main.rs	cli	yes	present	present	present
trusty-mpm	trusty-mpm	trusty-mpm	src/bin/tm/main.rs	cli	yes	present	present	present
trusty-review	trusty-review	trusty-review	src/main.rs	-	yes	present	present	present
# --- out of scope per DOC-1 D3; carried so --check-table can detect additions ---
trusty-agents	trusty-agents	tagent	src/main.rs	-	no	-	-	-
trusty-agents-ui	trusty-agents/ui/src-tauri	trusty-agents-ui	src/main.rs	-	no	-	-	-
trusty-agents-local	trusty-agents-local	trusty-agents-local	src/main.rs	-	no	-	-	-
trusty-channels	trusty-channels	slack-mcp	src/bin/slack-mcp.rs	-	no	-	-	-
trusty-channels	trusty-channels	telegram-mcp	src/bin/telegram-mcp.rs	-	no	-	-	-
trusty-code-gui	trusty-code-gui	trusty-code-gui	src/main.rs	-	no	-	-	-
trusty-common	trusty-common	candle_metal_bench	src/bin/candle_metal_bench.rs	embedder-candle	no	-	-	-
trusty-common	trusty-common	tickets-mcp	src/bin/tickets_mcp.rs	tickets	no	-	-	-
trusty-console	trusty-console	trusty-console	src/main.rs	-	no	-	-	-
trusty-embedderd-py	trusty-embedderd-py	trusty-embedderd-py	src/main.rs	-	no	-	-	-
trusty-gworkspace	trusty-gworkspace	trusty-gworkspace-mcp	src/bin/trusty-gworkspace-mcp.rs	-	no	-	-	-
trusty-kb	trusty-kb	trusty-kb	src/main.rs	-	no	-	-	-
trusty-mpm-gui	trusty-mpm-gui	trusty-mpm-gui	src/main.rs	-	no	-	-	-
trusty-publish-guard	trusty-publish-guard	publish-guard	src/main.rs	-	no	-	-	-
trusty-sld-lint	trusty-sld-lint	sld-lint	src/main.rs	-	no	-	-	-
```

**Three corrections to DOC-1 §7.2's seed table**, all found by enumerating the real
manifests:

1. **`trusty-memory` ships three binaries, not two.** DOC-1 §7.2 originally listed
   `trusty-memory` and `trusty-bm25-daemon` and **omitted `trusty-memory-mcp-bridge`**
   (`crates/trusty-memory/Cargo.toml`, `src/bin/mcp_bridge.rs` — a deprecation shim
   for pre-#914 users; the manifest comment states `cargo install trusty-memory`
   "produces all three binaries in one command"). The research track had it right —
   `vm-install-testing-trackB-opus.md:777` lists all three — so the omission entered
   at the DOC-1 seed table. This matters directly for DOC-1 §7.4: a Single-Install
   Convention gate that does not know about the third sidecar cannot detect its loss,
   which is the exact regression §7.4 exists to catch. **Corrected in DOC-1 §7.2 and
   §7.4 on 2026-07-31**, along with the project's Single-Install sidecar inventory
   checklist (`.claude-mpm/INSTRUCTIONS.md`), which carried the same omission and is
   the likely origin of it.
2. ~~**`trusty-review` is a publishable crate with a daemon and a `/health`
   endpoint (§1.3), and it is *not* in DOC-1 D3's seven.**~~ **Decided and closed
   2026-07-31 — `trusty-review` is IN scope.** This note asked that the question be
   "decided knowingly rather than by omission"; the owner decided it, and **DOC-1 D3
   is amended to eight crates** with a dated amendment recording the reasoning. Its
   row above now carries `in_scope=yes` with `present` in all three `expect_*`
   columns, on this evidence: `crates/trusty-review/Cargo.toml` declares
   `publish = true` **explicitly**, and `cargo search trusty-review --limit 5`
   returns `trusty-review = "0.10.1"`, so pattern (a) can install it. It has exactly
   **one** `[[bin]]` (`name = "trusty-review"`, `path = "src/main.rs"`,
   `required-features = []`, `Cargo.toml:16-19`), so it adds one in-scope row and
   **no** `verify_single_install` call — see §12.5. Its `/health` route
   (`crates/trusty-review/src/service/mod.rs:130`) is gated behind `http-server`,
   which is in the crate's `default` set, so a plain `cargo install` produces a
   binary whose serve path exists; the RC-1 consequence is recorded in §1.3.
3. The `trusty-mpm` rows are discussed in §9.5.

#### 9.4 `req_features` and the implicit target

**Why `req_features` is carried.** Four in-scope binaries are gated behind
`required-features`: `trusty-memory` on `axum-server`, `trusty-analyze` on
`http-server`, and both `trusty-mpm` bins on `cli`. All three features are in their
crate's `default` set today — verified at `crates/trusty-analyze/Cargo.toml:120-125`
and `crates/trusty-memory/Cargo.toml:172-177`, whose comments state explicitly that
the default exists so `cargo install` "keeps building exactly as before". So a
plain `cargo install` produces them.

That is a *current* property, not a guarantee. If a future change drops one of
these from `default`, `cargo install` **succeeds and silently produces no binary** —
a green install with a missing daemon, which is precisely the DOC-1 §7.4 failure
mode ("still installs successfully, still passes a naive smoke test, leaves the
user with a stack that cannot start its daemons"). Carrying the column lets
`--check-table` flag the day a gating feature leaves `default`.

**Still four after the D3 amendment, and `trusty-review` shows why the column means
exactly what it says.** *(Added 2026-07-31.)* `trusty-review` joined the in-scope set
with `required-features = []`, so it is not a fifth gated binary and its row reads
`-`. Its `/health` route *is* compiled only under `http-server` — but that gates the
*code inside* the binary, not the *target*, so cargo builds and installs
`trusty-review` regardless and `req_features` is correctly `-`. The column records
`[[bin]] required-features` and nothing else; reading it as "features this binary's
behaviour depends on" would be a different column with a different source of truth,
and `--check-table` could not derive it.

**The implicit target.** `crates/trusty-agents-local` has a `src/main.rs` and **no
`[[bin]]` section**, so cargo auto-infers a binary named `trusty-agents-local`. It
is out of scope, but `--check-table` must enumerate implicit targets or it will
report a spurious deletion. Deriving the table from `cargo metadata` rather than by
parsing manifests handles this for free, which is §9.6's argument.

#### 9.5 `trusty-mpm` — the DOC-1 premise that did not hold, and its resolution

**Resolved 2026-07-31. DOC-1 D2 has been amended; this section records how.**

The original DOC-1 D2 stated that `trusty-mpm` "carries `publish = false` and is
therefore not on crates.io", and D3, §7.2, and §7.5 all built on that.

**`crates/trusty-mpm/Cargo.toml` contains no `publish` key.** The `[package]` table
(lines 1–13) has `name`, `version`, `edition`, `rust-version`, `license`,
`repository`, `description`, `readme`, `homepage`, `keywords`, `categories`,
`exclude` — and no `publish`. Cargo's default is `publish = true`. Every textual
occurrence of "publish" in that manifest is a **comment about other crates**, and
those comments say the opposite of the original premise: lines 108–111 and 290–291
explain that the Tauri GUI is kept as a separate `publish = false` crate
specifically so that `trusty-mpm` itself "publishes cleanly to crates.io".
`.github/workflows/e2e-docker.yml:66` likewise offers a "latest published"
`trusty-mpm` version as an input.

**Confirmed against the registry, 2026-07-31.** `cargo search trusty-mpm --limit 5`
returns `trusty-mpm = "1.0.2"`. The crate is published. The manifest reading and the
registry agree.

**What changed in the table.** An earlier revision of this document declined to
change DOC-1 D2 and kept `expect_a = absent` on both `trusty-mpm` rows, reasoning
that `absent` is an asserted expectation rather than a silent skip and would
therefore surface the discrepancy loudly on the first pattern-(a) run. That was the
right call for a document that could only flag the premise. It is the wrong call now
that the premise has been checked and D2 amended: an assertion known in advance to
be false is not a safety net, it is a scheduled failure. Both `trusty-mpm` rows
above now carry **`expect_a = present`**, matching DOC-1 D2 and §7.5 as amended, and
all **thirteen** in-scope binaries are expected present under all three patterns
(twelve when this paragraph was written; the thirteenth is `trusty-review`, added
by the D3 amendment of the same date — §9.3 note 2).

The `expect_*` columns and the pattern-aware oracle **stay** regardless. With the
gap dissolved no in-scope row currently diverges across patterns, but the columns
are the recording mechanism for the next legitimate divergence, and collapsing them
would mean re-inventing them.

#### 9.6 `--check-table` self-diff

**Source of truth: confirmed as DOC-1 §7.2 recommends** — the `[[bin]]` targets
declared across the workspace. DOC-1's reasoning holds without amendment: it is the
same thing `cargo install` acts on, and deriving from a second document would
recreate the staleness problem the table exists to solve (DOC-1 §7.2 cites
`docs/reference/release-workflow.md` as already stale).

**One refinement:** read them via `cargo metadata --no-deps --format-version 1`,
not by parsing `Cargo.toml` files. Three reasons, each a real defect avoided:
`cargo metadata` reports **implicit** targets (§9.4's `trusty-agents-local`, which
manifest-parsing misses entirely); it resolves workspace-inherited fields; and it
enumerates the non-`crates/*` path member `crates/trusty-agents/ui/src-tauri` that a
`crates/*/Cargo.toml` glob would skip. DOC-1 §7.2 already offers `cargo metadata` as
an equivalent — it is not merely equivalent, it is strictly better.

**Algorithm:**

```
1. actual   := { (package, crate_dir, binary, bin_path, req_features) }
               from `cargo metadata --no-deps`, targets with kind == "bin"
2. declared := rows of expected-binaries.tsv, columns 1..5
3. key      := (package, binary)
4. ADDED    := keys in actual  \ declared      -> a new binary nobody declared
   REMOVED  := keys in declared \ actual       -> a binary that vanished
   CHANGED  := keys in both, differing in crate_dir | bin_path | req_features
5. RENAMED  := heuristic — one ADDED and one REMOVED sharing the same package
               and bin_path. Reported as a rename *suggestion* only; never
               applied automatically.
6. exit 0 if all sets empty; otherwise print each finding and exit 60.
```

Exit **60** (verification, §2) rather than a bespoke code: `--check-table` is the
oracle's input being wrong, which is a verification failure that happens not to need
a VM.

`--check-table` runs on the host with no VM (DOC-1 §3.1), needs only `cargo` and
`jq`, and should be run whenever a crate gains or loses a binary. It does **not**
auto-fix. A table that rewrites itself to match reality asserts nothing — the
human edit is the review step, and removing it would turn the authoritative
expectation source into a mirror.

Note that `--check-table` compares columns 1–5 only. `in_scope` and the three
`expect_*` columns are human judgments about the harness's scope, not facts about
the workspace, and nothing can derive them.

---

### 10. Polling timeouts and backoff

DOC-1 §4.3 mandates "poll for the observable condition, never sleep a fixed
interval" and §8.1 generalises it to "a tart exit code is not a completion signal".
No interval or maximum is given anywhere in DOC-1. This section supplies them.

**Two mechanisms, and the distinction matters.** A **poll** watches for an
observable condition the harness cannot otherwise be notified of. A **watchdog**
bounds a blocking call that *will* return, and whose exit code propagates exactly
(DOC-1 §5.1) — provisioning and builds are watchdogs, not polls, and describing
them as polling would invite someone to add pointless polling around a synchronous
`tart exec`.

#### 10.1 Poll sites

| Site | Observable condition | Interval | Maximum | Grounding |
|---|---|---|---|---|
| **boot-ready** | `tart exec <vm> /bin/sh -c 'exit 0'` returns 0 | 2 s | **150 s** | **Two sources, and they disagree.** *Original research:* 34.4 s first boot (`vm-install-probe-findings.md:378`, `BOOT_TO_READY_MS=34414`); 18.0 s subsequent (`:483`, `COLD_BOOT_TO_READY_MS=17993`). *Phase 3, 2026-08-02:* **24 s, 28 s, 33 s, 33 s** on four consecutive cold clones of a `stopped` `tahoe-base` (MANIFEST Phase 3, Measurement 1) — **the 18.0 s "subsequent" figure did not reproduce on that host**; every boot looked like the 34.4 s *first*-boot reading. The 150 s maximum is now sized against **the slowest observed boot, 33 s — ~4.5×** — and no longer against `:483`. *(Amended 2026-08-02.)* |
| **`wait_for_stopped()`** | `tart list` reports state `stopped` | 1 s | **120 s** | `tart stop` asynchrony measured in K1/K1b/K1c; poll overhead measured negligible (`../01-research/logs/k1d-state-poll-overhead.log`). Maximum is a **judgment call** — worst-case flush duration was never measured. The stop this waits on is issued by `vm_request_stop` (§12.2), whose exit code is discarded. |
| **daemon health** | `GET /health` returns 200 with parseable JSON (§1.3) | 1 s | **60 s** | **Wholly unmeasured.** `launchctl bootstrap` under `tart exec` is confirmed to work (DOC-1 §8.7) but daemon time-to-ready was never timed. |

`vm_wait_ready` polls at a **fixed** 2 s interval, not with exponential backoff.
Backoff would be a pessimisation here: the distribution is tight and known
(~24–34 s observed, *(range corrected 2026-08-02 from "~18–35 s")*), so backoff's
only effect is to overshoot a ready guest by however far
into a long interval it happens to land, in exchange for saving a handful of
`tart exec` calls whose cost was measured as negligible (K1d).

#### 10.2 Watchdog sites

| Site | Blocking call | Timeout | §8.2 key | Grounding |
|---|---|---|---|---|
| `tart clone` | `vm_clone` | **60 s** | **none — built-in** | 0.31 s measured, APFS CoW (`vm-install-probe-findings.md:875`). ~190× — deliberately loose because §3.3's by-construction variant may pull an image on the first run, which is unmeasured. |
| `provision.sh` | one `tart exec` | **300 s** | `provision_timeout` | 30.079 s measured (`vm-install-probe-findings.md:857-858`, `PROVISION_MS=30079`). ~~10×~~ **2.19× the slowest of thirteen harness observations (137 s)** — *(re-grounded 2026-08-04, P8-T2; see the amendment below)*. Network-bound (rust toolchain download alone is 20.8 s, `:854`); a slow link is not a defect. |
| single-crate install | one `tart exec` per crate | **900 s** | **none — built-in** | 112 s for `trusty-search`, 409 dependency crates compiled, 8 vCPU (`vm-install-probe-findings.md:934-935`); 131 s for `cargo install tga --locked` at 4 vCPU (DOC-1 §9). ~~~8×~~ **6.2× the largest single-crate install the harness has observed (146 s, `trusty-search`, MANIFEST Phase 5 Measurement 2)** — *(re-grounded 2026-08-04, P8-T2; the multiple moved, the budget did not)*. |
| full-stack scenario | scenario wall clock | ~~**2700 s** (45 min)~~ **1800 s** (30 min) | `install_timeout` | ~~DOC-1 §9 extrapolates 4–8 min **and labels it low-confidence, for six crates when D3 now scopes eight**. ~5.6× the upper bound.~~ **TIGHTENED 2700 → 1800 at plan P5-T8 on measurement, 2026-08-02.** Scenario wall clock 640 s / 748 s / 610 s across three pattern-(c) runs (MANIFEST Phase 5 Measurement 1); **2.4× the slowest**. Confirmed unchanged at P8-T2 against (b) 650 s and (a) 511 s total. |
| guest `git clone` (pattern b) | one `tart exec` | **300 s** | **none — built-in** | ~~50.131 s measured (`vm-install-probe-findings.md:942`, `GIT_CLONE_MS=50131`). ~6×.~~ **The research figure did not reproduce: measured 4 s, a 12.5× overstatement** (MANIFEST Phase 6 Measurement 1). The budget is **unchanged** and is now **~75×** the measured value — *(re-grounded 2026-08-04, P8-T2; see the amendment below)*. |

> **AMENDMENT 2026-08-04 (plan P8-T2) — three of these five rows now cite a
> harness measurement instead of, or beside, a research baseline. NO BUDGET IN
> THIS TABLE CHANGED IN THIS PASS.**
>
> **What changed and why.** P8-T2 owns grounding the timeouts, and the honest
> finding is that **several research baselines this table was built on did not
> reproduce on this host**. Each row is re-grounded on the observations rather
> than on the baseline, and each states which figure its multiple is now sized
> against. Where a budget still holds with margin it is **left alone and the
> margin is stated** — no maximum was invented, and none was tightened on a
> single reading.
>
> - **`provision.sh` — the multiple was wrong by 4.6×, and it is the row that
>   matters most.** Thirteen observations across Phases 1–8: **16, 17, 18, 19,
>   24, 35, 38, 40, 52, 64, 78, 97, 137 s** against the 30.079 s baseline. The
>   spread is **8.6× end to end on one host**, which is the finding — not the
>   mean. Plan P3-T2's acceptance note bounds provisioning at 3× baseline (90 s);
>   **two of thirteen runs exceeded it** (97 s at Phase 1 run 2, 137 s at Phase 5
>   run B), which is why that note is recorded as non-fatal. Against the slowest
>   observed run the 300 s budget is **2.19×**, not the "10×" this row claimed.
>   That is much thinner than the doc implied and it is still real margin, so the
>   budget stands and the multiple is corrected. **This is the one row where a
>   future host could plausibly hit the budget without being hung.**
> - **guest `git clone` — a 12.5× overstatement.** `GIT_CLONE_MS=50131` is
>   retained above as the budget's *provenance*; the measurement is **4 s** for
>   5,540 files. Plan P6-T4 predicted a ~50 s (b)−(c) transport delta from the
>   research figure and the observed delta is **~0 s**. One reading on one host is
>   not grounds to tighten a network-bound budget, so 300 s stands at ~75×.
> - **single-crate install — the baseline held.** 146 s worst observed against a
>   112 s baseline (1.3×), across 24 installs in three runs. 900 s stands.
> - **`tart clone` — untouched.** Still 0.31 s measured, still ~190×, and §10.3's
>   recorded residual (an unmeasured first-use image *pull* could blow 60 s) is
>   **not closed by anything the harness has run**: every run since has cloned a
>   local `tahoe-base`, so the pull path has still never been exercised.
> - **full-stack scenario — already tightened at P5-T8; re-confirmed, not
>   re-cut.** Five full-stack runs now span **511–919 s** total wall clock across
>   all three patterns and no scenario has come within 2.4× of 1800 s.
>
> **§10.1 is not re-grounded here.** Its boot-ready row was amended at source on
> 2026-08-02 on Phase 3's evidence and P8-T2 leaves it alone by explicit
> instruction; its daemon-health row is still **wholly unmeasured** and P8-T2 says
> so in `vmtest.defaults` rather than dressing 60 s up as grounded.

**The `§8.2 key` column is new.** *(Amended 2026-08-02.)* It records a fact that
was previously implicit and, once made explicit, resolves a contradiction between
this section and §10.3 — see §10.3's amendment. **Three of these five budgets have
no key and were never given one.** All six §10.1 poll parameters do have keys
(`boot_ready_timeout`/`_interval`, `stopped_timeout`/`_interval`,
`health_timeout`/`_interval`), which is why the gap reads as an oversight until
you count it: it is not one site missing a key, it is the entire watchdog tier
being only 2/5 covered. Note also that `install_timeout` (2700) is the **full-stack
scenario** budget despite its name; the single-crate 900 s budget is a different
site and has no key at all.

**Why the full-stack multiple was so loose, stated as reasoning rather than
precision.** *(Retained as written; the paragraph describes the **2700 s**
original, which P5-T8 replaced with 1800 s on 2026-08-02. Its closing request —
"it should be tightened once the first pattern-(c) full-stack run is timed" — is
the thing that was acted on, so the paragraph is kept rather than deleted: it is
the reasoning the tightening had to satisfy. Note also that `install_timeout` was
tightened in `vmtest.defaults` at P5-T8 but this table still read 2700 s until
P8-T2 corrected it on 2026-08-04 — a doc-vs-shipped-config drift of two days,
recorded rather than quietly fixed.)* DOC-1 §9 is explicit that 4–8 minutes is an
extrapolation, that it was
computed for six crates against what is now an **eight**-crate scope (six when the
range was computed, seven after the D2 reversal, eight after the 2026-07-31 D3
amendment), and that it should be treated
as a low-confidence planning estimate until a full-stack run is actually timed. A
tight timeout over a low-confidence estimate does not enforce a budget; it
manufactures flaky failures that get "fixed" by raising the timeout. 45 minutes is
set to catch a genuinely hung run and nothing else. **It should be tightened once
the first pattern-(c) full-stack run is timed** — DOC-1 §9 already asks for that run
to be recorded as the replacement measurement, and this timeout is a second
consumer of it.

#### 10.3 Timeout behaviour

Uniform across every site:

1. **No retry, ever.** The timed-out operation is not re-attempted, and the timeout
   is not extended in-run. This follows DOC-1 §4.1's rule for preflight refusals
   — "do not attempt to stop it, do not attempt to resume it, do not retry" —
   generalised. A retry that succeeds converts a reproducible failure into an
   intermittent one, and DOC-1 §8.2 shows a case (`tart run` re-entering a failing
   restore) where retrying is structurally incapable of helping.
2. **Classify by phase.** Exit with the §2 code for the phase that timed out — a
   boot-ready timeout is **20**, a provisioning timeout **40**, an install timeout
   **50**.
3. **Report the budget.** The message states the condition waited for, the elapsed
   time, the configured maximum, and **either** the `vmtest.defaults` key that
   changes it **or**, for a built-in budget, that no key exists — see the
   amendment below. It is never silent about which of the two it is.
4. **Teardown still runs.** The cleanup trap fires as on any other exit path, so a
   timed-out run does not leave a VM behind — unless `--keep` was requested.

**Clause 3 as originally written was unsatisfiable, and built-in budgets are now
permitted.** *(Amended 2026-08-02.)* The original clause required every timeout
message to name "the `vmtest.defaults` key that changes it". §10.2 budgets
`tart clone` at 60 s and §8.2 defines no key for it, so for that site the two
sections could not both be satisfied — a message could name a key that does not
exist, or omit the key and violate this clause. Neither is acceptable.

**Resolution: amend this clause, rather than add keys to §8.2.** Stated as
reasoning, because the other direction was live and is the one a reader will
expect:

- **The gap is systemic, not a single omission.** §10.2's new key column shows
  **three** of five watchdog sites with no key — `tart clone` (60 s), single-crate
  install (900 s), and guest `git clone` (300 s). Adding `clone_timeout` would fix
  the one site that happened to be noticed and leave the other two producing
  exactly the same contradiction. Fixing the class by adding keys means adding
  three now and one for every watchdog site added later.
- **§8.2 has already argued this, about flags.** "Adding a flag per tunable would
  give the driver a surface larger than its behaviour." The same reasoning applies
  a tier down: a config key per internal budget makes the configuration file a
  catalogue of the harness's internals, and every key is a promise that some
  operator's tuned value will keep working. §8.1's case for the file is that its
  contents are *claims worth holding in git and reviewing in a diff* — 8 vCPU,
  16 GiB, the digest pin. "How long a CoW clone may take before we call it hung"
  is not that kind of claim.
- **A tight budget is what needs tuning, and these are not tight.** The keyed
  budgets bound things that legitimately vary per host and per link — boot,
  provisioning, a full-stack build. The three built-ins are set at ~190×, ~8× and
  ~6× their measured values; they exist to catch a genuinely hung call, not to
  enforce a schedule. A budget nobody should be near is a budget nobody needs to
  tune, and offering the knob invites raising it in response to a real hang.
- **§8.2's file is copied verbatim into `vmtest-harness/vmtest.defaults`** (plan
  P2-T2), so adding a key is not a documentation edit — it changes shipped
  configuration, the driver's key list, and the site's implementation. That is not
  a reason on its own, but it means the burden of proof sits with the addition,
  and the three points above do not meet it.

**How a built-in budget's message reads.** It states the condition, the elapsed
time, the budget, **the §10.2 citation the budget comes from**, and explicitly
that **no `vmtest.defaults` key changes it** — it must not name a key that does
not exist, and it must not go quiet and leave the operator searching §8.2 for one.
The citation is what replaces the key: it points at the row where the number and
its grounding live, so a budget that is genuinely wrong is changed **in the
design, with its measurement**, which is where a 190× multiple should be argued.
`vm_clone`'s implemented message is the reference form:

```
tart clone '<src>' -> '<dst>' failed or exceeded its 60 s budget
(DOC-2 §10.2; no vmtest.defaults key exists for this budget). Output: ...
```

> **Residual, recorded rather than closed.** `tart clone`'s 60 s is the one
> built-in with a plausible path to being too tight: §3.3's by-construction
> variant may **pull an image** on first use, which is unmeasured, and a slow link
> would blow 60 s while the call is not hung at all. This amendment does not
> pretend that away. If that path is ever taken and the budget is hit, the answer
> is to re-ground the number against a measured pull — and *then*, if it turns out
> to vary by host the way provisioning does, it has earned a key on the same
> evidence every other key rests on. Revisit with P8-T2, which already revisits
> the unmeasured daemon-health maximum.

#### 10.4 Implementation note — there is no `timeout(1)` on macOS

`timeout` is GNU coreutils and is **not** present on a stock macOS host. A watchdog
must therefore be built from shell primitives: background the command, record its
PID, poll `kill -0 <pid>` at the site's interval until the deadline, then `kill`
and reap. Reaching for `timeout` or `gtimeout` would add a Homebrew dependency to a
harness whose host requirements are otherwise `tart`, `git`, `jq`, and `cargo` — and
would fail on a clean machine in a way that looks like a harness bug. This is
called out because it is the single most likely thing for an implementer to assume
is available.

---

### 11. Provisioning vs preinstalled `mise`

DOC-1 §3.3 says provisioning "installs `mise`, `rust@1.91`, `uv`, and `gh` in the
guest". The research says three of those four statements are wrong, and getting
this wrong has broken an image before.

#### 11.1 What the research actually records — verified

| Tool | State on `tahoe-base` | Citation |
|---|---|---|
| `mise` | **PREINSTALLED** via Homebrew at `/opt/homebrew/bin/mise`, version 2026.6.0 | `vm-install-probe-findings.md:51`, `:76`, `:866` |
| `gh` | **PREINSTALLED** via Homebrew, 2.93.0 — provisioning cost 616 ms, i.e. a no-op | `vm-install-probe-findings.md:80`, `:856` (`STEP_GH_MS=616`) |
| `rust` / `cargo` / `rustup` | **ABSENT** | `vm-install-probe-findings.md:867` |
| `uv` | **ABSENT** | `vm-install-probe-findings.md:867` |

And the two prohibitions, stated in the research as instructions:

- **`curl https://mise.run | sh` must not be run** — "that would create a second,
  conflicting mise in `~/.local/bin`" (`vm-install-probe-findings.md:77-78`).
  Restated at `:631`: "Do not install mise. It ships with the base image;
  installing it again creates a [conflict]."
- **`mise self-update` hard-fails** on a Homebrew-managed mise; the correct upgrade
  path is `brew upgrade mise` (`vm-install-probe-findings.md:79`).

`git`, `curl`, `node`, `python3`, `cmake` and `brew` are likewise preinstalled
(`vm-install-probe-findings.md:50-65`, `:866`).

#### 11.2 What provisioning does — specified

**Per tool:**

| Tool | Strategy | Command |
|---|---|---|
| `mise` | **detect and reuse. Never install.** | detection below |
| `gh` | **detect and reuse. Never install.** | `command -v gh` under `base_path` |
| `rust@1.91` | **install fresh** | `mise use -g rust@1.91` (measured 20.778 s, `:854`) |
| `uv` | **install fresh** | `mise use -g uv@latest` (measured 7.947 s, `:855`) |

**Detection command for `mise`**, run under the measured base PATH (§7.1):

```sh
tart exec vmtest-<runid> /bin/sh -c \
  'PATH=/bin:/usr/bin:/usr/sbin:/usr/local/bin:/opt/homebrew/bin; export PATH;
   command -v mise && [ ! -e "$HOME/.local/bin/mise" ] && mise --version'
```

Three assertions, all of which must hold:

1. `mise` resolves, and resolves **under `/opt/homebrew/`**. A `mise` found anywhere
   else is not the base image's Homebrew mise.
2. **No second mise exists at `$HOME/.local/bin/mise`** — the exact artefact
   `mise.run` would have created (`:77-78`). Asserting its absence turns "somebody
   ran the forbidden command" from a mystery into a named failure.
3. `mise --version` returns 0.

#### 11.3 Failure behaviour — fail, do not repair

**If detection fails, provisioning exits 40. It does not fall back to installing
mise.** Under no condition does the harness run `curl https://mise.run | sh` or
`mise self-update`.

The reasoning is DOC-1's own, applied one level down. A `tahoe-base` without a
Homebrew mise at `/opt/homebrew/bin/mise` is **not the base image this harness is
pinned to** — it is a drift signal, and §3's whole purpose is to catch drift.
Installing a second mise to paper over it would produce a guest whose toolchain
resolution differs from every measurement in the research, invisibly. DOC-1 §4.1's
rule is exact and applies verbatim: *"Do not attempt to stop it, do not attempt to
resume it, do not retry… an automated 'fix it up and carry on' path is exactly how
a broken image shipped once already."*

**This class of detail is not hypothetical.** DOC-1 §5.3 records a golden image that
shipped with `~/.zshenv` missing, which made `cargo` return **127** under both
`/bin/sh` and `/bin/zsh` (`vm-install-probe-findings.md:180-181`) and presented as
"cargo is not installed". A missing dotfile and a duplicated toolchain manager are
the same category of failure: a provisioning-environment detail that produces a
confident, wrong, silent-looking result.

#### 11.4 `~/.zshenv` — written, never depended on

Provisioning's measured step list includes `STEP_ZSHENV_MS=617` — writing
`~/.zshenv` (`vm-install-probe-findings.md:857`, content at `:132`). DOC-1 §5.3
forbids depending on rc files. Both are correct, and the reconciliation must be
explicit or someone will delete one or trust the other:

> Provisioning **may** write `~/.zshenv` as a convenience for a human who later
> inspects a `--keep` VM interactively. **No harness logic may read it, source it,
> or depend on it having been written.** Every harness command self-prefixes per
> §7. If `~/.zshenv` were deleted from the guest mid-run, every harness assertion
> must still pass — that is the test of whether §7 was implemented correctly, and
> it is worth running once deliberately.

#### 11.5 Amendment to DOC-1 §3.3

DOC-1 §3.3's phrasing — "installs `mise`, `rust@1.91`, `uv`, and `gh`" — is
accurate for `rust@1.91` and `uv`, and **wrong for `mise` and `gh`**, both of which
are preinstalled and must be reused. The measured 30 s provisioning total (DOC-1 §9,
`vm-install-probe-findings.md:858`) already reflects reuse: it is composed of
20.778 s of rust, 7.947 s of uv, 0.616 s of gh-already-present, and 0.617 s of
`~/.zshenv`. A provisioning step that genuinely installed mise and gh would not
cost 30 s. §11.2 is the operative specification; DOC-1 §3.3 should be read through
it.

---

### 12. Scenario ↔ lib calling convention

DOC-1 §3.6 defines a scenario as "a sequence of install steps plus the expectations
that follow from them", composing `lib/` functions with no `tart` calls and no
transport logic, and §12.1 makes that composability the precondition for upgrade
testing. No signature is given for anything.

#### 12.1 Conventions that apply to every `lib/` function

- **Arguments are positional strings.** bash 3.2 has no associative arrays and no
  namerefs (§Shell discipline), so there is no options-hash idiom available.
- **The return channel is the exit status.** `0` = success, non-zero = failure.
- **The value channel is stdout, and carries at most one value.** A function that
  needs to return several values writes a TSV to a path given as an argument
  (§7.1's `toolchain.tsv` is the pattern).
- **Diagnostics go to stderr, always.** stdout must stay clean because §1's oracle
  parses it. DOC-1 §5.1 records that `tart exec` keeps the streams separate, which
  is what makes this discipline possible at all.
- **Functions do not call `exit` — they call `die`,** except where noted.
- **`lib/` files define functions and nothing else.** No top-level statements, no
  `set`, no side effects at source time.

#### 12.2 Module surfaces

**`lib/vm.sh`** — the OS boundary (DOC-1 §3.2). The only file in the harness that
may contain the string `tart`.

| Signature | Returns / emits |
|---|---|
| `vm_require_cli` | 0 if `tart` is on `PATH`, else dies 10 with the host-dependency message |
| `vm_list <out_tsv_path>` | writes one `name<TAB>state<TAB>source` line per `tart list` entry to `<out_tsv_path>`; 0, or dies 10 |
| `vm_clone <src_ref> <vm_name>` | 0, or dies 20 |
| `vm_size <vm_name> <cpu> <mem_mib> <disk_gib>` | 0, or dies 20 |
| `vm_boot <vm_name>` | backgrounds `tart run --no-graphics`; writes the pid to `$VMTEST_RUNDIR/tart-run.pid`; 0 or dies 20 |
| `vm_wait_ready <vm_name> <timeout_s>` | polls §10.1; 0, or dies 20 on timeout |
| `vm_state <vm_name>` | **emits** the state string on stdout |
| `vm_exec <vm_name> <cmd_string>` | runs with `$VMTEST_GUEST_ENV` prefixed; **emits** guest stdout; **returns the guest's exit status verbatim** |
| `vm_exec_raw <vm_name> <cmd_string>` | as `vm_exec` but **no** env prefix — for N1 (§6.2) and for reading `toolchain.tsv` before the prefix exists |
| `vm_exec_stdin <vm_name> <cmd_string>` | as `vm_exec`, piping host stdin through `tart exec -i` |
| `vm_request_stop <vm_name>` | flushes the guest, then issues `tart stop` and **discards its exit code**; always returns 0 |
| `vm_wait_for_stopped <vm_name> <timeout_s>` | polls §10.1; 0, or dies 70 on timeout |
| `vm_assert_stopped <vm_name>` | 0 if state is `stopped`, else dies 10 |
| `vm_delete <vm_name>` | 0, or dies 70 |
| `vm_manual_hint <kind> <vm_name>` | **emits on stderr** the concrete manual commands for a human; `<kind>` is `keep` \| `running` \| `suspended`; always 0 |

**Fifteen signatures, not twelve — enumeration and operator guidance were
missing.** *(Amended 2026-08-02.)* This table carried twelve signatures until
Phase 2 implemented it, and the twelve turned out to cover the **lifecycle of one
VM** completely while covering **enumeration** and **operator guidance** not at
all. Three functions had to exist. Each of them is here to **preserve** DOC-1
§3.2's sole-`tart`-module invariant, not to bend it — every one was added because
the alternative was a `tart` call, or a string naming `tart`, somewhere outside
this file:

- **`vm_require_cli`** — DOC-1 §4.1's first preflight row is "`tart` present on
  `PATH`". The check names the binary, and the driver **may not contain that
  string**, so the check cannot live in the driver. It has to sit behind the
  boundary or the boundary is already broken by the very check that guards it.
- **`vm_list <out_tsv_path>`** — §5.1's four-condition orphan test and DOC-1
  §4.1's stopped-state refusal both require enumerating VMs **and their states**,
  and neither is writable without an enumeration function. It also makes §3.3's
  digest comparison possible from outside this file, because `tart list --format
  json` reports an OCI image's `Name` as `<oci_ref>@sha256:<64 hex>`. Per §12.1
  ("a function that needs to return several values writes a TSV to a path given
  as an argument") it writes a TSV rather than emitting rows on stdout — §12.1's
  one-value stdout rule is **honoured, not excepted**.
- **`vm_manual_hint <kind> <vm_name>`** — three sites must print concrete
  OS-level commands to an operator: §Shell discipline cleanup property 4's
  `--keep` inspection hint, §5.4 row 1's manual commands for a refused `running`
  VM, and §5.4 row 2 / DOC-1 §8.2's `state.vzvmsave` unwedge procedure. All three
  sites are in the driver. Keeping the *text* with the rest of the OS knowledge
  is the narrowest fix; the alternative was to weaken the hints until they no
  longer told a human what to type, which is the same as not having them.

**Anywhere the count "twelve signatures" appears it now reads fifteen** — plan
P2-T4's *Contract* line is corrected in the same change. Opened by Phase 2
([MANIFEST](../03-plan/MANIFEST.md) Phase 2, Deviations item 3), which proposed
writing it back at P8; resolved here instead, at source, because a surface that
is wrong in the spec is re-derived by every later reader.

**`vm_exec` deliberately does not die on non-zero.** It returns the guest's status
so a caller can distinguish "the command failed" from "the harness failed" — which
is exactly what N1 (§6.2) needs, since N1's *expected* result is a non-zero exit.
Callers that require success wrap with `vm_exec ... || die 50 "..."`.

**`vm_request_stop` is the shutdown initiator, and it was missing.**
*(Amended 2026-07-31.)* Before this amendment **nothing in the specified path ever
asked the guest to stop.** `vm_wait_for_stopped` *polls* `tart list` for state
`stopped` (§10.1), and the cleanup rule (§Shell discipline, property 5) ordered it
before `vm_delete` — so as literally written, cleanup polled a still-running VM for
its full 120 s budget and exited **70** on every run, including successful ones.
§10.1's own grounding cell cites "`tart stop` asynchrony measured in K1/K1b/K1c",
which presupposes a `tart stop` that no function in §12.2 issued. It lives in
`lib/vm.sh` because that is the only file permitted to contain the string `tart`
(DOC-1 §3.2).

What it does, in order:

```sh
# 1. flush the guest, from inside the guest — the only measured protection
vm_exec_raw "$vm" '/bin/sync; /bin/sync'     # failure is logged to stderr, not fatal
# 2. ask tart to stop it, and DISCARD the status
tart stop "$vm" >/dev/null 2>&1 || :
```

**Provenance: this implements two of the research procedure's four steps.**
*(Amended 2026-07-31 — the earlier wording claimed the procedure entire.)* The
procedure at `../01-research/vm-install-probe-findings.md:820-831` has four steps:
(1) flush from inside the guest **and confirm it returned** —
`tart exec "$VM" /bin/sh -c 'sync; sync; echo FLUSHED'`; (2) `sleep 10` to settle;
(3) `tart stop "$VM"`; (4) verify the artefact by clone→boot→assert, **not** by
trusting the stop's exit code. `vm_request_stop` implements **steps 1 and 3 only**,
and step 1 not verbatim: it drops the `echo FLUSHED` confirmation, because
`vm_exec_raw`'s failure here is logged to stderr rather than treated as fatal. Steps
2 and 4 are omitted deliberately; the durability paragraph below is why that is
safe. Step 1 is
non-fatal because cleanup runs on every exit path (§Shell discipline), including
ones where the guest is already unreachable; refusing to stop a VM because its
flush failed would leave a VM behind for a reason weaker than the stop itself.

**This does not violate DOC-1 §8.1.** That rule reads "never issue a bare `tart
stop` **and treat its return as completion**", generalised as "a tart exit code is
not a completion signal". The prohibition is on *trusting the return*, not on
issuing the command — the same research that produced the rule ends with a
procedure whose third step is `tart stop`. `vm_request_stop` discards the status
precisely so that nothing downstream can trust it, and `vm_wait_for_stopped` is
what decides the VM stopped.

**Durability is not what this teardown needs, and saying so prevents a false
inheritance.** The research is explicit that polling for `stopped` does **not**
protect a write — "the state flag is not a durability flag"
(`vm-install-probe-findings.md:814-817`). That finding was about *baking*, where
the disk image is the artefact being kept. This harness deletes the VM immediately
afterwards (§Shell discipline, property 5) and keeps it only under `--keep`, which
skips teardown entirely. No path both stops a VM and then relies on its disk, so
guest-write durability is not at stake — which is also why **step 4** has no
analogue here: there is no retained artefact to clone→boot→assert against. The
observable requirement here is only that the VM reach `stopped` before
`tart delete`. The `sync` is retained anyway: it costs one `tart exec` and it is
what the research calls "the only measured protection" (`:818`). **Step 2**, the
10 s settle, is dropped on the same measurement's authority — K1-iii's variant
isolation found that "both a guest-side `sync` and a settle delay were individually
sufficient in the trials run" (`:804`), so the retained half is a sufficient one,
and the settle would cost
10 s on every teardown to buy durability this path does not need.

**If the graceful path fails there is no escalation.** §10.3's "no retry, ever"
applies unchanged: `vm_request_stop` is issued once. If the VM has not reached
`stopped` within `stopped_timeout` (120 s, §8.2), `vm_wait_for_stopped` dies **70**
— "the run's result may have been fine; **the host is not clean**" (§2) — and the
VM is left in place for a human. The harness does **not** kill the `tart run`
process recorded in `$VMTEST_RUNDIR/tart-run.pid`, does not `tart delete` a running
VM, and does not `tart suspend` (DOC-1 §8.2). `vmtest clean` will refuse it in turn
(§5.4: `running`, no live registry entry → exit 10) and print the manual commands.
Force-killing a way to a clean `tart list` would be repairing, which DOC-1 §4.1
forbids in the one place this design is most emphatic about it.

**A guest-side shutdown is forbidden as the initiator.** *(Amended 2026-07-31.)*
This was previously recorded here as a judgment call to be validated on the first
real run. It is now settled, and it is a prohibition — stated in the register DOC-1
§8.1 and §8.2 use, because it is the same kind of rule.

**Rule:** `vm_request_stop`'s flush-then-`tart stop` sequence is the **only**
permitted shutdown initiator. No part of the harness may ask the guest to shut
itself down — no `shutdown -h now`, no `halt`, no `poweroff` — over `tart exec` or
any other channel.

**Evidence:** the guest-side alternative appears exactly once in the research
corpus, in the **superseded** Track A script
(`../01-research/vm-install-testing-trackA-fable.md:299`), where it is issued over
**SSH** — a transport DOC-1 §5.1 excludes outright. It also requires passwordless
`sudo` in the guest, which the research never measured and never recorded as
present on `tahoe-base`. There is therefore no measurement behind it at all, on
either the mechanism or its precondition. Adopting it would be inventing a
mechanism, and this document does not specify mechanisms it cannot ground.

**`lib/provision.sh`**

| Signature | Returns / emits |
|---|---|
| `provision_guest <vm_name>` | runs the §11.2 sequence; writes guest `toolchain.tsv`; copies it to `$VMTEST_RUNDIR`; **sets `VMTEST_GUEST_ENV`**; 0 or dies 40 |
| `provision_detect_mise <vm_name>` | **emits** the resolved mise path; dies 40 per §11.3 |
| `provision_load_toolchain <tsv_path>` | reads the TSV, composes and exports `VMTEST_GUEST_ENV` (§7.3) |

**`lib/source.sh`** — host→guest transport and install steps.

| Signature | Returns / emits |
|---|---|
| `source_deliver_local <vm_name> <host_repo> <guest_dir>` | `git ls-files -co --exclude-standard` \| `tar` \| `vm_exec_stdin` (DOC-1 §6.1); **emits the streamed byte count**, which DOC-1 §6.1 explicitly asks be logged; 0 or dies 50 |
| `source_deliver_branch <vm_name> <repo_url> <branch> <guest_dir>` | guest-side `git clone` + checkout (DOC-1 §6.2); 0 or dies 50 |
| `source_deliver_released <vm_name>` | no-op returning 0 — pattern (a) has no delivery step; exists so scenarios stay symmetric |
| `install_from_path <vm_name> <guest_dir> <crate_dir>` | asserts `rustc --version` first per DOC-1 §8.4, then `cargo install --path`; 0 or dies 50 |
| `install_from_registry <vm_name> <package> [version]` | `cargo install <package> --locked` (DOC-1 §6.3); 0 or dies 50 |

**Rule: both install functions install at *package* granularity — never `--bin`.**
*(Amended 2026-07-31.)* `install_from_path` and `install_from_registry` install a
**package**. Neither may pass `--bin`, and neither may pass `--bins` with a
filter. Until this amendment the only thing standing in the way was the
three-argument arity of `install_from_path` above — an accident of a signature,
not a stated rule, and not something a reader could be expected to read as a
prohibition.

**Why the rule is needed, and why it is the sharper hazard.** `cargo install
--path <dir>` has no per-binary granularity *unless* `--bin` is passed, so the
specified form is already correct. But §9.3's table carries a `binary` column on
**every** row, which makes a "row-faithful" install loop — `cargo install --path
<dir> --bin <binary>` — look like tidying-up rather than a change in meaning. It
is a change in meaning. The `binary` column is the **oracle's** input; it is
never the **installer's**.

DOC-1 §7.4's Single-Install Convention gate asserts exactly one thing: that **one
package-granular install yields every sidecar**. A per-binary install loop targets
each sidecar directly, so `verify_binaries` reports N/N present and every
`verify_single_install` call passes — while proving nothing at all about the
convention. A crate that silently stopped shipping a sidecar would still show
green, because the harness would have installed that sidecar by name. This is
precisely the failure DOC-1 §7.4 names — an omission is "not a weaker assertion,
it is *no* assertion, and it fails silently and permanently"
([`01-vm-install-harness.md:635-637`](./01-vm-install-harness.md)) — reached here
by a different route: not a missing row, but an install that answers the row
instead of being tested by it. `--check-table` cannot catch this one, because the
table is not what is wrong.

> **Naming tension, recorded.** DOC-1 §3.4 describes `source.sh` as owning "source
> delivery", but DOC-1 §12.1 requires reusable **install-step** functions so an
> upgrade scenario can be two install steps in one file. Putting
> `install_from_path` / `install_from_registry` here stretches the module's name.
> The alternative — a fifth `lib/install.sh` — departs from DOC-1 §3's component
> tree, which this document should not do unilaterally. Read `source.sh` as "source
> acquisition and installation". A later split into `lib/install.sh` is permitted
> and would not change any scenario, because scenarios call the functions, not the
> file.

**`lib/verify.sh`** — the oracle (DOC-1 §3.5). Reads only JSON (DOC-1 §7.1) and
`expected-binaries.tsv`. Must never call `tart` directly (DOC-1 §12.2) — it goes
through `vm_exec`.

| Signature | Returns / emits |
|---|---|
| `verify_binaries <vm_name> <pattern>` | for each `in_scope=yes` row, asserts present/absent per `expect_<pattern>` (§9.3); 0 or dies 60 |
| `verify_stack_doctor <vm_name> <pattern>` | §1.1 predicate; 0 or dies 60 |
| `verify_versions <vm_name> <pattern>` | §1.2 predicate; 0 or dies 60 |
| `verify_daemon_liveness <vm_name> <pattern>` | §1.3 **interim** predicate; 0 or dies 60 |
| `verify_rustc <vm_name> <guest_dir> <expected>` | DOC-1 §8.4, called from `install_from_path`; 0 or dies 50 |
| `verify_single_install <vm_name> <package>` | DOC-1 §7.4 — asserts **every** binary of `<package>` is present, not merely one; 0 or dies 60 |

`verify_single_install` is separate from `verify_binaries` on purpose. DOC-1 §7.4's
gate is specifically that installing a parent yields *all* its sidecars, and stating
it as its own function makes the failure message say "trusty-memory installed but
sidecar trusty-memory-mcp-bridge is missing" rather than "a binary is missing".
Given §9.3's finding that the third `trusty-memory` sidecar was dropped from DOC-1's
seed table, this gate earns its separate existence.

#### 12.3 Globals

Set by the driver during preflight; **read-only thereafter** unless marked.

| Global | Set by | Notes |
|---|---|---|
| `VMTEST_RUNID` | driver | §4.2 |
| `VMTEST_VM` | driver | `vmtest-$VMTEST_RUNID` |
| `VMTEST_RUNDIR` | driver | §4.3 |
| `VMTEST_PATTERN` | driver | `local` \| `branch` \| `released` |
| `VMTEST_KEEP` | driver | `0` \| `1` |
| ~~`VMTEST_CPU`, `VMTEST_MEM_MIB`, `VMTEST_DISK_GIB`~~ | ~~config (§8)~~ | **RESERVED — never assigned.** Read `conf_get cpu` etc. See below |
| ~~`VMTEST_GUEST_HOME`, `VMTEST_GUEST_SRC`, `VMTEST_GUEST_TARGET`~~ | ~~config (§8)~~ | **RESERVED — never assigned.** Read `conf_get guest_home` etc. See below |
| `VMTEST_GUEST_ENV` | **mutable** — base at preflight, replaced by `provision_guest` | §7.3 |
| `VMTEST_EXIT` | **write-once** — first `die()` only | §2 |
| `VMTEST_CLEANUP_DONE` | cleanup trap | idempotency guard |

Env vars **read** from the environment: `VMTEST_*` config overrides (§8.2),
`VMTEST_STATE_DIR` (§4.3), `TMPDIR`, `HOME`. Env vars **written**: none on the host
outside the harness's own process. In the guest, `PATH`, `CARGO_TARGET_DIR`, and
`SKIP_UI_BUILD` are set per-command by §7.3 and never persisted.

**Rule: configuration is read through `conf_get`, and the `VMTEST_<KEY>` names are
RESERVED for §8.2's env overrides. The harness must never assign one.**
*(Amended 2026-08-02.)* This table previously listed six config values as globals
the driver sets. It cannot set them, because **those names are already taken**:
§8.2's override mapping is *mechanical* — uppercase the key, prefix `VMTEST_` — so
`VMTEST_CPU` is simultaneously "the global holding the effective cpu" (here) and
"the environment variable that overrides key `cpu`" (§8.2). One name, two owners,
and the two definitions are in different sections that never mention each other.

**The failure is not a clash, it is a corrupted origin marker.** A driver that
assigned `VMTEST_CPU` would be **writing into its own override channel**. The next
resolution reads the environment, finds the value it just put there, and reports
origin **`env`** for a value that came from the defaults file — so §8.3's banner,
whose entire purpose is to record *where each number came from* so a run can be
compared against DOC-1 §9's cost baseline, would state a falsehood about every
config value the harness touched. Phase 2's checkpoint clause 2 tests that banner
directly, which is the only reason this was caught before Phase 3 relied on it.

**The rule, stated so it is checkable:**

1. Every config key is read **only** via `conf_get <key>` (and its origin via
   `conf_origin <key>`). There is no shell variable holding a config value.
2. The names `VMTEST_CPU`, `VMTEST_MEMORY_MIB`, `VMTEST_DISK_GIB`,
   `VMTEST_GUEST_HOME`, `VMTEST_GUEST_SRC_DIR`, `VMTEST_GUEST_TARGET_DIR` — and
   the `VMTEST_<KEY>` name derived from **any** other key in §8.2 — are **inbound
   only**. Assigning one is a defect, not a style choice.
3. The globals this table lists that are **not** config keys are still the
   driver's to set: `VMTEST_RUNID`, `VMTEST_VM`, `VMTEST_RUNDIR`,
   `VMTEST_PATTERN`, `VMTEST_KEEP`, `VMTEST_GUEST_ENV`, `VMTEST_EXIT`,
   `VMTEST_CLEANUP_DONE`.

> **Three of the six struck names were never the right names anyway.** §8.2's
> mapping is derived from the **key**, and the keys are `memory_mib`,
> `guest_src_dir`, `guest_target_dir` — which yield `VMTEST_MEMORY_MIB`,
> `VMTEST_GUEST_SRC_DIR`, `VMTEST_GUEST_TARGET_DIR`, not this table's
> `VMTEST_MEM_MIB`, `VMTEST_GUEST_SRC`, `VMTEST_GUEST_TARGET`. Had the driver
> assigned the abbreviated forms it would have silently created three globals that
> override **nothing**, alongside three real override names it never set — a
> collision *and* a near-miss in the same row. Recorded because it is evidence for
> rule 1: with `conf_get` there is exactly one spelling of a key, and it is the one
> in `vmtest.defaults`.

Opened by Phase 2 ([MANIFEST](../03-plan/MANIFEST.md) Phase 2, Deviations item 5),
which noted "a one-word note in §12.3 would close this". It needed more than a
word, because the reason is the part that stops someone re-adding the assignment.

#### 12.4 Error propagation

One rule, one function:

```sh
# die <exit-code> <message...>
# Records the FIRST classified failure and unwinds. Never overwrites VMTEST_EXIT.
die() {
  _code=$1; shift
  printf 'vmtest: FAIL[%s]: %s\n' "$_code" "$*" >&2
  if [ -z "${VMTEST_EXIT:-}" ]; then VMTEST_EXIT=$_code; fi
  exit "$VMTEST_EXIT"
}
```

The chain is: `die` → `exit` → the `EXIT` trap → `vmtest_cleanup` → teardown → the
process exits with `VMTEST_EXIT`. Nothing else terminates the run, and the write-once
guard is what implements §2's "first classified failure wins".

Scenarios do **not** call `die` with a code of their own. They call lib functions,
which die with their own phase code — so a scenario stays a description of steps and
expectations (DOC-1 §3.6) and never encodes the exit-code table.

#### 12.5 Worked skeleton — `scenarios/install-local.sh`

Pseudocode. Signatures and composition only; **this is not runnable and no part of
`vmtest-harness/` exists.**

```sh
# scenarios/install-local.sh — pattern (c), local source (DOC-1 §6.1)
# Sourced by the driver after provisioning. Composes lib/ functions only:
# no `tart`, no transport logic, no exit codes of its own (DOC-1 §3.6).

scenario_install_local() {
    # 1. Deliver source. Byte count is logged per DOC-1 §6.1's precision note.
    _bytes=$(source_deliver_local "$VMTEST_VM" "$PWD" "$VMTEST_GUEST_SRC")
    log "streamed ${_bytes} bytes of git-tracked + untracked-unignored source"

    # 2. Install each in-scope crate from the unpacked tree.
    #    install_from_path asserts `rustc --version` in the crate directory
    #    immediately before building (DOC-1 §8.4). Shared CARGO_TARGET_DIR
    #    rides in VMTEST_GUEST_ENV (DOC-1 §8.6, §7.3).
    for _dir in $(tsv_scope_crate_dirs); do          # column 2 where in_scope=yes
        install_from_path "$VMTEST_VM" "$VMTEST_GUEST_SRC" "$_dir"
    done

    # 3. Negative probe N2 — guide-and-abort now that tctl exists (§6.2/§6.3).
    negative_probe_n2 "$VMTEST_VM"

    # 4. Expectations that follow from those steps (DOC-1 §3.6).
    verify_binaries        "$VMTEST_VM" c
    verify_single_install  "$VMTEST_VM" trusty-search      # DOC-1 §7.4
    verify_single_install  "$VMTEST_VM" trusty-memory      # 3 binaries — §9.3
    verify_single_install  "$VMTEST_VM" trusty-installer
    verify_single_install  "$VMTEST_VM" trusty-mpm         # 2 binaries — added 2026-07-31
    verify_stack_doctor    "$VMTEST_VM" c                  # §1.1
    verify_versions        "$VMTEST_VM" c                  # §1.2
    verify_daemon_liveness "$VMTEST_VM" c                  # §1.3 interim, pending RC-1
}
```

> **Amendment, 2026-07-31 — the fourth `verify_single_install` call was missing.**
> The skeleton gated `trusty-search`, `trusty-memory`, and `trusty-installer` but
> not `trusty-mpm`, which ships **two** binaries (`tm` and `trusty-mpm`, §9.3).
> Four in-scope packages are multi-binary and only three were gated, so the one
> whose rows the D2 reversal had just changed was the one left ungated. The
> omission is the §7.4 failure mode reached by yet another route: `verify_binaries`
> would still find both `trusty-mpm` binaries, so nothing would go red, but nothing
> would have asserted that **one** `cargo install trusty-mpm` is what produced
> them. Every multi-binary in-scope package now gets a call; single-binary packages
> do not need one, because for them `verify_binaries` already is the whole claim.

Note what the skeleton does **not** contain: no `tart`, no `PATH`, no timeout, no
exit code, no `if` around a lib call. Every one of those lives in a `lib/` module or
in the config file. An upgrade scenario (DOC-1 §12.1) is this file with a second
install block between steps 2 and 4 — "two install steps in one scenario file, and
not a new mechanism" — which is the composability property DOC-1 §12.1 asks the
design to preserve today, and the reason these signatures look the way they do.

---

## CROSS-CUTTING

### Shell discipline

#### Bash version target: **3.2**

macOS ships bash **3.2.57** at `/bin/bash` and has since 2007, for licensing
reasons that are not going to change. The harness targets **bash 3.2 syntax** and
does **not** require a newer bash.

**Why not require bash 5 via mise or Homebrew.** The harness's host dependency
surface is otherwise `tart`, `git`, `jq`, and `cargo`. Adding "a bash newer than the
one macOS ships" means preflight must detect it, the shebang must find it, and a
maintainer on a clean machine hits an error before the harness does anything useful.
That is a poor trade for syntax sugar in a program whose data structures are three
TSV files.

**What targeting 3.2 costs — this materially shapes the code:**

| Unavailable in 3.2 | Consequence |
|---|---|
| `declare -A` (associative arrays, bash 4.0) | **The big one.** No maps. Lookups go through TSV files and `awk`, which is why §3, §8, and §9 all use the same key/value TSV format — that is not a stylistic preference, it is the substitute for a hash. |
| `local -n` (namerefs, 4.3) | Functions return one value on stdout (§12.1) rather than writing through a reference. |
| `mapfile` / `readarray` (4.0) | Read into arrays with `while read` loops. |
| `${var,,}` / `${var^^}` (4.0) | Case conversion via `tr`. |
| `globstar` (`**`, 4.0) | Recursive walks via `find`. |
| `wait -n` (4.3) | The §10.4 watchdog polls `kill -0` rather than waiting on the first child. |

3.2-compatible code runs unchanged under bash 4 and 5, so `#!/usr/bin/env bash` is
correct and a maintainer with a Homebrew bash first on `PATH` is unaffected. The
driver asserts `[ "${BASH_VERSINFO[0]}" -ge 3 ]` and prints the version in the
effective-configuration banner (§8.3), so a bug report says which bash produced it.

#### `set -euo pipefail`

Set **once**, at the top of the `vmtest` driver, before sourcing any `lib/` file.
`set` is shell-global, so a sourced file inherits it; `lib/` files must **not**
re-issue it (§12.1) — a stray `set +e` in a library would silently disarm the
driver.

Two gotchas that will bite an implementer and are worth stating rather than
rediscovering:

- **`set -e` is suppressed inside a condition.** In `if f; then`, `f && g`,
  `f || g`, or `! f`, a failing `f` does **not** exit the shell. So a lib function
  whose failure must abort the run must never be called in a conditional context.
  This is why §12.4 routes aborts through `die` (an explicit `exit`) rather than
  relying on `set -e` to notice.
- **`local x=$(cmd)` swallows `cmd`'s exit status** — `local` is itself a command
  and its own success masks the substitution's failure. Declare first, assign
  second: `local x; x=$(cmd)`.

`pipefail` matters specifically for pattern (c), where the delivery pipeline is
`git ls-files | tar | tart exec -i` (DOC-1 §6.1). Without `pipefail`, a `tar` that
fails mid-stream is invisible if `tart exec` exits 0 — a silently truncated source
tree that then fails to build for an unrelated-looking reason.

#### The trap / cleanup rule

DOC-1 §3.1 requires teardown on **every** exit path including interrupt. The
mechanism:

```sh
trap 'vmtest_cleanup' EXIT
trap 'vmtest_cleanup; exit 130' INT
trap 'vmtest_cleanup; exit 143' TERM
```

`vmtest_cleanup` must satisfy five properties:

1. **Capture `$?` on the very first line**, before anything else can clobber it.
2. **Be idempotent.** `EXIT` fires after the `INT` handler runs, so cleanup will be
   entered twice on SIGINT. Guard with `VMTEST_CLEANUP_DONE` and return immediately
   on the second entry.
3. **Tolerate a run that never got that far.** If `VMTEST_VM` is unset or the VM was
   never created, cleanup does nothing and returns 0. Preflight failures (exit 10)
   must not produce a teardown error on top of the real message.
4. **Do the right thing under `--keep`.** Write the `keep` marker into the run
   directory (§4.3, §5.3), bring the guest to `stopped` via `vm_request_stop` then
   `vm_wait_for_stopped` exactly as property 5 does, **skip only `vm_delete`**,
   print the VM name and an inspection hint, and leave the run directory in place.

   *(Amended 2026-08-02. This clause previously read "**skip all three** of
   `vm_request_stop`, `vm_wait_for_stopped`, and `vm_delete`", which left the VM
   `running`.)* **The original was wrong, and three other parts of this document
   are what prove it.** §5.1 condition 2 makes `stopped` a **requirement** of
   orphanhood, so `clean` classified a kept VM `REFUSED (running, no live registry
   entry)` and exited **10** — deleting nothing **even under `--include-kept`**,
   the one flag that exists to remove it. §5.3 justifies the `keep` marker by
   saying a kept VM "looks exactly like an orphan, because its run exited and its
   pid is dead"; an orphan is `stopped`, so §5.3 was describing a VM this path
   never produced. And `vm_manual_hint keep` offered `vmtest clean --include-kept`
   as an alternative to the manual pair — the harness printing a command that
   would refuse. Observed with output in MANIFEST Phase 3, Deviations item 2.

   **The other direction was rejected, and by the stronger rule.** Letting
   `clean --include-kept` accept a `running` kept VM requires `clean` to issue a
   stop, and §5.2 and §5.4 forbid that far more emphatically than this clause
   required the skip: *"`clean` never issues `tart stop`, never issues `tart
   suspend`, and never deletes a VM that is not already `stopped`."* Stopping the
   guest preserves everything `--keep` exists for — the disk image,
   `~/.vmtest/toolchain.tsv`, the delivered source tree — and costs only that
   inspection now begins by booting it. The inspection hint says so, and both
   commands it prints now work.

   The stop still goes request → **poll** → *(no delete)*. Never a bare `tart stop`
   treated as completion (DOC-1 §8.1); skipping `vm_delete` is the whole of the
   difference from property 5.
5. **Otherwise: `vm_request_stop`, then `vm_wait_for_stopped`, then `vm_delete`, in
   that order, always.** *(Amended 2026-07-31: `vm_request_stop` added. Nothing
   previously issued the shutdown, so the poll had nothing to observe and cleanup
   ran out its budget and exited 70 on every path — see §12.2.)* Never a bare
   `tart stop`, meaning never issue one and treat its return as completion (DOC-1
   §8.1 — write loss reproduced 4 of 5 attempts and the confirmed root cause of a
   golden image shipping broken); `vm_request_stop` discards the status and the poll
   is the completion signal. Then remove the run directory. Then re-exit with the
   preserved code.

The explicit `exit 130` / `exit 143` in the signal traps are not decoration. After a
trap handler returns, bash may resume the interrupted command rather than
terminating; the explicit exit is what guarantees the harness actually stops, and
it is what makes §2's user-abort codes real.

`set -u` and traps interact badly: cleanup runs in contexts where globals may not be
set yet, so every variable it touches must use `${VAR:-}`.

### JSON parsing dependency

**`jq` is a HOST dependency. It is not installed in the guest.**

All JSON the oracle reads is produced in the guest and streamed to the host over
`tart exec` stdout, then parsed on the host. This is sound because DOC-1 §5.1
records that `tart exec` keeps stdout and stderr separate and does not truncate at
volume — 200,000 lines passed through intact
(`vm-install-probe-findings.md:179-180`).

**Why host-side and not guest-side.** Installing `jq` in the guest would make the
test environment differ from a real user's machine in a way the harness itself
introduced. DOC-1's isolation argument (§11) is about protecting the host from the
guest; the mirror-image concern is keeping the guest representative of what it
models. Every tool the harness adds to the guest is a small lie in the fixture.
Parsing host-side costs nothing and avoids it entirely.

`jq` is **not** among the tools the research recorded as preinstalled on
`tahoe-base` (`vm-install-probe-findings.md:50-65`, `:866` — `git`, `curl`, `node`,
`python3`, `cmake`, `mise`, `gh`, `brew`). That list was not an exhaustive audit, so
treat `jq` as absent-until-measured in the guest — which is another reason not to
depend on it there.

**Preflight verification** (part of DOC-1 §4.1's checks, failing with exit 10):

```sh
command -v jq >/dev/null           || die 10 "jq not found on PATH (host dependency)"
printf '{"a":1}' | jq -e '.a == 1' >/dev/null || die 10 "jq present but not functional"
```

The functional smoke test is deliberate: a `jq` on `PATH` that is a broken symlink,
a wrapper script, or a differently-named tool passes `command -v` and fails at the
first assertion, several minutes into a run. Two milliseconds at preflight is the
better place to find out.

The complete host dependency set is therefore: **`tart`, `git`, `jq`, `cargo`, and
bash ≥ 3.2.** `cargo` is host-side for `--check-table` only (§9.6); a `vmtest run`
never builds on the host.

### Traceability

Each contract in this document and the DOC-1 section it completes.

| § | Contract | Completes | What DOC-1 left open |
|---|---|---|---|
| 1 | Assertion oracle JSON schema | §7.1, §7.5 | Named three JSON sources, no fields, no predicate |
| 2 | Exit-code contract | §3.1, §13 | "Emit a final pass/fail" — no codes |
| 3 | Base image digest pinning | §4.1 | "digest matches the pinned digest" — no location, format, or roll procedure |
| 4 | `--runid` generation and collisions | §3.1, §4.1 | Detection specified; prevention not |
| 5 | `vmtest clean` semantics | §3.1 | "orphaned" undefined; in-progress case unaddressed |
| 6 | Negative-probe mechanics | §4.2 | Sequencing flagged unresolved; command and predicate absent |
| 7 | Toolchain path hand-off | §3.3, §5.2, §5.3 | "exporting the resolved absolute toolchain paths" — no file, format, or composition |
| 8 | Configuration and tunables | §4.1, §8.5 | Literals in prose; host capacity flagged underspecified |
| 9 | `expected-binaries.tsv` schema | §7.2, §7.4, §7.5, D2, D3 | Seed table only; columns, `--check-table` source, and `tga` key unresolved |
| 10 | Polling timeouts and backoff | §4.3, §8.1 | "poll, never sleep" — no interval, maximum, or timeout behaviour |
| 11 | Provisioning vs preinstalled `mise` | §3.3 | "installs mise… and gh" — contradicts the research |
| 12 | Scenario ↔ lib calling convention | §3.2–§3.6, §12.1 | Module responsibilities; no signatures |
| — | Shell discipline | §2.3, §3.1 | "written in bash" — no version, no trap rule |
| — | JSON parsing dependency | §7.1 | JSON mandated; parser never named |

### Open items this document creates

Recorded in the same register as DOC-1 §14, so they are not lost.

- **RC-1 — unified daemon health envelope** (§1.3). Does not exist. Until it does,
  the oracle asserts liveness only, and DOC-1 §7.1's third JSON source is aspirational.
  **STILL OPEN at plan close-out, 2026-08-04, and deliberately so** — P8-T4's
  instruction is *"leave RC-1 open; it is not this plan's to close"*. Confirmed
  against the shipped oracle: `verify_daemon_liveness` probes **five** daemons and
  gets **five different `/health` body shapes**, so it asserts only that each
  answered, that the body parses as JSON, and that `.status` is a string outside
  `{down, error, unhealthy}` — **the status code is logged, not asserted**
  *(amended 2026-08-04, §1.3)*. It says `LIVENESS ONLY — see RC-1` in its own PASS
  line every run. Anyone reading a green run must not read it as "the daemons are
  healthy".

  > **`tctl stack health --json` WAS EVALUATED AS A CHEAP SUBSTITUTE FOR RC-1 AND
  > DECLINED AS THE PROBE** *(2026-08-04, by running it — see §1.3)*. It recovers
  > the part of RC-1 that is about **uniform classification**: one
  > product-maintained, body-based, 503-tolerant verdict vocabulary applied to the
  > whole stable set, so the harness need not invent a health rule. **It recovers
  > none of the envelope itself** — no `version`, no per-daemon body, and **no
  > `service` discriminator**, so the **#3364** squatter-collision class stays
  > open; that class is closable only by a product change RC-1 would carry. It
  > also reports `trusty-mpm` as `unknown` (`OwnVerb` → `Unprobeable`, #4246),
  > which is why substituting it for the probe would **lose** an assertion the
  > oracle already makes. The harness therefore adopted the product's
  > **classification rule** and declined its **command**. **RC-1's status is
  > unchanged by this evaluation** — it neither becomes more urgent nor less.
- ~~**RC-2 — `tctl install` cargo-absent exit code and message** (§6.2). Not pinned
  to a verified value. N2's predicate is deliberately weak until it is.~~
  **CLOSED 2026-08-03 as *unreachable-by-design*** (§6.2's RC-2 closure). Not
  satisfied and not outstanding: `decide_install_gate`'s `Refuse` arm returns **3**
  before `install_one` is called whenever `--yes` is absent and stdin is not a TTY,
  so the cargo guard at `install.rs:829` cannot be reached from a guest; and `--yes`
  would reach a prebuilt-tarball-first path that installs **released** binaries over
  the source-built ones under test, the exact false pass DOC-1 §6.5 exists to
  prevent. N2 records **BLOCKED** and prints it every run; every other failure shape
  still dies 30. Asserting the guard needs a route that is not `tctl install` on a
  networked guest — a `crates/trusty-installer` unit test, or an offline-network
  scenario — and neither is this harness's job. **Do not re-open and re-run the same
  probe.**

  > **STATUS CLARIFIED 2026-08-04 (plan P8-T4), because "closed" here has been read
  > as more than it is.** Two different things carry the name RC-2 and only one of
  > them is closed.
  >
  > - **The HARNESS obligation is closed.** N2 cannot reach the guard through
  >   `tctl install` from a guest, the reason is structural rather than incidental,
  >   and no further harness work will change that. Nothing here is outstanding.
  > - **The PRODUCT-side item is OPEN.** `crates/trusty-installer/src/commands/
  >   install.rs:829`'s cargo-absent exit code and message are **still unpinned by
  >   any test**, and P5-T2 did not pin them. The observed **3** is
  >   `decide_install_gate`'s consent-gate code, observed identically twice
  >   (in-guest source-built `tctl` 0.5.0, and host released `tctl` 0.4.10 with
  >   `stdin=/dev/null`), with **zero** cargo-related tokens in 204 bytes of
  >   stderr. Recording 3 as RC-2's value would be exactly the false precision
  >   §6.2 refuses.
  >
  > **N2 therefore reports BLOCKED on every run and prints why.** It is a known gap
  > in what a green run proves, not a silent omission — see `vmtest-harness/README.md`,
  > which states it for a reader who never opens this document. The plan's close-out
  > carries RC-2 as **open**.
- ~~**Full base-image digest** (§3.2). Never recorded in the research — only the
  truncated `sha256:a8e1...` (`vm-install-probe-findings.md:652`). Must be captured
  at implementation time.~~ **CLOSED 2026-07-31 by plan P1-T3; recorded here at
  source 2026-08-04 (P8-T4).** The full digest is
  `sha256:a8e1c8305758643f513fdccdd829c2243687c60791083dea42f73f0b7aeb435c`,
  captured **untruncated** and committed as `vmtest-harness/base-image.pin`, which
  preflight verifies on every run.
- ~~**`tart list` digest introspection** (§3.3). The invocation that yields a full
  machine-readable digest was not measured. §3.3's by-construction variant is the
  fallback.~~ **CLOSED 2026-07-31 by plan P1-T3.** The introspection branch works:
  `tart list --format json | jq -r '.[] | .Name'` emits the OCI row's name as
  `<oci_ref>@sha256:<64 hex>`, untruncated. **§3.3's by-construction variant was
  not needed** and remains the fallback it was specified as.
- ~~**`trusty-mpm` `publish` premise** (§9.5).~~ **Closed 2026-07-31.** The premise
  was false — no `publish` key, and `cargo search trusty-mpm` returns `1.0.2`. DOC-1
  D2 is reversed, D3 scopes `trusty-mpm` into pattern (a) along with the rest of the
  stack, DOC-1 §7.5 inverts to asserted-present, and the seed table's `expect_a` is
  now `present`. *(That closure put the stack at seven crates; a separate D3
  amendment the same day took it to eight — see the next item.)*
- ~~**`trusty-review` in or out of D3's scope** (§9.3 note 2).~~ **Closed
  2026-07-31 by owner decision — IN scope.** §9.3 note 2 asked that this be "decided
  knowingly rather than by omission", and it was: **DOC-1 D3 is amended to eight
  crates**, with a dated amendment recording the reasoning. `publish = true` is
  explicit in the manifest and `cargo search trusty-review` returns `0.10.1`, so
  pattern (a) reaches it; it has one `[[bin]]`, so it adds one in-scope row
  (thirteen total) and no `verify_single_install` call. Its `/health` now falls under
  the oracle's INTERIM liveness predicate, which §1.3 records as sufficient — the
  MCP/HTTP drift noted there is off the assertion path. **RC-1 is unaffected**: no
  new field dependency was introduced.
- ~~**`trusty-memory-mcp-bridge` missing from DOC-1 §7.2** (§9.3).~~ **Closed
  2026-07-31.** DOC-1 §7.2's table now lists all three `trusty-memory` binaries, and
  the project's Single-Install sidecar inventory checklist
  (`.claude-mpm/INSTRUCTIONS.md`) — the upstream source of the omission — is
  corrected in the same PR.
- ~~**Full-stack watchdog is 5.6× a low-confidence estimate** (§10.2). Tighten once
  the first pattern-(c) full-stack run is timed, as DOC-1 §9 already requests.~~
  **CLOSED 2026-08-02 by plan P5-T8 and confirmed at P8-T2, 2026-08-04.**
  `install_timeout` is **1800 s**, sized at **2.4×** the slowest measured scenario
  (640 / 748 / 610 s, three pattern-(c) runs) rather than at a multiple of an
  extrapolation. Five full-stack runs across all three patterns now span
  **511–919 s** total and none has approached it. §10.2's row is corrected at
  source; DOC-1 §9's `EXTRAPOLATION` row is superseded in the same pass.
- **Daemon time-to-ready** (§10.1). Wholly unmeasured; the 60 s maximum is a guess.
  **STILL OPEN at plan close-out, 2026-08-04, and P8-T2 declined to promote it.**
  P5-T7 produced no number: `_verify_wait_for_addr` and the `/health` probe assert
  an outcome and never log an elapsed time, so six full-stack runs bound daemon
  readiness only as *(0, 60] s* — the budget has never been hit, which is a bound
  in the unhelpful direction. `vmtest.defaults` labels both `health_timeout` and
  `health_interval` **`judgment call`** rather than citing a grounding they do not
  have. Closing this needs instrumentation, which is a code change P8-T2 does not own.
- ~~**Guest-side graceful shutdown** (§12.2, added 2026-07-31).~~ **Closed
  2026-07-31.** Not an open question and not a judgment call: a guest-side
  `shutdown -h now` is **forbidden** as the shutdown initiator (§12.2). Its only
  appearance in the corpus is the superseded Track A script issued over **SSH**,
  which DOC-1 §5.1 excludes, and it requires passwordless `sudo` in the guest, which
  was never measured. `vm_request_stop` is the only permitted initiator.

---

### References

- [DOC-1 — Tart VM Install Testing Harness](./01-vm-install-harness.md) — the design this document completes.
- [`../01-research/vm-install-probe-findings.md`](../01-research/vm-install-probe-findings.md) — raw measurements A–K; every cited number traces here.
- [`../01-research/conclusions-post-measurement.md`](../01-research/conclusions-post-measurement.md) — authoritative research outcome.
- [`../01-research/devils-advocate-review.md`](../01-research/devils-advocate-review.md) — adversarial review; critique #9 (`:20`) is the tar-transport gap DOC-1 D4 now records.
- [`docs/adr/0002-single-install-convention.md`](../../../adr/0002-single-install-convention.md) — the convention DOC-1 §7.4 gates on.
- [`docs/adr/0013-rename-trusty-controller-to-trusty-installer.md`](../../../adr/0013-rename-trusty-controller-to-trusty-installer.md) — why `crates/trusty-controller/` does not exist.

> The `01-research/` directory lands in **PR #4456** (branch
> `docs/tart-vm-research-final`). If that PR has not merged, the relative links
> above will not resolve on this branch; they resolve once both branches are on
> `main`.
