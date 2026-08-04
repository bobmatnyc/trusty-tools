# 0028. Memory recall splits into three tiers — Standing, Current, Episodic — with enforced retirement for Current

- **Status:** Proposed
- **Date:** 2026-08-04
- **Scope:** crates `trusty-common` (`memory_core`: retrieval layers, decay,
  drawer model), `trusty-memory` (prompt-facts surface, `prompt-context` hook
  command, MCP write tools). Read-side impact on every agent session that
  receives a `UserPromptSubmit` injection.
- **Reversibility Cost:** **Medium.** The design is **additive-only on the write
  path**: it introduces one new field and one new index, and it never deletes or
  rewrites a `DRAWERS` row across the 93 live palaces / 2,016 live drawers. A
  rollback is "stop reading the new field", not a data migration. The expensive
  half to reverse is the *injection contract* — once agents rely on a standing
  rule reaching every turn, removing that guarantee silently changes behaviour.
- **Decision Drivers:** the always-injected prompt-fact surface is empty
  estate-wide and has been since 2026-07-24 (`/api/v1/kg/prompt-context` returns
  the literal `EMPTY_PLACEHOLDER`); a relevance-ranked retriever structurally
  cannot surface an always-applicable rule; `expires_at` exists on the drawer
  model but is enforced nowhere; the decay half-life (90 days) exceeds the useful
  life of a point-in-time fact (hours) by three orders of magnitude; the
  most-injected drawer in the estate is a 19-day-old stale session checkpoint
  reaching 44.8% of all turns.
- **Supersedes / Superseded by:** extends ADR-0027 (Rooms/Wings/Closets). No ADR
  superseded.

---

## Context

### C1. The reported finding, verified

The input finding was: *"14 of 17 sampled facts never surfaced in 1,114 real
turns over 5 days… What surfaces: operationally recurring, tag-reinforced facts
— admin-merge policy, DCI. What doesn't: standing behavioral rules."*

Measured against the full hook log
(`~/Library/Application Support/trusty-memory/logs/enriched-prompts.*.jsonl`,
31 files, 2026-07-05 → 2026-08-04, 17,293 entries):

| Measurement | Value |
|---|---|
| `trusty-tools` prompt-context injections | **8,063** (not 1,114 — the finding sampled a 5-day window; the full corpus is 31 days) |
| Injections containing a **Project Context** (prompt-facts) section | **28 — 0.35%** |
| Injections containing a **Relevant memories** (drawer) section | 8,043 — 99.75% |
| Injections containing a **Relevant KG facts** section | 7,160 — 88.80% |

The finding's *direction* is confirmed and its *magnitude understated*. Per-fact
hit counts across all 8,063 injections:

| Fact | Injections | % of turns |
|---|---|---|
| admin-merge policy | 1,188 | **14.73%** |
| DCI | 112 | 1.39% |
| "LINK TO SOURCE-TREE URLs" (clickable links) | 2 | **0.02%** |
| "write plainly" / prose-correction | 13 | 0.16% |
| "just do it" / "don't ask" | 0 | **0.00%** |

### C2. Root cause — the two-tier hypothesis is CONFIRMED, with one correction

The hypothesis under test was that trusty-memory has two storage tiers with
different retrieval semantics, and behavioral rules were written into the wrong
one. That is **confirmed as to consequence and corrected as to mechanism.**

**Correction: prompt facts are not a separate storage tier.** They are a
*filtered view over the same knowledge graph*, selected by a four-entry
allow-list of "hot" predicates:

`crates/trusty-memory/src/prompt_facts.rs:54-59`
```rust
pub const HOT_PREDICATES: &[&str] = &[
    "is_alias_for",
    "has_convention",
    "is_fact",
    "is_shorthand_for",
];
```

`gather_hot_triples` walks every palace and keeps only triples whose predicate is
in that list (`prompt_facts.rs:170-200`). `build_prompt_context` returns an empty
`String` when nothing matched (`prompt_facts.rs:119-121`).

So the distinction is real and load-bearing, but it is a **predicate namespace**,
not a storage engine. That matters for the migration: promoting a fact between
tiers is a write of a new triple, not a data move.

**The retrieval semantics differ exactly as hypothesised.** Prompt facts are
genuinely always-injected — unfiltered, unranked, and placed **first** in the
block:

`crates/trusty-memory/src/commands/prompt_context/format.rs:29-32`
```rust
let mut out = String::new();
if let Some(facts) = global_facts {
    push_section(&mut out, facts.trim_end());
}
```

