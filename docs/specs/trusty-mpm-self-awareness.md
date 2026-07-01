# DOC-28 — trusty-mpm Self-Awareness and Instruction-Load Verification

**Status:** Draft
**Subsystem:** trusty-mpm — identity / instruction pipeline; trusty-memory — prompt-facts
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-30
**Spec ID:** `SPEC-SELFAWARE-01~draft` … `SPEC-SELFAWARE-04~draft` (DOC-28)
**Builds on:** DOC-21 — Harness Understanding (`docs/specs/harness-understanding.md`);
[Three-Harness Architecture](../architecture/harnesses.md)
**Cross-ref:** `crates/trusty-mpm/README.md`, `crates/trusty-mpm/src/assets/output-styles/{trusty-mpm,trusty-mpm-research,trusty-mpm-teacher}.md`,
`crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md`, `crates/trusty-mpm/src/core/bundle.rs`,
`crates/trusty-mpm/src/core/bundle_all.rs`, `crates/trusty-mpm/src/core/output_style_deployer.rs`,
`crates/trusty-mpm/src/core/session_launch/settings.rs`, `crates/trusty-mpm/src/core/instruction_overrides.rs`,
`crates/trusty-mpm/src/core/doctor.rs`, `crates/trusty-mpm/src/daemon/doctor.rs`,
`crates/trusty-memory/src/prompt_facts.rs`, `crates/trusty-memory/src/tools/kg_ops.rs`; issue tracked
as the "self-awareness incident" (2026-06-30 session transcript)

> **Scope note.** This is a behavior-contract spec for making a trusty-mpm-launched (or
> trusty-mpm-adjacent) Claude Code session **know what it is running under, know where to look
> to confirm it, and know when that knowledge failed to load**. It does not change the
> delegation model, the session-control-plane wire protocol (DOC-26), or the harness-mental-model
> content (DOC-21) — it adds an identity layer and a verification layer on top of the existing
> instruction-assembly and `tm doctor` mechanisms.

---

## 1. Motivation

A Claude Code session was asked *"are you self-aware of the framework?"* and failed in four
confirmed, distinct ways:

- **F1 — No memory/search consultation.** The session did not call `trusty-memory`'s
  `get_prompt_context()` / `memory_recall`, nor `trusty-search`, to answer a question about its
  own identity. Instead it shell-probed: `pip3 show claude-mpm`, `which claude-mpm`. There is
  currently no instruction anywhere in the bundled prompt assets that tells a session *identity
  questions are answered from memory + a canonical doc, never from shell probing*.
- **F2 — No canonical pointer.** `crates/trusty-mpm/docs/` does not exist (confirmed:
  `ls crates/trusty-mpm/docs/` → `No such file or directory`). The identity of trusty-mpm is
  scattered across `crates/trusty-mpm/README.md` (lines 1–12, the "Harness role" paragraph),
  `docs/architecture/harnesses.md` (§"trusty-mpm — the Meta-Harness", lines 80–115), the
  output-style assets (`crates/trusty-mpm/src/assets/output-styles/*.md`), and
  `crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md`. None of these is positioned as *the*
  answer to "what is this framework" — none is a single, stable, machine-and-human-readable
  self-description a session can be pointed at.
- **F3 — Conflation with Python claude-mpm.** Because no asset in F2 explicitly disambiguates,
  the session described the wrong system: it conflated the Rust `trusty-mpm` (binary `tm`, the
  Meta-Harness / control plane specified in DOC-26) with the unrelated Python `claude-mpm`
  project (an output-style + agent-fleet layer for Claude Code). Every bundled instruction asset
  inspected (`trusty-mpm.md` output style, `BASE_SM.md`) states role and prohibitions but never
  states "this is the Rust binary `tm`, not the Python `claude-mpm` package" — there is nothing
  to contradict a model's prior trained-in association between "mpm" and the Python project.
