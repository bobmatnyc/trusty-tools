# Self-Improvement Loop as a Standard Harness Feature — Design

**Status:** Draft (design, no code changes)
**Owner directive:** 2026-09-13 — "make this feature a standard part of mpm
(and code, which should pick up all harness conventions). In addition to
submitting coding fixes, save these as behavioral changes." Preferred
mechanism: a tagged room in trusty-memory, consolidated during dreaming.
`MEMORY.md` is the fallback only if that proves inefficient, and it must then
be pruned regularly.
**Scope:** how a self-improvement finding — from an agent's closing report or
from the PM's own `#6937` hypotheses — is captured, stored, consolidated into
a confirmed theme, surfaced back to the PM at a fixed cost, and routed to a
durable fix; and what `trusty-code` needs to reach parity with `trusty-mpm` on
all of it.

All citations are against `origin/main` at `ea58a8c4b` (2026-09-13), read
directly from the working tree and with `gh pr diff`/`gh issue view` for
in-flight work. No source file was edited to produce this document.

---

## 1. What exists today

### 1.1 The two closing blocks (BASE-AGENT)

`crates/trusty-agents-common/src/assets/agents/BASE-AGENT.md:516-614` defines
two mechanisms every composed agent carries today:

- **"Self-Analysis and Improvement Reporting"** (`:516-554`) — every task ends
  with an **Improvement recommendations** block (Symptom/Cause/Change/Evidence)
  when a finding exists. A dispatched subagent never files the issue itself;
  it ends its report with the block and the **PM** routes it to `ticketing`,
  which searches for the `self-improvement` label before opening a new issue
  (`:546-549`). This is the channel `#6933` (the recurring harness
  post-mortem) and `#6935` read.
- **"Continuous Self-Improvement — the Fast Loop"** (`:556-614`, `#6937`) —
  seven detection heuristics (retry-on-same-failure, gate-red-after-done,
  second review round, a PM/user correction, a circuit-breaker trip, a
  repeated tool error, an estimate overrun). A trip with a nameable
  alternative is written **directly by the agent**, via `memory_remember`,
  tagged `self-improvement-hypothesis`, as a seven-field record (trigger,
  what was tried, hypothesis, metric+baseline, judgement rule, status,
  evidence). The post-mortem reads it back with
  `memory_list(tag: "self-improvement-hypothesis")`.

**#7723, same day, moves both sections into a new skill,
`self-improvement-loop`** — this design is written against that skill name,
not against fixed `BASE-AGENT.md` line numbers, since those lines are mid-move.

These two mechanisms already answer "detect" and "record the hypothesis."
Neither answers: where the hypothesis lives so a THEME across many sessions
can be told apart from a one-off, what makes a theme "confirmed," what the PM
sees about it on its next turn, or what ships a durable fix. That gap is this
document's scope.

### 1.2 The prompt-feedback ledger (#7688 / PR #7702) — a proven, narrower analog