Drawers, by contrast, are relevance-ranked and capped at
`DEFAULT_TOP_K = 5` (`commands/prompt_context/mod.rs:129`).

**The structural argument holds and is now empirically demonstrated.** A
relevance-ranked retriever cannot surface an always-applicable rule, because such
a rule is equally relevant to every turn and therefore maximally relevant to no
particular query. Direct evidence, live against the daemon
(`GET /api/v1/palaces/trusty-tools/recall?top_k=5`):

- Prompt `"clickable links always"` → **the rule surfaces first**
  (`BOB CONVENTION 2026-08-01: LINK TO SOURCE-TREE URLs…`).
- Prompt `"merge this PR"` → 5 operational drawers, **rule absent**.
- Prompt `"run the tests"` → 5 operational drawers, **rule absent**.
- Prompt `"fix the bug in trusty-search"` → 5 release drawers, **rule absent**.

The rule is recallable. It surfaces **only when the prompt is about the rule** —
which is precisely the turn on which the model least needs it. That is the defect
in one sentence.

### C3. Why the section is missing — store empty, not code path absent

These are different bugs and the brief asked which one applies. It is the former,
verified at three levels:

1. The hook **does** query prompt facts:
   `commands/prompt_context/fetch.rs:24-40` issues
   `GET /api/v1/kg/prompt-context` (`PROMPT_CONTEXT_PATH`, `mod.rs:58`).
2. The live endpoint returns exactly `No prompt facts stored yet.` — the
   `EMPTY_PLACEHOLDER` constant (`mod.rs:200`), which
   `fetch_global_prompt_context` maps to `None` (`fetch.rs:35-39`), which
   `compose_injection` then omits.
3. `GET /api/v1/palaces/trusty-tools/kg/all?limit=200` returns **0 hot-predicate
   triples**.

**The mechanism works when populated.** The tier was in real use and decayed to
zero: 53 injections carried a Project Context section estate-wide, across four
palaces, first 2026-07-06, **last 2026-07-24**. The final one read:

```
## Project Context (from memory palace)

### Facts
- wiring test 2026-07-23
```

This is a tier that was tested, briefly used, and then abandoned — not one that
never worked. The failure is **disuse of a working surface**, which is a
materially better starting position than a broken one.

### C4. Quantification of the misfiling

Full **census** (not a sample) of the `trusty-tools` palace via
`GET /api/v1/palaces/trusty-tools/drawers?limit=2000` — 1,100 drawers returned
(daemon reports `drawer_count: 1098`; the two extra are concurrent writes during
the fetch). Estate: **93 palaces, 2,016 drawers**, of which `trusty-tools` holds
1,098 — **54% of the entire estate in one palace**.

Content-based classification, keyed on the real discriminator — *does this assert
a state that a later event falsifies?*

| Tier | Count | % of palace |
|---|---|---|
| **POINT-IN-TIME** (asserts a current, mutable state) | **674** | **61.3%** |
| **STANDING** (durable decision, rule, convention) | 342 | 31.1% |
| **EPISODIC** (completed historical record) | 46 | 4.2% |
| OTHER | 38 | 3.5% |

Tag census (of 7,032 distinct tags):

| Tag | Count | % of palace |
|---|---|---|
| `status` | 768 | 69.8% |
| `resume-target` | 654 | 59.5% |
| `bob-decision` | 232 | 21.1% |
| `standing-instruction` | 61 | 5.5% |
| `correction` | 37 | 3.4% |
| `feedback` | 25 | 2.3% |
| `repo-fact` | 9 | 0.8% |
| `doctrine` | 1 | 0.1% |

**The existing tag vocabulary does not encode the tier distinction.** Two
findings make this concrete:

- `resume-target` is overloaded. It means *"reload me when the session resumes"*,
  not *"I expire"*. Content-classifying the 654 `resume-target` drawers gives
  71.4% point-in-time but **25.5% standing** — one tag spanning both tiers.
- `standing-instruction` is overloaded in the opposite direction. Drawer
  `f59fb536` is tagged `standing-instruction` **and** `status` **and**
  `resume-target`, and its content is a *one-shot* pre-authorised action
  ("reinstall both daemons once merged"). A one-shot action is the precise
  opposite of a standing rule.

Any design that infers tier from the existing tags inherits this ambiguity. The
tier must be an explicit field.

### C5. Why the winners win — "tag harder" is a trap

