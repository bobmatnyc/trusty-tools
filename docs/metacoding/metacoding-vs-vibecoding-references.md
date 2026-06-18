# Metacoding vs. Vibe Coding — research & references brief

> **What this is — and is NOT.** This is a **research & references brief**: it
> collects the raw material (a comparison table, thesis/talking points, concrete
> reference hooks from trusty-tools, and open questions) that an external article
> will draw on. **The article itself is written in a separate writing project
> (HyperDev), not here.** This document does **not** contain the article — only
> the references and framing for it.
>
> **Planned article working title:** *"Introducing Metacoding: What Vibe Coding
> Should Have Been"* (alt: *"…What Vibe Coding Can Be"*).
>
> **Companion vision spec (this repo):** `docs/specs/metacoding-vision.md`
> (DOC-18, `SPEC-METACODING-01~draft`).

---

## 1. Vibe coding vs. meta-coding — comparison table

| Axis | Vibe coding | Metacoding |
|---|---|---|
| **Process rigor** | Ad-hoc, improvisational; rigor (if any) is whatever the human remembers to apply | Rigorous process **built in and managed** — prescribed workflow runs automatically |
| **Specification** | None of record; intent lives in the chat scrollback | Spec-driven — a specification of record states *what* before code is written (behavior-contract specs + SLD) |
| **Ticketing** | None; no tracked unit of work | Ticket-driven — each unit of work is a tracked, sequenced, traceable ticket |
| **Code review** | Manual, optional, or skipped | Automated review **gate** on every change (trusty-review) before it lands |
| **Structure** | Emergent / none; one conversation at a time | Formal structure — workflow phases, agent hierarchy, instruction layers, manifests |
| **Who it's for** | Developers comfortable holding the process in their head | **Non-technical users too** — the rigor is managed *for* them |
| **Single vs. multi-project** | Effectively single-project, single-session attention | **Multi-project / multi-session** — one operator drives many repos at once via a fleet |
| **Failure modes** | Silent drift, lost intent, unreviewed changes, irreproducible state | Caught at the gate; intent persists in specs/tickets; degrade-don't-block on infra faults |
| **Reproducibility** | Low — depends on the human + the chat history | High — spec + ticket + manifest + templated harness make runs reproducible |
| **Role of the human** | Pilots every keystroke / prompt | **Declares intent and supervises the fleet**; the system runs the disciplined machinery |

---

## 2. Key talking points / thesis

- **One-line thesis:** *Metacoding = spec-driven + ticket-driven development +
  managed rigor (prescribed workflow + automated code review + formal structure)
  + the factory model (a multi-session managed agent fleet) — all run **for** the
  user.*
- **"What vibe coding should have been."** Metacoding keeps the approachable,
  intent-first *feel* of vibe coding but adds the discipline that makes output
  trustworthy, reproducible, and safe to run unattended. Same fluid surface, real
  engineering rigor underneath.
- **The rigor is managed, not assigned.** The differentiator is not "add process"
  — anyone can add process. It is that provisioning, workflow sequencing, and the
  review gate are **autonomous**: the user never sets up CI, writes a process doc,
  or remembers to open a review.
- **It unlocks two audiences vibe coding can't serve well:** (a) **non-technical
  users**, who get a senior-engineer-grade process imposed *for* them; and (b)
  **multi-project operators**, who run many repos at once because the production
  engine is a fleet, not a single chat.
- **The factory model is the engine.** A multi-session managed harness runner
  templates every session from a manifest + BASE_AGENT hierarchy, so production
  capacity scales with sessions rather than with the operator's keystrokes.
- **Degrade, don't block.** A stale binary, unreachable catalog, or unconfigured
  surface is surfaced and gracefully degraded — never a hard failure. Rigor that
  is also resilient.

---

## 3. Concrete reference hooks from this project

The article can cite these as the working machinery behind the vision.

### 3.1 The five-service stack (one Homebrew install — ONB-1)