- **F4 — Silent instruction-load failure with no detection.** Most critically: the session did
  **not** appear to be running under the trusty-mpm output style / instructions at all. The
  *global* `~/.claude/settings.json` `outputStyle` key pointed at `claude_mpm` (not `trusty-mpm`),
  which does not resolve to any file trusty-mpm deploys (`crates/trusty-mpm/src/core/bundle.rs`
  `OUTPUT_STYLES` only registers `trusty-mpm` / `trusty-mpm-teacher` / `trusty-mpm-research`), so
  Claude Code silently fell back to its plain default system prompt. Tracing the mechanism
  (`crates/trusty-mpm/src/core/session_launch/settings.rs:233-260` `write_output_style`) shows
  that trusty-mpm **only** writes a correct `outputStyle` value into the *project-local*
  `<project>/.claude/settings.json` when a session is launched through `prepare_session` (i.e.
  via `tm run`/`tm load`/`tm login`). A session started any other way (bare `claude`, an IDE
  integration, a stale global config left by a previously-installed different tool) is not
  touched by that writer, so a stale or wrong global `outputStyle` string silently wins and
  nothing in the running session — and nothing in the framework today — detects or reports the
  gap. `tm doctor` (`crates/trusty-mpm/src/daemon/doctor.rs`) currently probes
  `last-instructions.md`, agents, skills, memory, search, and worktrees, but has **no check for
  the actually-active output style / system prompt**.

This spec defines four requirements (R1–R4) that close these four gaps.

---

## 2. Scope and non-goals

### 2.1 Scope

- A canonical, bundled self-description document for trusty-mpm (R1).
- Instruction text — exact wording, exact target files — that makes identity questions route to
  memory + the canonical doc instead of shell probing (R2).
- A trusty-memory prompt-fact seed so the identity pointer is present in `get_prompt_context()`
  on every session, regardless of which repo palace is active (R3).
- A best-effort, layered detection mechanism — one deterministic/external check plus one
  behavioral/model-side check — for "did the trusty-mpm instructions actually load," surfaced
  through `tm doctor` and a session-visible signal (R4).

### 2.2 Non-goals

- Changing the delegation model, the PM/SM prohibition tables, or `BASE_PM.md`/`BASE_SM.md`'s
  non-overridable-floor mechanics (governed by their own existing conventions).
- Rewriting `docs/architecture/harnesses.md` or DOC-21 — this spec **links** to them as the
  deeper architecture reference (per R1) rather than duplicating their content.
- A general "framework health" dashboard — R4 extends the existing `tm doctor` report with one
  additional check; it does not redesign `DoctorReport`.
- Fixing the root cause of *why* a stale `claude_mpm` global setting existed (that is an
  operator-environment cleanup, not a framework behavior change). This spec makes the condition
  **detectable**, not impossible.

---

## 3. R1 — Canonical self-description doc {#SPEC-SELFAWARE-01~draft}

**ID:** SPEC-SELFAWARE-01~draft
**Status:** Draft

### Behavior Contract (WHAT)

- **Inputs:** none at runtime — this is a static bundled asset, analogous to the entries in
  `crate::core::bundle::OUTPUT_STYLES` and `crate::core::bundle::ALL`.
- **Outputs:** a Markdown file, `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md`, embedded at
  compile time via `include_str!` and registered as a new `BundledArtifact` in
  `crates/trusty-mpm/src/core/bundle_all.rs`'s `ALL` table with
  `rel_path: "docs/WHAT-IS-TRUSTY-MPM.md"` and `install: InstallPolicy::Overwrite` (the doc is
  framework-owned and must track upgrades, matching the policy already used for
  `instructions/INSTRUCTIONS.md`).
- **Preconditions:** none — the doc has no dependency on project state.
- **Postconditions:**
  - The file exists in the source tree at `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` and is
    indexed like any other repo file by `trusty-search` when the trusty-tools repo itself is
    indexed (self-hosting case).
  - After `tm install` (or any code path that walks `bundle::ALL`, e.g.
    `crates/trusty-mpm/src/bin/tm/commands/install.rs:327-345` `install_to`), the file is also
    present at `~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md` — a stable, repo-independent
    path so a session on *any* project driven by trusty-mpm (not just trusty-tools itself) can
    resolve the doc without depending on that project's own file tree.
  - The doc states, in the first three sentences: (a) trusty-mpm is a Rust crate at
    `crates/trusty-mpm/`, binary `tm` (also `trusty-mpmd`/`trusty-mpm-tui`/`trusty-mpm-telegram`);
    (b) it is the **Meta-Harness** / control plane per `docs/architecture/harnesses.md` — it
    manages multi-project sessions and delegates coding work to `trusty-code` (`tcode`), it does
    not execute code itself; (c) it is **not** the Python `claude-mpm` package — no relation,
    different language, different maintainers, different distribution (crates.io/Homebrew vs.
    PyPI).
  - The doc links to `docs/architecture/harnesses.md` (full three-harness architecture) and
    `docs/specs/trusty-mpm-alpha-1-control-plane.md` (DOC-26, the control-plane behavior
    contract) as the deeper references, rather than duplicating their content.