The brief asked whether the surfacing mechanism makes tag reinforcement a viable
workaround. It does not. Two hypotheses were tested against the 8,063-injection
corpus and **both of the intuitive ones are refuted**:

| Hypothesis | Test | Verdict |
|---|---|---|
| Long, token-rich drawers win by lexical overlap | median content length: injected ≥100× = **1,597 chars**; never injected = **2,691 chars** | **REFUTED** — winners are *shorter* |
| Tag density drives surfacing | median tag count: injected ≥100× = **18**; never injected = **17** | **REFUTED** — no effect |
| Access-count feedback loop reinforces winners | `access_count` = **0 on all 1,100 drawers** | **REFUTED** — never incremented |

The actual mechanism is `importance`, and it is already a working privilege dial:

| `importance` | n | median injections | mean | max |
|---|---|---|---|---|
| 0.5 | 927 | 4.0 | 25.4 | 1,711 |
| 1.0 | **138** | **33.0** | 114.9 | **3,612** |

An `importance = 1.0` drawer receives **8.25× the median injection rate**.
Tagging harder does nothing; raising importance does everything.

This is decisive for the design in two ways. First, **a privileged marker already
exists** — the owner's proposed mechanism is not new construction, it is
reclamation of a field that already works. Second, **the privilege is currently
misallocated**: the single most-injected drawer in the estate, reaching 3,612
turns (**44.8%**), is `importance: 1.0`, created `2026-07-16`, and reads
`SESSION CHECKPOINT 2026-07-16 … MERGE TRAIN COMPLETE, EIGHT PRs merged`. A
19-day-old point-in-time fact occupies nearly half of all injections.

### C6. An always-on tier already exists, and issue #633 disabled it

`crates/trusty-common/src/memory_core/retrieval/types.rs:43`
```rust
pub(super) const L1_CAP: usize = 15;
```

L1 is the top-15 drawers by importance, refreshed by `refresh_l1`
(`retrieval/handle.rs:459-470`) and **present in every recall**
(`retrieval/layers.rs`, `build_l0_l1`; test `l0_l1_always_present`).

But it was deliberately neutered:

`crates/trusty-common/src/memory_core/retrieval/layers.rs:28-41`
```rust
/// Why: L1 drawers that were not in the vector search results have unknown
/// similarity to the query.  Assigning them their raw importance
/// (e.g. 1.0) made them dominate the ranked output even when they were
/// completely off-topic (issue #633). …turning
/// importance into a mild tiebreaker rather than the primary ranking signal.
pub(super) const L1_NO_SIMILARITY_PENALTY: f32 = 0.15;
```