[#7688](https://github.com/bobmatnyc/trusty-tools/issues/7688) /
[PR #7702](https://github.com/bobmatnyc/trusty-tools/pull/7702) (branch
`feat/prompt-self-improvement`, open, unarmed pending review) ships a sibling
feature for a narrower question — "what was wrong with the PROMPT, not the
work" — and its mechanism is the template this design extends rather than
duplicates:

- `prompt_self_improvement` config key, project `.trusty-mpm.toml` over host
  `[pm]`, default `false`
  (`crates/trusty-mpm/src/core/prompt_self_improvement.rs`, `enabled_for`).
- When on, `append_to_pm_prompt` appends a fixed addendum
  (`crates/trusty-mpm/src/assets/instructions/sections/prompt-self-improvement.md`)
  asking for a `## Prompt feedback` section, capped implicitly at 5 lines by
  instruction and explicitly at 4 KiB by the extractor
  (`MAX_FEEDBACK_BYTES`, `crates/trusty-mpm/src/core/prompt_feedback.rs:60-67`).
  The PM is asked to forward the same request to every dispatch brief — there
  is **no second, deploy-side injection** into the shared agent files, because
  those files are machine-global across projects (#4409) and a per-project
  flag baked into them would have two projects overwrite each other's copy.
- **Capture is harness-side, not agent-side.** `tm hook --prompt-feedback`
  registers on `Stop` AND `SubagentStop`
  (`crates/trusty-mpm/src/core/session_launch/prompt_feedback_hooks.rs:1-100`,
  `PROMPT_FEEDBACK_EVENTS = ["Stop", "SubagentStop"]`), reads the Claude Code
  hook JSON payload from stdin, tails the named transcript (2 MiB,
  `crates/trusty-mpm/src/bin/tm/commands/prompt_feedback_hook.rs:27-33`),
  extracts the `## Prompt feedback` section from the **last** assistant
  message, and appends one JSON-Lines row to
  `~/.trusty-mpm/prompt-feedback.jsonl`
  (`crates/trusty-mpm/src/core/prompt_feedback.rs:95-100`, `append_row`).
  `tm prompt-feedback --summary` reads it back, grouped by `agent_type`.
- **Every arm fails open.** An unreadable transcript, an absent payload, or an
  unwritable ledger logs one `warn` and exits 0
  (`prompt_feedback_hook.rs` module doc, "FAIL-OPEN, TOTAL"). A
  `SubagentStop` naming no `agent_type` is booked under a distinct
  `unknown-subagent` sentinel, never folded into the PM's own `pm` bucket —
  the #7702 fix for the specific failure mode "an untyped subagent's critique
  corrupts the PM's own count" (`prompt_feedback.rs:33-44`).

This is **not** the mechanism to extend wholesale — it deliberately writes a
flat file, not the knowledge graph, because a prompt critique is read back by
one narrow CLI command, not injected into anyone's next turn. But its three
properties are exactly what §2 below borrows: **harness-side capture from the
transcript** (no agent compliance dependency), **one constant, three
readers** (the heading string, the extractor, and the PM instruction all
cite the same symbol), and **fail-open, total**.

### 1.3 tm-postmortem / bug-reporting — a related but distinct pipeline

`Skill(skill="tm-bug-reporting")` and the `list_recent_errors` /
`preview_bug_report` / `report_bug` MCP tools
(`crates/trusty-mpm/src/daemon/mcp_bugreport.rs:32-93`) aggregate **crate
panic/error telemetry** (fingerprinted by `crate_target` + `crate_version`)
into GitHub issues. This is runtime-crash reporting, not prompt-quality or
workflow-quality feedback, and nothing here should route through it — the
"Improvement recommendations" block already has its own home in `ticketing`
(§1.1). Cited for completeness because the task asked for it; it is not a
candidate capture or storage mechanism for this design.

### 1.4 trusty-memory: rooms, wings, dreaming, and the two fact tiers that matter

**Rooms and wings (ADR-0027).** `room_create` accepts any label, built-in or
custom, case-insensitively deduplicated
(`mcp__trusty-memory__room_create` tool contract); `RoomType::Custom(String)`
is a first-class variant
(`crates/trusty-common/src/memory_core/palace.rs:62-73`, `RoomType::parse`,
`:85-98`) and `dream_consolidate_room`'s `room` argument accepts it — a custom
room named e.g. `self-improvement` needs no schema change to exist.

**What `palace_dream` / `dream_consolidate_room` does to a room today.** The
MCP handler (`crates/trusty-memory/src/tools/dream_ops.rs:49-129`,
`handle_dream_consolidate_room`) resolves an optional `room` and a
`max_age_days` (default 7), then calls `consolidate_scoped`
(`crates/trusty-common/src/memory_core/dream/semantic.rs:394-410`, thin
wrapper around `consolidate_scoped_within`, `:423-527`). That function:

1. No-ops on a non-positive age window, or when no inference backend is
   configured (`build_consolidator_from_config` returns `None`) — it never
   calls a model the operator did not configure (`semantic.rs:431-456`).
2. Selects every non-`Task` drawer in the room older than the cutoff
   (`:491-498`).
3. Passes the whole snapshot to `SemanticConsolidator::consolidate`, which
   asks an LLM to propose `Merge { canonical_content, superseded_ids }`
   actions across the set — genuine **semantic near-duplicate clustering**,
   not a keyword or tag group-by
   (`crates/trusty-common/src/memory_core/semantic_consolidation/inference.rs`).
4. Applies each merge: writes one new canonical drawer, evicts every
   superseded id (`apply_consolidation_result`, `handle.forget`), and flushes.
5. Returns `RoomConsolidationStats { summary_facts_created, facts_evicted }`
   (verified against `consolidate_scoped_filters_by_room`,
   `crates/trusty-common/src/memory_core/dream/tests.rs:1574-1661`: two aged
   Planning drawers in, one canonical summary out, both originals evicted).

**What it does NOT do today, and would need to:** the consolidator clusters
by semantic similarity across the whole room-and-age-window snapshot; it has
no notion of a "theme" tag, no per-theme occurrence count, and no promotion
rule. §2.3 below is the gap this design closes — grouping by a `theme` tag
before or alongside the LLM merge, so ten independent mentions of the same
root cause collapse to one row with a count, rather than ten rows an LLM
happens to cluster together or doesn't.

**Tier S vs Tier C — which resident-fact mechanism fits (ADR-0028).** The
hot-predicate prompt-facts surface
(`crates/trusty-memory/src/prompt_facts.rs:54-59`, `HOT_PREDICATES =
["is_alias_for", "has_convention", "is_fact", "is_shorthand_for"]`) splits
into two tiers with opposite properties
(`docs/adr/0028-memory-recall-tiers-standing-current-episodic.md:474-494`):

| | Tier S (standing) | Tier C (current) |
|---|---|---|
| Retirement | Human decision only | **Mandatory, machine-enforced** |
| Write authority | Deliberate, rare, reviewed | Routine, frequent, per-session |
| Cadence | Months | Hours |
| Budget | Hard cap **20 facts / 1,600 B** (`TIER_S_MAX_FACTS`, `prompt_facts.rs:90`) | 800 B, newest-first by `fact_key`, oldest dropped (ADR-0028 D7) |
| Admission | `add_alias`/`kg_assert` on `is_fact` et al.; degrades with a "Tier S is full" error past the cap | `fact_key` + `expires_at`/`live_while`/24h-default TTL; a write with no retirement condition degrades to Tier E, never blocks |

Tier S explicitly forbids agent auto-promotion (ADR-0028 D8.3, "No agent
auto-promotes to Tier S. Agents write Tier C and Tier E freely; Tier S is a
deliberate, rare operation.") — so an automatically-promoted self-improvement
theme belongs in **Tier C**, not Tier S, and §2.4 below designs against that.

### 1.5 The `UserPromptSubmit` injection path, and the absence of a `SubagentStop` memory hook

`crates/trusty-memory/src/commands/prompt_context/mod.rs:349-362`
(`emit_hook_event`) is the `UserPromptSubmit` hook that builds the injected
block every PM turn: Tier S first, then Tier C, then ranked Tier E drawers
(ADR-0028 D7), composed by `compose_injection`
(`crates/trusty-memory/src/commands/prompt_context/tests.rs` exercises the
composition order). This is the **existing, already-paid-for channel** a
confirmed theme's resident fact rides on — no new injection point needed,
only a new Tier C write.

trusty-memory has **no `SubagentStop` hook today** — its hooks are
`UserPromptSubmit` and `SessionStart` only
(`crates/trusty-memory/src/events.rs:30-35`, `HookType`). The capture point
for a subagent's closing block is `trusty-mpm`'s own `SubagentStop` hook
(already registered for #7702, §1.2), not a trusty-memory-side hook.

### 1.6 trusty-code: shares the asset pipeline, has no hook layer at all

`trusty-code` composes and deploys agents through the **same**
`trusty-agents-common` pipeline `trusty-mpm` uses —
`trusty_agents_common::agents::builder::compose_agent`
(`crates/trusty-code/src/agents/md_loader.rs:35`),
`deployer::deploy_agents_filtered`
(`crates/trusty-code/src/agents/deploy.rs:51`), and the same `AgentManifest`
(`crates/trusty-code/src/agents/describe.rs:36`). BASE-AGENT's two closing
blocks (§1.1) and the `self-improvement-loop` skill therefore already reach
every `tcode`-composed agent with zero extra work, exactly as they reach
`tm`'s — this is the one convention `tcode` has parity on for free today.

**What it does not have: any hook at all.**
`crates/trusty-code/src/lib.rs:20-21` states the constraint directly:

> "API / CLI / TUI driven — no hooks support (hooks are a Claude Code
> shell-level feature; `tcode` operates above that layer via its event bus)."

So the #7702 capture mechanism — a `Stop`/`SubagentStop` hook spawning `tm
hook` — has **no `tcode` equivalent to register**. `tcode`'s analog capture
point is its own in-process turn boundary (`crates/trusty-code/src/agent_loop/`,
`crates/trusty-code/src/runner/`) publishing onto its
`events` broadcast bus (`crates/trusty-code/src/events.rs`) — `tcode` can call
a shared extraction function directly, in-process, at the point a PM or
subagent turn ends, with no subprocess spawn and no transcript re-parse at
all (it already holds the final message in memory). This is a **cheaper**
integration than `tm`'s hook-and-reparse, not a missing one, once the
extraction logic sits in a crate both harnesses depend on (§2.7).

### 1.7 Token floor measurements that bound every design choice here

From `docs/research/token-floor-measurement-2026-09-13.md` (read on this
branch) and this session's own harness-guard observations:

- A composed engineer subagent's per-turn floor with the #7683 `tools:`
  allowlist is **63,007 tokens** (arm C, two runs; round 1 table).
- Removing the project `CLAUDE.md` (24,966 bytes) from that floor drops it by
  **9,997 tokens** (round 2, D1) — the task brief's "~10K, now ~3.9K" reflects
  a since-landed trim of that file; either figure is "a five-digit,
  non-trivial fixed cost," which is why §2.4 below puts a hard byte cap on
  what a confirmed theme is allowed to add to that same injected block rather
  than trusting "it's just one more fact."
- The composed agent's own prose (BASE-AGENT + BASE-ENGINEER + the leaf
  agent) is **19,981 tokens**, 32% of the floor (round 2, D3) — the reason
  §2.1 rejects "ask every agent to call an extra MCP tool every run" as a
  prompt-text cost, in favor of a mechanism with **zero added instruction
  text** (the two closing blocks already exist; nothing new is asked of the
  agent in the common case).
- A bundled skill's listing entry costs on the order of 100-200 tokens per
  turn merely by being enumerated (order-of-magnitude, from the same
  decomposition); this design adds **no new skill**, only extends the
  `self-improvement-loop` skill #7723 is already relocating BASE-AGENT's
  content into.

---

## 2. Decisions

### 2.1 Capture — who writes the record, and when

| | Harness hook (extend #7702's pattern) | Agent self-report (`memory_remember`, #6937's pattern) |
|---|---|---|
| Token cost per agent run | **Zero added instruction text.** The block is already emitted; the hook parses the transcript the harness already wrote. | One extra tool-call round-trip per finding — an MCP call the agent must remember to make, plus the result tokens. |
| Reliability | Deterministic: fires on `Stop`/`SubagentStop` every time, independent of agent compliance. | Depends on the agent actually calling the tool — the exact failure BASE-AGENT's "Routing depends on what you are" section exists to manage, and the exact gap `#6937`'s own tag convention cannot close. |
| Works with no memory MCP configured | Yes — the ledger write (or, for this feature, the tagged-room write, §2.2) is the ONLY place the memory MCP is touched, and it fails open exactly as #7702 does. | No — every agent run in every project needs a live memory MCP connection merely to report nothing happened. |
| Works in `tcode` | Needs a `tcode`-side capture point (§1.6) — an in-process call at the turn boundary, not a hook. | Works identically in both harnesses today, since it is the agent's own tool call — but inherits the reliability column's gap in both. |
| Malformed block | Harness extractor returns `None`; nothing is written, no error surfaces. Exactly #7702's `a_message_without_the_section_writes_nothing` case. | A malformed `memory_remember` call either succeeds with garbage content or the agent's own retry burns more tokens. |

**Decision: harness-side capture, extending #7702's extractor rather than
asking every agent to call `memory_remember` for the Improvement
recommendations block too.** The two closing blocks are free-form, already
written, and already well-specified (Symptom/Cause/Change/Evidence); parsing
them at `Stop`/`SubagentStop` costs nothing new in the prompt and inherits
#7702's proven fail-open contract. `#6937`'s fast-loop hypothesis record
**stays agent-written** — unlike a closing-report block, it is explicitly a
record the agent consults and re-measures against across its OWN future
runs (BASE-AGENT "Consult before you start; measure after you finish"), which
is a read/write loop the agent must drive, not a one-shot extraction.

### 2.2 Storage — room, wing, tags, and how the two sources share space

| Field | Value | Why |
|---|---|---|
| Room | `self-improvement` (custom, `RoomType::Custom`) | Scopes `dream_consolidate_room`'s room filter to exactly this content, so consolidating it never drags in unrelated Planning/Backend drawers, and vice versa (§1.4). |
| Wing | Default | No per-owner split is needed; every agent type and the PM write into the same room, distinguished by tag, not by wing. |
| Tag (contract) | `self-improvement-hypothesis` — kept, unchanged | `#6937`'s existing tag stays the query key the scheduled post-mortem already uses (`memory_list(tag: "self-improvement-hypothesis")`); this design does not fork it. |
| Tag (added) | `agent:<type>` (e.g. `agent:rust-engineer`, `agent:pm`) | Lets a post-mortem or the consolidator filter by source without parsing content. |
| Tag (added) | `theme:<slug>` | The grouping key §2.3's consolidation pass clusters on — assigned at capture time by a cheap heuristic (cause-text fingerprint), corrected by the LLM merge during dreaming. |
| Tag (added) | `status:hypothesis` \| `status:confirmed` \| `status:shipped` | The lifecycle state (§2.3, §2.5); one tag at a time, the #6937 `open`/`improved`/`regressed`/`inconclusive` judgement-rule status stays a FIELD inside the record, not a tag — the two are different axes (per-hypothesis statistical judgement vs. cross-session theme lifecycle). |
| Tag (added) | `session:<id>` | Provenance — which session produced this row, for the same reason #7702 records `session_id` on its ledger rows. |
| Tag (added) | `issue:<N>` (once filed) | Links the memory-side record to the GitHub-side fix (§2.4), so a later dream pass can check the issue's state without an extra out-of-band index. |

**#6937's hypotheses and an agent's closing-block findings share this one
room.** They differ in who writes them (§2.1) and in the `theme:` tag's
granularity (a hypothesis already names its own metric; a closing-block
finding is clustered into a theme by the consolidator) — not in where they
live. One room, one consolidation pass, one promotion path, rather than two
parallel lifecycles the PM has to reconcile by hand.

### 2.3 Consolidation — what `palace_dream` needs, and when it runs

Today's `consolidate_scoped` (§1.4) already does steps 2-4 of what a themed
pass needs — age-windowed snapshot, LLM-proposed merges, eviction of
superseded originals. Two gaps, both additive to `semantic_consolidation`
rather than a new pipeline:

1. **Pre-group by `theme:` tag before the LLM merge call**, so the model
   clusters within a tag rather than across the whole room — the owner's
   "merge near-duplicates" requirement from a roomful of unrelated findings is
   a tag filter away, not a new consolidator.
2. **Per-theme occurrence counting**, carried through
   `RoomConsolidationStats` as a new field (`theme_counts: Vec<(String,
   usize)>`) so a scheduled check (not necessarily every dream cycle) can ask
   "did any theme cross N occurrences across M distinct `session:` tags in
   the last 30 days" without re-scanning the room by hand.

**Cadence:** the existing idle dreamer already runs this room on its normal
~300 s-idle / palace-wide schedule (`crates/trusty-common/src/memory_core/dream/dreamer.rs:204`,
`dream_cycle`) with no code change — a themed room is just another room it
touches. The **promotion check** (step 2 above) does not need to run every
cycle; `tm session stop` is the natural synchronous point to run
`dream_consolidate_room(room="self-improvement")` explicitly (the same
on-demand call #1721 built `palace_dream` for), because that is when a
session's outstanding findings are known to be final.

**Promotion threshold:** a theme moves `status:hypothesis` → `status:confirmed`
when it has **3 or more occurrences across 2 or more distinct sessions**
within the last 30 days — low enough to catch a recurring structural problem
within a week of agent activity on this repository's volume, high enough that
one agent's one bad run never reaches the PM's resident facts. This is a
judgement call for the first implementation slice (§3) to revisit against
real data, not a number load-bearing enough to block the design.

### 2.4 Two outputs per confirmed theme

**(a) A resident fact for the PM, via `get_prompt_context` / Tier C.** A
confirmed theme writes (or re-writes) one Tier C fact:
`kg_assert(subject="self-improve:<theme-slug>", predicate="is_fact",
object="<≤80-char summary>")` with `fact_key="self-improve:<theme-
slug>/status"` and an explicit `expires_at` set to the next scheduled
consolidation pass plus a grace window (e.g. 14 days) — re-affirmed (not
re-created) at each dream cycle that still finds the theme active, exactly
the #4818/#4820 "stale-fact-retires-itself" walkthrough ADR-0028 §"Consequences"
already specifies for a Tier C write. This rides the **existing** injection
(§1.5) — no new hook, no new section in the composed block.

**Budget: reserve a fixed share of Tier C's 800 B, not all of it.** Tier C is
a shared 800-byte budget across every current fact any tool writes (ADR-0028
D7); this design does not get to claim it all. Cap self-improvement's share
at **3 resident themes (≈240 B at the Tier-S-style 80-char line limit)** —
enough to surface "this keeps happening" without crowding out a `pr:*/state`
fact or another tool's Tier C write. Oldest-by-`fact_key` eviction (ADR-0028
D7, "newest-first... oldest slots dropped") already handles overflow; nothing
new to build there.

**(b) A durable fix, routed to `ticketing`.** A theme's resident fact changes
what the PM is TOLD; it does not change what a dispatched subagent DOES,
because the subagent's own context is composed fresh from the deployed
assets and never reads the PM's Tier S/C facts. A theme that needs the
subagent to behave differently must ship as an edit filed through
`ticketing` — the existing routing §1.1 already defines — not as a memory
write. The kind of theme maps to the kind of fix as follows (this table
names the mapping, not the wording — no BASE-AGENT or skill prose is
authored by this design):

| Theme pattern | Fix shape | Example (illustrative, not a real finding) |
|---|---|---|
| Same tool misused by one agent type repeatedly | A `tools:` allowlist edit for that agent (#7683's mechanism) | "rust-engineer" keeps calling a Bash reader the allowlist could remove |
| Same step skipped by every agent type | A BASE-AGENT (or `self-improvement-loop` skill) line | A verification step agents keep forgetting regardless of type |
| A destructive or unsafe call pattern | A `PreToolUse`/`PreBash` hook deny rule | A command shape that should be blocked, not merely discouraged |
| A project-specific gotcha (wrong command, wrong path) | A project `CLAUDE.md` line | A convention specific to this repository, not the framework |
| A prompt-composition defect (unclear, redundant instruction) | Routed through #7702's existing `tm prompt-feedback --summary`, not this pipeline | Out of scope here — #7688 already owns it |

### 2.5 Retirement and pruning

- **Shipped themes** (`status:shipped`, set when the linked `issue:<N>` closes
  `status:tested` per this repo's issue lifecycle) have their Tier C fact's
  `expires_at` NOT re-affirmed at the next dream cycle — it ages out on its
  own existing schedule rather than being force-retracted, so a fix that
  regresses still has its prior fact briefly visible rather than silently gone.
- **Stale hypotheses** (never reached `status:confirmed`, no new occurrence
  in 30 days) are tagged `status:stale` by the same scheduled
  `dream_consolidate_room` call that does promotion, and become eligible for
  the dreamer's existing prune pass (`prune_pass`,
  `crates/trusty-common/src/memory_core/dream/dreamer.rs:242`) on its normal
  importance/age rule — no new prune mechanism, just a tag that routes into
  the one that exists.
- **Confirmed-but-not-yet-fixed** themes keep their Tier C fact re-affirmed
  every cycle until `status:shipped` — this is the intended "still nagging
  the PM" behavior, bounded by the 3-theme resident cap in §2.4.

### 2.6 The `MEMORY.md` alternative

**Cost today, on this host:** no `MEMORY.md` exists in this checkout or the
home directory at time of writing (`ls ~/.claude/MEMORY.md` → not found). The
44.7K-token zero-tool-call baseline that includes it
(`docs/research/input-token-optimization-spike-2026-09-12.md:236`) did not
isolate its share from the project `CLAUDE.md`'s or the skills listing's, so
there is no clean "MEMORY.md alone costs X" figure to quote — what is known
is that it is loaded **in full, unranked, every turn**, with no room/tag
filter and no tier cap, which is structurally worse than either Tier S's
hard 20-fact/1,600-byte cap or Tier C's 800-byte newest-first window (§1.4).

**Pruning it would require building, from nothing, exactly the machinery the
tagged room already has:** an age/occurrence threshold, a promotion rule, and
a retirement sweep — except hand-rolled against a flat Markdown file instead
of reusing `dream_consolidate_room`, `expires_at`, and the existing prune
pass. There is no part of this design that gets cheaper by targeting
`MEMORY.md` instead of the tagged room.

**The binding constraint: [#7685](https://github.com/bobmatnyc/trusty-tools/issues/7685)**
rules that Claude Code's auto-memory is disabled outright whenever the
trusty-memory daemon is reachable, and is a fallback only when it is not —
"zero `MEMORY.md`, store it in trusty-memory, disable when the daemon is
reachable." That ruling, not a preference on this document's part, is what
makes the tagged room the primary path: `MEMORY.md` is structurally
unavailable as a PRIMARY mechanism under #7685 in any project where the
feature could normally run. **Recommendation: the tagged room, full stop, for
every project where the trusty-memory daemon is reachable — which is every
project this feature is standard in.** `MEMORY.md` is not a fallback this
design needs to build, because #7685 already scopes it to "daemon
unreachable," a state in which this feature (which needs `palace_dream`,
`get_prompt_context`, and `kg_assert`) cannot run at all. There is nothing to
prune because there is nothing this feature would ever write there.

### 2.7 Shared placement, and `trusty-code` parity

**Candidate owner: `trusty-agents-common`**, for the pure-text half only —
extraction of the `## Improvement recommendations` section from a final
assistant message (the same shape of function #7702 already wrote once, in
`trusty-mpm`, for `## Prompt feedback`) has zero dependency on Claude Code's
hook transport or on `tm`'s CLI. It takes a `&str`, returns an `Option<Vec<
Finding>>`, and both harnesses can call it. Today `trusty-agents-common`
composes and deploys agent assets but is not a trusty-memory client
(`crates/trusty-agents-common/Cargo.toml` carries no `trusty-memory`
dependency) — this design does not change that. The MCP write
(`kg_assert`/`memory_remember` into the `self-improvement` room) stays where
every other memory write already happens: made by whichever process already
holds an MCP connection to trusty-memory for that session (the PM, via its
own tool access), never by a new daemon-to-daemon client.

**Split by layer:**

| Layer | Owner | Why |
|---|---|---|
| Block extraction (pure text → structured findings) | `trusty-agents-common` | Harness-agnostic, no new dependency, reused by both `tm hook --prompt-feedback`'s sibling and `tcode`'s in-process call. |
| Transcript read + hook registration | `trusty-mpm` (`session_launch/`, `bin/tm/commands/`) | Claude-Code-specific; extends the #7702 module pattern directly. |
| Turn-boundary call, no transcript | `trusty-code` (`agent_loop/`, `events.rs`) | No hook layer exists (§1.6); `tcode` already holds the final message in memory at the point a turn ends, so it calls the shared extractor directly — cheaper than `tm`'s reparse, not a missing capability. |
| Room/tag write, consolidation, resident-fact promotion | `trusty-memory` | Already owns rooms, wings, dreaming, and Tier C (§1.4); no new service. |

**`trusty-code` harness-convention parity checklist** (the owner directive's
"code... should pick up all harness conventions," scoped to what this
feature touches):

- [ ] Composes `BASE-AGENT.md` / `self-improvement-loop` skill content for
      every agent type — **already true** (§1.6), no work needed.
- [ ] Captures the closing "Improvement recommendations" block at its own
      turn boundary and writes it through the shared extractor (§2.7 table,
      row 3) — new work, this design's §3 slice 4.
- [ ] Resolves and honors the same `self-improvement` room/tag/Tier-C contract
      as `tm` when it has its own MCP access to trusty-memory — new work.
- [ ] Surfaces the same resident-fact cap behavior to its own PM-equivalent
      loop (`agent_loop/`) — new work, contingent on `tcode` having a
      `get_prompt_context`-equivalent injection point; out of scope to invent
      here if none exists yet (a gap to confirm in slice 4, not assumed).

---

## 3. Implementation slices

Each slice is one PR, ships its own changelog fragment, and states the
measurement that proves it, not just that it compiles.

1. **Extract the shared block parser into `trusty-agents-common`.** Move the
   text-extraction shape of `crates/trusty-mpm/src/core/prompt_feedback.rs`'s
   `extract_feedback` (heading-match, truncate-on-char-boundary, last-
   occurrence-wins) into a new, harness-agnostic function parsing the
   `## Improvement recommendations` Symptom/Cause/Change/Evidence shape.
   *Acceptance:* unit tests mirroring #7702's (`absent_heading_extracts_
   nothing`, `stops_at_the_next_heading`, multi-entry parsing); `trusty-mpm`
   depends on it with no behavior change to #7702's existing feature (prove
   with its own existing test suite, unchanged, green).
2. **`tm hook --improvement-recommendations` on `Stop`/`SubagentStop`,
   writing into the `self-improvement` room.** Mirrors
   `prompt_feedback_hook.rs`'s transcript-tail read and fail-open contract;
   writes via a trusty-memory MCP call made from the PM's own session context
   at hook time is not available to a background hook process — so this
   slice's actual write target is a **local ledger**, structurally identical
   to `prompt-feedback.jsonl`, and slice 3 is what promotes it into the
   knowledge graph. *Acceptance:* the #7702 test family ported 1:1
   (unreadable transcript writes nothing, malformed block writes nothing,
   `SubagentStop` without `agent_type` uses the same `unknown-subagent`
   sentinel); `cargo test -p trusty-mpm --no-fail-fast` green.
3. **`tm session stop` flushes the ledger into the `self-improvement` room via
   the PM's MCP connection, tagged per §2.2.** This is the seam where a
   process that already holds live MCP access to trusty-memory performs the
   write the background hook could not. *Acceptance:* an integration test
   with a fixture ledger of 3 entries sharing a cause-text fingerprint proves
   one `theme:` tag assigned to all three and a `kg_assert`/`memory_remember`
   call per entry, observed against a test palace.
4. **`dream_consolidate_room` theme pre-grouping and `theme_counts`.** Extend
   `consolidate_scoped_within` to accept an optional pre-filter by tag before
   the LLM merge call, and `RoomConsolidationStats` with the new field.
   *Acceptance:* a new test alongside `consolidate_scoped_filters_by_room`
   (`dream/tests.rs`) proving two tags' drawers are never merged across the
   tag boundary, and `theme_counts` reports the right per-tag count.
5. **Promotion check + Tier C write.** A `tm` command (or a step inside slice
   3's `session stop` flush) that reads `theme_counts`, applies the §2.3
   threshold, and writes/re-affirms the capped Tier C facts (§2.4a).
   *Acceptance:* a theme crossing the threshold produces exactly one Tier C
   fact visible in the next `get_prompt_context` call in a test palace; a
   4th theme crossing the threshold when 3 are already resident evicts the
   oldest by `fact_key`, not the newest.
6. **Ticketing route for `status:confirmed` themes lacking an `issue:` tag.**
   Wires into the existing `ticketing` search-before-file path (§1.1), never
   a new filer. *Acceptance:* a confirmed theme with no `issue:` tag produces
   exactly one new or commented-on issue (dedup against the `self-improvement`
   label, per existing convention), and the theme's room-side record gains
   the `issue:<N>` tag.
7. **`trusty-code` turn-boundary capture.** Calls slice 1's shared extractor
   directly at `agent_loop`'s turn-end point, no subprocess, no transcript
   re-read (§1.6), writing to the same ledger-or-direct-write seam slice 2/3
   established, gated on `tcode` actually holding an MCP connection to
   trusty-memory (confirm this exists before building; if it does not, that
   gap is its own prerequisite slice, not assumed here).
   *Acceptance:* a `tcode`-driven subagent run with a deliberately malformed
   block produces no write (parity with slice 2's fail-open test), and a
   well-formed block produces the same room/tag shape slice 3 produces for
   `tm`.
8. **End-to-end loop-closed proof.** One measurement run, not a unit test:
   seed 3 sessions' worth of the same synthetic finding through slice 2/3,
   confirm promotion (slice 5) surfaces the resident fact in a real
   `get_prompt_context` read, confirm `ticketing` opens the issue (slice 6),
   merge a fix, and confirm the theme's status moves to `shipped` (§2.5) on
   the next dream cycle. This is the proof the loop closed at least once,
   named explicitly because every slice above can pass its own unit tests
   while the seam between slices silently does not connect.

---

## 4. Open questions for the owner

1. The 3-occurrences/2-sessions/30-day promotion threshold (§2.3) and the
   3-resident-theme Tier C cap (§2.4a) are both first-cut numbers with no
   production data behind them yet — confirm, or name different ones, before
   slice 5 ships.
2. Whether `tcode` currently has ANY MCP connection to trusty-memory from
   inside `agent_loop`/its PM-equivalent, which slice 7 depends on and this
   document did not find evidence of either way within its reading scope.
