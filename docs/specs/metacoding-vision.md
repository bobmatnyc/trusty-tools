# DOC-18 — Metacoding: the trusty-tools product north-star

**Status:** Draft
**Subsystem:** trusty-tools — product vision (cross-crate: trusty-memory,
trusty-search, trusty-mpm, trusty-review, trusty-analyze)
**Owner:** Product / Engineering (trusty-tools)
**Last-updated:** 2026-06-17
**Spec ID:** `SPEC-METACODING-01~draft` (DOC-18)
**Builds on:** DOC-17 — Autonomous Multi-Session Managed Harness Runner
(`docs/specs/harness-runner-vision.md`, the runner), DOC-16 — Interactive
Sessions TUI (`docs/specs/sessions-tui-interactive.md`, the operator surface),
DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`)
**Cross-ref:** the 5-service stack (`crates/trusty-memory/`,
`crates/trusty-search/`, `crates/trusty-mpm/`, `crates/trusty-review/`,
`crates/trusty-analyze/`); the Homebrew tap `bobmatnyc/trusty` and the
binary-release workflow (`.github/workflows/release.yml`); the metaharness
runner-core epic **#1045**; the interactive-TUI epic **#1272** / DOC-16; the
harness-runner vision **#1405** / DOC-17; the Telegram control surface
(`crates/trusty-mpm/src/telegram/`).

> **Scope note.** This is the **product vision** spec for trusty-tools. It names
> the north-star — **metacoding** — defines it precisely, states the end-to-end
> onboarding journey as first-class requirements (**ONB-1…ONB-3**), and maps
> each pillar of the metacoding process onto the delivery machinery that already
> exists or is in flight (DOC-16, DOC-17, #1045, #1272, trusty-review). It sits
> **above** those specs: they are *how* metacoding is delivered; this doc is
> *what metacoding is and why*. It implements nothing — the PR that carries it
> opens **no** Rust changes.

---

## 1. What metacoding is

**Metacoding** is a combination of **specification-driven** and
**ticket-driven** development layered onto the fluid "vibe" of AI coding — but
with a **rigorous process built in**: a prescribed workflow, automated code
reviews, and formal structure. Crucially, that enhanced process is **largely
managed for the user**. The operator declares intent; the system runs the
disciplined machinery underneath.

Stated as one sentence:

> **Metacoding = spec-driven + ticket-driven development + a managed, rigorous
> process (prescribed workflow + automated code review + formal structure) +
> the factory model (a multi-session managed agent fleet) — all of it run *for*
> the user.**

### 1.1 The vibe-coding → metacoding framing

"Vibe coding" is conversational, improvisational AI coding: the human nudges a
model in natural language and accepts what comes back. It is fast and fluid, but
it is also unstructured — there is no specification of record, no ticket trail,
no review gate, no reproducible process. Quality and coherence depend entirely
on the human keeping it all in their head, one conversation at a time.

Metacoding keeps the fluid, conversational *feel* of vibe coding but wraps it in
the rigor that real software needs: a spec says what is being built, a ticket
tracks the unit of work, a prescribed workflow sequences the phases, and an
automated reviewer gates the result before it lands. The difference is that the
rigor is **built in and managed** — the user is not asked to set up CI, write a
process doc, run a linter, or remember to open a review. The system does that.

> **Positioning.** Metacoding is **what vibe coding should have been** — the same
> approachable, intent-first surface, but with the discipline that makes the
> output trustworthy, reproducible, and safe to run unattended.

### 1.2 Why it suits non-technical users and multi-project operators

- **Non-technical users.** Because the rigor is managed — provisioning,
  instruction assembly, workflow sequencing, and the review gate are all
  autonomous — the user does **not** need to know how to configure a harness,
  wire up CI, or run a code review. They point at a repo and describe what they
  want. The process that a senior engineer would normally impose by hand is
  imposed *for* them.
- **Multi-project operators.** Because the production engine is a **multi-session
  agent fleet** (the factory model, §4) addressed through a single control
  surface (TUI / Telegram / Slack), one person can drive work across **many
  repositories simultaneously** without context-switching between local
  checkouts, terminals, and toolchains. Each project runs as a managed session
  in the fleet; the operator supervises the fleet, not the individual keystrokes.

---

## 2. The product north-star + onboarding journey

> ### N0 — One install, point at a repo, drive it from anywhere — and the rigor runs itself.
>
> A user installs the whole trusty-tools stack from Homebrew; the install
> provisions the five services together. The user points the system at a repo
> they have access to, runs a **meta connect**, and then drives the work from
> **Telegram, Slack, or the TUI**. Behind that simple surface, the full
> metacoding process — spec-driven + ticket-driven + prescribed workflow +
> automated review + the factory model — runs autonomously. **If the user has to
> assemble the process themselves, metacoding has failed.**

N0 inherits and extends DOC-17 §G0 ("the user has to do NOTHING to manage their
harness"): G0 makes a *single* harness autonomous; metacoding makes the *whole
product* — install, connect, fleet, process — autonomous and addressable from a
chat surface.

The onboarding journey is the spine of the product. Its three legs are
first-class requirements.

### ONB-1 — One Homebrew install provisions the five services {#SPEC-METACODING-01~draft}

A single Homebrew install brings up the metacoding stack: **trusty-memory**,
**trusty-search**, **trusty-mpm**, **trusty-review**, and **trusty-analyze** —
provisioned together, version-coordinated, ready to run.

- **Behavior contract.**
  - **Inputs:** a Homebrew install of the trusty-tools stack (tap
    `bobmatnyc/trusty`).
  - **Outputs:** all five services installed and runnable: the memory palace
    engine, the hybrid code-search daemon, the MPM harness runner + daemon, the
    PR-review service, and the code-analysis sidecar — with their MCP/daemon
    wiring in place.
  - **Preconditions:** prebuilt binaries exist and are current for every service
    on the target platform.
  - **Postconditions:** `brew install` (or a single stack formula) yields a
    working backplane that the TUI banner can confirm (DOC-16 §3.1: memory +
    search "active").
  - **Error conditions:** a missing/stale binary for any service degrades the
    stack — the offending service shows unreachable rather than the whole install
    failing — and is treated as a release-pipeline defect to fix (see below).

- **Prerequisite gaps (NORMATIVE for N0, tracked outside this doc).** The
  one-install promise is **not yet fully met**. Known release-pipeline gaps that
  gate ONB-1:
  - **trusty-search / trusty-memory / trusty-mpm / trusty-code binary releases are
    missing or stale** — they must be built, current, and published per release
    before the unified install is trustworthy.
  - **AL2023 build coverage** must be complete and green for every service that
    ships a Linux load-dynamic variant (the `*-x86_64-linux-al2023.tar.gz`
    asset, `.github/workflows/release.yml`).
  - **The trusty-memory `BINARIES` list** (the set of `[[bin]]` targets the
    release matrix packages — e.g. `trusty-memory trusty-bm25-daemon`) must be
    correct so the published archive contains every binary the service needs.

  These are **prerequisites**, not part of this vision's scope — but ONB-1 is not
  "done" until the release pipeline reliably produces current binaries for all
  five services on all supported platforms.

- **Effort: M (release-pipeline completion, not new product code).**

### ONB-2 — "meta connect": point at a repo and connect a managed harness

The user names a repository they have access to and runs a **meta connect**. The
runner provisions a managed harness against that repo — workspace, instruction
layers, agent hierarchy, output style, MCP wiring — autonomously (DOC-17 §G0).

- **Behavior contract.**
  - **Inputs:** a repo reference the user has access to (path / git URL / ref);
    optional project hints.
  - **Outputs:** a managed harness bound to that repo, provisioned end-to-end by
    the runner (the resolved manifest → materialized workspace + assembled
    instructions + composed agents, DOC-17 HR-1…HR-3).
  - **Preconditions:** the five services from ONB-1 are live; the repo is
    accessible.
  - **Postconditions:** the project appears as a managed session/project the
    operator can address (DOC-16 numbered list); content stays current against
    the catalog (DOC-17 HR-3).
  - **Error conditions:** inaccessible repo → surfaced inline, no harness
    created; unreachable catalog → fall back to compiled-in defaults, never block
    (DOC-17 §6).

- **Effort: M (builds directly on the DOC-17 runner core, #1045).**

### ONB-3 — Drive it via Telegram, Slack, or the TUI

Once connected, the operator drives the work from any of three **control
surfaces**: a **Telegram** bot, a **Slack** integration, or the interactive
**TUI** (`tm sessions tui`, DOC-16). All three address the same managed fleet.

- **Behavior contract.**
  - **Inputs:** operator messages/commands on a control surface (Telegram chat,
    Slack channel/DM, or TUI input box); session addressing (inline `/<n>` or
    filter-to-session, DOC-16 §5).
  - **Outputs:** intent delivered to the addressed managed session(s) via the
    managed send API; progress/summary streamed back to the surface (DOC-16 §4
    transcript model).
  - **Preconditions:** at least one managed harness exists (ONB-2); the surface
    is paired/authorized (Telegram pairing exists today; Slack is post-MVP, §5).
  - **Postconditions:** the operator supervises and steers the fleet from a chat
    surface or the TUI without touching a local checkout.
  - **Error conditions:** unpaired/unauthorized surface → setup prompt;
    non-managed session targeted → "not managed" notice (DOC-16 §5.2).

- **Status note.** The **TUI** (DOC-16) and **Telegram** bot
  (`crates/trusty-mpm/src/telegram/`, teloxide, `tm telegram` pairing) are the
  near-term surfaces. **Slack** is an explicit post-MVP / L-effort item
  (DOC-17 §4.3, §5.3; restated in §5 below).

- **Effort: TUI/Telegram — M (in flight); Slack — L (post-MVP).**

---

## 3. How metacoding works (the rigorous managed process)

Metacoding's value is that it imposes a senior-engineer-grade process *without
asking the user to run it*. The process has five pillars; each maps onto existing
or in-flight machinery.

| Pillar | What it means | Delivered by |
|---|---|---|
| **Spec-driven** | A specification of record states *what* is being built before code is written. | The `docs/specs/` behavior-contract discipline (this directory; DOC-13…DOC-18) + Spec-Linked Documentation (SLD); intent/method conformance (DOC-15). |
| **Ticket-driven** | Each unit of work is a tracked ticket; work is sequenced and traceable. | Ticketing integration (claude-mpm parity item; harness instruction layers carry ticket intent into each session). |
| **Prescribed workflow** | A fixed, multi-phase workflow (not ad-hoc chat) sequences each task. | The MPM 5-phase workflow + PM instruction floor assembled by the runner (DOC-17 §2.1, `assemble_system_prompt` / `prepare_session`). |
| **Automated code review** | Every change is reviewed by an automated reviewer before it lands. | **trusty-review** — LLM-backed PR review with search + analysis context, as a gate (§3.1). |
| **Factory model** | The work is produced by a multi-session managed agent fleet, run for the user. | The autonomous managed harness runner (DOC-17 / #1405 / #1045) + the interactive operator surface (DOC-16 / #1272). See §4. |

Supporting substrate under all five pillars: **trusty-memory** (durable context /
palace) and **trusty-search** (hybrid code search) — the backplane the TUI banner
confirms on startup (DOC-16 §3.1, D4) and that trusty-review draws context from.

### 3.1 The trusty-review gate

The **automated code-review** pillar is delivered by **trusty-review**: it
fetches a PR diff, retrieves code context from **trusty-search**, queries
**trusty-analyze** for complexity data, then calls an LLM to produce a structured
review verdict with actionable findings. In metacoding, this is the **gate** the
prescribed workflow runs every change through before it merges — the rigor that a
human reviewer would normally provide, made automatic and managed. It exposes a
CLI (`run`/`compare`), an HTTP webhook server, and an MCP stdio service for
in-harness use.

### 3.2 Where the pillars are assembled

The runner (DOC-17) is what makes the process *managed*: it assembles the PM
instruction floor + workflow + agent hierarchy + output style per session
(`prepare_session`, DOC-17 §2.1–§2.2), keeps that content current against the
catalog (HR-3), and provisions every harness from a manifest (HR-2). The operator
never hand-edits a `CLAUDE.md`, never opens a review by hand, never wires a
workflow — that is the "largely managed for the user" property that makes
metacoding usable by non-technical operators.

---

## 4. The factory model

The **factory model** is the production engine of metacoding: a **multi-session
agent fleet** — a managed harness runner that owns the full lifecycle of many
concurrent Claude Code sessions — run **for the user**.

- **Many sessions, one operator.** Each project/ticket runs as a managed session
  in the fleet (DOC-16 numbered list; DOC-17 multi-session runner). The operator
  supervises the fleet through a single control surface (ONB-3) rather than
  babysitting individual sessions.
- **Templated, not bespoke.** Every harness is provisioned from a **manifest** and
  a **BASE_AGENT hierarchy** (DOC-17 HR-1/HR-2): agents compose base-first via the
  `extends:` chain, instruction layers concat in a fixed order, content stays
  current against the catalog. This is the "factory templating" — every session
  rolls off the line with the same disciplined configuration, derived
  automatically, not hand-assembled.
- **Managed lifecycle.** Provisioning, instruction/agent assembly, content
  updates, summarization, and teardown are autonomous (DOC-17 §G0). The fleet
  reports progress as growing summary transcripts (DOC-16 §4) the operator reads
  like a dashboard.
- **Run for the user.** The defining property: the user declares intent and the
  factory does the rest. The fleet is the reason one non-technical operator can
  run many projects at once — the production capacity scales with sessions, not
  with the operator's keystrokes.

The factory model is therefore not a separate feature — it is metacoding's
*engine*. DOC-17 builds the runner that is the factory floor; DOC-16 is the
operator's console onto it.

---

## 5. Relationship to other specs

Metacoding sits **above** the delivery machinery. The specs below are *how* it is
delivered; this doc is *what it is*.

- **DOC-17 / #1405 — Autonomous Managed Harness Runner.** The runner is the
  factory floor (§4). It makes a single harness autonomous (G0); metacoding makes
  the whole product autonomous (N0). DOC-18 sits above DOC-17.
- **#1045 — Metaharness (`tm meta run`) runner core.** The runner-core epic that
  builds the multi-session engine the factory model rides on. Metacoding's "meta
  connect" (ONB-2) is the product-facing front door onto this core.
- **DOC-16 / #1272 — Interactive Sessions TUI.** The operator surface — one of
  the three ONB-3 control surfaces, and the console onto the factory fleet.
- **DOC-14 — Session Manager (SM) Agent.** The daemon-side brain that produces the
  per-session summaries the operator reads.
- **trusty-review.** The automated code-review gate (§3.1).
- **trusty-memory / trusty-search / trusty-analyze.** The backplane substrate
  (context, search, complexity) under the process pillars.

### 5.1 Non-goals / future

The **L-effort** items remain explicitly **post-MVP** (consistent with DOC-17
§4.3 / §5.3):

- **Web dashboard / monitoring** (`trusty-console` / `trusty-mpm-gui` territory) —
  a graphical fleet console beyond the TUI.
- **Slack integration depth** — the Slack control surface named in ONB-3 is a
  near-term *surface goal* but full Slack integration parity is an **L-effort,
  post-MVP** deliverable. The TUI + Telegram surfaces carry the near-term ONB-3
  story.

These do not gate the metacoding north-star: the autonomous, managed, multi-repo
metacoding loop is usable through Homebrew install (ONB-1) → meta connect
(ONB-2) → TUI/Telegram drive (ONB-3) without them.

---

## 6. Behavior contract (summary)

- **Inputs:** a Homebrew install of the stack; a repo reference for meta connect;
  operator intent on a control surface (Telegram / Slack / TUI); the spec +
  ticket + workflow + review machinery the runner assembles per session.
- **Outputs:** a provisioned five-service backplane (ONB-1); a managed harness
  bound to the named repo (ONB-2); a multi-session fleet driven from a chat
  surface or the TUI (ONB-3); changes produced under a managed rigorous process
  (spec + ticket + prescribed workflow + trusty-review gate, §3) by the factory
  fleet (§4).
- **Preconditions:** none are operator-facing — a stale binary, unreachable
  catalog, or unconfigured surface degrades gracefully and is surfaced, never a
  hard block (per the underlying DOC-16/DOC-17 contracts).
- **Postconditions:** the operator manages **nothing** about the process (N0,
  extending G0); one operator can drive many repositories at once; every change
  passes the review gate before it lands.
- **Error conditions:** missing/stale service binary → that service shows
  unreachable, fixed as a release-pipeline defect (ONB-1); inaccessible repo →
  inline error, no harness (ONB-2); unpaired/unauthorized surface → setup prompt
  (ONB-3).

---

## 7. References

- North-star runner: **DOC-17** / **#1405** (`docs/specs/harness-runner-vision.md`).
- Metaharness runner core: **#1045** (`tm meta run`).
- Operator surface: **DOC-16** / **#1272** (`docs/specs/sessions-tui-interactive.md`).
- Session Manager agent: **DOC-14** (`docs/specs/session-manager-agent.md`).
- Intent/method conformance: **DOC-15** (`docs/specs/intent-conformance.md`).
- The five services: `crates/trusty-memory/`, `crates/trusty-search/`,
  `crates/trusty-mpm/`, `crates/trusty-review/`, `crates/trusty-analyze/`.
- Release pipeline + Homebrew tap: `.github/workflows/release.yml`;
  `docs/reference/release-workflow.md`; tap `bobmatnyc/trusty`.
- Telegram control surface: `crates/trusty-mpm/src/telegram/` (teloxide,
  `tm telegram`).
- Companion research/references brief (for the external article):
  `docs/metacoding/metacoding-vs-vibecoding-references.md`.

---

## 8. Change log

- **2026-06-17** — Initial draft (DOC-18, `SPEC-METACODING-01~draft`). Product
  vision spec naming **metacoding** as the trusty-tools north-star: the precise
  definition (spec-driven + ticket-driven + a managed rigorous process +
  automated review + the factory model, run *for* the user) and the vibe-coding →
  metacoding framing ("what vibe coding should have been"). States the
  onboarding journey as first-class requirements **ONB-1** (one Homebrew install
  provisions the five services — trusty-memory/search/mpm/review/analyze — with
  the known binary-release gaps: stale/missing search/memory/mpm/code binaries,
  AL2023 build coverage, and the trusty-memory `BINARIES` list, as prerequisites),
  **ONB-2** ("meta connect" — point at an accessible repo and connect a managed
  harness), and **ONB-3** (drive via Telegram / Slack / TUI). Maps the five
  process pillars onto the machinery (specs/SLD, ticketing, the MPM workflow +
  instruction floor, the trusty-review gate, the factory model = DOC-17 runner +
  DOC-16 surface). Sits **above** DOC-17 (#1405), #1045 (metaharness), and
  DOC-16 (#1272); carries the L-effort post-MVP non-goals (web dashboard, Slack
  integration depth).