This is the crux. The codebase **already tried an always-on high-importance
tier**, and it failed — because what was placed in it was episodic and
point-in-time content that was off-topic on most turns. The remedy (#633) was to
penalise the whole tier into near-irrelevance, which also destroyed the
legitimate always-on use case.

The tier is also structurally oversubscribed: **138 drawers at `importance = 1.0`
compete for 15 L1 slots.** Selection among ties is arbitrary. A standing rule
cannot reliably hold a slot even if promoted to maximum importance.

**Conclusion: the answer is not to re-enable L1 for everything. It is to separate
"always applicable" from "currently true" so each gets a surface with the right
semantics — which is what "trusty must serve both" requires.**

### C7. Point-in-time facts fail by the OPPOSITE mechanism from standing rules

The brief required determining which of two failure modes applies: *"never
surfaced during the window when it mattered"* or *"still ranked high after it
stopped being true."*

**The data is unambiguous: it is the latter.** The 15 most-injected drawers in
the estate are, without exception, expired point-in-time session checkpoints:

| Injections | % of turns | Drawer (created) |
|---|---|---|
| 3,612 | 44.8% | `SESSION CHECKPOINT 2026-07-16 — MERGE TRAIN COMPLETE, EIGHT PRs merged` |
| 1,711 | 21.2% | `RESUME (trusty-code M1, PAUSED 2026-07-06). origin/main = bf7f6df8` |
| 1,257 | 15.6% | `SESSION PAUSE #19 2026-07-17 — RESUME TARGET, FIVE IN-FLIGHT CHAINS` |
| 1,096 | 13.6% | `SESSION PAUSE #17 2026-07-16 — RESUME TARGET, TWO IN-FLIGHT CHAINS` |
| 1,027 | 12.7% | `MAIN IS GREEN AGAIN 2026-07-22 (supersedes the "MAIN IS RED" drawer…)` |

Drawer #2 pins `origin/main = bf7f6df8` — a value that has been wrong for four
weeks — and asserts it into one turn in five.

The arithmetic explains it exactly. Decay uses a **90-day half-life**
(`memory_core/decay.rs:12-31`, `half_life_days: 90.0`):

```
effective = base * 2^(-age_days / 90)
```

| Age | Effective importance from base 1.0 |
|---|---|
| 5 days | 0.962 |
| 19 days | 0.864 |
| 30 days | 0.794 |

A point-in-time fact has a useful life measured in **hours**. The decay curve
retains 86% of its weight at **19 days**. That is a mismatch of roughly three
orders of magnitude, and it is why stale status drawers dominate.

**These two tiers therefore fail for opposite reasons, and a single fix cannot
serve both:**

- Standing rules fail because they are **never ranked high enough** — they are
  equally relevant to everything, so they win nothing.
- Point-in-time facts fail because they are **still ranked high long after they
  stopped being true** — nothing retires them.

Critically, these failures are **coupled**: the stale point-in-time drawers are
consuming the five top-k slots that standing rules would otherwise compete for.
Fixing retirement directly improves standing-rule surfacing even before any
always-injected tier exists.

### C8. `expires_at` exists and is enforced nowhere

The drawer model already carries `expires_at`. It is **set on 12 of 1,100
drawers (1.1%)**, and — decisively — it is **read by nothing in the retrieval
path**. Workspace-wide, the only non-test references in the memory crates are:

- `crates/trusty-memory/src/tools/memory_ops.rs:395` — serialises it to JSON output.
- `crates/trusty-memory/src/service/helpers.rs:486` — hardcodes it to `None`.

There is no filter on expiry anywhere in `memory_core`. Of the 674 point-in-time
drawers, **99.6% carry no `expires_at` at all**, and **79.1% were created before
2026-08-01** (≥4 days stale at time of writing).

### C9. The supersession convention is real but unreliable

The estate already uses hand-written supersession. Measured across the census:

| Verb | Edges |
|---|---|
| `supersedes-` | 62 |
| `amends-` | 22 |
| `extends-` | 15 |
| `corrects-` | 9 |
| `replaces-` | 1 |
| **Total** | **109** |

But only **45 of 109 (41.3%)** resolve to an actual drawer id. The other
**64 (58.7%)** are free-text labels (`supersedes-open-item`,
`supersedes-stale-process-theory`) that no machine can follow.

And on the case that matters most, **the chain is broken**. Verified:

- Drawer `e80475d0` (2026-08-04T20:09:36Z) asserts
  `PR #4818 … head d39638482bfe8de462c02c4f40e02b56b16897ff`.
- `git merge-base --is-ancestor d3963848 origin/main` → **not an ancestor**. The
  SHA is stale; the branch was rebased during merge.
- The drawer that carries the correction, `a61749d7` (21:27:53Z), states
  `it merged at head 59ae50d8, NOT the d…` — but it is tagged
  `extends-f59fb536`. **Nothing points at `e80475d0`.**

So the correction exists, and a machine following the supersession graph would
never find it. The convention is **load-bearing enough to formalise** (109 edges
of real authorial intent) and **too unreliable to trust as-is** (59% dangling,
and it missed the one link that mattered).

### C10. The KG section is consuming budget with noise

Of 32,622 triples actually injected into real turns:

| Predicate | Injected | Share |
|---|---|---|
| `tags` (`tag:X --tags--> drawer:UUID`) | 26,234 | **80.4%** |
| `mentioned-in` | 2,267 | 6.9% |
| `contains` | 2,038 | 6.2% |
| `is-a` | 1,067 | 3.3% |
| `uses` / `depends-on` | 926 | 2.8% |
| `is_alias_for` / `is_fact` (hot) | 82 | **0.25%** |

**93.5% of injected triples are structural graph plumbing** — tag-to-drawer and
room-to-drawer membership edges that tell the model nothing. The `is-a` entries
are the low-precision regex table's output (`but --depends-on--> shared`,
`which --is-a--> different`).

Cost: the KG section is present in 88.8% of injections at a **median 353 bytes**,
**10.5% of the median injection**. This is not neutral; it is roughly 330
bytes/turn of near-pure noise. Related open issues #4775, #4776, #4810.

### C11. The budget is already tight