- **Error conditions:** none (static content; `include_str!` fails at compile time, not runtime,
  if the source file is missing — the same guarantee every other bundled asset already has).

### Rationale (WHY)

The gap (F2) is not "no information exists" — it is "no single, stable, canonically-pointed-at
answer exists." Four existing sources each carry partial identity content (README's "Harness
role" paragraph, `harnesses.md`'s full architecture, the output-style prose, `BASE_SM.md`'s one
sentence) but a session asked "what are you" has no instruction telling it which of these is
authoritative, and none of them is written as a direct, disambiguating answer to that exact
question. Following the pattern already established for output styles (`OUTPUT_STYLES`, deployed
via `output_style_deployer.rs`/`session_launch/settings.rs`) and general framework artifacts
(`bundle_all.rs`'s `ALL` table, deployed via `install_to`), a new bundled, versioned doc is the
established idiom for "content that must exist identically for every trusty-mpm install," not a
one-off ad hoc file. Placing the source under `crates/trusty-mpm/docs/` (new directory) rather
than inside `src/assets/` keeps it human-readable/reviewable as ordinary documentation while
still being embeddable via `include_str!` from `bundle_all.rs`, mirroring the existing split
between `crates/trusty-mpm/README.md` (human docs) and `crates/trusty-mpm/src/assets/*`
(embedded instruction assets) — this doc is deliberately both.

### Acceptance Criteria

- `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` exists and is non-empty.
- `cargo test -p trusty-mpm bundle` (extending `bundle_tests.rs`'s `bundle_table_is_complete`)
  asserts `ALL` contains an entry with `rel_path == "docs/WHAT-IS-TRUSTY-MPM.md"`.
- A test analogous to `install_writes_all_artifacts` asserts that after `install_to`, the file
  exists at `<framework_root>/docs/WHAT-IS-TRUSTY-MPM.md` with content identical to the embedded
  constant.
- The doc's text contains the literal substrings `"Rust"`, `"tm"`, `"tcode"`, and
  `"claude-mpm"` (the last inside an explicit disambiguation sentence, not incidentally).

### Implementing Modules

| Module | Role |
|--------|------|
| `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` (new) | The canonical self-description source text. |
| `crates/trusty-mpm/src/core/bundle_all.rs` | Registers the doc as a `BundledArtifact` in `ALL`. |
| `crates/trusty-mpm/src/bin/tm/commands/install.rs` (`install_to`) | Writes the doc to `~/.trusty-mpm/framework/docs/` on `tm install`. |
| `crates/trusty-mpm/src/core/paths.rs` (`FrameworkPaths`) | Resolves the `~/.trusty-mpm/framework` root the doc installs under. |

---

## 4. R2 — Identity/self-awareness protocol in instruction assets {#SPEC-SELFAWARE-02~draft}

**ID:** SPEC-SELFAWARE-02~draft
**Status:** Draft

### Behavior Contract (WHAT)

- **Inputs:** none — this is added instruction text, evaluated by the model at inference time
  like every other instruction in the assembled prompt.
- **Outputs:** when a user asks an identity/framework question (patterns: "what are you", "what
  framework is this", "are you self-aware", "is this claude-mpm", "what is trusty-mpm"), the
  session's response is grounded in (in this order): (1) `get_prompt_context()` /
  `memory_recall` against the active trusty-memory palace, then (2) the canonical
  `WHAT-IS-TRUSTY-MPM.md` doc (R1) — resolved either by reading the deployed copy at
  `~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md` or via `trusty-search` if the session is
  inside the trusty-tools repo itself.
- **Preconditions:** the session is running the trusty-mpm-bundled instruction assets (output
  style and/or `BASE_SM.md`/`BASE_PM.md`) — see R4 for what happens when it is not.
- **Postconditions:** the session's answer never conflates trusty-mpm with claude-mpm and never
  relies on `pip3 show`, `pip show`, `which claude-mpm`, or grepping `site-packages`/`dist-info`
  to answer an identity question.
- **Error conditions:** if memory recall and the canonical doc are both unavailable (e.g. no
  trusty-memory MCP configured, doc not found on disk), the session states that plainly rather
  than falling back to shell probing or guessing.

### Exact instruction text to add

A new subsection, **"Identity & Self-Awareness Protocol,"** added verbatim (parameterized only by
which SM/PM voice needs it) to two locations:

1. `crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md` — appended as a new `##` section
   after "## Trusty Tool Priority (Non-Overridable)" (i.e. still inside the non-overridable
   floor, so it cannot be silently dropped by an override file per the file's own stated
   invariant, lines 40–54).
2. `crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md` (and mirrored, trimmed, into
   `trusty-mpm-research.md` / `trusty-mpm-teacher.md`) — appended as a new `##` section after
   "## Allowed Tools".

Proposed text (identical body, inserted into both floors):

```markdown
## Identity & Self-Awareness Protocol (Non-Overridable)

When asked what this framework/system/tool is, whether it is "self-aware," or to explain its own
identity:

1. **Consult memory first.** Call `get_prompt_context()` (trusty-memory MCP) and/or
   `memory_recall` before answering. The active palace carries an `is_fact` triple identifying
   this framework (see docs/specs/trusty-mpm-self-awareness.md §5).
2. **Then consult the canonical doc.** Read `~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md`
   (or, inside the trusty-tools repo itself, `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` via
   `trusty-search`/direct read) for the authoritative description and the claude-mpm
   disambiguation.
3. **Never shell-probe for identity.** `pip3 show`, `pip show`, `which claude-mpm`, or grepping
   `site-packages`/`dist-info` are FORBIDDEN ways to answer an identity question — they interrogate
   the wrong (Python) ecosystem and cannot see this Rust binary at all.
4. **State the disambiguation explicitly when relevant.** This is `trusty-mpm` (binary `tm`), a
   Rust Meta-Harness / control plane. It is NOT `claude-mpm`, the unrelated Python project. If the
   two could plausibly be confused given the user's phrasing, say so.
```

### Rationale (WHY)

F1 and F3 are both instruction gaps, not capability gaps — the session already had
`trusty-search`/`trusty-memory` MCP tools available and chose not to use them for this class of
question, and nothing told it the Python/Rust ecosystems were distinct. Placing the fix in the
non-overridable floor (`BASE_SM.md`'s existing floor mechanic, and the output-style's core body)
rather than in an overridable section (`SM_INSTRUCTIONS.md`, `WORKFLOW.md`) ensures a
project-level override cannot accidentally silence self-identity correctness — this is treated
as a framework invariant, the same class of guarantee `BASE_SM.md` already gives the
delegation-only prohibition.

### Acceptance Criteria

- `BASE_SM.md` and all three `OUTPUT_STYLES` entries contain the literal heading
  `"Identity & Self-Awareness Protocol"`.
- A golden-text test (mirroring the existing `instruction_overrides.rs` test style — e.g.
  `identity_protocol_present_in_assembled_prompt`) asserts `resolve_pm_prompt`/the SM prompt
  assembler output contains the heading regardless of which project-level overrides are applied
  (proving it is in the non-overridable portion, analogous to `pm_deployed_replaces_body_but_keeps_base_floor`).
- The instruction text contains the literal forbidden-tool list (`pip3 show`, `which claude-mpm`)
  so a reviewer can grep for regressions.

### Implementing Modules

| Module | Role |
|--------|------|
| `crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md` | Adds the non-overridable SM-side identity protocol section. |
| `crates/trusty-mpm/src/assets/output-styles/trusty-mpm.md`, `trusty-mpm-research.md`, `trusty-mpm-teacher.md` | Adds the PM-side identity protocol section to each bundled style. |
| `crates/trusty-mpm/src/core/instruction_overrides.rs` (`resolve_pm_prompt`) | Existing assembler that guarantees the floor section survives project overrides; extended tests assert the new section too. |

---

## 5. R3 — Memory prompt-fact identity pointer {#SPEC-SELFAWARE-03~draft}

**ID:** SPEC-SELFAWARE-03~draft
**Status:** Draft

### Behavior Contract (WHAT)

- **Inputs:** a one-time (or idempotent, re-runnable) `kg_assert` MCP call:
  `kg_assert(palace: "<any palace>", subject: "trusty-mpm", predicate: "is_fact", object: "<identity
  statement, see below>", provenance: "DOC-28 self-awareness seed")`.
- **Outputs:** the triple becomes part of every palace's active KG; because
  `crate::prompt_facts::gather_hot_triples` (`crates/trusty-memory/src/prompt_facts.rs:170-200`)
  iterates **every registered palace** (not just the caller's active one) and
  `is_fact` is already a member of `HOT_PREDICATES` (`prompt_facts.rs:54-59`), the fact appears
  in the `"### Facts"` section of every subsequent `get_prompt_context()` call from any palace,
  with no code change to trusty-memory required.
- **Preconditions:** a running trusty-memory daemon with at least one palace registered (the
  seed call itself creates/uses whichever palace the seeding step targets — the palace choice is
  irrelevant to visibility per the above).
- **Postconditions:** `get_prompt_context()`'s "Facts" section contains a line whose object text
  states trusty-mpm's identity and points at the R1 doc path, e.g.:
  `- trusty-mpm (binary tm) is the Rust Meta-Harness / control plane, NOT the Python claude-mpm
  project; see crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md or
  ~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md`.
- **Error conditions:** if the seed was never run (fresh trusty-memory install with an empty KG),
  `get_prompt_context()` degrades to its existing behavior (omits the "Facts" section entirely,
  or reports "no project context found" if no hot triples exist at all) — this is the same
  degradation path that already exists today for any other missing prompt-fact; R3 does not
  change that contract, it only ensures the fact is seeded so the degradation path is not hit in
  practice.

### Seeding mechanism — when and how

The seed is **not** a trusty-memory code change. It is a **documented, one-time operational step**
run once per trusty-memory install/upgrade (analogous to how `kg_bootstrap`
(`crates/trusty-memory/src/tools/kg_ops.rs:372-390`) is already documented as a manual
re-seedable step in `palace_ops.rs:133`'s comment "can re-run `kg_bootstrap` manually if
needed"). Two triggering points, either sufficient on its own:

1. **Manual/operator step**, documented in `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` itself
   (R1) under a "Memory seeding" subsection: run `kg_assert` (via the trusty-memory MCP tool or
   `mcp__trusty-memory__kg_assert`) once after installing/upgrading trusty-memory, with the
   subject/predicate/object given above.
2. **Bundled bootstrap hook (preferred, future work — see §10):** extend
   `crates/trusty-mpm/src/core/session_launch/mod.rs`'s `prepare_session` (which already injects
   the per-project `trusty-memory` MCP server block, per DOC-26 §14) to call `kg_assert` once
   (guarded by an idempotency check — `kg_query(subject: "trusty-mpm")` first, skip if an
   `is_fact` triple already exists) the first time a trusty-mpm-managed session boots against a
   fresh palace. This spec specifies the *fact content and idempotency contract*; wiring the
   automatic call is deferred implementation work (§7 Phase 2), not required for R3's Draft
   acceptance.

### Rationale (WHY)

`get_prompt_context()` already exists precisely to solve "the model shouldn't have to discover
ambient facts via blind searches" (`prompt_facts.rs:1-16`) — it is the mechanism, not a new one.
The gap is purely a missing seed: no `is_fact` triple about trusty-mpm's own identity has ever
been asserted. Because `gather_hot_triples` reads across every registered palace unconditionally,
a single seed call (run against any convenient palace, e.g. a `trusty-tools` or
`session-manager` palace) is visible to every session regardless of which project palace it is
scoped to — this matches the incident scenario, where the failing session may not even have been
scoped to a trusty-tools-specific palace.

### Acceptance Criteria

- After running the seed `kg_assert` call against a test palace, a subsequent
  `get_prompt_context()` call from a **different** palace context contains the seeded fact text
  (proves the cross-palace visibility claim, exercised via a test in the style of
  `gather_hot_triples_skips_non_hot` / `rebuild_prompt_cache_reflects_writes`).
- `list_prompt_facts` (the existing inspection tool,
  `crates/trusty-memory/src/tools/mod.rs:58`) lists the seeded fact.
- The fact's object text contains both the R1 doc's canonical deployed path
  (`~/.trusty-mpm/framework/docs/WHAT-IS-TRUSTY-MPM.md`) and the disambiguation clause
  ("NOT the Python claude-mpm").

### Implementing Modules

| Module | Role |
|--------|------|
| `crates/trusty-memory/src/prompt_facts.rs` | Existing `HOT_PREDICATES`/`gather_hot_triples`/`build_prompt_context` — unchanged, reused. |
| `crates/trusty-memory/src/tools/kg_ops.rs` (`handle_kg_assert`) | Existing tool handler used to write the seed triple — unchanged, reused. |
| `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` (R1) | Documents the manual seed step and the exact `kg_assert` arguments. |
| `crates/trusty-mpm/src/core/session_launch/mod.rs` (`prepare_session`) | Future automatic-seed hook site (§7 Phase 2, deferred). |

---

## 6. R4 — Instruction-load self-verification {#SPEC-SELFAWARE-04~draft}

**ID:** SPEC-SELFAWARE-04~draft
**Status:** Draft

### Behavior Contract (WHAT)

Two independent, layered mechanisms — one external/deterministic, one internal/behavioral —
because (per the Rationale below) neither alone is reliable.

**(a) External check — `tm doctor` output-style verification (primary, deterministic):**

- **Inputs:** the resolved `outputStyle` value from the effective `.claude/settings.json` seen by
  a launched session (project-level if present, else global `~/.claude/settings.json`), and the
  set of style files actually present under the corresponding `output-styles/` directory.
- **Outputs:** a new `DoctorCheck` (name: `"output_style"`) added to `run_doctor`'s check list
  (`crates/trusty-mpm/src/daemon/doctor.rs:44-63`), reported as part of the existing
  `DoctorReport`.
- **Preconditions:** none beyond what other `tm doctor` checks already require (project dir
  optional, as with `check_instructions`).
- **Postconditions:**
  - `Ok`: the effective `outputStyle` string matches one of `bundle::OUTPUT_STYLES`' `id`s
    **and** the corresponding file exists on disk at the resolved `output-styles/` dir with
    non-empty content.
  - `Warn`: `outputStyle` key is absent entirely (Claude Code will use its own default; this is
    a valid, if unconfigured, state — same severity class as `check_instructions`'s "missing
    file" case).
  - `Fail`: `outputStyle` is present but its value does **not** match any known trusty-mpm style
    id (this is the exact incident condition: `outputStyle: "claude_mpm"`), or the file the id
    should resolve to is missing/empty on disk. The check message states the configured value,
    the list of valid ids, and the fix (`tm run`/`tm load` rewrites it correctly, or manually
    correct the `outputStyle` key).
- **Error conditions:** an unreadable `settings.json` (permissions, malformed JSON) is reported as
  `Fail` with the parse error included — never silently skipped, since a broken settings file is
  itself the kind of failure this check exists to surface.

**(b) Internal check — session-visible load acknowledgment (secondary, behavioral, best-effort):**

- **Inputs:** none — purely instructional.
- **Outputs:** the R2 non-overridable floor sections (`BASE_SM.md`, each output style) each carry
  a fixed, greppable **load marker** — a single distinctive line,
  `<!-- trusty-mpm-instructions-loaded: v1 -->` — placed at the very top of the floor section (so
  it survives every override branch in `resolve_pm_prompt`, per the existing
  `pm_deployed_replaces_body_but_keeps_base_floor` invariant). The session is instructed: *if a
  user or operator asks "did your instructions load" / "confirm trusty-mpm is active," restate
  this marker verbatim as part of the answer.*
- **Preconditions:** none.
- **Postconditions:** an operator (or an automated harness-adapter probe, per DOC-21 §2.4's
  tool-call-pattern precedent) can ask the running session to echo the marker and get a
  deterministic yes/no signal about whether *this specific session* has the floor text in its
  context.
- **Error conditions / fundamental limitation:** if the instructions never loaded at all (the
  exact F4 failure), there is **no instruction present to tell the model to state the marker** —
  a model cannot reliably self-report the absence of content it never received. This mechanism
  therefore only catches **partial** degradation (stale/truncated prompt, wrong-but-present
  style) when explicitly probed; it does **not** reliably catch **total** omission. Mechanism
  (a) is the one that catches total omission, because it inspects the launch configuration from
  outside the model's own context — see §9.

### Rationale (WHY)

The incident's actual failure (F4) was precisely the case mechanism (b) cannot see: `outputStyle`
was set to a *value that does not exist* (`claude_mpm`), so Claude Code silently used its plain
default and no instruction — including a hypothetical "please confirm you loaded" instruction —
was ever delivered to the model. Only an **external** check that inspects the *configuration*
(what string is in `settings.json`, what files exist on disk) rather than *asking the model*
can catch this class of failure deterministically, which is why `tm doctor` (already the
project's single "is this wired correctly?" collapse point, `daemon/doctor.rs:1-13`) is the
right home for the primary check — it already runs equivalent existence/content checks for
`last-instructions.md`, agents, and skills. The behavioral marker (b) is retained anyway because
it is cheap, catches a different (partial-degradation) failure class, and gives a human operator
a fast, no-tooling way to sanity-check a live session mid-conversation.

### Acceptance Criteria

- `run_doctor` returns 7 checks (up from 6); a new test `run_doctor_produces_seven_checks`
  replaces/extends `run_doctor_produces_six_checks`.
- Given a temp project dir with `.claude/settings.json` containing
  `{"outputStyle": "claude_mpm"}` and no matching file under `output-styles/`, the new check
  returns `CheckStatus::Fail` with a message containing `"claude_mpm"` and the valid id list.
- Given a temp project dir with `{"outputStyle": "trusty-mpm"}` and the matching file present,
  the check returns `CheckStatus::Ok`.
- Given no `outputStyle` key at all, the check returns `CheckStatus::Warn`, not `Fail`.
- `BASE_SM.md` and all three output styles contain the literal marker line
  `<!-- trusty-mpm-instructions-loaded: v1 -->` as the first line of their respective floor
  sections.

### Implementing Modules

| Module | Role |
|--------|------|
| `crates/trusty-mpm/src/daemon/doctor.rs` (`run_doctor`) | Adds the new `check_output_style` probe to the check list. |
| `crates/trusty-mpm/src/core/doctor.rs` (`DoctorCheck`, `DoctorReport`) | Existing types, reused unchanged. |
| `crates/trusty-mpm/src/core/bundle.rs` (`OUTPUT_STYLES`) | Existing registry of valid style ids/files, read by the new check. |
| `crates/trusty-mpm/src/core/session_launch/settings.rs` (`write_output_style`) | Existing writer whose output the new check validates; unchanged. |
| `crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md`, output-style assets | Carry the load marker in the non-overridable floor. |

---

## 7. Implementation sketch

| Phase | Scope | Files touched |
|-------|-------|----------------|
| **Phase 1 — R1 doc + bundling** | Write `WHAT-IS-TRUSTY-MPM.md`; register in `bundle_all.rs`; add bundle test. | `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md` (new), `crates/trusty-mpm/src/core/bundle_all.rs`, `crates/trusty-mpm/src/core/bundle_tests.rs` |
| **Phase 2 — R2 instruction text** | Add the Identity & Self-Awareness Protocol section (+ R4's load marker line) to `BASE_SM.md` and all three output styles; extend `instruction_overrides.rs` tests. | `crates/trusty-mpm/src/assets/sm_instructions/BASE_SM.md`, `crates/trusty-mpm/src/assets/output-styles/{trusty-mpm,trusty-mpm-research,trusty-mpm-teacher}.md`, `crates/trusty-mpm/src/core/instruction_overrides.rs` (tests only) |
| **Phase 3 — R4(a) doctor check** | Add `check_output_style` to `run_doctor`; update `DoctorReport` test expectations. | `crates/trusty-mpm/src/daemon/doctor.rs` |
| **Phase 4 — R3 memory seed** | Document the manual `kg_assert` seed step in the R1 doc; write the integration test proving cross-palace visibility. | `crates/trusty-mpm/docs/WHAT-IS-TRUSTY-MPM.md`, `crates/trusty-memory/src/tools/tests.rs` |
| **Phase 5 (deferred, future work)** | Automatic idempotent seed call from `prepare_session`. | `crates/trusty-mpm/src/core/session_launch/mod.rs` |

Each phase is independently mergeable; Phases 1–4 have no ordering dependency between them
(only Phase 5 depends on Phase 4's fact-content contract being settled).

---

## 8. Test / verification strategy

- **R1:** `cargo test -p trusty-mpm bundle` — extend `bundle_table_is_complete` and
  `install_writes_all_artifacts` (or equivalents) to cover the new artifact.
- **R2:** extend `crates/trusty-mpm/src/core/instruction_overrides.rs`'s test module with an
  assertion that the assembled prompt contains the new heading regardless of override branch
  (mirrors the existing `pm_deployed_replaces_body_but_keeps_base_floor` pattern). An equivalent
  test is added wherever the SM prompt assembler (`crates/trusty-mpm/src/core/sm/prompt.rs`, per
  DOC-21 cross-ref) has its own test module, asserting `BASE_SM.md`'s new section is present and
  non-overridable there too.
- **R3:** an integration test in `crates/trusty-memory/src/tools/tests.rs` seeds the fact via
  `handle_kg_assert` against palace A, then calls the `get_prompt_context` dispatch path scoped
  to palace B, and asserts the fact text is present — proving the cross-palace visibility this
  spec relies on.
- **R4(a):** unit tests in `crates/trusty-mpm/src/daemon/doctor.rs`'s existing `#[cfg(test)]`
  module (alongside `instructions_present_is_ok`/`instructions_missing_is_warn`) for the three
  new states (`Ok`/`Warn`/`Fail`) of `check_output_style`, using `tempfile::TempDir` fixtures the
  same way the existing checks do.
- **R4(b):** a static-content test asserting the marker line's exact text and position (first
  line of the floor section) in each of the four bundled assets — no runtime/behavioral test is
  possible for the "model restates it on request" half, which is inherently a manual/operator
  verification step (documented as such in the R1 doc).
- **Whole-spec smoke test:** manually reproduce the original incident — set
  `~/.claude/settings.json` `outputStyle` to a bogus value, run `tm doctor`, confirm `Fail` is
  reported with the correct diagnostic message; this is the acceptance replay of the actual
  reported failure.

---

## 9. Risks and limitations

- **R4(b)'s fundamental introspection limit (restated from §6).** A language model cannot
  reliably report the absence of instructions it never received — there is no code path by which
  "no instructions loaded" produces a self-generated warning, because the warning instruction
  itself would be part of what failed to load. This is why R4(a) (external, configuration-level)
  is the primary mechanism and is required for Draft acceptance; R4(b) is explicitly secondary
  and best-effort, and this spec does not claim it closes the total-omission case.
- **R4(a) does not cover every launch path.** The check inspects `.claude/settings.json` at the
  time `tm doctor` runs; a session already running under a *different* set of instructions than
  what `settings.json` currently states (e.g. settings changed after the session's process
  started) will not be caught retroactively — `tm doctor` reports the *current* configuration
  state, not a historical guarantee about any specific already-running session's actual system
  prompt.
- **R3's automatic-seed (Phase 5) is deferred.** Until Phase 5 lands, R3's fact only appears
  after a human runs the documented manual `kg_assert` step once. A fresh trusty-memory install
  with no operator having run that step will still hit the "no project context found" fallback
  for this specific fact (though not for other, unrelated hot facts).
  Fact content in R3 lives in the KG (mutable data), not in the compiled binary — an operator
  could edit or remove it via `remove_prompt_fact`; that is consistent with every other
  prompt-fact and is not treated as a regression here.
- **Global vs. project settings ambiguity.** `write_output_style` (§4/R2's precondition) only
  ever writes the *project-level* `.claude/settings.json`. A stale *global* `outputStyle` (the
  literal incident condition) is never touched by trusty-mpm at all in the current design — R4(a)
  detects this class of drift but does not prevent or clean it up. Whether trusty-mpm should ever
  write to the global settings file is out of scope for this spec (a global write has broader
  blast-radius implications for unrelated Claude Code sessions on the same machine, per the
  precedent in `session_launch/settings.rs:160-171`'s `clean_global_trusty_memory_hooks`, which
  already treats global-settings writes/cleanups as a deliberately separate, careful concern).

---

## 10. Open questions / future work

- Should Phase 5 (automatic prompt-fact seeding) run unconditionally on every `prepare_session`
  call (cheap idempotency check via `kg_query`) or only on `tm install`/first-run? Deferred to
  implementation.
- Should `tm doctor`'s new `output_style` check also validate the *global*
  `~/.claude/settings.json` in addition to the project-level file, given the incident's stale
  value was global? This would require deciding precedence semantics (Claude Code's own
  project-over-global resolution order) inside the check, which is additional scope beyond this
  spec's Draft acceptance bar.
- Should the R2 identity protocol additionally instruct sessions to proactively (not just
  on-demand) restate the R4(b) load marker once per session, in the first response? This would
  strengthen the behavioral signal (an operator watching the transcript would see it even
  without asking) at the cost of a small amount of every first-response token budget. Left as a
  follow-up rather than blocking this spec.
- A future t-code-as-overseer (DOC-21 §7) could run the R4(a)-style check *before* launching a
  session rather than after, turning `tm doctor` from a diagnostic into a launch gate. Out of
  scope here; noted as a natural extension.

---

## References

- [DOC-21 — Harness Understanding](./harness-understanding.md) — the harness mental model this
  spec's identity layer sits alongside.
- [DOC-26 — trusty-mpm alpha-1 control plane](./trusty-mpm-alpha-1-control-plane.md) — the
  control-plane behavior contract R1's canonical doc links out to.
- [Three-Harness Architecture](../architecture/harnesses.md) — the deeper architectural
  reference for the trusty-code / trusty-mpm / trusty-agents split R1's doc summarizes and links
  to rather than duplicates.
- `crates/trusty-mpm/README.md` — the existing partial self-description (lines 1–12) this spec
  consolidates and disambiguates, not replaces.
