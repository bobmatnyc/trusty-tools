# tm issue epic — CLI verb family

Plan document for the epic that automates tracker and phase-issue authoring.
Read with [`tracker-phases-pattern.md`](tracker-phases-pattern.md), the owner's
origin document for the pattern — provenance, not authority. The bundled
`tm-epic` skill governs; this plan says what to build against it.

## Epic plan

The `tm-epic` skill and its templates shipped in
[#8412](https://github.com/bobmatnyc/trusty-tools/pull/8412), and
`TICKETING.md` now carries the `epics.*` standard. Both describe an epic that
is authored and maintained entirely by hand. The `ticketing` agent has no
`Write` tool, so "regenerate the `phases` block wholesale" is today an LLM
retyping a markdown table through `Bash` on every phase transition. That is the
drift the pattern's second rule exists to prevent, reintroduced as the
implementation. This epic makes the deterministic half code.

### Outcomes

- **O1** A tracker and its phase issues are created from a committed plan
  document by one command, with titles, labels, milestone and native sub-issue
  links applied without anyone typing them.
- **O2** The `phases` block is regenerated from live child state by a command,
  never by a session retyping a table, and content outside the markers is
  provably untouched.
- **O3** A phase transition updates its tracker without a human remembering to,
  so a closed phase never leaves a stale tracker.
- **O4** `tm issue audit` reports a tracker whose block has drifted from its
  children, so the guarantee is checked rather than asserted.

### Ratified decisions

| # | Decision |
|---|---|
| D1 | Titles are `[EPIC <epic#>] <outcome>` and `[EPIC_<epic#> PHASE_<n>] <what>`, matching `TICKETING.md`'s `epics.title_format` and live practice on [#8378](https://github.com/bobmatnyc/trusty-tools/issues/8378), [#8380](https://github.com/bobmatnyc/trusty-tools/issues/8380) and [#8387](https://github.com/bobmatnyc/trusty-tools/issues/8387)–[#8390](https://github.com/bobmatnyc/trusty-tools/issues/8390). Tracker creation is two-step: file with a placeholder, read the number back, edit it in |
| D2 | The tracker body carries three marker blocks — `phases:`, `deferred:`, `followups:`. The skill's "two blocks, two rules" section is the artifact of record; `TICKETING.md`'s `followups.tracker` wording ("checklist") describes the same thing less precisely and is aligned to "block" in Phase 1 |
| D3 | Resolution and refusal live in the CLI, not in agent judgement: phase numbers are max+1 over existing children and never reused; a missing or duplicated marker pair is a refusal, not a silent append |
| D4 | Creation refuses until the plan document is on `origin/main`, and links it by a SHA-pinned blob permalink, never a branch path |
| D5 | Issue numbers are never written back into the plan document. The tracker points at the plan; the plan does not point at the tracker |
| D6 | A phase takes a type label from the existing six-value set. There is no seventh `phase` type |
| D7 | The backend is a narrow `EpicBackend` trait over the existing `CommandRunner` seam, not a widening of `TicketSystem` — `ticket/system.rs` is already 299 SLOC and carries no create, no body write and no sub-issue verb |

### Ordering

Phase 1 must be used against a real epic before Phase 2 is written. Every verb
in Phase 2 acts on a tracker that Phase 1 produced: `close` reads the children
`create` linked, the transition hook calls the `sync` `create` shaped, and the
audit rows compare against the block `sync` renders. Writing them against an
imagined tracker body means writing them twice.

The gate is therefore evidence, not opinion: one epic created and synced by
Phase 1's code, its tracker body inspected, before Phase 2 starts. Phase 1 is
also the larger blast radius — it mutates several GitHub issues in sequence and
can fail partway — so it earns its own review round and its own revert unit.

<!-- phases:start -->
| # | Phase | Issue | State | Gate |
|---|-------|-------|-------|------|
| 1 | `tm issue epic create\|sync` | TBD | not started | Shape ratified — satisfied: `TICKETING.md` `epics.*` and the `tm-epic` skill are both on `main` |
| 2 | `defer\|close`, transition hook, audit rows | TBD | not started | Phase 1 used live against one real epic |
<!-- phases:end -->

### Phase: tm issue epic create|sync

Gate: none — the shape is ratified and on `main`.

Parses the plan document's `## Epic plan` section, files the tracker, reads its
number back, retitles it, then files each phase as a native sub-issue with
`--parent`. `sync` regenerates the `phases` block from live child state. New
module under `crates/trusty-mpm/src/bin/tm/commands/issue/epic/`, reaching
GitHub through an `EpicBackend` trait over `CommandRunner` so the whole verb
family is testable against a fake.

The obvious wrong implementation batches the tracker and its phases in one
pass, so phase titles carry a placeholder instead of the tracker's number; the
second-most obvious patches rows inside the `phases` block with a regex rather
than replacing the block, which is exactly the drift rule 2 forbids.

#### Acceptance criteria

- **AC1** A run interrupted after the tracker is filed and before phase 2
  leaves no issue whose title contains a placeholder. Re-running `create` on
  the same plan document skips children that already exist under that tracker
  and files only the missing ones — no duplicates.
- **AC2** `sync` replaces the whole region between `phases:start` and
  `phases:end`. A hand-edited row inside the block is discarded, not merged.
  Every byte outside the two markers is identical before and after, asserted on
  a tracker body carrying prose, the `deferred` block and the `followups` block.
- **AC3** Given a tracker whose existing children are `PHASE_1`, `PHASE_2` and
  `PHASE_6`, a new phase is numbered `PHASE_7`. Deleting `PHASE_6` and adding
  another still yields `PHASE_8`, never a reused number.
- **AC4** A plan document committed locally but absent from `origin/main` is
  refused, naming both the local SHA and the remote ref it compared against. A
  document with no `## Epic plan` section is refused naming the missing
  heading.
- **AC5** The tracker body's plan link matches `/blob/<40-hex>/`. A body
  containing `/blob/main/` fails the test.
- **AC6** A body with no marker pair, or with two `phases:start` lines, exits
  nonzero and changes nothing. The test asserts the body is byte-identical
  after the refused run.
- **AC7** Every issue the run creates carries its type label, `ws/<session>`,
  its component label(s) and a milestone; each phase carries the tracker's
  milestone. Where the project attach fails on token scope, the run still exits
  0 having posted the `no-project:` comment, and the test covers that arm.

#### Non-goals

No `defer`, `close`, transition hook or audit row — Phase 2. No rewriting of
the plan document (D5). No reconciliation of the two diverged `ticketing.md`
agent assets, which is [ADR-0059](../../adr/0059-canonical-agent-source.md)'s
problem and predates this work.

#### Risk

Multi-issue mutation against a live tracker, partway-failure resumable by
design. A wrong `sync` silently destroys authored prose, which is why AC2
asserts the bytes outside the markers rather than only the rows inside them.

### Phase: defer|close, transition hook, audit set rows

Gate: Phase 1 used live against one real epic, its tracker body inspected.

`defer` appends one row to the `deferred` block. `close` refuses while any
phase is open and posts the closing comment mapping each outcome to evidence.
`tm issue transition` on a phase issue regenerates its tracker's `phases` block
as a side effect. `tm issue audit` gains set-level rows: every
`[EPIC_<n> PHASE_<m>]`-titled child is a native sub-issue, and the regenerated
block equals the current one.

The obvious wrong implementation makes the transition hook fail open — the
label advances, the tracker sync throws, the error is swallowed as a warning,
and the tracker is stale with nothing recording that it is. That is the
fail-open shape the review standard names, and it needs an error-arm test that
fails against the pre-fix commit.

#### Acceptance criteria

- **AC1** When the tracker sync fails during `tm issue transition`, the command
  does not report plain success. The failure reaches the caller — a nonzero
  exit or a tested field naming the stale tracker — and a regression test
  exercises that arm with a backend that errors only on the sync call.
- **AC2** `transition` on an issue whose title is not `[EPIC_<n> PHASE_<m>]`,
  or which has no parent, performs no tracker read and no tracker write. The
  test asserts the backend saw zero extra calls.
- **AC3** `close` on a tracker with one open child exits nonzero and names that
  child. It succeeds once the child is closed, and its comment contains one
  line per outcome declared in the tracker body.
- **AC4** `tm issue audit <tracker>` exits 1 when the regenerated block differs
  from the current one, printing `phases block stale — run tm issue epic sync
  <N>`. A phase-titled child that is not a native sub-issue is a FAIL row, not
  INFO.
- **AC5** `defer` appends exactly one row and leaves the `phases` and
  `followups` blocks byte-identical.

#### Non-goals

No automatic follow-up filing — the two-per-phase HIGH-severity budget stays a
human judgement. No migration of existing hand-built trackers.

#### Risk

The transition hook adds a GitHub round trip to a command run constantly, and
couples two previously independent operations. AC1 and AC2 exist to bound that
coupling.

### Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| Reconciling the two diverged `ticketing.md` agent assets | Predates this work; ADR-0059's scope | unscheduled |
| Aligning `TICKETING.md`'s `followups.tracker` wording from "checklist" to "block" | One-line doc edit, belongs with whoever next touches that file | Phase 1, per D2 |
| Auto-provisioning a trusty-search index for a dispatched worktree | Blocks the review gate, not this feature | [#8411](https://github.com/bobmatnyc/trusty-tools/issues/8411) |
| Reclaiming agent-dispatch worktrees | Unrelated guard path-scope defect | [#8413](https://github.com/bobmatnyc/trusty-tools/issues/8413) |
<!-- deferred:end -->

## Maintenance

The sections above the markers are authored once. The `phases` block is
regenerated from child issue state — by hand until Phase 1 lands, by
`tm issue epic sync` after. The `deferred` block is amended deliberately.

Issue numbers are not written back here (D5). The `TBD` cells in the phases
block above are this document's own copy and are superseded by the tracker's
block once the epic is filed; the tracker is the live artifact.