| Metric | Value |
|---|---|
| `INJECTION_BYTE_CAP` (`mod.rs:142`) | **4,096 bytes** |
| Median injection | **3,349 bytes (81.8% of cap)** |
| p90 | 3,779 bytes |
| p99 / max | 4,096 (at cap) |
| Injections ≥4,000 bytes | 339 |
| Median drawer section | 2,983 bytes |
| Median KG section | 353 bytes |

There is a median of ~750 bytes of headroom. An always-injected tier must be
sized against that, and cannot simply be added on top.

---

## Decision

Split memory into **three explicit tiers**, distinguished by *lifetime*, served
by **two injection surfaces plus one unchanged ranked surface**.

| Tier | Lifetime | Surface | Ranked? | Budget |
|---|---|---|---|---|
| **S — Standing** | Permanent until retired by a human | Always-injected prompt block | No | Hard cap 20 facts / 1,600 B |
| **C — Current** | Bounded; must declare how it ends | Always-injected *while live* | No | Hard cap 6 facts / 800 B |
| **E — Episodic** | Permanent | Relevance-ranked drawer recall | **Yes — unchanged** | Remainder |

### D1. The authoring rule — one question decides the tier

> **"When does this stop being true?"**

- **"It doesn't."** → Tier **S**. It is a rule, convention, or preference.
  *Test:* it constrains **how** work is done, never **what** is currently the
  case. "Write plainly." "Clickable links always." "Never merge red CI."
- **"When <specific event> happens."** → Tier **C**. The writer must be able to
  name the event. If they can name it, they can encode it (D4).
  "PR #4818 is in flight at head d3963848."
- **"It was true then and will always have been true."** → Tier **E**. A
  historical record. "trusty-search 0.34.0 shipped 2026-07-17."

The discriminator is deliberately **not** "is it dated". A `BOB DECISION
2026-07-18` is Tier S with a provenance date; the date is when it was *decided*,
not when it *expires*. Misreading provenance as expiry is what makes naive
date-based classification inflate the point-in-time bucket from 61.3% to 91.4%
(measured, §C4).

### D2. Tier S — standing rules

**Storage:** hot KG predicates, the existing surface (`prompt_facts.rs:54-59`).
`has_convention` for conventions, `is_fact` for ambient facts, `is_alias_for` /
`is_shorthand_for` for naming. **No new storage engine.**

**Injection:** already implemented and already first in the block
(`format.rs:29-32`). Nothing to build; the tier is empty, not broken.

**Form constraint:** each fact **must fit on one line, ≤ 80 characters.** This is
a forcing function, not formatting fussiness — a rule that cannot be stated in 80
characters is a document, and belongs in `CLAUDE.md` with a pointer. It also
makes the budget arithmetic exact: 20 × 80 = 1,600 bytes worst case.

### D3. Tier C — current facts, and the "privileged" marker

The owner's proposal — *"can we have point-in-time facts tagged as privileged?"*
— **survives contact with the code, and is better than it looks**, because the
privilege dial already exists and already works: `importance` delivers 8.25×
median injection lift (§C5). This is reclamation, not new construction.

**Are Tier S and Tier C one mechanism with a lifetime attribute, or two?**

**Two mechanisms.** They differ on the property that governs everything else:

|  | Tier S | Tier C |
|---|---|---|
| Retirement | Human decision only | **Mandatory, machine-enforced** |
| Failure if wrong | Rule is outdated — annoying | Fact is **false** — actively harmful |
| Write authority | Deliberate, rare, reviewed | Routine, frequent, per-session |
| Cadence | Months | Hours |

Collapsing them into one tier with a nullable lifetime field would make the
mandatory-retirement invariant optional, and the enforcement in D4 is the entire
value of Tier C. A nullable field is an invariant you do not have.

### D4. Mandatory retirement — the load-bearing invariant

> **A fact cannot enter Tier C without declaring how it ends.**

Every Tier C write supplies **at least one** retirement condition:

1. **`expires_at`** — an explicit timestamp. Already on the model (§C8); needs
   *enforcement*, not invention.
2. **`live_while`** — an optional verifiable predicate, e.g.
   `gh:pr-open:4818`. Precise rather than merely safe: it retires the fact when
   the PR actually merges, not when a timer happens to fire.
3. **Default TTL — 24 hours** when the writer supplies neither.

**Fail-closed admission.** A Tier C write with no valid retirement condition is
**not admitted to the privileged tier**; it degrades to an ordinary Tier E
drawer. This is the safety property that makes the whole design defensible:

> **The worst case degrades to today's behaviour, never below it.**

