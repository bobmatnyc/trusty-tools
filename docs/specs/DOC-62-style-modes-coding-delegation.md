---
spec_refs:
  - id: SPEC-AGENTSTD-04~draft
    path: docs/specs/DOC-61-canonical-agent-standard.md
    anchor: SPEC-AGENTSTD-04~draft
    note: >-
      Per-product system-prompt builders are prospective, not built. §6 of this
      document rules that style carriage therefore uses the existing text
      preamble rather than literal system-prompt composition.
  - id: SPEC-SLD-01~draft
    path: docs/specs/spec-linked-documentation.md
    anchor: SPEC-SLD-01~draft
    note: DOC-N assignment (scan-before-claim) — this document claims DOC-62.
---

# DOC-62 — Style Modes for Coding Delegation: `hack` / `vibe` / `engineer`

**Status:** Accepted — all six §9 open questions resolved by owner decision
2026-08-03 (see §9); landed 2026-08-01 (#4529).
**Spec ID:** `SPEC-STYLE-01~draft` … `SPEC-STYLE-10~draft`
**Subsystem:** cross-crate — trusty-agents (delegation surface, `HandoffContext`, preamble carriage); trusty-code (style parameter, internal pipeline selection); trusty-mpm/GUI (style selector, downstream)
**Owner:** Architecture / Technical Leadership
**Last-updated:** 2026-08-01
**Epic:** [#4345](https://github.com/bobmatnyc/trusty-tools/issues/4345) — tcode-as-coding-delegate
**Builds on:** [ADR-0024](../adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md) (L0/L1 tier model), [DOC-41](./trusty-agents-eve-style-agents-spec.md) §5 (`HandoffContext`, propose-not-authorize), [DOC-61](./DOC-61-canonical-agent-standard.md) §4 (source model vs per-product builder), [`docs/trusty-code/vision-and-architecture-spec.md`](../trusty-code/vision-and-architecture-spec.md) §5.10 + §10 D3 (Execution Patterns)
**Cross-ref:** [#4346](https://github.com/bobmatnyc/trusty-tools/issues/4346) (this spec), [#4348](https://github.com/bobmatnyc/trusty-tools/issues/4348) (tcode style parameter), [#4349](https://github.com/bobmatnyc/trusty-tools/issues/4349) (`HandoffContext` style field + policy preamble), [#4350](https://github.com/bobmatnyc/trusty-tools/issues/4350) (addressable tcode PM target), [#4353](https://github.com/bobmatnyc/trusty-tools/issues/4353) (GUI selector), [#2596](https://github.com/bobmatnyc/trusty-tools/issues/2596) (VIBE execution tier), [#4126](https://github.com/bobmatnyc/trusty-tools/issues/4126) (prompt-injection floor)
**DOC-N claim:** `DOC-62`, scan-before-claim per DOC-38 §4.1. The scan covered every tracked file under `docs/specs/**` and `docs/trusty-installer/research/02-design/**` on `origin/main` **and on every remote branch** (`git for-each-ref refs/remotes/origin`) — the highest claimed DOC number anywhere in the repository, on any branch, is `DOC-61`. `DOC-62` is also what the catalog's (advisory) "next free" note advertises, so hint and scan agree. Four pre-existing collisions are grandfathered in `.doc-number-allowlist.tsv` (`DUP-DOC 28/32/33`, `DUP-ADR 21`); this document deliberately adds no fifth.

---

## 1. Executive Summary {#SPEC-STYLE-01~draft}

**ID:** SPEC-STYLE-01~draft
**Status:** Draft

A **style mode** is a named ceremony level a caller may attach to a coding
delegation. Epic #4345 decision 1 fixes the three names to the already-ratified
Execution Patterns tiers (`docs/trusty-code/vision-and-architecture-spec.md`
§5.10, §10 D3), with no new axis invented:

| Style | Execution Pattern | One-line meaning |
|---|---|---|
| `hack` | QUICK OPS | Trivial — a direct answer or a couple of reads; no engineering ceremony at all. |
| `vibe` | VIBE | Quick coding — no full test suite, no full spec, no tickets; work directly from `main`. |
| `engineer` | FULL LOOP | The full lifecycle — spec → issue → worktree → branch → PR → review → merge → CI. |

This document specifies four things and deliberately declines to specify a
fifth.

**It specifies:**

1. **The boundary (§3).** A style mode selects *tcode's own internal ceremony*
   and has no channel by which it could disable a gate the **target
   repository** enforces. This is the load-bearing invariant of the whole
   feature and is stated first because everything else is conditioned by it.
   Epic #4345's open question 5 asked for confirmation; §3 confirms it **with
   code evidence** — an audit found **no path by which any caller-supplied
   parameter suppresses any repository-enforced gate** — and converts it from
   an open question into a normative rule with a named failure mode. The audit
   also surfaced one asymmetry the epic did not anticipate: **the
   trusty-review gate is process-enforced, not CI-enforced**, so it does not
   inherit the structural protection the GitHub Actions gates have. §3.2 SM-10
   closes that with a rule rather than leaving it to convention.
2. **The per-axis definitions (§4)** — spec artifacts, testing, review gates,
   commit/PR discipline, docs — for each of the three styles.
3. **Where style lives and how it resolves (§5)** — config field, delegation
   parameter, default, precedence.
4. **How style crosses the product boundary (§6).** Epic #4345's open question
   4 asked whether "the full TA PM behavior travels with the delegation" needs
   literal system-prompt composition or whether a text preamble suffices. §6
   rules **text preamble**, with code evidence, and adds the constraint that
   makes that ruling safe: a preamble is *advisory to a model* and therefore
   may carry policy but may **never** be the sole enforcement of a security
   control.

**It declines to specify:** the internal behavior of the VIBE tier. Epic
#4345's open questions 1–3 are inherited verbatim from issue #2596 and are
VIBE-tier pipeline decisions, not style-mode interface decisions. §7 rules that
this spec **consumes** whatever #2596 settles rather than absorbing decisions
that belong there, and defines a fail-safe degradation so that #4349/#4350 are
**not blocked** on #2596 landing.

### 1.1 The single sentence this spec exists to protect

> A style that runs **fewer** gates is legitimate. A style that makes a
> **failing** gate report success is not.

Cutting ceremony where blast radius is small is the entire point of the
feature. Turning red green by deleting coverage is the one thing no style
value may ever do. §3 makes that distinction mechanical rather than
aspirational, because open questions 2, 3, and 5 all circle around it.

---

## 2. Scope and Non-Goals {#SPEC-STYLE-02~draft}

**ID:** SPEC-STYLE-02~draft
**Status:** Draft

### 2.1 In scope

- The **name, meaning, carriage, default, and precedence** of the style value
  at the trusty-agents → trusty-code delegation interface.
- The **normative boundary** between tcode ceremony and repository-enforced
  gates (§3).
- The **reporting contract** for a styled delegation: what a caller is
  entitled to be told about which gates ran (§3.4).
- **Acceptance criteria** the implementing issues (#4348, #4349, #4350) are
  measured against (§8).

### 2.2 Out of scope (explicitly)

- **VIBE's internal pipeline.** Which circuit breakers run, which verification
  steps survive, and what heuristic classifies an unstyled task belong to
  #2596. See §7.
- **Widening `NON_CODING_TARGETS`.** Epic #4345 decision 2 is normative and
  restated here as a constraint, not reopened: the tcode **PM** is the sole
  coding delegation surface; no coding sub-agent becomes reachable from a
  trusty-agents assistant. This preserves the #4126 protection.
- **`HarnessMode`.** `HarnessMode::{DailyDriver, Parity}`
  (`crates/trusty-code/src/mode.rs`) governs prompt/schema **fidelity** and is
  an orthogonal axis. Vision spec §5.10 says plainly: "Do not conflate the
  two." Nothing in this document reads or writes `HarnessMode`.
- **The GUI.** #4353 surfaces the selector; this spec only fixes the
  vocabulary and the precedence semantics that the GUI must mirror.
- **Renaming the existing tcode `engineer` sub-agent.** Decision 1 notes the
  collision (`crates/trusty-code/src/assets/agents/engineer.md`); the style
  value `engineer` and the sub-agent role `engineer` are different namespaces
  and this document does not merge or rename either. §9 OQ-3 flags the
  ergonomic risk.

---

## 3. The Ceremony/Gate Boundary (NORMATIVE) {#SPEC-STYLE-03~draft}

**ID:** SPEC-STYLE-03~draft
**Status:** Draft

> This section is the answer to epic #4345 open question 5, promoted from
> "confirm this" to a normative rule with code evidence. It is stated before
> the style definitions because it constrains all of them.

### 3.1 Two categories, defined so the difference is mechanical

**Ceremony** is process that **tcode itself chooses to run** during a task and
could choose not to run without any external system noticing. Writing a spec
document first; opening a ticket; cutting a worktree; running the
Research → Plan → Code → QA sub-pipeline; running its own verification loop
and circuit breakers. Ceremony is *internal* — its presence or absence is
visible only in how tcode worked, not in what any other system will accept.

**A gate** is a check the **target repository** enforces on the artifact, at a
point tcode does not control. A gate's verdict is produced by a system outside
the delegated task: GitHub Actions, GitHub branch protection, a required
reviewer, or a human-operated review step. Gates in this repository include —
non-exhaustively — the workflows in `.github/workflows/` (`ci.yml`,
`line-cap.yml` (the 500-SLOC cap), `changelog-fragment.yml` (the per-PR
changelog requirement), `sld-lint.yml`, `doc-numbers.yml`, `test-pointers.yml`,
`token-drift.yml`, `capabilities-drift.yml`, `version-parity.yml`,
`cargo-audit.yml`, `generation-artifact-lint.yml`, `agent-assets.yml`), GitHub branch protection
and required reviews on `main`, and the trusty-review gate in the delivery
chain (`CLAUDE.md:493`).

The distinguishing test is **who renders the verdict**. If the delegated task
renders it, it is ceremony. If something outside the delegated task renders
it, it is a gate.

**One gate on that list is not like the others, and the spec says so rather
than assuming it away.** Every gate above except one is a GitHub Actions
workflow evaluated on GitHub's runners from a fresh `actions/checkout`. The
**trusty-review gate is process-enforced, not CI-enforced** — it has no
workflow file; it is invoked by the PM via
`mcp__trusty-review__review_diff`/`review_pr`
(`crates/trusty-mpm/src/assets/skills/tm-pr-workflow.md:100-108`). It is
therefore the one gate on the list whose enforcement a lower-ceremony
*convention* could plausibly erode, even though no parameter can reach it.
§3.2 SM-10 addresses this directly.

### 3.2 The rules

- **SM-1 (scope).** A style mode is an input to tcode's selection of its own
  ceremony. It MUST NOT be an input to any gate. There is no style value —
  present, absent, or malformed — that changes what the target repository will
  accept.
- **SM-2 (no suppression channel).** No style value may cause tcode to emit a
  gate-suppressing flag or environment variable on the caller's behalf. This
  includes, non-exhaustively: `gh pr merge --admin`, `--force`/`-f` on a merge
  or push to a protected branch, `git commit --no-verify`, `git push
  --no-verify`, dismissing or bypassing a required review, and any
  `SKIP_*`/`*_SKIP`/`NO_VERIFY`-shaped environment variable read by a gate
  script. If such a plumbing path is ever introduced, it MUST NOT take a style
  value as an input.
- **SM-3 (the hard line — never turn red green by deleting coverage).**
  Reducing the **set of checks a style runs** is in scope for every style.
  Making a check that **would have failed** report success is out of scope for
  **every** style, including `hack`. Concretely, a style mode may never cause,
  and no implementation of a style may include:
  - deleting, `#[ignore]`-ing, skipping, or weakening an existing test in
    order to make a suite green;
  - narrowing an assertion so a real failure passes;
  - adding a row to a ratcheted allowlist (`.line-cap-allowlist.tsv`,
    `.doc-number-allowlist.tsv`, `.sld-lint-allowlist.tsv`,
    `.test-pointer-allowlist.tsv`) to silence a **new** violation — those
    ratchets may only shrink;
  - reporting a **skipped** check as a **passed** check (see SM-4).

  Note the asymmetry deliberately: "we did not run the integration suite" is a
  legitimate `vibe` outcome; "the integration suite is green" when it was not
  run, or was made green by deletion, is a defect at any style.
- **SM-4 (honest reporting).** A styled delegation's result MUST distinguish
  three states per check: **ran and passed**, **ran and failed**, **not run
  (style)**. "Not run" MUST NOT be rendered, summarized, or aggregated as
  "passed". A caller that asked for `vibe` is entitled to know exactly what it
  bought.
- **SM-5 (the floor).** Whatever gates the target repository enforces are run
  at the point that repository enforces them, at every style. A `hack`-styled
  change that reaches a PR faces exactly the same CI as an `engineer`-styled
  one. Style changes how the work was produced, never how it is judged.
- **SM-6 (style is not authority).** Consistent with DOC-41 §5.5's
  propose-not-authorize rule — "No `HandoffContext` field (§5.2) grants
  elevated authority to a callee; there is no mechanism by which a delegate
  can act as the user" — the style field is a `HandoffContext` field and
  therefore grants nothing. It selects ceremony; it confers no capability.
- **SM-10 (the process-enforced gate gets the same protection as the
  server-enforced ones).** Because the trusty-review gate is convention-
  enforced rather than CI-enforced (§3.1), a style value MUST NOT be
  admissible as justification for skipping it, and MUST NOT appear in any
  review-gate waiver rationale. "It was a `hack`/`vibe` task" is not a reason
  a review gate was not run; it is at most a reason the *work* carried less
  ceremony before reaching the gate. This rule exists precisely because SM-5's
  structural protection (§3.3) does not cover this gate.
- **SM-11 (style never touches the tool surface).** A style value MUST NOT
  widen, narrow, or otherwise modify which tools the executing agent may call,
  nor their permissions. Ceremony selection and capability grants are separate
  concerns; conflating them would turn a ceremony dial into a privilege dial.

### 3.3 Why the boundary holds structurally, not just by policy

The boundary is not merely asserted. A code audit of `crates/trusty-code`,
`crates/trusty-agents`, `crates/trusty-mpm`, `scripts/`, and
`.github/workflows/` against `origin/main` found **no path by which any
caller-supplied parameter suppresses any repository-enforced gate**, and four
independent structural reasons why:

1. **The gates are server-side.** Every check listed in §3.1 except the
   trusty-review gate is a GitHub Actions workflow triggered by
   `pull_request`/`push` against `main` and evaluated on GitHub's runners from
   a fresh `actions/checkout@v5`. A parameter travelling inside a local
   delegation payload has no representation in that computation. The only way
   to change such a gate's verdict is to change the tree it runs against —
   which is the honest path. The gate design is explicitly anti-bypass:
   `.github/workflows/changelog-fragment.yml` deliberately carries **no**
   `paths:` filter ("a required check that is skipped by a path filter never
   reports on the PRs it skips"), and `scripts/check_changelog_fragment.sh`
   states there is "deliberately NO 'trivial change' escape hatch".
2. **The ratchets can only shrink.** The allowlist-backed gates
   (`check_line_cap.sh`, `check_doc_numbers.sh`, `check_sld.sh`,
   `check_test_pointers.sh`) fail on a **stale** allowlist row as well as on a
   new violation. Grandfathering a fresh violation is itself a red build, so
   "add an allowlist row" is not an available shortcut for any style.
   `check_line_cap.sh`'s `--seed`/`--force-add` flags only regenerate the
   tracked `.line-cap-allowlist.tsv` — a reviewable diff — and CI invokes the
   script with no flags.
3. **The delegation is propose-only, structurally.** `ProposalEnvelope`'s only
   constructor, `for_cross_product`
   (`crates/trusty-agents/src/tools/cross_product.rs:259-273`), hardcodes
   `disposition: Disposition::Proposal` at line 270. Its own doc comment says
   making this the sole constructor "is what makes DOC-41 §5.5's absolute rule
   **structural rather than a convention** a future edit could forget — there
   is no code path through this type that yields `Disposition::Action`,
   regardless of `authority`." **This is the model a style field should
   follow**: a recorded, non-authorizing field, with the non-authorization
   enforced by construction rather than by discipline.
4. **The tcode pipeline never reaches the merge surface at all.** No Rust code
   in the workspace constructs an argv containing `--admin`, `--no-verify`,
   `gh pr merge`, or `git push`; the only `--admin` occurrences are agent
   prompt text *forbidding* it (e.g.
   `crates/trusty-code/src/assets/agents/BASE-AGENT.md:56`). trusty-agents'
   GitHub surface (`crates/trusty-agents/src/tools/gh_tools/`) is "read-only by
   construction" — inspection subcommands only — and a test asserts no tool
   name contains `merge`/`create`/`comment`/`close`/`edit`/`rerun`/`approve`
   (`gh_tools/tests.rs:125,145`). tcode terminates at a **diff**: it snapshots
   trees into a throwaway index via `GIT_INDEX_FILE`
   (`crates/trusty-code/src/run_task/diff.rs:168-175`) and reports a
   `RunReport` (`run_task/mod.rs:820-829`). The PR/merge step is a separate
   action outside tcode entirely.

Two further audit results bear directly on open questions 2 and 3, and are
recorded here so #2596 starts from fact rather than assumption:

- **The verify gate is attached unconditionally at all three production
  construction sites** — `crates/trusty-code/src/runner/in_process.rs:420`,
  `run_task/mod.rs:428`, `task/executor.rs:491` — as bare
  `.with_finish_gate(...)` calls in a builder chain, with no `if`, no
  `Option`, and no config read. Its only inertness condition is
  **content-derived**: `verify_gate.rs:187` returns early when the seed text
  names no test command. `TaskRunParams`
  (`crates/trusty-code/src/task/executor.rs:94-118`) carries no field any
  `with_finish_gate` call consults. **Making `vibe` skip verification is
  therefore new plumbing, not a flag flip** — which is exactly why it needs
  #2596's ratification rather than this spec's assertion.
- **trusty-code has no circuit breaker at all.** Circuit breakers live in
  trusty-mpm (`crates/trusty-mpm/src/core/circuit.rs`), checked
  unconditionally in `daemon/mcp_backend.rs:232-238` from a hardcoded
  `CircuitConfig::default()` (`daemon/state/core.rs:441,511`). Open question 2
  ("does vibe skip circuit breakers, or run a reduced set?") is therefore
  **not currently a question about trusty-code** — there is nothing there to
  skip. Answering it as posed requires first deciding which product's breakers
  are in scope. §9 OQ-5.

Finally, epic #4345 decision 2 keeps the surface narrow independently: all
coding delegates through the tcode PM only, and `NON_CODING_TARGETS`
(`cross_product.rs:69`, `&["research", "ticketing"]`) is a closed literal the
bridge enforces "regardless of caller configuration". A style value therefore
lands inside a pipeline that already applies its own gates. Decision 3's
stated reasoning survives contact with the code.

**One thing the spec states rather than assumes away.** The bash tool is an
unrestricted `sh -c` with no command denylist
(`crates/trusty-code/src/tools/bash/mod.rs:155`; the only gate is
`restricted_tiers()` at `:135-137`). A *model* can therefore author any shell
string, including one this section forbids. That is a pre-existing property of
the tool surface, is not reachable from any parameter, and would not be
widened by a style value — SM-11 makes that explicit. It is recorded here
because "no parameter can reach it" and "nothing can reach it" are different
claims, and only the first is true.

### 3.4 The reporting contract

A styled delegation returns, in addition to whatever result payload #4348
defines:

- the **effective style** actually applied, and the **resolution path** that
  produced it (§5.3) — so a caller can see when its request was overridden or
  degraded;
- a **gate ledger**: for each check the pipeline knows about, one of
  `passed` / `failed` / `not-run(style)`;
- an explicit statement when the effective style differs from the requested
  style, with the reason (unsupported value, unimplemented tier per §7.3,
  or escalation).

SM-4 makes this contract normative rather than nice-to-have: without it,
"fewer gates" and "green" become indistinguishable to the caller, which is
precisely the failure mode SM-3 exists to prevent.

---

## 4. The Three Styles, Per Axis {#SPEC-STYLE-04~draft}

**ID:** SPEC-STYLE-04~draft
**Status:** Draft

Every cell below describes **ceremony**. Per SM-1/SM-5, no cell may be read as
relaxing anything in §3.1's gate list.

| Axis | `hack` (QUICK OPS) | `vibe` (VIBE) | `engineer` (FULL LOOP) |
|---|---|---|---|
| **Intended shape of work** | Answerable in fewer than ~3 tool calls; a read, a lookup, a one-line answer. Not a code change. | A small, self-contained code change whose blast radius the caller believes is local. | Anything else, and everything the caller is unsure about. |
| **Spec artifacts** | None. | None required. No spec document, no design doc. | Spec or design artifact where the work is spec-governed (DOC-38 §4); otherwise a written plan. |
| **Ticketing** | None. | None required. | Issue exists and is referenced (`Closes #N`). |
| **Branch/worktree** | N/A — no change produced. | May work directly from `main` per vision spec §5.10 (subject to §4.1). | Dedicated worktree + branch; never the main checkout. |
| **Testing** | None. | Advisory: run what is fast and relevant; **not** required to run the full suite. Failures that ARE observed are reported as failures (SM-3/SM-4). | Full suite, with observed raw output. Blocking. |
| **Verification** | None. | Deferred to #2596 — see §7. Fail-safe until then: §7.3. | Mandatory verification gates and circuit breakers, as today. |
| **Review** | None. | Advisory. | trusty-review gate, per the delivery chain. |
| **Commit discipline** | N/A. | Conventional commits still apply; atomicity advisory. | Conventional commits; atomic; issue reference in body. |
| **PR expectations** | N/A. | A PR is still a PR: it faces the same CI, the same changelog requirement, the same SLOC cap. `vibe` buys less pre-PR ceremony, never a cheaper PR. | Full PR discipline. |
| **Docs** | None. | Advisory. | Per-package changelog entry and any user-visible doc updated in the same PR. |

### 4.1 Two clarifications the matrix cannot carry

**"Work directly from `main`" is a ceremony statement about worktrees, not a
licence to push to `main`.** Vision spec §5.10 defines VIBE as "work directly
from `main`" — meaning the tier does not require the cut-a-worktree,
cut-a-branch ceremony before starting. Branch protection on `main` is a gate
(§3.1). Under SM-5, a `vibe` change that ends in a PR is subject to branch
protection exactly as an `engineer` change is. Where this repository's own
worktree discipline applies, it applies at every style.

**`hack` is not "coding with no gates".** `hack` maps to QUICK OPS, which
vision spec §5.10 defines as work "achievable in fewer than ~3 tool calls;
trivial, no ceremony" — the classifier replies directly, with no pipeline. It
is the *absence of a code change*, not a code change with the checks removed.
A delegation that asks for `hack` and then turns out to require a code change
MUST escalate (§5.4), not proceed unchecked. Without this rule `hack` becomes
the gate-suppression channel SM-2 forbids, wearing a different name.

---

## 5. Where Style Lives, and How It Resolves {#SPEC-STYLE-05~draft}

**ID:** SPEC-STYLE-05~draft
**Status:** Draft

### 5.1 The value

`ExecutionStyle` is a closed enum with exactly three variants — `hack`,
`vibe`, `engineer` — serialized lowercase. It is closed deliberately: an open
string field is a place for a fourth, undocumented tier to appear without a
decision. Epic #4345 decision 1 forbids inventing a new axis; a closed enum is
how that decision is enforced mechanically rather than by convention.

An unrecognized value is a **recoverable error** returned to the caller, never
a silent fallback. Silently mapping an unknown style to a default is
indistinguishable, from the caller's side, from the style having been honored.

### 5.2 The three places it can come from

1. **Per-delegation parameter** — supplied by the caller on the delegation
   call. Epic #4345 decision 3 authorizes this explicitly.
2. **Configuration default** — a per-assistant (and/or global) default. No
   such field exists anywhere today; #4350 introduces it.
3. **Built-in default** — used when neither of the above supplies a value.

**The built-in default is `engineer`.** This is the fail-safe direction: the
most ceremony, matching today's behavior exactly (every `Implementation`-class
task receives the full gated pipeline). Backward compatibility for #4349 is
therefore literal — a `None` style is byte-equivalent in behavior to today.

### 5.3 Precedence (NORMATIVE)

```
caller parameter  >  configuration default  >  built-in default (`engineer`)
```

Resolution is first-match-wins down that list, and the **resolution path** is
reported back per §3.4 so the caller can see which level supplied the value.

### 5.4 Escalation is permitted; de-escalation is not

The tcode PM MAY apply **more** ceremony than the resolved style requests. It
MUST NOT apply **less**.

This asymmetry is deliberate and it resolves a real tension with this
repository's own captured research. `docs/research/quality-gates-agent-prs-article-2026-04.md:122`
states the principle directly:

> "Classification must be **automatic**, derived from the semantic diff plus
> the blast radius/dependency graph — **not** from author tags, since an agent
> has no self-awareness of how risky its own change is."

A caller-supplied style *is* an author tag. Treating it as a **ceiling
request that may be raised but never lowered by the callee** keeps decision
3's ergonomics (a caller can ask for less ceremony) while honoring the
research finding (the party with the diff in front of it, not the party
writing the request, gets the final say on risk). The same source frames the
goal correctly for this feature: "the fix is smarter routing, not a weaker
mesh" (`:120`).

What triggers automatic escalation — size, blast radius, touched paths — is a
routing question inside tcode's dispatch and belongs to #2596 (§7.2).

---

## 6. Cross-Boundary Carriage: Preamble, Not System-Prompt Composition {#SPEC-STYLE-06~draft}

**ID:** SPEC-STYLE-06~draft
**Status:** Draft

> This section answers epic #4345 open question 4.

### 6.1 Decision

**A text preamble is sufficient and is the specified mechanism.** Style and the
TA-PM-policy block travel as structured fields on `HandoffContext`, rendered
into the existing preamble text by `render_preamble()` and prepended to the
task description that crosses into tcode. Literal per-product system-prompt
composition is **not** required for this epic and MUST NOT be made a
prerequisite for #4349/#4350.

### 6.2 Evidence

Epic #4345 called a preamble "the cheap, already-precedented path". The code
says something stronger: **it is not merely the cheap path, it is the only
path that exists.**

1. **The boundary is one argv slot.** `ProcessPmBridge::run_tcode`
   (`crates/trusty-agents/src/tools/pm_bridge_backend.rs:134-148`) spawns
   `tcode run-task <agent> <task> --project <dir> --json` — five argv slots,
   one of which is the task string. There is no slot for a system prompt, a
   persona, a style, or any structured context. The backend trait itself
   (`pm_bridge_backend.rs:73`) is
   `run(&self, route: BridgeRoute, target: Option<&str>, task: &str)` — three
   parameters, one a bare string. `DEFAULT_TCODE_AGENT = "pm"`
   (`pm_bridge_backend.rs:84`), consistent with decision 2.
2. **Flattening to text is the existing, documented design.**
   `HandoffContext::render_preamble()`
   (`crates/trusty-agents/src/tools/cross_product.rs:144-174`) renders
   `summary` / `relevant_state` / `constraints` into a plain-text
   `"Context handed to you:\n- …"` block. Its doc comment
   (`cross_product.rs:136-139`) states the rationale verbatim: *"the external
   CLI leg accepts one task string and nothing else, so a structured handoff
   must be flattened to cross the process boundary. Plain text (not JSON)
   because the receiving side is an LLM persona, not a parser."*
3. **The join is a string prepend.** `crates/trusty-agents/src/tools/pm_bridge.rs:254-257`:
   `format!("{preamble}\n{task}")`, with the result passed as the single task
   argument (`pm_bridge.rs:265`). Cap validation happens **before** dispatch
   (`pm_bridge.rs:232-237`), and the target is resolved against the fail-closed
   allow-set (`pm_bridge.rs:243-252`) before `backend.run`. Adding a style
   field extends a validated, ordered path rather than creating a new one.
4. **The 4 KiB cap already exists and is fail-closed.**
   `HANDOFF_MAX_BYTES = 4096` (`cross_product.rs:50`), enforced by
   `validate()` (`cross_product.rs:108-114`) over `serde_json::to_vec`, with a
   serialization failure treated as `usize::MAX` (over cap). #4349's cap
   requirement is therefore already satisfied by the mechanism it extends.
5. **System-prompt composition across products does not exist.** DOC-61 §4
   describes per-product builders as a proposal, and concedes the compose
   chain is *"not yet universally shared: today, only trusty-mpm's own builder
   actually resolves `extends:`"* (`DOC-61-canonical-agent-standard.md:272-275`)
   and that the versioning work is *"new work, not already built"* (`:298-302`).
   The one real cross-product precedent — trusty-code ingesting trusty-mpm's
   frontmatter parser via `plugins/agents.rs` — is a **config projection, not
   a system-prompt composition**, and Phase 1 locks plugin agents as leaves
   with `extends:` uncomposed.
6. **`run-task` has no style parameter today, on either side.** CLI:
   `crates/trusty-code/src/main.rs:144-188` (`agent`, `task`, `--project`,
   `--json`, `--engineer-model`, `--legacy-in-process`, `--mode`,
   `--timeout-seconds`). Wire: `crates/trusty-code/src/cli/run_task.rs:69-75`
   sends exactly `task_description`, `agent_name`, `model_override`, `mode`,
   `deadline_secs`. Server: `crates/trusty-code/src/task/protocol.rs:98-126`
   adds `context?`, `session_id?`, `project?`, `workstream_id?` — still no
   ceremony tier. `mode` is `HarnessMode` only
   (`crates/trusty-code/src/mode.rs:58-60`), confirming §2.2's non-conflation
   requirement is a live risk worth restating.

**The code does not contradict the issue's leaning; it strengthens it.**
Choosing system-prompt composition would mean building DOC-61 §4's unbuilt
fan-out as a prerequisite for #4349 — a large, separately-specified piece of
work — in order to deliver something the existing capped, tested, byte-
identical-when-absent channel already carries. Tests already pin the
preamble's behavior in both directions
(`handoff_renders_into_the_task_preamble`, `empty_handoff_renders_nothing`).

### 6.3 The constraint that makes the preamble ruling safe

A preamble is **text a model reads**. It is advisory by construction: nothing
mechanically prevents a model from disregarding it. That is acceptable for
what §6.1 asks it to carry — a ceremony level and a statement of policy — and
unacceptable for anything security-relevant.

Therefore, NORMATIVE:

- **SM-7.** The policy preamble MAY carry: the effective style, the
  human-readable meaning of that style, decision 2's PM-only statement as
  *information*, and the §3 boundary as *instruction*.
- **SM-8.** The policy preamble MUST NOT be the sole enforcement of any
  security control. Specifically, the `NON_CODING_TARGETS` floor
  (`crates/trusty-agents/src/tools/cross_product.rs:69`) stays a
  **code-enforced** closed literal, applied "regardless of caller
  configuration", and is not weakened, supplemented, or restated-in-lieu-of by
  preamble text. #4126's protection is a code property; a paragraph telling a
  model to behave is not a substitute for it, and a delegation payload derived
  from untrusted content (live Gmail/Drive) is exactly the case where advisory
  text is worth least. The same reasoning applies to SM-1 through SM-6: the
  preamble may *state* them, but AC-5 and AC-6 (§8) are what *enforce* them.

The distinction is the same one §3 draws: the preamble is ceremony carriage,
not a gate. It tells the callee what kind of work this is. It never widens what
the callee is allowed to do.

### 6.4 Consequence for #4349

#4349's acceptance criteria are satisfiable within the existing 4 KiB
`HandoffContext` cap and require no new prompt-composition machinery. The
policy block should be short, fixed-length, and independent of the task text
so the cap is not a function of task size. A `None` style renders no policy
block at all, preserving byte-identical behavior for existing callers —
`render_preamble()` already returns `None` for an empty handoff
(`cross_product.rs:145-147`), so the backward-compatibility requirement is met
by the shape of the existing code rather than by a new branch.

Two shaping notes for the implementer:

- The style field is **structured on `HandoffContext`, flattened only at
  render time**. Do not have callers hand-write style prose into `summary` or
  `constraints`: a typed field is what makes AC-4 and AC-8 testable, and what
  keeps the GUI (#4353) and the wire in agreement.
- The policy block is emitted **after** the existing `Summary` /
  `Relevant state` / `Constraint` lines and in a distinguishable form, so a
  caller-supplied `constraints` entry can never be mistaken for — or forge —
  the policy block. Caller text and policy text arriving in the same string is
  precisely why SM-8 refuses to let the preamble be a security control.

---

## 7. Relationship to #2596: What This Spec Consumes vs. Defines {#SPEC-STYLE-07~draft}

**ID:** SPEC-STYLE-07~draft
**Status:** Draft

### 7.1 Ruling

Epic #4345's open questions 1–3 are **inherited verbatim** from issue #2596:

1. What heuristic routes vibe vs engineer (size? explicit flag? keyword detection)?
2. Does vibe skip circuit breakers entirely, or run a reduced set?
3. Does vibe require any verification at all?

**This spec CONSUMES those answers; it does not define them.** They are VIBE
**tier** decisions, not style-**mode** decisions. The dividing line:

| Question | Owner | Why |
|---|---|---|
| What `vibe` is **called**, how it is **requested**, how it **resolves**, how it **travels**, what it may **never** do | This spec (DOC-62) | Interface semantics at the trusty-agents → trusty-code boundary. |
| What `vibe` **does inside the pipeline** — which circuit breakers, which verification steps, what heuristic classifies an unstyled task | #2596 | Internal to `crates/trusty-code/src/intent/` dispatch. Amending the always-on-gates decision requires explicit owner ratification (vision spec §10 D3). |

Absorbing #2596's questions into this document would relocate a ratification
decision away from the issue that tracks it, and would make this spec the de
facto owner of trusty-code's internal pipeline — which it is not.

**Two facts from §3.3's audit that #2596 should start from**, because both
change the shape of its questions:

- Question 2 ("skip circuit breakers entirely, or a reduced set?") is posed
  against a construct that **does not exist in trusty-code**. Circuit breakers
  live in trusty-mpm (`crates/trusty-mpm/src/core/circuit.rs`, enforced at
  `daemon/mcp_backend.rs:232-238`). #2596 must first say which product's
  breakers it means; if the answer is trusty-code's, the question is "should
  VIBE have breakers at all", not "should it skip them". See §9 OQ-5.
- Question 3 ("does vibe require any verification?") is posed against a gate
  attached **unconditionally at all three production sites** with no parameter
  input (`runner/in_process.rs:420`, `run_task/mod.rs:428`,
  `task/executor.rs:491`). Making VIBE verify less is new plumbing, not a flag
  flip — which is consistent with vision spec §10 D3 requiring explicit owner
  ratification to amend the always-on-gates decision.

### 7.2 Scoped hand-back for question 1

Question 1 has two halves that were tangled together in #2596's phrasing:

- **(a) How a style is selected at the interface** — explicit caller flag,
  config default, built-in default, precedence. **Answered here**, §5.
- **(b) How tcode routes when no style is supplied, and what triggers
  automatic escalation (§5.4)** — size, semantic diff, blast radius, keyword
  detection. **Remains #2596's**, and §5.4 records the constraint any answer
  must satisfy (automatic classification may raise ceremony, never lower it
  below the resolved style).

With (a) answered, #2596's remaining question 1 is narrower and better posed:
not "how do we pick a tier" but "when does the callee overrule the caller".

### 7.3 Fail-safe: this chain is NOT blocked on #2596

VIBE is unimplemented (#2596 open; vision spec §5.10 marks it "❌ Not
implemented"). That must not block #4349/#4350, which are carriage and
addressability work.

**NORMATIVE — SM-9 (degrade upward, and say so).** Until #2596 lands, a
resolved style of `vibe` MUST execute the `engineer` pipeline and MUST report
an effective style of `engineer` with reason `tier-unimplemented`, per §3.4.

- Degrading **upward** (to more ceremony) is the only safe direction; the
  alternative — silently accepting `vibe` and running less — would ship the
  reduced tier without the ratification vision spec §10 D3 requires.
- Reporting it is what distinguishes this from a silent no-op. A caller must
  never believe it received a lighter tier that does not exist.

The consequence is that #4349, #4350, and #4353 can ship the complete style
vocabulary, plumbing, and UI with `vibe` behaving as `engineer` and saying so,
and #2596 later changes behavior only — no interface change, no re-plumbing.

---

## 8. Acceptance Criteria {#SPEC-STYLE-08~draft}

**ID:** SPEC-STYLE-08~draft
**Status:** Draft

Testable, and mapped to the implementing issues.

- **AC-1** (#4348) `ExecutionStyle` is a closed three-variant enum; an
  unrecognized value returns a recoverable error, never a silent default.
- **AC-2** (#4349) `HandoffContext` carries `style: Option<ExecutionStyle>`;
  `None` produces a preamble byte-identical to today's, and every existing
  call site compiles and behaves unchanged.
- **AC-3** (#4349) With a style set, `render_preamble()` emits a fixed-length
  policy block; total serialized `HandoffContext` remains ≤ 4 KiB for a
  maximal task within the existing cap.
- **AC-4** (#4350) Style resolves caller > config > built-in `engineer`, and
  the resolution path is present in the returned result (§3.4).
- **AC-5** (#4350) `NON_CODING_TARGETS` is unchanged by the diff; a test
  asserts the constant's exact membership so a future widening is a red build,
  not a review miss.
- **AC-6** (§3, SM-2) A repository-wide check finds no path by which a style
  value reaches a merge/push/commit flag or a gate-skip environment variable.
  The audit in §3.3 establishes this holds on `origin/main` today with no
  style field present; AC-6 is what keeps it true afterwards. Recommended
  shape: a test asserting no argv-construction site in `crates/trusty-code`
  or `crates/trusty-agents` contains `--admin`, `--no-verify`, `--force` (on a
  git/gh invocation), or `gh pr merge`, and that no style-typed value is an
  input to any `Command::new` argument list.
- **AC-7** (§3.4, SM-4) A styled result distinguishes `passed` / `failed` /
  `not-run(style)` per check, and a test asserts that a not-run check never
  serializes into a passed-shaped summary.
- **AC-8** (§7.3, SM-9) A `vibe` request today yields effective style
  `engineer` with reason `tier-unimplemented`, asserted by test.
- **AC-9** (§3.2, SM-11) The set of tools available to the executing agent,
  and their permissions, are byte-identical across all three style values for
  the same task — asserted by test, not by inspection.
- **AC-10** (§3.2, SM-6) `Disposition` remains `Proposal` for every style;
  a test asserts a styled `ProposalEnvelope` is indistinguishable in
  disposition from an unstyled one, extending the existing
  `cross_product_result_is_always_a_proposal` /
  `envelope_records_caller_authority_without_upgrading_disposition` pair.
- **AC-11** (#4353) The GUI selector's labels and precedence match §4 and
  §5.3; a per-delegation override, where offered, is presented as a request,
  not a guarantee (§5.4).

---

## 9. Open Questions for the Owner {#SPEC-STYLE-09~draft}

**ID:** SPEC-STYLE-09~draft
**Status:** Draft — all six questions below RESOLVED by owner decision
2026-08-03; recorded inline, not deleted, so the reasoning trail survives.

Only genuine remaining forks are listed. Epic #4345's questions 4 and 5 are
**not** here: they are decided in §6 and §3 with evidence. Questions 1–3 are
**not** here either: §7 rules that they belong to #2596, and this document
consumes them.

- **OQ-1 — Default scope for the config default (§5.2).** Per-assistant,
  global, or both with per-assistant winning? #4353 needs this to draw the
  pane. Recommendation: **both, per-assistant wins**, because the style that
  suits `izzie` is not the style that suits a CTO assistant. Low reversibility
  cost either way.
  **RESOLVED (owner decision, 2026-08-03):** both, with per-assistant winning
  over global — the recommendation is adopted as-is. Unblocks #4353.

- **OQ-2 — Does `hack` belong on the delegation surface at all?** §4.1 argues
  `hack` is the absence of a code change; a coding delegation that resolves to
  `hack` is arguably a mis-routed task the intent classifier should have kept
  as `Conversational`. Options: (a) accept `hack` and escalate per §4.1;
  (b) reject `hack` at the delegation boundary as a category error.
  Recommendation: **(a)**, because rejecting it makes the enum non-uniform
  across the three surfaces (config, delegation, GUI) for no safety gain —
  §4.1's escalation rule already closes the hole. Flagged because it is a
  product-vocabulary call, not a technical one.
  **RESOLVED (owner decision, 2026-08-03):** yes — `hack` stays on the
  delegation surface; accept and escalate per §4.1's existing position.

- **OQ-3 — The `engineer` name collides with the tcode `engineer` sub-agent
  role** (`crates/trusty-code/src/assets/agents/engineer.md`), noted in
  decision 1 but not resolved. Once the tcode PM is addressable by name
  (#4350), a user will plausibly read `style=engineer` as "delegate to the
  engineer agent" — which decision 2 forbids. Recommendation: keep the value
  `engineer` (decision 1 fixed it) and disambiguate in the **GUI label and
  preamble wording** rather than the wire value. Owner confirmation wanted
  because the mitigation is presentational and therefore easy to lose.
  **RESOLVED (owner decision, 2026-08-03):** keep the wire value `engineer`;
  disambiguate in the GUI label and preamble only, per the recommendation.

- **OQ-4 — Should the gate ledger (§3.4) be structured or prose?** Structured
  is enforceable by AC-7 and consumable by the GUI; prose is cheaper and
  matches today's `Session`-snapshot return shape (`cli/run_task.rs:123-128`
  prints only the final `Session` snapshot). Recommendation: **structured**,
  because SM-4 is the rule most likely to erode silently, and a prose summary
  is exactly where "not run" quietly becomes "fine".
  **RESOLVED (owner decision, 2026-08-03):** structured, enforceable by AC-7
  and GUI-consumable, per the recommendation.

- **OQ-5 — Which product's circuit breakers does #2596's question 2 refer
  to?** The audit (§3.3, §7.1) found trusty-code has **no circuit breaker**;
  they exist only in trusty-mpm. This is a fact the owner should see before
  #2596 is answered, because it changes that question from "should VIBE run a
  reduced set" to "should trusty-code have breakers at all, and if so does
  VIBE get them". No recommendation offered — this is #2596's call, and it is
  flagged here only because #4345 inherited the question in a form that
  presumes a construct that does not exist.
  **RESOLVED (owner decision, 2026-08-03) on fact, not preference:**
  trusty-mpm's circuit breakers — trusty-code has none at all, per the code
  audit already recorded in DOC-62 §3.3.

- **OQ-6 — Should a per-delegation style override be exposed in the GUI at
  all, given §5.4?** #4353 lists a delegation-level override as a "product
  decision on priority". §5.4 makes any caller-supplied style a request the
  callee may raise. A UI control whose value the system may silently overrule
  is a known source of mistrust. Recommendation: **expose it, but render the
  effective style and its resolution path (§3.4) in the result**, so the
  override is honest rather than decorative. Owner call because it is a
  product-trust decision, not a technical one.
  **RESOLVED (owner decision, 2026-08-03):** expose it, and always render the
  effective style plus its resolution path so an overruled request is
  visible rather than silently ignored, per the recommendation.

---

## 10. References and Change Log {#SPEC-STYLE-10~draft}

**ID:** SPEC-STYLE-10~draft
**Status:** Draft

### 10.1 References

- Epic [#4345](https://github.com/bobmatnyc/trusty-tools/issues/4345) — decisions 1–3 and the five open questions this document folds in.
- Issue [#4346](https://github.com/bobmatnyc/trusty-tools/issues/4346) — the ticket this document discharges.
- Issue [#2596](https://github.com/bobmatnyc/trusty-tools/issues/2596) — VIBE execution tier; owns §7's questions 1–3.
- Issue [#4126](https://github.com/bobmatnyc/trusty-tools/issues/4126) — prompt-injection protection behind `NON_CODING_TARGETS`.
- [ADR-0024](../adr/0024-subagents-in-process-only-assistants-communicate-not-delegate.md) — L0/L1 tier model; out-of-process tcode work stays in the cross-product `dispatch_task` lane.
- [DOC-38](./spec-linked-documentation.md) §4 — spec-document conventions this document follows.
- [DOC-41](./trusty-agents-eve-style-agents-spec.md) §5, §5.5 — `HandoffContext`, the 4 KiB cap, propose-not-authorize.
- [DOC-61](./DOC-61-canonical-agent-standard.md) §4 — source model vs per-product builder; §9 declarative-agents-only.
- [`docs/trusty-code/vision-and-architecture-spec.md`](../trusty-code/vision-and-architecture-spec.md) §5.10, §10 D3 — Execution Patterns; the `HarnessMode` non-conflation rule.
- [`docs/research/quality-gates-agent-prs-article-2026-04.md`](../research/quality-gates-agent-prs-article-2026-04.md) — risk-weighted routing; the author-tags finding cited in §5.4.
- [`CLAUDE.md`](../../CLAUDE.md) — the delivery chain and the repository's own gate list.

### 10.2 Change log

- **2026-08-01 (rev 1, draft)** — Initial draft (#4346). Folds in epic #4345's
  five open questions: questions 4 and 5 decided with code evidence (§6, §3);
  questions 1–3 routed to #2596 as a consumed dependency with a fail-safe that
  unblocks #4349/#4350 (§7).
