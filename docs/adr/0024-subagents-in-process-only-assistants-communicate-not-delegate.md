# 0024. Assistants Are Level-0 Delegators; Sub-Agents Are In-Process, Single-Edge Leaves That Never Delegate

- **Status:** Amended by [0058](0058-trusty-code-is-an-independent-product-owned-harness.md)
- **Acceptance:** All six Decision clauses were ratified by the owner on
  2026-07-28 or 2026-07-29. Implementation remains incomplete for the
  three-tool-call router and assistant-to-assistant communication primitive;
  acceptance records the architectural choice, not implementation completion.
  See "Implementation status" for the clause-by-clause state.
- **Current amendment:** ADR-0058 preserves this model for Trusty Agents L1
  leaves and distinguishes a cross-product, daemon-owned Trusty Code coding task
  from an in-process sub-agent.
- **Date:** 2026-07-28
- **Ratification:** L0-assistant clause recorded 2026-07-28;
  editable-whitelist clause recorded 2026-07-29
- **Scope:** crate `trusty-agents` (the `delegate_to_agent` / `dispatch_task` boundary; the L0/L1 tier model, #4167/#4200; touches the `trusty-code` cross-product bridge target and the Sub-agents API/pane, `#4029`/`#4211`)
- **Reversibility Cost:** High — reverses/re-scopes shipped, tested, owner-directed machinery from epic #4021 (#4026/#4027/#4028/#4211, merged within 48 hours of this ADR), INVERTS the population assignment of the L0/L1 tier model merged the SAME DAY as this decision (PR #4200, squash `ada4d351`), and requires new, currently nonexistent machinery (an editable sub-agent whitelist, an assistant-to-assistant messaging primitive, and a tool-call-counted skill/delegate router)
- **Decision Drivers:** Product framing clarity (the Sub-agents pane could not honestly present a name that means two different things), the owner's rejection of a UI-only fix, the owner's explicit generalization of the YOLO risk posture to every assistant, a documentation gap (DOC-57 does not yet cover Sub-agents at all, #4182), the owner's own PM/trusty-mpm prior art as an explicit analogy with an owner-named limit, and — underlying all of the above — the owner's virtual-twin authority principle (see "Rationale" below): each assistant must take authority over its own actions, and that authority is not transferable between assistants

- **Supersedes / Superseded by:** — this ADR's own prior revision recommended
  a hand-authored constant (`ASSISTANT_ALLOWED_DELEGATE_ROLES`-style) as the
  reachable-target model; the accepted editable-config whitelist replaces that
  recommendation. The later 2026-07-28/29 ratification resolves the tension
  with the owner's informal 2026-07-26 OQ-2 ruling in favor of this ADR.

## Context

### 1. Five decisions, verbatim, in the order they were given

This ADR was originally drafted against a single decision (item 1 below). A
routing error meant four more, given in the same review, did not reach the
first draft. All five are recorded here as one coherent whole; the first
draft's Context/Consequences on item 1 are retained and NOT softened, per
explicit instruction — they are extended, not replaced, by what follows.

1. *"This isn't a UI thing. Let's make an ADR. Sub-Agents are always
   in-process, Assistants can communicate with each other, but never
   delegate."*
2. *"Let's make agents a configuration with a whitelist (editable), and
   re-use existing agents."*
3. *"So let's also firm up the distinction. Assistants are a level 0 agent,
   like a PM, that can delegate to sub-agents and communicate with each
   other."*
4. *"Sub-agents can never delegate and talk out of processes. The only
   respond to an assistant."* [sic; read as "They only respond to an
   assistant."]
5. *"Yes on the YOLO."* — ratifying, on direct question, that the YOLO risk
   posture (previously scoped to a single, rare, not-yet-instantiated L0
   orchestration persona, #4167) generalizes to every assistant now that
   every assistant is L0 under decision 3.

Plus, on the skill-vs-delegate routing question: *"The answer is that it
depends on the complexity of the task,"* then *"Let's use our 3 tool call
threshold."*

**The complete ratified model**, stated as one whole:

- **ASSISTANT = tier L0, PM-like.** Delegates DOWN to sub-agents.
  Communicates LATERALLY with other assistants, never delegates to them.
  Holds YOLO/owner-accepted responsibility. Calls skills DIRECTLY as well
  as delegating.
- **SUB-AGENT = tier L1.** Always in-process. LEAF NODE: never delegates
  (not up, not lateral, not at all); never talks out-of-process; responds
  only to its invoking assistant. Exactly one edge.
- **REACHABLE SUB-AGENT SET = an editable config whitelist over EXISTING
  host agents** — not a hand-authored Rust constant. This supersedes this
  same document's earlier recommendation (see the header's "Supersedes"
  note and "Conflicts and open questions").
- **ROUTING** between an assistant's own skill and its sub-agent for the
  same domain = task complexity, measured by a 3-tool-call threshold: ≤3
  expected tool calls, call the skill directly; more, delegate to the
  sub-agent that owns that domain.

### 2. Current state: two delegation mechanisms exist today (retained from the first draft)

**`delegate_to_agent` — in-process, lane 2.**
`crates/trusty-agents/src/tools/delegate.rs` implements `DelegateToAgentTool`,
gated by two independent checks at one choke point (`delegate.rs:346-448`):
a role allowlist, `runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES`
(`tool_registry.rs:137-145`: `engineer`, `qa`, `researcher`, `documentation`,
`ops`, `planner`, and `ASSISTANT_TIER_ROLE` — "assistant" — itself, the
constant this decision's clause about lateral communication directly
contradicts), and the L0/L1 tier gate (#4169/#4200, detailed in §3 below).
The role allowlist is applied at only two of the three sites that construct
a `DelegateToAgentTool` for an assistant-tier caller —
`runtime::tool_registry::build_assistant_tier_registry`
(`tool_registry.rs:556-566`) and
`ctrl::pm_task::dispatch::history::ctrl_delegate_posture`
(`history.rs:412-417`) — while the third,
`ctrl::pm_task::dispatch::persona::run_pm_task_with_persona`
(`persona.rs:307-310`, backing the REPL `/agent` command), never calls
`.with_allowed_target_roles(...)`. **This gap is now independently confirmed
in issue #4201's second comment (2026-07-28), filed during this ADR's
research:** neither the role allowlist NOR the tier gate is effective on
that path — the tier gate is "structurally in place" but inert, because no
bundled agent declares `tier = "l0"`, so the target-tier check
(`target_tier == L0Orchestration`) never fires. Concretely, today, the REPL
`/agent` path can delegate an assistant persona into `pm` (role
`orchestrator`), gated by nothing.

**`dispatch_task` — out-of-process, lane 3, the "cross-product bridge."**
`crates/trusty-agents/src/tools/pm_bridge.rs` / `cross_product.rs`. A
bridge-owned floor, `NON_CODING_TARGETS = ["research", "ticketing"]`
(`cross_product.rs:63`), intersected with a per-agent `[subagents].allowed`
TOML list (`SubagentAllowSet`, `agents::config::SubagentsConfig`,
`config.rs:239-243`) — absent config denies everything, deny-by-default.
Results are wrapped in a structurally-enforced `ProposalEnvelope` that can
never carry `Disposition::Action` (`cross_product.rs:375-399`), per DOC-41
§5.5's absolute propose-not-authorize rule. `ticketing-agent` declares
`role = "ticketing"` (`.trusty-agents/agents/ticketing-agent.toml:3`), not a
member of `ASSISTANT_ALLOWED_DELEGATE_ROLES` — unreachable in-process today,
reachable only via this cross-product leg (ported into `trusty-code`'s own
roster by #4027, closed/merged: `crates/trusty-code/src/assets/agents/
ticketing.md`). `GET /api/agents/:name/subagents` (`agent_subagents.rs`,
#4029/#4211) is the pane that surfaced this asymmetry and, in its own module
doc, calls both mechanisms "the two delegation mechanisms an agent may
reach" — the terminology decision 1 overturns.

### 3. The L0/L1 tier model as SHIPPED (PR #4200) — and how decision 3 inverts its population

Epic #4167's owner decision (2026-07-27, quoted in the issue): *"Let's
create a level 0 - orchestration assistant for this purpose, with a YOLO
responsibility on the owner. Standard assistants are level 1, tied to
option 2."* The shipped model (`config.rs:680-713`, PR #4200, squash
`ada4d351` — the tip commit on this branch's history at the time of this
ADR):

- `AgentTier::L1Standard` is `#[default]` — "every accessor that falls back
  to `Default::default()`... lands on the restricted tier."
- `AgentTier::L0Orchestration` — "PM-tier grants; YOLO risk posture...
  Only reachable by an EXPLICIT `tier = "l0"`... declaration — never a
  default, never inferred from `role`."
- Crucially, the doc comment states plainly: *"L1 ('standard assistant') is
  TODAY'S `assistant`/`cto-assistant`/`izzie`"* (`config.rs:687-688`,
  emphasis added). **Under the model as merged hours before this decision,
  the entire existing assistant-persona population is L1 — the RESTRICTED
  tier — and L0 was reserved for a new, not-yet-authored, rare orchestration
  persona.** No bundled agent declares `tier = "l0"` anywhere in
  `.trusty-agents/agents/*.toml`; cross-checked directly against the live
  Sub-agents pane, which — per the routing coordinator — "renders tier l1"
  for every agent, `cto-assistant` included.
  **NOW SUPERSEDED IN CODE** (see "Implementation status"): no bundled agent
  declares `tier = "l0"` STILL, and none needs to — `AgentInfo::tier()` derives
  the tier from KIND, so the assistant population resolves L0 with the on-disk
  files unchanged. Read this bullet as the record of what #4200 shipped, not as
  current behavior; the pane now renders `tier l0` for `cto-assistant`.
- The one-directional gate (`delegate.rs:361-438`) blocks exactly one
  direction: `target_tier == L0Orchestration && delegator_tier !=
  L0Orchestration`. Its purpose, as shipped, was to stop an untrusted,
  content-ingesting L1 STANDARD ASSISTANT from delegating into a trusted,
  YOLO-authorized L0 orchestration persona.
- Capability grants were explicitly deferred: *"This PR defines the tier and
  its one-directional delegation boundary ONLY; it grants L0 no actual
  capabilities (those are #4170-#4173) and creates no L0 persona
  instance"* (`config.rs:691-693`). Checking those four follow-ups: **only
  #4171 (session-state read access) is CLOSED/shipped** — the other three,
  #4170 (GitHub PR/CI tool surface), #4172 (cross-project store/git
  scoping), and #4173 (shell/build/test execution grant), are all still
  OPEN. Concretely, the only tier-conditioned capability delta that exists
  in code today is `crate::tools::session_state::session_state_tools`
  returning real tools for L0 and an empty vector for L1
  (`tool_registry.rs:597-611`, #4171). The 12-tool git surface
  (`tools::git_tools::git_tools`, including `commit`/`push`/`pull`/
  `checkout`/`create_branch`) is registered UNCONDITIONALLY inside
  `build_assistant_tier_registry` for every assistant-tier persona regardless
  of tier (`tool_registry.rs:514-518`) — broader than the module doc's own
  gloss, which undersells it as "`git_log`/`git_status`" only
  (`tool_registry.rs:450`) — and no shell/bash tool is registered for the
  assistant tier at all. **Decision 3 inverts this model's population
  assignment: it does not create a new, rare L0 persona above the existing
  assistants — it declares the EXISTING assistant population (izzie,
  cto-assistant, personal-assistant, ctrl) to BE L0.** See "Consequences"
  for whether the one-directional gate, unchanged, still expresses a
  coherent rule once the population it protects is inverted.

### 4. The two-days-prior owner ruling this decision revises (retained)

Epic #4021's OQ-2 asked whether cross-product delegation should return as a
legitimate runtime delegation primitive. On 2026-07-26 the owner ruled
(issue #4021 comment): **"OQ-2 this directive supersedes #3816's subagent
drop (external cross-product delegation returns; the drop applied to
internal machinery)."** Two days before decision 1, the owner explicitly
called cross-product delegation a legitimate sub-agent mechanism — the
opposite of "Sub-Agents are always in-process." The same 2026-07-28 review
thread that produced this ADR's decisions (issue #4021, second comment)
independently confirms, in the owner's own words, that `ticketing-agent`
"has `role = "ticketing"` which is NOT in `ASSISTANT_ALLOWED_DELEGATE_ROLES`
so it is unreachable in-product today," alongside a proposal to curate a
starting Sub-agents set including "Research Agent," "Ticketing Agent,"
"MacOS Manager," and "AWS Manager" — the latter two not existing in any
form yet, and decision 2's "re-use existing agents" clause directly bears
on that gap (see "Consequences").

### 5. "Communicate" — checked against the codebase, not invented (retained)

No agent-to-agent MESSAGING primitive (distinct from delegation) exists in
`trusty-agents` today. `TmSendMessageTool` (`tm_tools/control.rs:105`)
targets a **trusty-mpm** tmux session, not a trusty-agents persona.
**ADR-0019** (Accepted, 2026-07-21) designs a unified, role-addressed,
durable cross-agent IPC bus, but its own Consequences section states the
bus "remains unimplemented. No code has landed since [2026-07-18]"
(`0019:112`) and its role model is built on **ADR-0016**'s SINGLETON
"ASSISTANT" (the one holder of user authority, `0016:47-50`) — a different
sense of "assistant" than `trusty-agents`' plural `role = "assistant"`
persona population, the exact population decisions 3/4 now formally place
at tier L0. The product spec's decision log (2026-07-24,
`trusty-agents-product-spec.md:485`, Bob: *"we can allow agents to talk to
each other"*) is the only other prior ruling on record, and the SAME spec
lists the protocol as an open, undesigned question on the same date
(`:495`).

### 6. DOC-57 does not yet cover Sub-agents (retained)

`docs/specs/agent-config-five-sections.md` (DOC-57) never mentions a
Sub-agents section; issue #4182 is open and unaddressed. This ADR remains
the first normative text on the subject, now covering five ratified
decisions rather than one.

### 7. Prior art: the trusty-mpm PM, and the limit the owner himself named

The owner offered the trusty-mpm PM as precedent: *"[the model] is
logically consistent, and similar to what we've done with the PM in our
harnesses."* Checked directly against `crates/trusty-mpm/src/assets/
instructions/`:

- `BASE_PM.md:7`: *"PM agent in trusty-mpm. Role: orchestration +
  delegation, **never direct impl**."* (emphasis added).
- `PM_INSTRUCTIONS.md:8-9`: *"PM = orchestrator + QA coordinator. Delegates
  ALL work to specialist agents. DEFAULT: delegate. EXCEPTION: user says
  'you do it' / 'don't delegate'."*
- `PM_INSTRUCTIONS.md:134`: *"Each delegation reloads ~95K tokens of
  context. Fewer, larger delegations = cheaper, faster."* — the concrete
  cost that makes the PM's DEFAULT posture "delegate," full stop: every
  delegation is a fresh ~95K-token context reload, so the PM economizes by
  delegating in large, batched units and otherwise doing almost nothing
  itself (a narrow allowlist: git ops, reading ≤3 small config files, 3-5
  orientation greps — `PM_INSTRUCTIONS.md:37-46`).

  > **Citation note (2026-08-01).** `BASE_PM.md` and `PM_INSTRUCTIONS.md` as
  > standalone files were replaced by per-section files under
  > `assets/instructions/sections/` (#4183, landed 2026-07-28, after this
  > ADR). The quotes above are unchanged in substance; their current
  > locations are
  > [`sections/identity.md:7`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/sections/identity.md#L7)
  > ("Role: orchestration + delegation, never direct impl"),
  > [`sections/core.md:8-9`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/sections/core.md#L8-L9)
  > ("PM = orchestrator + QA coordinator..."), and
  > [`sections/core.md:118`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/sections/core.md#L118)
  > ("Each delegation reloads ~95K tokens..."). This is a citation-location
  > correction only; it does not touch this ADR's Decision.

**The owner named the limit of this analogy himself**, and it is real and
load-bearing: the PM is delegation-ONLY specifically *for context economy*
— it never acts directly on source code (P1 in its own prohibitions table,
`PM_INSTRUCTIONS.md:17`, now
[`sections/core.md:11`](https://github.com/bobmatnyc/trusty-tools/blob/8abf30962863e143ed405e8d6cabe33f6b0f0b6d/crates/trusty-mpm/src/assets/instructions/sections/core.md#L11))
because every direct action it might take is
better done by a specialist whose OWN context is scoped to the task, and
reloading that specialist repeatedly is the expensive part the PM's
posture is designed to minimize. **The trusty-agents assistant's context is
not reloaded per turn the way a fresh delegation is — it is a persistent,
effectively unbounded conversation** (the product spec's own "ONE
continuous conversation per agent" decision,
`trusty-agents-product-spec.md`, decision log, 2026-07-24). That is exactly
why decision 3 grants the assistant something the PM structurally cannot
have: the right to call skills DIRECTLY, not only delegate. The analogy
holds for the SHAPE of the hierarchy (a trusted top-tier orchestrator that
delegates downward to narrower-scoped workers) but does not hold, and was
never claimed by the owner to hold, for the ECONOMICS that make the PM
delegate ALWAYS rather than SOMETIMES. Any implementer reusing PM
instruction patterns (e.g. the circuit-breaker table, `AGENT_DELEGATION.md`)
as a template for assistant routing must not import the "always delegate"
default along with the hierarchy shape — decision 3+5's routing model is
deliberately a SOMETIMES-delegate design, and that departure is the whole
point of the analogy's stated limit.

### 8. The "3 tool call threshold" — prior art, not a literal reuse

Two existing "3"-shaped conventions were checked; neither is what decision 5
literally reuses, though both are the number's likely ancestry:

- `PM_INSTRUCTIONS.md:302`: *"\>2-3 bash commands for one task -> CB#1 or
  CB#7"* — a RETROSPECTIVE violation-detection heuristic in the PM's "Quick
  Violation Detection" table: it recognizes, after the fact, that the PM
  should have delegated instead of running that many bash commands itself.
  It is scoped to Bash commands specifically, not to "tool calls" in
  general, and it is not a prospective routing gate between two named
  alternatives.
- `PM_INSTRUCTIONS.md:29-35` (the `pm_guard` hook, issue #2918): a
  MECHANICALLY-ENFORCED per-turn budget of "up to 3 combined P1+P5 file
  changes" — the closer structural analog (a hard integer cap that decides
  whether the actor may act directly or must hand off), but scoped to
  SOURCE-FILE EDIT COUNT specifically, and existing for the opposite stated
  reason: to let a trivial one-line fix skip a delegation round-trip, not
  to gate a skill-vs-sub-agent choice by estimated tool-call count.

**Honest assessment: decision 5's "3 tool call threshold" borrows the
NUMBER and the general shape (a small-integer cap distinguishing "handle it
myself" from "hand it off") from these two conventions, but reuses neither
rule's code, scope, or trigger condition verbatim.** It is a new threshold,
inspired by, not identical to, prior art. See "Consequences" for the
harder problem this raises: unlike the PM's rules (which count something
already OBSERVED — bash commands run, files changed), the assistant's
threshold must be applied BEFORE the work happens, to route it in the first
place.

## Decision

We adopt the complete model stated in Context §1. Open implementation mechanics
remain follow-up work and do not change the accepted architectural boundary:

1. A sub-agent is always in-process (unchanged from the first draft).
2. **Assistant = tier L0.** Delegates down to sub-agents; communicates
   laterally with other assistants (never delegates to them); holds the
   YOLO/owner-accepted posture; may call skills directly as an alternative
   to delegating.
3. **Sub-agent = tier L1.** In-process only; a leaf node with exactly one
   edge (responds to its invoking assistant); never delegates in any
   direction; never reaches outside the process.
4. **The reachable sub-agent set is an editable configuration whitelist**
   over agents that already exist in the roster — not a hand-authored Rust
   constant, and not net-new agent authoring for the purpose of populating
   the whitelist.
5. **Routing between an assistant's own skill and delegating to the
   sub-agent that owns the same domain is decided by task complexity,
   measured against a 3-tool-call threshold**: ≤3 expected tool calls,
   call the skill directly; more, delegate.
6. **Delegation authority is governed by KIND (assistant vs. sub-agent),
   never by tier order.** Formalized as a normative rule, not merely an
   observed consequence, in the section immediately below — added by owner
   instruction after clauses 1-5 were ratified, in direct response to this
   ADR's own finding that the shipped tier gate (§3, PR #4200) cannot
   express the peer prohibition clauses 2-4 require.

This ADR does not yet decide, and defers to the owner (see "Consequences"
and "Conflicts and open questions"), the mechanics each clause raises but
does not itself answer — enumerated in the Consequences sections below,
each ending in an explicit recommendation marked owner-ratify.

## Implementation status

**Numbering warning, because this document has two.** Context §1 numbers the
owner's five verbatim quotes in the order given; the Decision section above
renumbers the model into six normative clauses. They do not line up: the
L0-assistant clause is Context §1 item **3** and Decision clause **2**; the
editable whitelist is Context §1 item **2** and Decision clause **4**. This
table is keyed by CONTENT so a reader cannot pick the wrong "decision 3".

| Clause (by content) | Ctx §1 | Decision | Ratified | Implemented |
|---|---|---|---|---|
| Sub-agents are always in-process | 1 | 1 | yes | yes (pre-existing — see "Is 'sub-agents never delegate' enforced") |
| **Every assistant is tier L0** | **3** | **2** | **yes, 2026-07-28** | **yes — this PR** |
| Sub-agent = tier L1, single-edge leaf | 4 | 3 | yes | yes (L1 is the derived default for every non-assistant role) |
| Reachable sub-agent set is an EDITABLE CONFIG WHITELIST | 2 | 4 | **yes, 2026-07-29** | **yes** — `[subagents].delegate_allowed` over the `ASSISTANT_REACHABLE_SUBAGENTS` floor; both owner-ratify sub-questions answered (fail-closed + seeded default; server-side floor on writes). See "What the editable-whitelist clause's implementation actually did" |
| 3-tool-call skill-vs-delegate routing | (5th quote) | 5 | yes | **NO** — the a-priori-vs-reactive question below is unanswered |
| Delegation authority is governed by KIND, not tier order | — | 6 | yes | yes (PR #4240 — `agents::delegation`, and the `execute()` choke point) |
| Assistant-to-assistant COMMUNICATION primitive | 1 | — | yes | **NO** — nothing implements it; see "'Communicate' has to be built, not renamed" |

### What the L0-assistant clause's implementation actually did

**Tier is now DERIVED FROM KIND, not declared per file.** `AgentInfo::tier()`
resolves an explicit, non-blank `[agent].tier` declaration first (unchanged,
still fail-closed: an unrecognized value narrows to `L1Standard` and can never
elevate), and otherwise derives from `role` via the new `AgentTier::for_kind` —
`L0Orchestration` for the assistant kind, `L1Standard` for everything else.
**Zero agent TOMLs were edited.** The alternative — a `tier = "l0"` literal in
each bundled assistant persona — was rejected because there is no single file
per persona to put it in: `izzie`, `cto-assistant` and `ctrl` each ship BOTH a
directory package and a flat `extends`-shadow-fallback TOML, so the literal
would have to be written six-plus times and stay in sync forever, and a future
assistant persona that omitted it would silently resolve L1 — reintroducing,
in the data, exactly the decorrelation the "Why this class of error recurs"
section above is written about.

This NARROWS #4168's "never inferred from `role`" rule rather than discarding
it. The property that rule protected is preserved: an operator adding a new
SPECIALIST role still lands on `L1Standard`, because the derivation recognizes
one value and nothing else. And `role == "assistant"` was already the most
privileged non-orchestrator role in the crate — it is the ONLY role
`build_registry_for_agent` routes into `build_assistant_tier_registry`, the one
branch that registers `delegate_to_agent` and the git tool surface at all — so
deriving L0 from it restates a trust decision the codebase already makes rather
than opening a new escalation path. An explicit declaration still wins in both
directions, so a genuinely-L0 non-assistant persona can be pinned, and a single
assistant can be deliberately narrowed back to L1; both are declared intent,
never an accident of omission.

**The assistant-kind population, verified against the roster rather than
guessed:** `assistant`, `cto-assistant`, `ctrl`, `izzie`, `personal-assistant`
— every agent whose `[agent].role` is `assistant`. Notably NOT `research-agent`
(it declares `role = "researcher"`, a sub-agent role) and there is no
`writing-assistant` in the roster at all; both appear on informal lists of
"the assistants" and both are wrong. `agents::tests::loading::
bundled_assistant_personas_resolve_l0_and_gain_nothing` walks the shipped
roster and pins this set, so a new assistant is a reviewed addition.

### The measured blast radius of the flip

Enumerated across every `AgentTier` / `tier()` / `tier_blocked` / `wire_label`
consumer in the workspace, and confirmed by the test suite:

- **Delegation enforcement (`tools::delegate::execute`): no new refusal on any
  shipped path.** The kind gate (Decision clause 6, PR #4240) already refused
  assistant→assistant before the flip and still does, and it is checked FIRST,
  so the peer message is unchanged. Assistant(L0)→sub-agent(L1) is permitted by
  both gates before and after. Every other delegator that reaches the gate is
  already handed `L0Orchestration` explicitly (`ctrl_delegate_posture` and
  `persona_gate`'s `role != "assistant"` branches), and `pm_mode` passes no
  `config_dirs` so it runs no gate at all. **Decision clause 6 was load-bearing
  here: had refusal still keyed on tier order, this flip would have silently
  opened the peer edge.** Verified, not assumed.
- **One enforcement-point behavior change, fail-CLOSED and unreachable from any
  shipped construction site:** a caller that opts into the role allowlist but
  declares NO delegator identity is treated as `L1Standard`
  (`delegator_tier.unwrap_or_default()`, #4169 constraint 1) and is now refused
  when the target is an assistant, because assistants are L0 targets. All three
  assistant-tier construction sites call `with_delegator`, so this shape exists
  only in tests — pinned by
  `delegate_without_declared_identity_is_tier_blocked_from_an_assistant`.
- **Capability grants: zero observable delta.** The only tier-conditioned
  capability in shipped code is #4171's read-only session-state surface
  (`session_state_list`/`_status`/`_snapshot`); #4170/#4172/#4173 are still
  open, so nothing else is tier-gated. The flip REGISTERS those three executors
  into each assistant's registry, but `retain_tier_permitted` is deny-only (it
  never adds a tool) and reachability still requires the persona's own
  `[tools].allow` to name the tool: no bundled assistant names any
  `L0_ONLY_SESSION_STATE_TOOLS` entry, none declares a `[skills]` section, and
  none declares a glob broad enough to match one (`granola_*` is the only glob
  in the entire assistant roster). `ctrl` declares no `[tools]` section at all,
  and both of its dispatch paths fail closed on that — `scope_assistant_allowed
  _tools` returns an empty allow-list, and the persona-chat path builds an empty
  registry — so it gains nothing either. Pinned mechanically by
  `bundled_assistant_personas_resolve_l0_and_gain_nothing`, which fails if a
  future persona edit ever makes the grant real.
- **The Sub-agents pane (`in_product_surface`) after the change:** an assistant
  viewing it now reads `delegator_tier: "l0"` (the label the owner reported as
  wrong), peer assistants render `tier l0` and stay `reachable: false` with the
  unchanged KIND reason, and sub-agents render `tier l1` and stay reachable.
  `tier_blocked` is no longer always false — but for an assistant viewer it is
  still never the reason anything is refused, exactly as Decision clause 6
  predicate 3 says (redundant-as-designed). It DOES become true in one case: a
  NON-assistant viewer (a worker role, or `pm`) is L1 and now sees assistant
  targets reported as tier-refused where they previously read as reachable.
  That is display-only and honest — the pane already stamps `tool_registered:
  false` for those viewers, since only `role == "assistant"` ever holds
  `delegate_to_agent` — but it is a real change to the payload and is pinned by
  `subagents_route_reports_tool_not_registered_for_a_worker_role`. One known
  imprecision it exposes, pre-existing and deliberately NOT fixed here: the
  pane reads `pm`'s declared tier (L1) while `ctrl_delegate_posture` hands
  `pm`-as-orchestrator `L0Orchestration` at dispatch, so the pane under-reports
  `pm`'s reach. `pm` sits outside this ADR's graph by predicate 1's scope
  caveat; folding it in is separate work.

### Explicitly NOT in this implementation

**Read this section as of the L0-assistant PR; the whitelist paragraph is
superseded — see the section after it.** The editable config whitelist
(Context §1 item 2 / Decision clause 4) is **not ratified and not built**.
Neither is the assistant-to-assistant communication primitive, nor the
3-tool-call routing rule. The `ASSISTANT_ALLOWED_DELEGATE_ROLES` constant still
contains `assistant` and is unchanged — the peer edge is refused by the kind
gate at `execute()`, not by removing that entry, which the
"`ASSISTANT_TIER_ROLE` in the allowlist" consequence below explains must not
happen until the lateral communication mechanism exists.

> **SUPERSEDED IN PART, 2026-07-29.** The editable config whitelist IS now
> ratified and built (see the next section). The rest of this paragraph still
> holds: the communication primitive and the 3-tool-call rule remain unbuilt,
> and `ASSISTANT_ALLOWED_DELEGATE_ROLES` still contains `assistant` for exactly
> the stated reason. That constant DID gain one entry in the whitelist change —
> `ticketing`, so `ticketing-agent` is role-eligible at all — which is a
> widening of the COARSE pre-filter, not of the reachable set; the whitelist is
> now the binding gate. Left unedited above per this document's convention of
> annotating the historical record rather than overwriting it.

### What the editable-whitelist clause's implementation actually did

**The reachable set is a per-agent config list bounded by a server-owned
floor — the same two-layer shape the cross-product bridge already used, with
the machinery SHARED rather than copied.** `SubagentAllowSet` (written for
#4026's `dispatch_task` floor) moved from `tools::cross_product` to
`tools::subagent_allow` and became floor-parameterized; both mechanisms now
call the same `resolve()`. The alternative — a second, parallel implementation
for the in-process path — was rejected under the crate's "no second copy of any
gate" principle (`api::server::agent_subagents`'s module doc): a second copy is
how the reporting surface and the enforcement point drift apart, which is the
class of defect #4201 and #4235 both were.

**The floor is NAMES, not roles, and it is not the whitelist.** The new
constant is `agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS =
["research-agent", "ticketing-agent"]`. Decision 4 says the reachable set must
not be a hand-authored Rust constant, and this is not it: it is the CEILING the
editable list is bounded by, the exact counterpart of `NON_CODING_TARGETS` for
the other mechanism. The editable list is `[subagents].delegate_allowed` in
each agent's TOML. Names rather than roles because that is the vocabulary
`delegate_to_agent`'s `agent_name` parameter takes; the two mechanisms keep
separate floors because `research` (a trusty-code specialist) and
`research-agent` (a trusty-agents roster entry) are different things.

**A NEW key in `[subagents]`, not a reinterpretation of the existing one.**
The "cross-product bridge is no longer a 'sub-agent' mechanism" consequence
below notes that renaming the cross-product section would free `[subagents]`
for this whitelist. That rename is a SEPARATE, unratified decision, so this
change took the additive key (`delegate_allowed`, alongside the existing
`allowed`) and left the rename to it.

**Sub-question 1 — the absent-whitelist default — answered: fail-closed WITH a
seeded default (option (a)).** An absent list reaches nothing, matching the
cross-product precedent exactly and avoiding option (b)'s two-different-
absent-semantics split. The migration option (a) called for shipped: all eight
bundled assistant files carry `[subagents].delegate_allowed = ["research-agent",
"ticketing-agent"]` — the four packages (`assistant/`, `izzie/`,
`cto-assistant/`, `ctrl/`) AND the four flat files (`izzie.toml`,
`cto-assistant.toml`, `ctrl.toml`, `personal-assistant.toml`). The flat
`extends`-shadow fallbacks are seeded too, because they are what loads when the
`extends` chain does not resolve; a seed on the package alone is a
half-migration. `bundled_assistant_personas_seed_the_reachable_subagent_whitelist`
pins all eight so a persona added later cannot silently ship without one.

**Sub-question 2 — can a write widen past a floor — answered: no, enforced
server-side.** `PATCH /api/agents/:name` gained
`subagents_delegate_allowed`, and unlike its `tools_allow` neighbour it
validates: `subagent_allow::narrow_to_floor` checks every entry against the
floor BEFORE the file is touched, and a widening request is a `400` naming the
offenders with nothing written. Two layers, not one — a config that reached
disk some other way is still refused at dispatch by `resolve()`, which
re-checks the same floor. `tools_allow`'s own unvalidated behaviour is
deliberately unchanged: auditing it against a capability ceiling is separate,
unratified work, and tightening it silently inside this change would be an
unreviewed break for every existing GUI edit.

**The gates stay independent; they were not collapsed.** Reachability is
`!(kind_blocked || tier_blocked || !whitelisted)`. The conformance checklist
above is explicit that the kind exclusion must remain "a property of the CODE,
not of the data an operator is trusted to curate correctly" — which only holds
while the kind predicate and the whitelist are separate conjuncts. The
whitelist gate is also SCOPED to the assistant population (it runs when the
delegator declares the assistant kind, or opts into the role allowlist, which
only assistant-tier construction sites do), so `pm`/`ctrl`-as-orchestrator
delegation is byte-identical to before.

**Agent definitions were NOT removed — only reachability changed.** Every
engineer, QA, docs, planning and ops agent is still in the catalog, still
dispatchable by `pm`, still listed by the roster. What changed is that an
ASSISTANT no longer reaches them.

**The persona prose had to be rewritten in the same change, and that was not
optional.** `.trusty-agents/agents/assistant/persona.md` instructed the model
to delegate by exact name to `engineer`, `python-engineer`, `qa-agent`,
`docs-agent`, `local-ops-agent` and `plan-agent`; `personal-assistant.toml`
told it to "bring in an engineering specialist… never frame it as something you
can't do"; `ctrl/persona.md`, `ctrl.toml`, `izzie.toml` and
`cto-assistant.toml` carried the same lists. Every one of those became a live
instruction to make a call the gate now refuses. Deleting them would have left
a hole, so each site was rewritten to state what the persona CAN reach and what
to do when asked for coding work (say so plainly, do the part it can, offer a
ticket). `assistant_tier_persona_carries_curated_worker_routing_list` now
asserts both halves — the two reachable names are present AND the six removed
ones are absent — so a partial revert of either the gate or the prose fails a
test.

**The skills surface lost ticketing entirely (owner, same session).** The
`function_skill("ticketing", …)` row and the twelve leaf ticket/CI skill rows
were deleted, and `cto-assistant`'s four direct ticket-tool grants
(`ticket_search`, `list_tickets`, `get_ticket`, `create_ticket`) were dropped
from both its package and its shadow. Ticketing is reachable as a SUB-AGENT
only. The ticketing TOOLS are untouched and still granted to
`ticketing-agent.toml`; the two authored assets
`.trusty-agents/skills/ticketing-epic.md` and `ticketing-ticket.md` are a
different subsystem — that sub-agent's own domain knowledge, injected into its
system prompt — and were PRESERVED. `every_tool_declared_in_source_has_a_skill`
gained one closed, documented exception list rather than being widened.

## Rationale: The Virtual-Twin Authority Principle

The owner gave the underlying reason the boundary above falls exactly where
it does, in these words:

> "assistants and COMMUNICATE with each other, the difference is each
> assistant is a virtual twin that must take authority over its own
> actions."

**The principle:** each assistant is a virtual twin — it must take authority
over its own actions. Delegation TRANSFERS authority: one agent commanding
another to act on its behalf. Communication transfers nothing: it is an
exchange between two parties, each still acting under its own authority. Two
twins cannot command each other, because neither can surrender authority
over its own actions — that authority is not transferable.

This DERIVES the boundary stated in the Decision above, rather than merely
stipulating it:

- **A sub-agent holds no independent authority.** It is an in-process
  extension of the assistant that invoked it (single-edge leaf, Context §2),
  so delegating to it exercises the assistant's OWN authority rather than
  transferring authority to a second, independent holder. Delegation is
  coherent for this edge.
- **An assistant owns its own actions.** One assistant commanding another
  would require the target to surrender authority over its own actions to
  the source — the one thing a virtual twin cannot do. Only communication
  (decisions 1/3/4 above; DOC-60 §5.3) is coherent on this edge.
- **This is also why YOLO / owner-accepted responsibility attaches to
  assistants, never to sub-agents** (decision 5): responsibility follows
  authority, and only assistants hold any.

**Terminology note.** "Virtual twin" also appears elsewhere in this corpus —
DOC-42 ("Engineering Lead / Virtual Twin Cross-Tool Orchestration
Architecture," PR #3006, closed unmerged, its live claim since moved to
DOC-44) and ADR-0016 (which cites DOC-42) use it as a **role name** for the
Engineering Lead, a persistent, portfolio-level supervisor above PM. Here,
by contrast, "virtual twin" is **philosophical framing, not a technical
identifier**: it names the sense in which each assistant owns its own
actions, motivating the authority principle above. The two usages coexist
and neither constrains the other.

## Normative Rule: Delegation Authority Is Governed by Kind, Not by Tier Order

This section formalizes, as a binding architectural rule rather than an
observed defect, the finding first raised in this document's "Does the
one-directional L0/L1 gate, as implemented, still express the intended
rule?" consequence (retained below, unchanged, as the motivating analysis).
The owner instructed that this finding be recorded normatively so an
implementer can check conformance against it directly, independent of
which specific lines a fix lands on.

### The structural claim

**A total order cannot express a peer prohibition.** Any mechanism of the
shape "delegation is permitted iff `tier(source)` dominates `tier(target)`
in some fixed ordering" is structurally incapable of forbidding an edge
BETWEEN two nodes at the SAME rank, for any numbering whatsoever — not
because the shipped numbering (`L0Orchestration` > `L1Standard`) was chosen
badly, but because no numbering can encode "these two are peers who may
not delegate to each other" using only "is my rank higher than yours." The
shipped gate (`delegate.rs:373-374`: `target_tier == L0Orchestration &&
delegator_tier != L0Orchestration`) worked, before decision 3, only because
tier happened to coincide with the property the rule actually cares about:
every assistant was L1 and L0 was an empty, unpopulated tier, so "assistant
delegating to assistant" and "L1 delegating to L1" were the same event, and
the tier check was never asked to adjudicate an L0-vs-L0 edge because no
such edge could exist. Decision 3 breaks that coincidence on purpose (every
assistant becomes L0); the tier check does not know this happened, because
it was never a check ABOUT assistants and sub-agents — it was, and remains,
a check about tier numbers. **This is why renumbering cannot fix it, and
why any future fix that leans on tier values reintroduces the same defect
under a different set of labels.**

### The rule, as a conjunction of predicates on the edge (source, target)

Delegation from `source` to `target` is authorized if and only if all three
hold:

1. **`KIND(source) = assistant`.** Only assistants may delegate at all.
   `KIND` here is the existing `agent.role` field — specifically,
   `role == ASSISTANT_TIER_ROLE` (`tool_registry.rs:98`) — not a new
   attribute; this predicate already has a partial structural analog in
   shipped code (`build_registry_for_agent` never registers
   `DelegateToAgentTool` for any other role, `tool_registry.rs:188-193`,
   confirmed in "Consequences" below). **Scope caveat, checked against the
   code:** `pm`/`ctrl`-as-orchestrator (role `orchestrator`) also delegates
   today, unrestricted, through a THIRD and FOURTH construction site this
   predicate deliberately does not cover —
   `ctrl_delegate_posture`'s `role != "assistant"` branch
   (`history.rs:395-397`, returning no taint and no role allowlist) and
   `runtime::pm_mode::run_pm`'s direct, config-dir-less
   `DelegateToAgentTool::new(runner)` (`pm_mode.rs:111-113`, documented
   inline as deliberate: "`pm`... is the trusted top-level orchestrator,
   already unreachable as a delegation TARGET from any L1 persona"). Both
   are pre-existing, separately-trusted actors OUTSIDE the assistant/
   sub-agent model this ADR defines, by the code's own documented design.
   This predicate set governs the ASSISTANT/SUB-AGENT graph only; it
   neither constrains nor is threatened by `pm`/`ctrl`'s own orchestrator-
   kind delegation, which is a different, older capability this ADR does
   not revisit.
2. **`KIND(target) ≠ assistant`**, i.e. target is a sub-agent. **This single
   predicate subsumes both the peer-prohibition case (assistant-to-
   assistant) and the now-vacuous L0-to-L0 case** — there is no need for a
   separate "no lateral delegation" rule once the target's kind is checked
   directly, because "not an assistant" already excludes every assistant,
   including ones that happen to share the source's tier.
3. **Tier ordering holds** (`target_tier != L0Orchestration ||
   delegator_tier == L0Orchestration` — the existing, unmodified gate).
   **Retained as defense-in-depth, not as the primary carrier of the
   rule.** Given predicates 1 and 2 are correctly enforced AND the ratified
   tier assignment holds (every assistant declares `tier = "l0"`, every
   sub-agent remains `L1Standard` by the existing fail-closed default),
   predicate 3 is checked but never itself the reason a call is refused —
   the source is always L0 (predicate 1) and the target is always L1
   (predicate 2 + the ratified assignment), so the tier condition is always
   already satisfied by the time it runs. **This redundancy is
   deliberate, not vestigial**: predicate 3 is evaluated over a DIFFERENT
   piece of config (the `tier` field) than predicates 1-2 (the `role`
   field), read by independent code paths. If a future defect in the
   role/kind check (predicate 1 or 2) ever let an assistant-kind target
   slip through — and that target's `tier` was correctly declared `l0`
   as this rollout requires — predicate 3 still catches it. A bug in one
   layer does not, by itself, open the graph.

### The permitted graph

The predicates above are derivable from, and should be read as consequences
of, this closed graph — the owner has now ratified every edge in it:

- **`user <-> assistant`** — assistants are always user-instantiated; the
  user converses with an assistant directly.
- **`assistant -> sub-agent`** — delegation (predicates 1+2+3 above).
- **`sub-agent -> its invoking assistant`** — response only; not a new
  outbound edge, the return path of the delegation above.
- **`assistant <-> assistant`** — COMMUNICATION only, never delegation.
  Currently UNIMPLEMENTED (see "Consequences," "'Communicate' has to be
  built, not renamed").

**No other edges exist.** Sub-agents are leaves: no outbound delegation
(enforced structurally today — see "Consequences," "Is 'sub-agents never
delegate' enforced, or merely conventional?"), no out-of-process
communication (decision 1). The `pm`/`ctrl`-as-orchestrator edges named in
predicate 1's scope caveat are explicitly OUTSIDE this graph — a separate,
pre-existing capability this ADR does not fold in, revisit, or constrain.

### Why this class of error recurs

Concretely, not moralizing: **an authority rule encoded against a proxy
attribute (tier) rather than the attribute it is actually about (kind)
will hold exactly as long as the proxy correlates with the real thing, and
fail silently the moment a product decision decorrelates them.** Three
specific, checked facts about why THIS instance was silent, not loud:

- **No log fires on the permitted path.** `delegate.rs`'s `execute()` logs
  a `tracing::warn!` when `tier_blocked` is true (`delegate.rs:376`) and
  `tracing::debug!` when a role is disallowed or a name fails to resolve
  (`delegate.rs:389`, `403`, `412`), but the SUCCESS path — falling
  through the `if tier_blocked` / `if rejected` checks straight to
  `let final_task = ...` and `self.runner.run(...)` (`delegate.rs:448-456`)
  — emits no
  tracing statement at all recording which gate combination let the call
  through. An operator watching logs sees rejections; a permitted
  assistant-to-assistant delegation, once the population inverts, looks
  identical in the logs to any other successful delegation.
- **No test exercises the newly-real case.** `delegate_tests.rs` has
  `delegate_l1_to_l0_is_refused` and `delegate_l0_to_l1_succeeds`
  (`delegate_tests.rs:554-619`) but no `delegate_l0_to_l0_is_refused` —
  grepped directly, no such test exists. This is not an oversight in test
  coverage discipline; before decision 3, an L0-to-L0 scenario had no
  realistic fixture to write a test against (L0 was unpopulated), so the
  gap in the test suite exactly mirrors the gap in the rule, for the same
  underlying reason.
- **The regression is invisible to `cargo test` by construction.** Because
  the tier gate's CODE did not change between "L0 empty" and "L0 = every
  assistant" — only the DATA (which personas declare which tier) changed —
  no test suite run, no diff review of `delegate.rs` itself, and no CI
  green/red signal was ever going to catch this. This is why the finding
  surfaced only through a fresh architectural read of the predicate against
  the NEW population, not through any mechanical check — which is exactly
  the situation a proxy-attribute rule produces: it is not that testing
  was inadequate, but that the wrong thing was being tested because the
  wrong thing was being CHECKED, in the shipped code.

**A second, independently-found instance of the same class**, checked
rather than merely asserted, and named per the owner's instruction rather
than left unsearched: `cross_product::NON_CODING_TARGETS`
(`cross_product.rs:63`) authorizes cross-product reach by AGENT NAME
membership in a hardcoded list, standing in for a capability the codebase
does not yet have a field for — "is this agent actually non-coding" (no
`is_coding: bool`, no capability tag exists on `AgentConfig` today). The
module's own doc comment already names this as deliberate and temporary:
"#4030's runtime-built domain authority is what will eventually FEED this
set, and until it exists an exact list is the only honest source"
(`cross_product.rs:59-60`). Its failure mode differs from the tier-gate
case (a name list fails by silently OMITTING a newly-eligible agent, never
by silently ADMITTING an ineligible one — the opposite polarity, and a
safer one), but the root pattern — a security-relevant property encoded via
a coincidentally-correlated stand-in rather than the property itself — is
the same. No third instance was found; this ADR looked (`grep`-searched
name-based and tier-based conditionals across `crates/trusty-agents/src`
for further authority-relevant special-casing) and is reporting a negative
result for anything beyond these two, not an absence of looking.

### Conformance

An engineer is implementing this finding on branch
`fix/persona-dispatch-role-allowlist` (kind-primary, tier retained as
defense-in-depth, per the coordinator). **This ADR could not locate that
branch pushed to `origin` at the time of this revision** and is not
claiming to have reviewed its diff; the checklist below is written so
conformance can be checked against the RULE stated above, independent of
which lines the implementation touches:

- [ ] The predicate-2 check (`target KIND != assistant`, when
  `source KIND == assistant`) is enforced **unconditionally inside
  `DelegateToAgentTool::execute`**, not merely by curating the contents of
  decision 4's editable whitelist. A whitelist is caller-supplied config;
  if the kind exclusion lives ONLY in "which names happen to be listed,"
  the same class of silent gap reopens the moment a whitelist is
  misconfigured to include an assistant-role name. The kind check must be
  a property of the CODE, not of the data an operator is trusted to curate
  correctly.
- [ ] The fix closes the gap at **all** sites that construct a
  `DelegateToAgentTool` for an assistant-tier caller — today that is
  `history.rs`, `persona.rs`, and `tool_registry.rs`'s
  `build_assistant_tier_registry` (the `pm_mode.rs` site is explicitly
  OUT of scope — it is the `pm`/`ctrl`-as-orchestrator case predicate 1's
  caveat excludes). Per-call-site opt-in (`.with_allowed_target_roles(...)`
  as a builder method each caller must remember to call) is exactly the
  shape that produced #4201's gap; moving the kind check into the shared
  choke point (`execute()` itself, unconditional whenever the delegator's
  OWN role is `ASSISTANT_TIER_ROLE`) removes the opt-in entirely rather
  than adding a fourth site that could someday forget it too.
- [ ] A test named for the KIND relationship, not the tier labels — e.g.
  `delegate_assistant_to_assistant_is_refused` rather than
  `delegate_l0_to_l0_is_refused` — asserts the peer-prohibition case, and
  is written against role/kind fixtures. Per the coordinator: this test
  must keep passing if assistants and sub-agents are ever renumbered again
  (tiers swapped, a third tier introduced, labels changed) — which is only
  possible if the test's fixture setup and its assertion are both phrased
  in terms of KIND, never in terms of which tier enum variant currently
  happens to represent that kind.
- [ ] The now-logged success path (if added) records which predicate
  combination authorized a delegation, closing the "no log fires on the
  permitted path" gap named above — an operator should be able to observe
  from logs alone that predicate 3 was redundant-as-designed on a given
  call, not have to infer it.
- [ ] The PR's own description states, explicitly, that predicate 3 (tier
  ordering) is retained as defense-in-depth and is NOT expected to block
  anything additional once predicates 1-2 are correct — so a future reader
  of that PR does not conclude the tier gate was the fix and quietly let
  predicates 1-2 rot.

## Consequences

### The cross-product bridge is no longer a "sub-agent" mechanism (retained, options unchanged)

Three options remain as drafted: (A) deprecate `dispatch_task`'s
named-specialist widening entirely; (B) retain it, re-scoped and renamed
away from "sub-agent"/"cross-product" language (the owner's own 2026-07-28
review independently asked to "remove 'cross-product' as user-facing
terminology" — item (c) in that review); (C) build a genuine in-process
ticketing capability and retire the cross-product leg for ticketing
specifically. **Recommendation (unchanged, owner must ratify): Option B.**
One additional benefit surfaces now that decision 4 introduces a NEW
editable `[subagents]`-shaped whitelist for the in-process mechanism (see
below): renaming the cross-product TOML section away from `[subagents]`
frees that name for the in-process whitelist, resolving the naming
collision this document's first draft flagged against DOC-41's older,
unbuilt `subagents.allowed` design.

### `ASSISTANT_TIER_ROLE` in the allowlist is a behavior change, and breaks a tested feature (retained, reframed)

Unchanged from the first draft: removing `assistant` from
`ASSISTANT_ALLOWED_DELEGATE_ROLES` breaks
`delegate_assistant_role_gate_allows_peer_assistant_role`
(`delegate_tests.rs:411-435`, the Izzie↔cto-assistant peer-consult lane) and
must land in the SAME change that builds the lateral "communicate"
mechanism, not as a bare removal — assistants would otherwise lose peer
interaction entirely for however long the gap lasts. Now reframed correctly
under decision 3's L0/L1 assignment: this is not merely "removing one
constant entry," it is removing the ONLY EXISTING peer-interaction path for
a population (assistants) that decision 3 newly and explicitly requires to
have one (laterally, non-delegated). The persona.rs gap (§2/#4201) must be
closed in the SAME change — landing the fix at the two protected sites
while leaving the third unpatched would let the REPL `/agent` path continue
to permit exactly the lateral (and worse, upward — to `pm`) delegation this
decision forbids.

### `ticketing-agent` is unreachable in-process — decision 2 changes the fix (retained, reframed)

The three prior options (widen the const, recategorize the role, or build a
new in-process capability) are now subsumed by decision 2's mandate: the
FIX must be an editable config-whitelist ENTRY (adding `ticketing-agent`'s
name to whichever assistant's whitelist should reach it), not a Rust source
change. This removes the "widens a global constant for every caller
simultaneously" cost the first draft flagged — the whitelist is per-agent
config, not a shared constant — but see the "editable" consequence below
for a NEW cost this introduces (write-time widening past a floor).
"MacOS Manager"/"AWS Manager" (the owner's 2026-07-28 curated-set proposal)
remain net-new authoring regardless of decision 2's "re-use existing
agents" framing, since no such agents exist anywhere in the roster today —
decision 2 supplies the REACHABILITY mechanism, not the missing agents
themselves.

### "Communicate" has to be built, not renamed (retained)

Unchanged: no code path implements assistant-to-assistant messaging today.
The two candidate foundations (ADR-0019's unbuilt bus, built on ADR-0016's
singleton-Assistant addressing; or a minimal purpose-built messaging tool
scoped to `role = "assistant"` targets) remain unreconciled. This ADR still
does not choose between them.

### Does the one-directional L0/L1 gate, as implemented, still express the intended rule? (new)

**The gate is written in terms of TIER ORDERING, not specific roles**
(`delegate.rs:373-374`: `target_tier == L0Orchestration && delegator_tier
!= L0Orchestration`) — it has never known or cared which role occupies
which tier. That is precisely why decision 3's population inversion changes
its effective meaning without changing one line of its code:

- **Assistant(L0) → sub-agent(L1): still correctly ALLOWED** — the gate
  never blocks a delegation into a non-L0 target, and under decision 3
  every sub-agent is L1 by the existing fail-closed default (no code
  change needed here; this is the one case where the shipped gate and the
  new model happen to agree).
- **Assistant(L0) → assistant(L0), i.e. LATERAL delegation between
  assistants: NOT blocked by the gate as coded.** The condition requires
  `delegator_tier != L0Orchestration`; once every assistant is L0 (decision
  3), the delegator IS L0 in exactly the case decision 4 wants forbidden,
  so the tier check evaluates false and nothing refuses the call. **The
  gate that was built to protect the (formerly rare, formerly untouchable)
  L0 tier from below now structurally cannot protect L0 from ITSELF**,
  because the shipped model only ever conceived of one L0 actor at a time,
  never many peers at the same tier.
- **Sub-agent(L1) → anything: not something the tier gate needs to
  block**, because — see the next consequence — it is already impossible
  through a separate mechanism.

**This is not a "reads backwards" bug in the strict sense (it never
inverts a comparison), but it is now VACUOUS for the scenario it was built
for and SILENT on the scenario decision 4 actually needs guarded.** The
gate's original threat model (an untrusted L1 escalating into a trusted,
rare L0) no longer maps onto the population it will be evaluated against
(many mutually-lateral L0 peers, none of them untrusted-content-holding in
quite the old sense, since untrusted-content ingestion is an L1-only
constraint that decision 3 does not relax). A NEW, role-aware or
identity-aware check is required to enforce "L0 may never delegate to
another L0" — the existing tier-ordering gate cannot express it without a
second axis (e.g. comparing delegator identity to target identity, or a
distinct "peer" relation orthogonal to tier). This is implementation work,
not a documentation fix.

### Is "sub-agents never delegate" enforced, or merely conventional? (new)

Checked directly: `runtime::tool_registry::build_registry_for_agent`
(`tool_registry.rs:178-427`) is the ONLY place `DelegateToAgentTool` is ever
registered into an agent's native tool registry, and it does so EXCLUSIVELY
inside the `role == ASSISTANT_TIER_ROLE` branch
(`build_assistant_tier_registry`, line 188-193). Every other named branch —
`research-agent`, `analysis-agent`, `code-agent`, `plan-agent`, `qa-agent`,
`local-ops-agent`, `docs-agent` — and the `_` catch-all (lines 414-426)
build a registry with NO `DelegateToAgentTool` entry at all. **A sub-agent
spawned via `run_subagent`** (`subagent_mode.rs:116`, which calls
`build_registry_for_agent` at line 382 for every spawn, regardless of
whether it was reached by `--direct`/`--agent` or by an assistant's
`delegate_to_agent` call) **structurally cannot call `delegate_to_agent`,
because the tool is never in its own registry to begin with — not merely
denied by a scope/allowlist check, but absent.** This means "sub-agents
never delegate" is **already enforced today**, by construction, for the
native trusty-agents tool-calling loop — this is required work only for the
LATERAL and UPWARD directions the model already permits assistants (via the
allowlist gap above), not for sub-agents, which have no path to delegate at
all under the current registry-construction code. One caveat, not fully
traced in the time available: agents with `runner = "claude-code"` dispatch
through a separate `ClaudeCodeAgentRunner` that shells out to the actual
`claude` CLI, a process with its own independent tool surface; this ADR
does not claim to have verified whether any MCP bridge could expose
`delegate_to_agent` to that subprocess, only that the trusty-agents-native
`ToolRegistry` path — the mechanism the question was asked about — does
not.

### The absent-whitelist default is a breaking change for every existing assistant (new, owner-ratify)

> **RESOLVED 2026-07-29 — the owner ratified option (a).** Fail-closed when
> absent, WITH a seeded default shipped in every bundled assistant persona so
> nothing drops to zero reachable targets on rollout. The analysis below is
> retained unedited as the record of what was weighed; see "What the
> editable-whitelist clause's implementation actually did" for what shipped.

Today, in-process reachability for an assistant-tier caller is an
UNFILTERED SCAN: every agent in `agents::agents_dir_candidates()` whose
`role` is in `ASSISTANT_ALLOWED_DELEGATE_ROLES` is a target, no per-agent
config required — confirmed directly by the owner's own 2026-07-28 review
comment on #4021 ("it currently shows all 18 role-eligible agents... with
no per-agent narrowing possible today") and by `in_product_surface`
(`agent_subagents.rs:232-309`), which scans the WHOLE catalog and includes
any role-eligible match. By contrast, the EXISTING cross-product mechanism
this ADR's decision 4 explicitly takes as its shape — `SubagentAllowSet`,
built from `[subagents].allowed` — treats an ABSENT config section as
EMPTY, deny-by-default (`cross_product.rs:119-131`, `SubagentAllowSet::
empty`: "an absent `[subagents]` section is NOT a silent capability
grant"). **If decision 4's editable whitelist reuses that same
absent-means-empty posture for the IN-PROCESS mechanism, every existing
assistant persona — none of which has ever needed to declare a
`[subagents]` section, because none exists for this purpose today — drops
from ~18 reachable targets to 0 the moment this ships, with no migration.**
Options:

- **(a) Ship a pre-populated default `[subagents].allowed` list in every
  bundled persona TOML** (`izzie.toml`, `cto-assistant.toml`,
  `personal-assistant.toml`, `ctrl.toml`) at rollout, listing today's
  effective role-eligible set. Cost: a one-time content migration across
  the shipped roster; any user-AUTHORED custom persona still gets the
  honest deny-by-default (no section = no targets), which is arguably the
  correct behavior for a NEW agent.
- **(b) Treat "absent `[subagents]`" as "grandfather in the current
  role-scan behavior"** rather than empty. Cost: this creates TWO different
  absent-semantics for a structurally identical config shape, depending on
  which mechanism reads it (cross-product: absent = empty; in-process:
  absent = full legacy scan) — a durable, confusing inconsistency baked
  into the same product surface that will need explaining forever.
- **(c) Accept the breaking change outright and require every assistant
  persona to be re-configured before this ships**, treating it as a hard
  cutover coordinated with the rollout. Cost: highest short-term
  disruption; cleanest long-term posture (one absent-semantics rule,
  matching the cross-product precedent exactly).

**Recommendation (owner must ratify): Option (a).** It preserves the
deny-by-default principle for every future agent while avoiding both the
silent capability loss of doing nothing and the semantic split of Option
(b).

### "Editable" means the GUI starts writing agent config — can a write widen past a floor? (new, owner-ratify)

> **RESOLVED 2026-07-29 — the owner ratified the recommendation below.** The
> write path enforces a server-side floor: `PATCH /api/agents/:name`'s new
> `subagents_delegate_allowed` field runs `subagent_allow::narrow_to_floor`
> before touching the file and rejects a widening request outright. The
> `tools_allow` audit this section describes is NOT part of that change and
> remains open. The analysis below is retained unedited.

The nearest existing precedent for a GUI-driven agent-config write is
`PATCH /api/agents/:name` (`agent_patch.rs:159-170`), specifically its
`tools_allow` field (added by #3819): *"overwrites `[tools].allow`... with
this exact list"* (`agent_patch.rs:140-147`). **Checked directly: the
write path applies NO validation against any floor or capability ceiling.**
`patch_agent_at` inserts the caller-supplied array into `[tools].allow`
verbatim (`agent_patch.rs:380-392`) — there is no check that the new
patterns are a subset of anything, no comparison against a role-derived or
tier-derived maximum. **A `PATCH` call today can WIDEN a persona's declared
tool-allow list arbitrarily far**, subject only to which registered tools
actually exist to match against (the SEPARATE, unrelated
`scope_assistant_allowed_tools` gate narrows what's GRANTED at dispatch
time, but that is a read-time computation over whatever `[tools].allow`
currently says — it does not stop the write itself from saying something
broader than before). **This is the opposite of `SubagentAllowSet`'s
design**, which is explicit that config "can only ever narrow that floor,
never widen it" (`cross_product.rs:213-215`, `agents::config::
SubagentsConfig`'s own doc: "`allowed` list is intersected with
`tools::cross_product::NON_CODING_TARGETS`... config can only ever narrow
that floor, never widen it"). **If decision 4's editable whitelist is
implemented by following the `tools_allow` PATCH precedent literally, a
GUI write could widen an assistant's reachable sub-agent set past whatever
floor exists (e.g. past the role-eligibility check, or past whatever
replaces `ASSISTANT_ALLOWED_DELEGATE_ROLES`) with no server-side
enforcement — a materially different, and weaker, security posture than
the fail-closed, floor-only design OQ-7 established for the cross-product
mechanism it is meant to parallel.** Recommendation (owner must ratify):
the new whitelist's write path must intersect against a server-owned floor
the SAME way `SubagentAllowSet::resolve` does — the floor can still be
"any agent currently in the resolvable roster" (decision 2's "re-use
existing agents"), but role-ineligible or otherwise-disqualified agents
(e.g. `pm`, `ctrl`, any future genuinely L0-only orchestration persona)
must be rejected by the WRITE endpoint itself, not merely left unreachable
by a downstream read-time filter that a future refactor could silently
drop.

### The 3-tool-call threshold is not knowable before the work is done — raised, not decided (new, owner-ratify)

Two readings are both defensible and produce different architectures:

- **A-priori estimate**: the assistant, before acting, predicts how many
  tool calls the task will take and routes on the prediction. Problem: this
  is an LLM self-estimate with no ground truth to check it against —
  routing correctness depends entirely on estimation accuracy, which is
  neither measured nor bounded anywhere in this decision.
- **Reactive handoff-at-3-calls**: the assistant starts calling its own
  skill directly and, on exceeding 3 tool calls WITHIN that attempt, hands
  the remainder off to the sub-agent. This avoids the estimation problem
  but creates a genuine **state-transfer question this ADR does not
  answer**: what does the assistant pass to the sub-agent at the handoff
  point (the 3 calls' results? a summary? nothing, and the sub-agent
  restarts from the original task)? And is work REPEATED — does the
  sub-agent redo the first 3 calls' worth of work because it has no memory
  of them, or does the handoff need a `HandoffContext`-shaped payload
  (mirroring `cross_product::HandoffContext`, `cross_product.rs:211-222`,
  the EXISTING pattern for handing state across exactly this kind of
  boundary) built for this specific transition?

There is also a second-order consequence, noted but not resolved here: the
"call a skill directly" half of the routing decision presumes skills are
INVOKABLE, tool-shaped, and countable per call. `build_assistant_tier_
registry`'s own doc comment currently states the opposite design intent —
that assistant-tier personas "never need... a catalog-browsing tool"
because their skills are already "injected as system-prompt CONTENT"
(`tool_registry.rs:440-449`), and the function deliberately OMITS
`list_skills`/`load_skill` for this tier. Decision 3's "calls skills
directly" and decision 5's "3 tool call threshold" together imply skills
must become invokable, countable actions for the assistant tier — a
different mechanism than what is built, not a rewording of it.

**Recommendation (owner must ratify): the reactive reading**, because it
does not depend on an unverifiable LLM estimate, PROVIDED the state-transfer
question is answered as part of the same implementation — reusing
`HandoffContext`'s existing shape (summary/relevant_state/constraints, 4
KiB cap) for the skill-to-sub-agent handoff is the natural fit, since it
already exists, is already tested, and already solves an adjacent
boundary-crossing problem. This ADR flags the question; it does not resolve
it.

### The YOLO generalization's actual blast radius, as coded today (new)

Decision 5 ratifies generalizing YOLO from a single, rare, not-yet-
instantiated L0 orchestration persona to the entire assistant population.
**What that concretely unlocks in shipped code, right now, is narrower than
epic #4167's original four-item table promised**: of #4170 (GitHub PR/CI
tool surface), #4171 (session-state read access), #4172 (cross-project
store/git scoping), and #4173 (shell/build/test execution grant), **only
#4171 is closed/shipped**. The other three remain open. So today, flipping
every assistant persona's declared `tier` to `l0` grants, in practice, only
`session_state_list`/`session_state_status`/`session_state_snapshot`
(`tool_registry.rs:597-611`) — plus removes the ONE existing tier-gated
restriction (delegation into an L0 target) that these personas already
happened to satisfy trivially once they are themselves L0. It does NOT (yet)
grant shell execution, GitHub PR/CI access, or cross-project reach, because
those grants are unbuilt.

**Measured, post-implementation: the delta is not merely "narrow", it is
ZERO for the shipped roster.** `retain_tier_permitted` is deny-only and never
adds a tool, so becoming L0 only makes the three executors REGISTRABLE — a
persona still has to name one in its own `[tools].allow` to reach it, and none
does. No bundled assistant declares any `L0_ONLY_SESSION_STATE_TOOLS` entry, no
`[skills]` section, and no glob wide enough to match one; `ctrl` declares no
`[tools]` section at all and both of its dispatch paths fail closed on that.
`bundled_assistant_personas_resolve_l0_and_gain_nothing` asserts this against
the real files, so the day a persona edit makes the grant real, it fails.

The blast radius the owner ratified is real but
currently small; it will grow as #4170/#4172/#4173 land, and each of those
follow-ups should be read, from this point forward, as extending a grant to
the ENTIRE assistant population rather than to a single rare persona —
which changes their own risk calculus and may warrant the owner re-reviewing
them individually rather than assuming the 2026-07-27/2026-07-28 rulings
already cover their specifics.

## Conflicts and open questions

- **This decision revises the owner's own 2026-07-26 ruling on epic
  #4021's OQ-2.** The later 2026-07-28/29 owner ratification resolves the
  conflict in favor of this ADR; the earlier ruling remains historical context.
- **This decision, dated 2026-07-28, inverts the population assignment of
  the L0/L1 tier model merged the SAME DAY** (PR #4200, squash `ada4d351`)
  — L0 was built as "a new, rare orchestration persona above today's
  assistants"; decision 3 makes L0 "today's assistants, all of them." This
  is not a contradiction requiring resolution so much as a FAST reversal —
  worth the owner's explicit awareness that the tier model he approved
  hours earlier in the day is being redefined, not merely extended, by the
  same day's later decisions. See Context §3 and the "one-directional gate"
  consequence above for the concrete mechanism this leaves inert/vacuous
  until patched. **RESOLVED as of the implementation**: the owner ratified
  the inversion on 2026-07-28 and it has landed. The tier gate is now
  redundant-as-designed for assistant sources (predicate 3), the peer
  prohibition is carried by the kind gate, and #4200's own tests are intact
  because its fail-closed DECLARATION contract is unchanged — only the
  meaning of an ABSENT declaration moved. What remains genuinely open is
  narrower and is recorded above: `pm`-as-orchestrator's tier is reported
  from its declared value on the pane while dispatch hands it
  `L0Orchestration`, an imprecision this ADR's predicate-1 scope caveat puts
  outside the model rather than fixes.
- **DOC-41 §5.5's propose-not-authorize guarantee is not cross-product-
  specific** (retained from the first draft, unchanged) — the in-process
  path satisfies it via manifest-level `user_authority` non-inheritance;
  the cross-product path satisfies it via the explicit `ProposalEnvelope`.
  Collapsing the boundary does not remove the guarantee; it changes which
  mechanism carries it.
- **"Assistant" is overloaded across ADR-0016/0019 and this ADR** (retained)
  — ADR-0016's singleton hierarchy role vs. `trusty-agents`' plural
  persona-tier population, now formally the L0 population under this
  decision. Not resolved here.
- **The `[subagents]` TOML naming collision** (retained) — resolved in
  direction, not yet in code: Option B (cross-product renamed away from
  "subagents") frees the name for decision 4's new in-process whitelist,
  which is the natural place for it, but this requires the cross-product
  rename to land first or concurrently, or the collision persists.
- **The persona-gate role-allowlist finding is now independently confirmed
  by issue #4201's comment thread** (upgraded from "surfaced during this
  ADR's own research" to "cross-verified against a separate, pre-existing
  issue"), and remains a pre-existing, security-relevant finding
  independent of this ADR's ratification — it should be fixed regardless
  of how the rest of this document resolves.
- **This ADR's own first draft is partially superseded by decision 2**,
  explicitly marked at the top of this document and in the relevant
  Consequences sections — this is noted here too because "Related
  Decisions" vetting protocol (DOC-46 §3) calls for surfacing self-
  supersession exactly like inter-ADR supersession, not silently editing
  the earlier recommendation out.
- **This ADR is still the first normative text on "Sub-agents" anywhere in
  the spec corpus** (retained) — DOC-57 (#4182, open) has never documented
  the section.

## Related Decisions

Vetted against prior ADRs (`docs/adr/INDEX.md`) on 2026-07-28 (revision 2):

- **ADR-0004 (Three harnesses, shared event-driven common):** Consistent —
  unchanged from the first draft's vetting.
- **ADR-0015 (Unified agent composition, Proposed):** Consistent, with the
  same note as the first draft — this ADR's purpose-level distinctions
  (in-process leaf sub-agent / cross-product dispatch / lateral
  communication) are not yet named in ADR-0015.
- **ADR-0016 (Orchestration Hierarchy: Lead/PM/Assistant, Proposed):**
  Extends, with the SAME flagged naming collision as the first draft, now
  sharper: ADR-0016's singleton "ASSISTANT" and this ADR's newly-formal L0
  "assistant" population are still different things sharing one name.
  Additionally, §7 above uses ADR-0016's PM-analogy sibling (the trusty-mpm
  PM, not ADR-0016 itself) as prior art — worth a forward cross-reference
  from ADR-0016 once this ADR is accepted. "Rationale" above also uses
  "virtual twin" in a philosophical sense distinct from ADR-0016's Engineering
  Lead role name of the same phrase; the two are not in tension.
- **ADR-0018 (Loopback-only doctrine, Accepted):** Consistent — unchanged.
- **ADR-0019 (Unified IPC messaging, Accepted, unimplemented):** Extends —
  unchanged, still the most likely foundation for the "communicate"
  primitive, still unreconciled with ADR-0016's singleton addressing.
- **ADR-0020, ADR-0021 (Slack gateway), ADR-0022:** No interaction —
  unchanged.
- **Owner ruling from 2026-07-26:** Conflict resolved — the later
  2026-07-28/29 ratification adopts this ADR's model. The earlier ruling was
  never an ADR, so there is no separate status field to update.
- **New: this ADR's own first-draft recommendation is explicitly
  superseded by decision 2** (see header and "Conflicts and open
  questions") — recorded here per DOC-46 §3's self-consistency
  expectation, even though it is an intra-document, not inter-ADR,
  supersession.

No prior ADR formally addresses tiered agent delegation population
assignment or an editable reachability whitelist; this remains the first.