This directly answers the sharpest objection in the brief — *a privileged fact
that goes stale is worse than an ordinary stale drawer, because it surfaces every
turn.* That failure mode is **unreachable by construction**: a fact with no
retirement condition never becomes privileged, and a fact whose condition has
fired is no longer injected. Privilege and mandatory expiry are the same
transaction.

**Enforcement point:** expiry is evaluated at **read time**, in the retrieval
path, not by a background sweeper. A sweeper that fails silently reintroduces
exactly the bug being fixed; a read-time filter cannot fail open without the
recall itself failing.

**Decay:** Tier C uses a **half-life of hours, not 90 days**. The 90-day constant
(`decay.rs:12-31`) stays correct for Tier E and is left alone.

### D5. Supersession-on-replace — the replacement key

The owner identified this as slot semantics rather than append semantics. Agreed,
and **the replacement key is the load-bearing decision.** Options evaluated
against the estate:

| Candidate key | Verdict |
|---|---|
| Subject string | **Rejected.** The same PR appears as `PR #4818`, `#4818`, and `the dir_size_bytes fix` across real drawers. Nothing would ever supersede. |
| Tag | **Rejected.** Tags are an unordered set with no cardinality guarantee — the real drawers carry 18 of them. "Which tag is the key?" has no answer, and `resume-target` / `standing-instruction` are demonstrably overloaded (§C4). |
| Writer-chosen explicit slot | **Accepted.** |

**Decision: a first-class `fact_key` field** — an explicit, writer-chosen,
namespaced slot name.

```
pr:4818/state
ws:tm-trusty-tools-03/resume
daemon:trusty-search/install-state
```

Writing a Tier C fact with an existing `fact_key` **atomically retires the prior
occupant of that slot.** One slot, one live fact. This is what makes the store
self-limiting: `pr:4818/state` can be written fifty times and still occupies
exactly one slot.

Namespacing is required (`<domain>:<id>/<aspect>`) to prevent the failure the
brief warned about — unrelated facts clobbering each other. A bare key like
`state` would collide across every workstream.

**Does supersession need to be explicit or can it be inferred?** Explicit, with
one concession to what people actually do: **the existing `supersedes-<drawer-id>`
tag becomes a second valid explicit form.** The 45 resolvable edges (§C9) are
genuine authorial intent already expressed in a machine-readable way; honouring
them costs nothing and formalises a convention rather than competing with it. The
64 free-text edges are recorded as prose provenance and are explicitly **not**
machine-followed — silently guessing what `supersedes-stale-process-theory` means
is worse than not trying.

Inference is rejected as the primary mechanism precisely because §C9 shows what
happens when humans are left to link by hand: the one correction that mattered
pointed at the wrong drawer.

### D6. What happens to a superseded fact

**Demoted, never deleted.** On supersession the prior fact:

1. loses its Tier C privilege (stops being injected),
2. **remains a fully readable drawer** in Tier E,
3. gains a machine-readable `superseded_by` pointer to its replacement.

Hard deletion is rejected on evidence, not principle: this estate produced 109
hand-written amendment edges (§C9) specifically so that the *record of a
correction* survives. Deleting the superseded fact would destroy the artefact the
authors were at pains to create. Tombstoning without readable content would do
the same more quietly.

This also satisfies the migration-safety constraint absolutely: **no drawer row
is ever deleted or rewritten.** Demotion is the absence of a promotion.

### D7. Injection budget and tier interaction

Fixed allocations, evaluated in order, each independently capped:

| Order | Section | Cap | Overflow behaviour |
|---|---|---|---|
| 1 | **Tier S** — standing rules | 1,600 B | Cannot overflow; write-time cap of 20 |
| 2 | **Tier C** — current facts | 800 B | Newest-first by `fact_key`; oldest slots dropped |
| 3 | **Tier E** — ranked drawers | remainder (≥ 1,300 B) | Existing top-k truncation, unchanged |
| 4 | **KG facts** | 256 B, hot predicates only | Structural predicates excluded entirely |

**Where the budget comes from — the honest accounting.** The constraint was
explicit: do not fund standing rules by shrinking drawer recall. Two reclamations
pay for the new tiers without touching the working drawer path:

1. **KG noise: ~330 B/turn.** 93.5% of injected triples are structural
   (§C10). Excluding `tags` / `mentioned-in` / `contains` from injection removes
   noise, not signal.
2. **Stale Tier C eviction: the large one.** The top drawer alone occupies a slot
   in 44.8% of turns with 19-day-old false information. Retiring expired facts
   frees top-k slots that are currently spent asserting things that are untrue.

