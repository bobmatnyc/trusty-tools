# DOC-87 — Typed owner constraints

**Status:** Draft
**Subsystem:** `trusty-architect` ledger, Architect relay, PM dispatch, `trusty-mpm` pm-guard
**Owner:** owner
**Last-updated:** 2026-11-07
**Spec ID:** `SPEC-OWNERCON-01~draft` … `SPEC-OWNERCON-08~draft` (DOC-87)
**DOC-N claim:** `DOC-87`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). No other `DOC-87` on `origin/main` at 2026-11-07.

Owner ruling (Dd0, relayed 2026-11-07 16:08Z): "Provenance check now in 1.12.1; typed owner constraints as a spec and follow-up issue."

## 1. Problem {#SPEC-OWNERCON-01~draft}

**ID:** SPEC-OWNERCON-01~draft

[#10780](https://github.com/bobmatnyc/trusty-tools/issues/10780): at 2026-11-06T20:48Z the owner ruled four helpers until Mon 2026-11-09 09:00Z. At 23:39Z the Architect inferred "two helpers" from a pacing figure and relayed it as `User instruction (Matt):`. The PM complied for about 6 h. Two /clear cycles then restored the inference as an owner ruling ("two-helper ruling"), and a stale handoff line outranked the palace ruling.

Root causes:

- An owner decision is prose in a relay, a handoff line or a palace drawer. Nothing parses it, so nothing checks an action against it.
- The source label (`User ruling`) is free text. Handoff and digest keep conclusions, not sources.
- Pacing logic and owner rulings share one channel (prose), so pacing can overwrite a ruling.

The second instance in #10780 (a gate ruling that contradicted a written CLAUDE.md rule) has the same shape: a rule existed, no machine check consulted it.

## 2. Goals and non-goals {#SPEC-OWNERCON-02~draft}

**ID:** SPEC-OWNERCON-02~draft

Goals:

- G1. Store owner decisions as typed constraints with source, verbatim quote, timestamp, scope and lifetime.
- G2. Check every constrained action (relay, dispatch, spawn, merge, release) against active constraints before it runs.
- G3. Pacing and inference may change model mix and queue order. They never create, change or lift a constraint.
- G4. Reversal, expiry and supersession are explicit events with their own source.
- G5. No per-person hardcoding: the owner is a field, not a code path.
- G6. A constraint survives /clear, handoff, and PM restart without a prose round trip.

Non-goals:

- The 1.12.1 provenance check is the baseline and out of scope: `relay.sh` refuses `User ruling (`/`User instruction (` without a citation to a real owner turn, and ledger/digest records carry `source=owner|architect`. This spec builds on it and does not re-specify it.
- Natural-language understanding of arbitrary owner chat. Extraction stays a human-confirmed step (§6).
- A general policy engine. Six constraint kinds only (§3).

## 3. Constraint model {#SPEC-OWNERCON-03~draft}

**ID:** SPEC-OWNERCON-03~draft

### 3.1 Kinds

| `kind` | Param schema | Example (#10780) |
|---|---|---|
| `limit` | `{resource, op: min\|max\|exact, value, unit}` | `{resource: helpers, op: exact, value: 4}` |
| `scope` | `{allow: [path\|repo\|issue], deny: [...]}` | "licensing is least important" is a queue weight, not a scope; see §3.4 |
| `hold` | `{target: pr\|branch\|dispatch\|resource, id}` | "do not merge #123"; "no new helpers" |
| `model` | `{role, allow: [model], deny: [model]}` | "mechanical work on the cheaper model" |
| `gate` | `{action: merge\|release\|publish, requires: [check\|approval]}` | release needs owner approval |
| `approval` | `{action, boundary: needs_owner\|pre_approved, bound}` | production deploy only for named scope |

A kind outside this table is stored as prose with `kind=note` and is never enforced. Adding a kind is a spec change.

### 3.2 Record

```
id          C<2-hex>          one counter, same rule as r-039
kind        see 3.1
params      kind schema, validated at write
scope       project:<slug> | session:<id> | fleet
source      owner               only value allowed for an enforced record
cite        {channel: dashboard|chat|pane|palace, ref, ts}
quote       verbatim owner text, non-empty
owner       <name>               field, never code
created_at  RFC 3339 UTC
effective   {from, until|null}   until may be an event: "reset:2026-11-09T09:00Z"
status      active | reversed | expired | superseded
supersedes  [C..]
ended_by    {cite, ts} | null
```

`source=architect` records are valid as notes and proposals (`kind=note`, `status=proposed`). They are never enforced and never rendered under an owner label.

### 3.3 Storage: ledger primary, palace mirror

Chosen: the `trusty-architect` ledger (`crates/trusty-architect/src/ledger/{model,store}.rs`) is the system of record; the palace holds a read-only mirror drawer per record (`fact_key=ownercon:<id>/state`, ADR-0028 Tier C).

Why the ledger: it already has typed items, ids, a single writer, an append-only log and the dashboard link (`owner_answers`). Schema validation and the supersession graph need one writer; palace drawers have no schema and retire by slot, not by relation.

Why the mirror: PMs and other palaces read the palace, not the Architect ledger. The mirror lets `memory_recall` surface a constraint, and the `fact_key` slot retires a mirror when the record ends. The mirror is derived. On disagreement the ledger wins and the mirror is rewritten.

Rejected: palace-only. No schema, no machine check, and the #10780 failure (palace ruling beaten by a handoff line) was a precedence bug that typed records remove.

### 3.4 Pacing boundary

Pacing and queue weights ("licensing least important", "target 1.75%/h") are Architect policy, not constraints. They may reorder the ready queue and pick the model within any `model` constraint. They cannot read a `limit` as a suggestion: `helpers exact 4` holds regardless of the usage rate.

## 4. Enforcement points {#SPEC-OWNERCON-04~draft}

**ID:** SPEC-OWNERCON-04~draft

One checker, `ownercon check --action <json>`, returns `allow | deny(C..) | ask(C..)`. Every point below calls it; none re-implements matching.

| Point | Where | Behavior |
|---|---|---|
| Relay | `python/trusty-architect/scripts/relay.sh` | An outbound relay whose parsed action violates an active constraint is refused. A relay that tells a PM to change a constrained quantity (helper count) without a reversal record is refused. |
| Architect actions | architect.md r-075, r-076 | Before spawning, stopping, merging or ruling, the Architect runs the check and cites the constraint id in the `basis:`. |
| PM dispatch | `content/instructions/supervisor/relay-protocol.md` | The PM reads active constraints for its project and fleet scope at session start and before each dispatch. A dispatch that exceeds `limit` or touches a `hold` is not sent. |
| pm-guard | `crates/trusty-mpm` pm-guard | Hook-level deny for spawn beyond a `limit`, and for merge or publish under a `gate` or `hold`. The deny names the constraint id and quote. |
| Digest / resume | `datg*.py`, handoff template | The digest has an "Active owner constraints" block rendered from the ledger, each line with id, quote, cite, effective window. Resume reads that block first. A handoff prose line that conflicts with it is flagged, not obeyed. |

Fail mode: if the ledger is unreadable, `deny` for `hold` and `gate` actions, `ask` for `limit`, `allow` otherwise, and the failure is logged. A constraint store outage never silently allows a release.

Every check writes an audit line: action, result, constraint ids, caller.

## 5. Lifecycle {#SPEC-OWNERCON-05~draft}

**ID:** SPEC-OWNERCON-05~draft

- **Create.** From an owner turn only. The Architect proposes a record (`status=proposed`, quote and cite filled) and the 1.12.1 provenance check validates the cite. Chat or pane turns activate on cite verification. Dashboard answers activate directly. An ambiguous turn becomes a D item with options, recommendation first (r-039).
- **Reverse.** A new owner turn that states the opposite creates a record with `supersedes=[C..]`. The old record becomes `superseded` with `ended_by` set. "Return to two helpers after Monday" is one record with `effective.until`, not a reversal. Silence, pacing and Architect inference never reverse.
- **Expire.** `effective.until` as a time or an event. A sweeper marks `expired` and writes an audit line. An expired constraint stays queryable and renders in the digest for 24 h as "expired" so the Architect does not re-apply it from memory.
- **Supersede.** The newer owner record wins for the same `(kind, scope, resource)` key. Narrower scope (session) overrides broader (project, fleet) only when it is also an owner record and names the broader one or does not conflict; otherwise the check returns `ask`.
- **Conflict with written rules.** A constraint against project CLAUDE.md text returns `ask`; the owner resolves it. The Architect cites the rule it applies (second instance in #10780).
- **Withdrawal by the owner of an Architect proposal.** `status=withdrawn`, no effect on enforcement.

## 6. Migration from prose rulings {#SPEC-OWNERCON-06~draft}

**ID:** SPEC-OWNERCON-06~draft

1. Read-only scan of palace drawers and ledger items with the label `User ruling`/`User instruction` and 1.12.1 `source=owner`. Emit candidate records with quote and cite.
2. Candidates appear on the dashboard as one D item per batch. The owner confirms, edits or drops each. Nothing activates without confirmation.
3. Unconfirmed prose rulings keep working as today (notes, not enforced). Records with no verifiable owner cite stay `kind=note`.
4. After confirmation, handoff and digest templates drop prose "Owner rulings" in favor of the rendered block.
5. No automatic rewrite of old drawers; the mirror carries the new `fact_key` slot.

## 7. Test plan {#SPEC-OWNERCON-07~draft}

**ID:** SPEC-OWNERCON-07~draft

Unit (ledger, checker):

- T1. Schema: each kind rejects a malformed `params`; an `architect` source record cannot reach `active`.
- T2. Supersession key resolution and the narrower-scope `ask` case.
- T3. Expiry by time and by event; expired records render for 24 h and are not enforced.
- T4. Fail mode on an unreadable ledger per §4.

Regression, four-to-two (#10780):

- R1. Active `limit helpers exact 4` from 20:48Z. Architect proposes "two helpers" from a pacing figure. `relay.sh` refuses; the digest still shows 4.
- R2. The same proposal re-enters through a /clear handoff line "TWO helpers only". Resume flags the conflict and keeps 4.
- R3. Pacing at 3.4%/h: queue order and model mix change; helper count does not.
- R4. Owner turn "return to four helpers" at 05:3x creates a record superseding nothing wrongly and the check allows 4.

False-refusal cases (the check must allow):

- F1. Valid owner reversal: owner says "go to two helpers now". A new record with a verified cite supersedes the 4-limit; relay of "two" is allowed, and a relay of "four" afterward is refused.
- F2. Expired ruling: after 2026-11-09T09:00Z the 4-limit is `expired`; "back to two" is allowed with no new owner turn, because the original record set `until` and the follow-on `exact 2` record was created with it.
- F3. Legitimate resource HOLD: owner holds "no new helpers while at 98%". Spawning is denied; finishing and stopping running helpers is allowed; the deny text names the hold and how to lift it.

Integration:

- I1. pm-guard denies a spawn over `limit` and a merge under `hold`; allows both once reversed.
- I2. Digest round trip across `/clear`: constraint ids and quotes identical before and after.
- I3. Mirror drawer retires with `fact_key` when the record ends; ledger wins on a forged mirror edit.
- I4. No hardcoded owner name: run T1–R4 with a second owner name.

Rung (CLAUDE.md test ladder): 5 (cross-crate contract, persistence, process lifecycle). `code-critic` round required.

## 8. Open questions {#SPEC-OWNERCON-08~draft}

**ID:** SPEC-OWNERCON-08~draft

1. Id prefix: reuse `D`/`A` families or add a new prefix (`C`)? r-039 fixes the current set.
2. Action parsing in `relay.sh`: structured relay header (`action:` line) or text matching on helper/hold keywords? Structured is reliable but changes every relay.
3. Who writes: only the Architect, or may a PM propose a constraint from an owner pane turn it witnessed?
4. Fleet-scope constraints: which ledger holds them when no single project owns the fleet?
5. Does `ask` block or time out to `deny`? A blocked PM idled 17 min in #10780.
6. Is the 24 h "expired" display window right, or should expiry notify the owner once?
7. Where the checker runs: Python script now, Rust in `trusty-architect` later; which is the contract?