| Service | One-line role |
|---|---|
| **trusty-memory** | Durable memory-palace context engine (the project's long-term memory). |
| **trusty-search** | Hybrid (lexical + semantic) code-search daemon + MCP server. |
| **trusty-mpm** | The MPM platform: harness runner, daemon, CLI/TUI, and the Telegram control surface. |
| **trusty-review** | LLM-backed automated PR-review service — the code-review gate. |
| **trusty-analyze** | Code-analysis sidecar (cyclomatic/cognitive complexity, smells, quality metrics). |

Install reference: Homebrew tap `bobmatnyc/trusty`; per-crate
`brew install trusty-<svc>`; release matrix in `.github/workflows/release.yml`.

### 3.2 Machinery hooks

- **Autonomous managed harness runner** — `docs/specs/harness-runner-vision.md`
  (DOC-17, `SPEC-HARNESS-01~draft`); north-star **G0** ("the user manages
  nothing"); epic **#1405**.
- **Metaharness `tm meta run`** — the runner-core epic **#1045** (the multi-session
  engine "meta connect" rides on).
- **Interactive sessions TUI** — `docs/specs/sessions-tui-interactive.md`
  (DOC-16, `SPEC-SM-TUI-01~draft`); epic **#1272**. The operator console:
  numbered fleet list, cost statusline, growing summary transcripts, inline
  session addressing.
- **trusty-review as the automated code-review gate** — fetches the PR diff,
  pulls context from trusty-search, queries trusty-analyze for complexity, then
  emits a structured LLM verdict with actionable findings (CLI / webhook / MCP
  stdio).
- **Manifest-driven provisioning + BASE_AGENT hierarchy ("factory" templating)** —
  DOC-17 HR-1/HR-2: agents compose base-first via the `extends:` chain;
  instruction layers concat in fixed order; harnesses provision from a manifest;
  precedence `project > user > manifest > compiled-in`.
- **Control surfaces (ONB-3)** — Telegram bot
  (`crates/trusty-mpm/src/telegram/`, teloxide, `tm telegram` pairing) and the
  DOC-16 TUI are near-term; **Slack** is the post-MVP / L-effort surface.

---

## 4. Open questions / things to validate before publishing

- **Homebrew install completeness.** Is the *single-install-provisions-all-five*
  promise actually true end-to-end today? Known gaps to confirm closed first:
  stale/missing **trusty-search / trusty-memory / trusty-mpm / trusty-code**
  binary releases; **AL2023** build coverage for every load-dynamic variant; the
  **trusty-memory `BINARIES` list** correctness. Don't claim "one install" in the
  article until these are green.
- **Non-technical onboarding UX.** Has the meta-connect → drive-from-Telegram flow
  been validated with an actual non-technical user? What's the real first-run
  experience (pairing, repo access, first ticket)?
- **"Meta connect" surface.** Is the product-facing `meta connect` command/flow
  named and shipped, or still the underlying `tm meta run` (#1045)? Align article
  terminology with what actually ships.
- **Review-gate enforcement.** Is trusty-review wired as a *blocking* gate in the
  prescribed workflow, or advisory today? The article's "every change is reviewed
  before it lands" claim depends on this.
- **Slack depth.** ONB-3 names Slack, but Slack integration is L-effort/post-MVP.
  Frame Slack as roadmap, not shipped, unless it lands first.
- **Multi-project scale claims.** What's a defensible concurrency number for "one
  operator, many repos"? Validate against real fleet runs before quoting figures.

---

## 5. References

- Vision spec: `docs/specs/metacoding-vision.md` (DOC-18).
- Harness runner: `docs/specs/harness-runner-vision.md` (DOC-17, #1405).
- Sessions TUI: `docs/specs/sessions-tui-interactive.md` (DOC-16, #1272).
- Metaharness runner core: #1045.
- Release pipeline / Homebrew: `.github/workflows/release.yml`,
  `docs/reference/release-workflow.md`, tap `bobmatnyc/trusty`.
- Article home: the **HyperDev** writing project (external to this repo).