This is why the design **does not degrade episodic recall — it improves it.**
Drawer recall keeps its ranked semantics, its top-k, and its budget floor; what it
loses is competition from content that is actively wrong. Operational recall
(admin-merge at 14.73%, DCI at 1.39%) is untouched: both are ranked Tier E hits
and neither depends on anything this ADR changes.

`INJECTION_BYTE_CAP` rises from 4,096 → **4,608 bytes** (+512 B ≈ +128 tokens/turn)
to preserve the ≥1,300 B drawer floor at p90. This is a deliberate, bounded cost,
and it is smaller than the reclamation in (1) and (2) combined.

### D8. Keeping Tier S small — the budget-control answer

The scepticism was demanded and it is correct: ten standing rules is fine, two
hundred is a new problem. Four mechanisms, of which only the first is load-bearing:

1. **A hard numeric cap of 20, enforced at write time.** Not a guideline. The
   21st write **fails** with an error naming the current 20 and requiring the
   author to retire one. This is the forcing function: promotion becomes
   zero-sum, so every addition is an explicit trade rather than an accretion.
   Every "review discipline" that is not a hard cap has already failed here —
   the estate accumulated 1,098 drawers with no cap and 12 expiry dates.
2. **The 80-character form constraint** (D2) bounds worst-case bytes at 1,600
   independently of count.
3. **Promotion requires an explicit human act.** No agent auto-promotes to Tier S.
   Agents write Tier C and Tier E freely; Tier S is a deliberate, rare operation.
4. **Quarterly re-affirmation.** Each Tier S fact carries `affirmed_at`. Facts
   unaffirmed for 90 days are surfaced in `trusty-memory doctor` as candidates
   for retirement — reported, never auto-removed, because silently dropping a
   standing rule is the failure this ADR exists to prevent.

The cap is the answer. The rest is hygiene.

---

## Consequences

### Walkthrough: the #4818 / #4820 case under this design

The test case, traced through all three moments. Today's behaviour is measured
(§C9); the proposed behaviour follows from D3–D6.

**At write time (2026-08-04T20:09:36Z).** The author writes the in-flight state
of PR #4818.

- *Today:* drawer `e80475d0`, `importance` unset-to-default, `expires_at: None`,
  18 tags including `status` and `resume-target`. Nothing records that it will
  expire.
- *Proposed:* Tier C write with `fact_key = pr:4818/state`. Admission requires a
  retirement condition; the author supplies `live_while: gh:pr-open:4818`, or
  else the 24-hour default TTL applies. The fact is injected on every turn while
  live — which is correct, because during that window it was genuinely important.
  **This is the half the current system gets right by accident and the design
  keeps on purpose.**

**At merge time (21:14:49Z, squash `4c412ae1` at head `59ae50d8`).**

- *Today:* nothing happens. `e80475d0` continues to assert head `d3963848`, a SHA
  that `git merge-base --is-ancestor` confirms is **not an ancestor of
  origin/main**. The author notices and hand-writes correction `a61749d7`, tagging
  it `extends-f59fb536` — **pointing at the wrong drawer**. The correction exists
  and is unreachable from the thing it corrects.
- *Proposed:* two independent mechanisms retire it, and either alone suffices.
  (a) `live_while: gh:pr-open:4818` goes false → the fact leaves Tier C at the
  next read. (b) The author writes the post-merge state to the **same**
  `fact_key = pr:4818/state`; that write *is* the supersession — one slot, one
  live fact. The author cannot mis-link it, because the key is the link. The
  stale fact is demoted to Tier E with `superseded_by → <new id>`, still readable,
  never deleted.
- The `amends-04f4bc2f` tag `e80475d0` already carries is honoured under D5 as an
  explicit edge, since `04f4bc2f` resolves to a real drawer.

**When a future session recalls (now).**

- *Today:* the stale fact is eligible for injection indefinitely — its
  `importance` decays only 13.6% in 19 days (§C7), and its cohort demonstrably
  reaches up to 44.8% of turns. A future session reads head `d3963848` and acts
  on a SHA that does not exist on main. The correction is one hop away through a
  broken link the session has no reason to follow.
- *Proposed:* the stale fact is not in Tier C (retired) and does not surface as
  ambient truth. It remains recallable as history, and it carries a
  `superseded_by` pointer, so a session that *does* reach it is led to the
  correct SHA rather than away from it.

**This case improves at every one of the three moments**, which was the stated bar.

