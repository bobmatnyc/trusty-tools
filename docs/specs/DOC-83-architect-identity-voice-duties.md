---
spec_refs:
  - id: SPEC-LEDGER-05~draft
    path: docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md
    anchor: SPEC-LEDGER-05~draft
---

# DOC-83 — The Architect: identity, voice, duties and the 2.0 build

**Status:** Draft (phase 1 implemented in the Architect project, 2026-09-28; the 2.0 native build is tracked in [#8436](https://github.com/bobmatnyc/trusty-tools/issues/8436) and [#8536](https://github.com/bobmatnyc/trusty-tools/issues/8536))
**Subsystem:** `trusty-mpm` — Architect launch and seed (`tm fleet init`), shipped manual `content/instructions/architect/architect.md`; `trusty-architect` — launcher crate ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
**Owner:** owner
**Last-updated:** 2026-10-24
**Spec ID:** `SPEC-ARCHITECT-01~draft` … `SPEC-ARCHITECT-07~draft` (DOC-83). `SPEC-ARCHITECT-01` is the id the Architect project's records used before the move; it stays valid as the alias for this document, and each section below carries its own `SPEC-ARCHITECT-NN~draft` id.
**DOC-N claim:** `DOC-83`, scan-before-claim per [DOC-38 §4.1](./spec-linked-documentation.md#SPEC-SLD-01~draft). Moved from the Architect project under [#9291](https://github.com/bobmatnyc/trusty-tools/issues/9291).
**Builds on:** [DOC-84](./DOC-84-fleet-supervisor-design-and-decisions.md) (question collection and monitoring, superseded in part by this document); [DOC-82](./DOC-82-portfolio-intent-ledger-dashboard.md) (Ledger and dashboard data contract).

The fleet supervisor becomes the Architect, after the Architect in The Matrix Reloaded. This spec records the owner's requirements of 2026-09-28, the rulings, what phase 1 changed in the Architect project, and what the native build in trusty-mpm 2.0 must do. The operating manual is the shipped file `content/instructions/architect/architect.md`; [DOC-84](./DOC-84-fleet-supervisor-design-and-decisions.md) remains the design of record for question collection and monitoring.

**Scope note.** Sections 3, 4, 6 and 7 describe the phase-1 prototype, which lives in the Architect project repository (a separate, local-only repository). Paths there that carry no `content/` or `crates/` prefix name files in that project, not in this checkout. Where the prototype's `ARCHITECT.md` is named, the shipped equivalent is `content/instructions/architect/architect.md`.

---

## 1. Requirements (owner, 2026-09-28) {#SPEC-ARCHITECT-01~draft}

**ID:** SPEC-ARCHITECT-01~draft

**A. Rename to the Architect.** The supervisor is renamed the Architect. Functionality stays the same. Phase 1 renames the identity wherever it is described: the manual, AGENTS.md, the CLAUDE.md IDENTITY/ENFORCEMENT/WORKFLOW sections, agent files, skill descriptions, README and templates. A new entry-point agent `architect` is added; `trusty-supervisor` stays as a pointer. Relays carry `[<Name> Architect HH:MMZ]`; every script and prompt that recognises a relay accepts that and the old `[<Name> supervisor HH:MMZ]`, which stays valid indefinitely because PM transcripts contain it. The directory, tmux sessions, hook paths in other projects, inbox paths and skill directory names do not change in phase 1.

**B. Voice.** The Architect has a voice (diction, sentence patterns, self-introduction, guardrails, examples) defined in the Architect project's voice guide (`docs/architect/voice.md` there). It applies only to messages, dialogs, briefs and reports for human users. He introduces himself as the Architect on first contact with a new user, when asked who he is, and in the first report after a restart; the introduction says plainly what he does. The prose rules stay: evidence labels, multiple-choice decisions with the recommendation first, the open-question tally.

**C. Local-only repository.** The Architect project has no git remote and is never pushed unless the user explicitly asks. The same applies to its `local/` directory, its own git repository.

**D. All persistent data in repos or memory.** Durable records (briefs, SSM and diagnostic output, decisions) live in the `local/` git repository or the trusty-memory palace. `inbox/` holds only transient queues and logs that scripts regenerate, with documented retention. `inbox/briefs/` moves to `local/briefs/`, with a symlink left behind for the absolute paths PM sessions hold.

**E. Architect duties.**

1. Skills for provisioning a new project (`architect-project-provision`) and for architectural reviews delegated to code-analyzer or research agents (`architect-review`); the Architect does not code.
2. Offer reviews proactively (new project, large merge, detected drift, at least weekly per active project) as proposals the user can decline.
3. Track architectural drift against a per-project baseline with read-only git history (`architect-drift-check`); report unintended drift as a proposal.
4. Enforce institutional mandates the user supplies in a manifest format: one TOML file per mandate in `local/mandates/`. Violations go to the owning PM as plain relays; conflicts with a project's own rules go to the user.
5. Learn user patterns and teach. Profiles support several users (`local/users/<name>.md`, with `local/user.md` as the primary user), and carry per-domain skill levels with evidence and labels, observed work patterns and a teaching log. Teaching is short, once per topic unless asked again, never preachy, and logged.

## 2. Rulings {#SPEC-ARCHITECT-02~draft}

**ID:** SPEC-ARCHITECT-02~draft

| # | Date | Source | Ruling |
| --- | --- | --- | --- |
| R1 | 2026-09-28 | owner | The Architect voice is for human users only. PM relays, `local/` records, logs, issues and PRs stay in the plain house style, because PMs and scripts parse them. |
| R2 | 2026-09-28 | owner | The voice document is a style guide, not a script. |
| R3 | 2026-09-28 | owner | Rename in phases. Phase 1 (now): identity only. Phase 2: the full rename (directory, tmux sessions, hooks, skill names) happens when the Architect launches natively from `tm` (trusty-mpm 2.0: trusty-tools #8436, #8536). |
| R4 | 2026-09-28 | owner | The origin remote was removed. Neither the Architect project nor `local/` is pushed unless the user asks. |
| R5 | 2026-09-28 | owner | Durable records live in a git repository or the palace; `inbox/` is transient. |
| R6 | 2026-09-28 | owner | The old relay marker stays valid indefinitely. |
| R7 | 2026-10-04 | owner (Codex voice) | Two-way Codex voice relay is part of the Architect spec (§6). Codex delivers straight into the Architect session; it may read Trusty MPM project files directly; every project change goes through the Architect and the owning PM. Direct writes are allowed only to the Architect voice conversation log and to shared brainstorming notes. |
| R8 | 2026-10-04 | owner (Codex voice) | While the voice relay is active, the Architect asks decisions inline with a two-hex-digit question id and opens no modal dialog. Answers come back as `ANSWER <id>` and resolve only that id. |
| R9 | 2026-10-11 | owner ruling 2026-10-11 | The dashboard gets a Briefs section and a Markdown reader. A brief is a one-paragraph answer. Longer text lives in `docs/briefs/` of the owning project, and every dashboard section shows at most one paragraph inline (§7). |

## 3. Phase 1 in the Architect project {#SPEC-ARCHITECT-03~draft}

**ID:** SPEC-ARCHITECT-03~draft

| Area | Where (Architect project) |
| --- | --- |
| Identity | the operating manual, `AGENTS.md`, `CLAUDE.md` marker sections, `.claude/agents/architect.md` (entry point), `.claude/agents/trusty-supervisor.md` (pointer), skill descriptions, `README.md`, `templates/`, `instructions/SUPERVISOR.md` |
| Relay marker | `scripts/relay-marker.py` (used by `posted.sh`; tests `tests/test_relay_marker.py`), `scripts/relay.sh` (the collector-agent file was retired 2026-10-04) |
| Voice | the manual's "Voice" section, which points to `docs/architect/voice.md` |
| Repository and data | the manual's "Repository and data" section, `scripts/README.md` "Retention", `scripts/init.sh` |
| Duties | the manual's "Architect duties" section; skills `architect-project-provision`, `architect-review`, `architect-drift-check`; `templates/projects/_template.md` baseline section |
| Mandates | `templates/mandates/README.md` (schema), `templates/mandates/example.toml` (fake) |
| Users | `architect-owner-model` skill ("Working with users"), `templates/user.md` |

Relay-marker finding: live PM panes show relays delivered by SendMessage as `› Message from @<date>: [<Name> supervisor 21:02Z] …`, sometimes without a name or with an estimated minute (`22:2xZ`). The phase 1 matcher accepts those forms; the former `posted.sh` pattern matched only `❯ [<Name> supervisor HH:MMZ]` and so missed them.

## 4. Persistence audit (2026-09-28) {#SPEC-ARCHITECT-04~draft}

**ID:** SPEC-ARCHITECT-04~draft

| Data | Location | Kind | Home |
| --- | --- | --- | --- |
| Briefs, relay attachments, evidence, SSM and diagnostic output | `local/briefs/` (was `inbox/briefs/`, now a symlink) | durable | `local/` git |
| Question events, collector ledger | `local/question-events.jsonl`, `local/collector-state.json` | durable | `local/` git |
| State, actions, goals, users, projects, mandates | `local/*.md`, `local/users/`, `local/projects/`, `local/mandates/` | durable | `local/` git, palace copy for goals and users |
| Push events | `inbox/events.jsonl` | transient queue | rotate (`scripts/README.md`) |
| Alerts, poll state, poll and collector logs | `inbox/alerts.jsonl`, `inbox/poll-state.json`, `inbox/poll.log`, `inbox/collector.log` | transient | rotate |
| Relaunch and restart logs | `inbox/relaunch.log`, `inbox/restart.log` | transient | keep until the post-restart check |
| Zero-byte procedure markers | `inbox/*-marker*`, `inbox/*.tmp` | stale | delete when no procedure is in flight |
| Agent scratchpads and task outputs | the harness scratchpad directory, per project and session | transient, harness-owned | copy into `local/briefs/` before a record cites it |
| Secret-bearing backups | a user-private backup directory (mode 700/600) | deliberately outside git | location recorded only |
| Session snapshots | `.trusty-mpm/sessions/` | tm-owned | tm publishes them to a session git ref |
| Claude transcripts | the `projects/` directory under the tm-managed Claude config dir | harness-owned | read by `self-ctx.py` |
| Machine config | the tm host `config.toml` | host config | changes logged in `local/actions.md` |

## 5. trusty-mpm 2.0 native build (trusty-tools #8436, #8536) {#SPEC-ARCHITECT-05~draft}

**ID:** SPEC-ARCHITECT-05~draft

The native Architect ships with `tm` through the tm-supervisor-setup skill (#8436) and the supervisor-first installer (#8536). It must provide:

- **N1 Full rename.** Directory, tmux sessions (the Architect, poller and collector sessions), the push hook and its path in every watched project, inbox paths, skill names and the memory palace move to Architect names. A migration rewrites hook paths in watched projects, leaves forwarding links for old paths for at least one release, and keeps recognising the old relay marker (R6).
- **N2 Persona.** The voice guide ships with the Architect and applies to user-facing channels only (chat, Remote Control, Slack, Telegram). Relays, records, logs, issues and PRs stay plain (R1). The self-introduction runs on first contact, on request and after a restart.
- **N3 Mandates engine.** Load `mandates/*.toml` (schema in `templates/mandates/README.md`), match scope, run command checks read-only with a timeout, run review checks through an agent, apply exceptions, relay violations to the owning PM, and raise conflicts with project rules to the user. Results are events on the question/event log.
- **N4 Provisioning.** The installer sets up the Architect first, then offers project provisioning: stack detection, CLAUDE.md marker seeding, ADR folder, gates, ticketing conventions, mandates, `tm project init` and the push hook as a built-in (retiring `scripts/install-notify-hook.sh`, #8392).
- **N5 Drift tracking.** A per-project baseline store, a scheduled read-only drift check, classification against the ADR index and recorded rulings, and review offers as proposals the user can decline.
- **N6 Multi-user profiles.** A profile store per user, speaker identification per channel, per-domain skill levels, work patterns and a deduplicated teaching log.
- **N7 Local-only data.** No remote by default; durable data in a git-backed store or the palace; inbox files as transient queues with rotation.
- **N8 Voice relay channel.** §6 below, built as a supported channel rather than terminal scraping: a native inbound message path that preserves user input and confirms receipt, inline decision requests with two-hex-digit ids, and acceptance cases A1-A11 as tests.

## 6. Two-way Codex voice relay (owner, 2026-10-04) {#SPEC-ARCHITECT-06~draft}

**ID:** SPEC-ARCHITECT-06~draft

Executor instructions: `docs/architect/codex-voice-relay.md` in the Architect project (verbatim copy of the owner's Codex artifact, with the access policy). The live handoff directory sits under the Codex config directory, outside the synced documents folder, which hung an Architect read on 2026-10-04 20:16Z.

### 6.1 Routing (R7)
- Codex reads the Architect pane and delivers the owner's words straight into the Architect session. Relays are marked `[<Name> via Codex voice HH:MMZ]` and are the owner's rulings, as any owner turn in the session.
- Codex may read Trusty MPM project files directly. Every project change (spec, source, tickets, operational records) goes through the Architect to the owning PM. Codex never instructs a PM to change files around the Architect.
- Direct-write exceptions: the Architect voice conversation log (`local/logs/voice/`) and shared brainstorming notes.

### 6.2 Verified current behavior (2026-10-04)
- Delivery uses tmux: discover the pane, classify the input box with `scripts/input-state.py`, type literal text, verify, send Enter separately, then confirm a submitted turn and an acknowledgement (verified: the 20:16Z request was delivered this way and acknowledged).
- No Remote Control is needed for the relay. Remote Control must not be toggled to repair tmux reading.
- No continuous voice watcher runs. Capture happens when Codex is invoked.

### 6.3 Architect behavior while the relay is active (R8)
- Decision requests go inline in normal output as `QUESTION <id>: ...` with the options, the recommended option and why, what waits on the answer, and what work continues. No AskUserQuestion dialog opens, so the loop never blocks on voice.
- Ids come from the existing hex counter (`NEXT HEX ID` in `local/state.md`): two hex digits, unique among open questions, never reused while a delayed answer could still refer to the old question. Matching is case-insensitive.
- An `ANSWER <id>` resolves only that id. The Architect acknowledges the id and its reading, relays the ruling, and resumes the dependent work. Silence never selects the recommendation. A direct answer in a session withdraws the voice question.

### 6.4 2.0 build requirement (proposed automation)
- A native inbound channel (a tm message endpoint or a SendMessage-style path) that never types over a draft and reports receipt, replacing tmux key injection.
- A question registry keyed by id, source session and generation, shared by the Architect and the relay, so redraws and reconnects never re-ask.
- A conversation log and a delivery record kept apart: intent is not proof of delivery.

### 6.5 Acceptance cases
A1 a fresh executor discovers the right pane and socket. A2 a bounded capture gives a faithful update with no duplicate speech. A3 an authorized reply is submitted once and acknowledged. A4 a human draft, a running turn and an open dialog are preserved. A5 a dim suggestion is told apart from typed text. A6 a pane or socket change triggers rediscovery. A7 a long payload travels as a shared file path. A8 an uncertain delivery never causes a second submission. A9 a missing monitoring capability is reported plainly. A10 two inline decisions are each asked once by voice without a modal, independent work continues, and each answer resolves only its own id. A11 a direct user answer or a withdrawn question is reconciled before the next voice prompt.

## 7. Dashboard briefs and the Markdown reader {#SPEC-ARCHITECT-07~draft}

**ID:** SPEC-ARCHITECT-07~draft

Owner ruling 2026-10-11 (R9). The owner asks for a Briefs section on the dashboard and a Markdown reader that renders diagrams. This section is the requirement. The Architect's dashboard skill (`architect-dashboard`) carries the per-pass procedure. The dashboard data contract is [DOC-82](./DOC-82-portfolio-intent-ledger-dashboard.md); the `briefs` collection below extends it.

### 7.1 Requirement

- The dashboard has a fifth top-level section, Briefs, after Sessions.
- A brief is a one-paragraph answer to an owner question.
- Anything longer than one paragraph is a Markdown file in the owning project's `docs/briefs/`. The brief row links that file.
- Every dashboard section shows at most one paragraph inline. Long text is never placed in a dashboard field. It is linked instead.
- The dashboard has a reader for brief files and other project Markdown. It works on phones and on desktops.

### 7.2 Briefs collection

The dashboard store holds a `briefs` collection. One document per brief.

| Field | Meaning |
| --- | --- |
| `question` | The owner's question. Shown as the row title. |
| `answer` | One paragraph of plain text. |
| `project` | The owning project's name. |
| `session` | Optional. The S id of the session that answered. |
| `goal` | Optional. The G id the brief serves. |
| `doc` | Optional. Path to the Markdown file, relative to the owning project root. Always begins `docs/briefs/`. |
| `status` | `open` or `done`. |
| `created`, `updated` | ISO UTC times. |

The document id is `YYYY-MM-DD-<slug>`. The collection adds no id prefix. The G, P, A, D, T and S counters from §3 of the manual do not change.

### 7.3 Location rule

- Each brief file lives at `<owning project root>/docs/briefs/YYYY-MM-DD-<slug>.md`.
- The owning project is the project the question concerns. The Architect's own briefs go to the `docs/briefs/` of its own repository.
- Existing files in `local/briefs/` stay where they are. The reader keeps reading them.

### 7.4 Section rule

- A field longer than one paragraph shows a short excerpt. If the field has a doc, a "Read" link opens that doc in the reader.
- A field longer than one paragraph with no doc is clipped with a "more" control. The control opens the full text in the reader, rendered as Markdown.
- Applies to every section: Decisions, Asks, Goals, Sessions and Briefs.
- An owner question answered in the Briefs section gets one paragraph. Anything longer is written to a brief file, and the row links it.

### 7.5 Markdown reader

- Phone widths get a full-screen view. Desktop widths get a wide panel. Both have a close or back control.
- The reader has a hash route, `#doc/<project>/<path>`, so a link can be shared.
- It renders GitHub-flavoured Markdown: tables, fenced code blocks and task lists.
- A fenced `mermaid` block renders as a diagram. Diagrams follow the light or dark theme.
- A relative link between brief files opens in the reader.
- Rendered HTML is sanitised. A brief file cannot run script.
- Mermaid runs with `securityLevel: 'strict'`.
- The renderer, mermaid setup and sanitiser follow the trusty-agents UI components `MarkdownEditor` and `ProjectFilesPanel`, adapted to the dashboard's single-file page.
- Library versions are pinned. The dashboard server serves vendored copies, so the page loads nothing from an outside host. This keeps it usable over a private network.

### 7.6 Server route

- The route is read-only. It serves a brief file only when all of these hold: the path ends in `.md`, the file is a regular file, and its real path lies under `<project root>/docs/briefs/` for a known project root or under the Architect project's `local/briefs/`.
- Project roots come from a fixed allowlist: the project registry, or the project records. The client never supplies a root.
- The route returns 404 for: a symlink that leaves the allowed tree, `..`, an absolute path in the request, a file that is not `.md`, and an unknown project.
- The route applies the same Host check as the rest of the dashboard server.
- Tests cover: an allowed path, `..`, a symlink escape, an unknown project and a non-`.md` file. A separate test covers the `briefs` collection.

### 7.7 Mobile

- At 375 px width the page has no horizontal scroll.
- Wide tables and diagrams scroll inside their own box.
- Tap targets are at least 40 px.
- The reader text is readable without zooming.

### 7.8 Prototype and native build

- The Architect project is the prototype. Its Python scripts and page carry the feature first. Commands move to the native build after they are baked in, under the port rule in §5.
- The native build serves brief files and writes the `briefs` collection through the tm daemon. The Python code is removed after the port is live and checked against the Python output.