### What this does not fix

- The 64 free-text supersession edges (§C9) stay unresolvable. The design
  formalises the 45 that resolve and stops the bleeding for new writes; it does
  not retro-link prose labels, and guessing at them would manufacture false
  provenance.
- Tier assignment for the 1,100 existing drawers is a **classification** problem,
  and §C4 shows tags cannot settle it (`resume-target` splits 71/26 across tiers).
  Migration (below) is therefore additive and human-gated, not automatic.
- The KG's low-precision `is-a` extraction (#4775/#4776/#4810) is *excluded from
  injection* here, not repaired. Repair is separate work.

### Migration

**Constraint honoured absolutely: nothing deletes or rewrites a drawer row.**
1,098 live drawers in `trusty-tools`, 2,016 across 93 palaces.

1. **Tier S is additive from empty.** The surface is at zero (§C3), so there is
   nothing to migrate *out of*. Promotion writes a **new KG triple**; the source
   drawer is untouched and keeps working. Worst case on rollback: some new triples
   nobody reads.
2. **Tier C is opt-in for new writes.** Existing drawers do not become Tier C.
   They keep exactly today's ranked behaviour.
3. **Backfill is read-only and human-gated.** A report lists the 674 point-in-time
   candidates ranked by injection frequency, so the worst offenders — the ones
   reaching 44.8% and 21.2% of turns — are triaged first. Retirement is applied by
   setting `expires_at` on a drawer, which **adds** information and removes none.
4. **Rollback** is to stop reading `fact_key` / `expires_at` and restore
   `INJECTION_BYTE_CAP`. No data is recoverable-only-by-backup at any point.

### Risks

| Risk | Mitigation |
|---|---|
| Tier S grows past usefulness | Hard write-time cap of 20 (D8); the cap is the mechanism, not the guidance |
| Authors skip `fact_key`, Tier C stays empty | Fail-closed degradation means the cost of skipping is *today's behaviour*, never worse |
| `live_while` checker unavailable/slow | It is optional and advisory; the mandatory TTL is the fail-safe floor |
| Read-time expiry filter adds latency | Timestamp comparison per candidate drawer, on an already-capped result set |
| Re-enabling an always-on tier repeats #633 | #633 failed because *episodic* content was always-on. Tier S admits only 80-char standing rules; Tier C admits only facts with enforced retirement. `L1_NO_SIMILARITY_PENALTY` is unchanged. |

---

## Related Decisions

Vetted against `docs/adr/INDEX.md` and prior decisions on 2026-08-04:

- **ADR-0027 (Rooms are real; Wings are scopes; Closets are an index):**
  *Consistent, and adopts the same safety doctrine.* 0027's load-bearing
  guarantee is that existing drawers are named, never reclassified or rewritten.
  This ADR applies that doctrine to a different axis: tiers are assigned by
  **adding** a `fact_key` / `expires_at` to new writes and by adding KG triples
  for promotions, never by rewriting or deleting a `DRAWERS` row. Both ADRs are
  additive-only over the same 93-palace estate, so their migrations compose
  without ordering constraints.
- **ADR-0009 (External-extractor KG ingest contract):** *Consistent.* This ADR
  adds no KG ingest path. It **narrows what is read out** of the KG at injection
  time (D7: hot predicates only, structural `tags` / `mentioned-in` / `contains`
  excluded), which is a consumer-side filter and leaves the ingest contract
  untouched.
- **ADR-0010 (KG edge-kind extensibility — standard kinds plus `Custom`):**
  *Consistent.* Tier S reuses the four existing hot predicates
  (`prompt_facts.rs:54-59`) rather than minting new edge kinds. No extension to
  0010's taxonomy is required or proposed.
- **Issue #633 (L1 importance dominated off-topic recall):** *Preserved, not
  reversed.* `L1_NO_SIMILARITY_PENALTY = 0.15` stays exactly as-is
  (`retrieval/layers.rs:41`). This ADR does not re-enable always-on ranking for
  ordinary drawers; it routes always-on content to two surfaces with admission
  rules (an 80-char cap for S, enforced retirement for C) that the content
  failing #633 could not have satisfied.
- **Issues #4775 / #4776 / #4810 (KG query defects):** *Orthogonal, and partly
  mitigated.* D7 excludes the low-precision structural and regex-derived triples
  from injection, which removes their user-visible cost (~330 B/turn) without
  claiming to repair the underlying extraction. Those issues remain open on their
  own merits.
