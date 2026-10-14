# SPEC-ARCHITECT-01 — the Architect: identity, voice, duties and the 2.0 build

Status: phase 1 implemented in this repository (2026-09-28). Owner: the owner.

The fleet supervisor becomes the Architect, after the Architect in The Matrix
Reloaded. This spec records the owner's requirements of 2026-09-28, his rulings,
what phase 1 changed here, and what the native build in trusty-mpm 2.0 must
do. `architect.md` is the operating manual; `SPEC-SUPERVISOR-01.md` remains the
design of record for question collection and monitoring.

## 1. Requirements (the owner, 2026-09-28)

**A. Rename to the Architect.** The supervisor is renamed the Architect.
Functionality stays the same. Phase 1 renames the identity wherever it is
described: architect.md, AGENTS.md, the CLAUDE.md IDENTITY/ENFORCEMENT/WORKFLOW sections,
agent files, skill descriptions, README and templates. A new
entry-point agent `architect` is added; `trusty-supervisor` stays as a
pointer. Relays carry `[<Name> Architect HH:MMZ]`; every script and prompt
that recognises a relay accepts that and the old `[<Name> supervisor HH:MMZ]`,
which stays valid indefinitely because PM transcripts contain it. The
directory, tmux sessions, hook paths in other projects, inbox paths and skill
directory names do not change in phase 1.

**B. Voice.** The Architect has a voice (diction, sentence patterns,
self-introduction, guardrails, examples) defined in
`docs/architect/voice.md`. It applies only to messages, dialogs, briefs and
reports for human users. He introduces himself as the Architect on first
contact with a new user, when asked who he is, and in the first report after
a restart; the introduction says plainly what he does. The prose rules stay:
evidence labels, multiple-choice decisions with the recommendation first, the
open-question tally.

**C. Local-only repository.** This repository has no git remote and is never
pushed unless the user explicitly asks. The same applies to `local/`, its own
git repository.

**D. All persistent data in repos or memory.** Durable records (briefs,
SSM and diagnostic output, decisions) live in the `local/` git repository or
the trusty-memory palace. `inbox/` holds only transient queues and logs that
scripts regenerate, with documented retention. `inbox/briefs/` moves to
`local/briefs/`, with a symlink left behind for the absolute paths PM sessions
hold.

**E. Architect duties.**
1. Skills for provisioning a new project (`architect-project-provision`) and
   for architectural reviews delegated to code-analyzer or research agents
   (`architect-review`); the Architect does not code.
2. Offer reviews proactively (new project, large merge, detected drift, at
   least weekly per active project) as proposals the user can decline.
3. Track architectural drift against a per-project baseline with read-only git
   history (`architect-drift-check`); report unintended drift as a proposal.
4. Enforce institutional mandates the user supplies in a manifest format:
   one TOML file per mandate in `local/mandates/`. Violations go to the owning
   PM as plain relays; conflicts with a project's own rules go to the user.
5. Learn user patterns and teach. Profiles support several users
   (the primary user's profile and a line per other user, in `AGENTS.md`), and
   carry per-domain skill levels with evidence and labels, observed work
   patterns and a teaching log. Teaching is short, once per topic unless
   asked again, never preachy, and logged.

## 2. Rulings

| # | Date | Source | Ruling |
| --- | --- | --- | --- |
| R1 | 2026-09-28 | the owner | The Architect voice is for human users only. PM relays, `local/` records, logs, issues and PRs stay in the plain house style, because PMs and scripts parse them. |
| R2 | 2026-09-28 | the owner | The voice document is a style guide, not a script. |
| R3 | 2026-09-28 | the owner | Rename in phases. Phase 1 (now): identity only. Phase 2: the full rename (directory, tmux sessions, hooks, skill names) happens when the Architect launches natively from `tm` (trusty-mpm 2.0: trusty-tools #8436, #8536). |
| R4 | 2026-09-28 | the owner | The origin remote was removed. Neither this repository nor `local/` is pushed unless the user asks. |
| R5 | 2026-09-28 | the owner | Durable records live in a git repository or the palace; `inbox/` is transient. |
| R6 | 2026-09-28 | the owner | The old relay marker stays valid indefinitely. |
| R7 | 2026-10-04 | the owner (Codex voice) | Two-way Codex voice relay is part of the Architect spec (§6). Codex delivers straight into the Architect session; it may read Trusty MPM project files directly; every project change goes through the Architect and the owning PM. Direct writes are allowed only to the Architect voice conversation log and to shared brainstorming notes. |
| R8 | 2026-10-04 | the owner (Codex voice) | While the voice relay is active, the Architect asks decisions inline with a two-hex-digit question id and opens no modal dialog. Answers come back as `ANSWER <id>` and resolve only that id. |

## 3. Phase 1 in this repository

| Area | Where |
| --- | --- |
| Identity | `architect.md` (composed by tm), `AGENTS.md`, `CLAUDE.md` (a short pointer), `.claude/agents/architect.md` (entry point), skill descriptions, `templates/README.md`, `templates/` |
| Relay marker | `scripts/relay-marker.py` (used by `posted.sh`; tests `tests/test_relay_marker.py`), `scripts/relay.sh` (`.claude/agents/collector-agent.md` retired 2026-10-04, f4) |
| Voice | `architect.md` "Voice" → `docs/architect/voice.md` |
| Repository and data | `architect.md` "Repository and data", `scripts/README.md` "Retention", `scripts/init.sh` |
| Duties | `architect.md` "Architect duties"; skills `architect-project-provision`, `architect-review`, `architect-drift-check`; the baseline in each project's palace |
| Mandates | `templates/mandates/README.md` (schema), `templates/mandates/example.toml` (fake) |
| Users | `architect-owner-model` skill ("Working with users"), `AGENTS.md` |

Relay-marker finding: live PM panes show relays delivered by SendMessage as
`› Message from @<date>: [Owner supervisor 21:02Z] …`, sometimes without a name
or with an estimated minute (`22:2xZ`). The phase 1 matcher accepts those
forms; the former `posted.sh` pattern matched only `❯ [<Name> supervisor
HH:MMZ]` and so missed them.

## 4. Persistence audit (2026-09-28)

| Data | Location | Kind | Home |
| --- | --- | --- | --- |
| Briefs, relay attachments, evidence, SSM and diagnostic output | `local/briefs/` (was `inbox/briefs/`, now a symlink) | durable | `local/` git |
| Question events, collector ledger | `local/question-events.jsonl`, `local/collector-state.json` | durable | `local/` git |
| State, actions, goals, mandates | `local/*.md`, `local/mandates/` | durable | `local/` git, palace copy for goals |
| Users, owner model, per-project records | `AGENTS.md`; the `owner` palace; each project's palace | durable | palaces; `AGENTS.md` in `local/` git |
| Push events | `inbox/events.jsonl` | transient queue | rotate (scripts/README.md) |
| Alerts, poll state, poll and collector logs | `inbox/alerts.jsonl`, `inbox/poll-state.json`, `inbox/poll.log`, `inbox/collector.log` | transient | rotate |
| Relaunch and restart logs | `inbox/relaunch.log`, `inbox/restart.log` | transient | keep until the post-restart check |
| Zero-byte procedure markers | `inbox/*-marker*`, `inbox/*.tmp` | stale | delete when no procedure is in flight |
| Agent scratchpads and task outputs | `/private/tmp/claude-<uid>/<project>/<session>/` | transient, harness-owned | copy into `local/briefs/` before a record cites it |
| Secret-bearing backups | `~/backups/` (700/600) | deliberately outside git | location recorded only |
| Session snapshots | `.trusty-mpm/sessions/` | tm-owned | tm publishes them to a session git ref |
| Claude transcripts | `~/.trusty-tools/trusty-mpm/claude-config/projects/` | harness-owned | read by `self-ctx.py` |
| Machine config | `~/.trusty-mpm/config.toml` | host config | changes logged in `local/actions.md` |

## 5. trusty-mpm 2.0 native build (trusty-tools #8436, #8536)

The native Architect ships with `tm` through the tm-supervisor-setup skill
(#8436) and the supervisor-first installer (#8536). It must provide:

- **N1 Full rename.** Directory, tmux sessions (`tm-supervisor`,
  `tm-supervisor-poll`, `tm-supervisor-collector`), the push hook and its path
  in every watched project, inbox paths, skill names and the memory palace
  move to Architect names. A migration rewrites hook paths in watched
  projects, leaves forwarding links for old paths for at least one release,
  and keeps recognising the old relay marker (R6).
- **N2 Persona.** The voice guide ships with the Architect and applies to
  user-facing channels only (chat, Remote Control, Slack, Telegram). Relays,
  records, logs, issues and PRs stay plain (R1). The self-introduction runs on
  first contact, on request and after a restart.
- **N3 Mandates engine.** Load `mandates/*.toml` (schema in
  `templates/mandates/README.md`), match scope, run command checks read-only
  with a timeout, run review checks through an agent, apply exceptions, relay
  violations to the owning PM, and raise conflicts with project rules to the
  user. Results are events on the question/event log.
- **N4 Provisioning.** The installer sets up the Architect first, then offers
  project provisioning: stack detection, CLAUDE.md marker seeding, ADR
  folder, gates, ticketing conventions, mandates, `tm project init` and the
  push hook as a built-in (retiring `scripts/install-notify-hook.sh`, #8392).
- **N5 Drift tracking.** A per-project baseline store, a scheduled read-only
  drift check, classification against the ADR index and recorded rulings,
  and review offers as proposals the user can decline.
- **N6 Multi-user profiles.** A profile store per user, speaker
  identification per channel, per-domain skill levels, work patterns and a
  deduplicated teaching log.
- **N7 Local-only data.** No remote by default; durable data in a git-backed
  store or the palace; inbox files as transient queues with rotation.
- **N8 Voice relay channel.** §6 below, built as a supported channel rather
  than terminal scraping: a native inbound message path that preserves user
  input and confirms receipt, inline decision requests with two-hex-digit ids,
  and acceptance cases A1-A11 as tests.

## 6. Two-way Codex voice relay (the owner, 2026-10-04)

Executor instructions: `docs/architect/codex-voice-relay.md` (verbatim copy of
The owner's Codex artifact, with the access policy). Live handoff directory:
`~/.codex/voice-relay/2026-10-04/` (outside `~/Documents`, which
hung an Architect read on 2026-10-04 20:16Z).

### 6.1 Routing (R7)
- Codex reads the Architect pane and delivers the owner's words straight into the
  Architect session. Relays are marked `[Owner via Codex voice HH:MMZ]` and are
  The owner's rulings, as any the owner turn in the session.
- Codex may read Trusty MPM project files directly. Every project change
  (spec, source, tickets, operational records) goes through the Architect to
  the owning PM. Codex never instructs a PM to change files around the
  Architect.
- Direct-write exceptions: the Architect voice conversation log
  (`local/logs/voice/`) and shared brainstorming notes.

### 6.2 Verified current behavior (2026-10-04)
- Delivery uses tmux: discover the pane, classify the input box with
  `scripts/input-state.py`, type literal text, verify, send Enter separately,
  then confirm a submitted turn and an acknowledgement (verified: the 20:16Z
  request was delivered this way and acknowledged).
- No Remote Control is needed for the relay. Remote Control must not be
  toggled to repair tmux reading.
- No continuous voice watcher runs. Capture happens when Codex is invoked.

### 6.3 Architect behavior while the relay is active (R8)
- Decision requests go inline in normal output as `QUESTION <id>: ...` with
  the options, the recommended option and why, what waits on the answer, and
  what work continues. No AskUserQuestion dialog opens, so the loop never
  blocks on voice.
- Ids come from the existing hex counter (`NEXT HEX ID` in `local/state.md`):
  two hex digits, unique among open questions, never reused while a delayed
  answer could still refer to the old question. Matching is case-insensitive.
- An `ANSWER <id>` resolves only that id. The Architect acknowledges the id
  and its reading, relays the ruling, and resumes the dependent work. Silence
  never selects the recommendation. A direct answer in a session withdraws the
  voice question.

### 6.4 2.0 build requirement (proposed automation)
- A native inbound channel (a tm message endpoint or a SendMessage-style
  path) that never types over a draft and reports receipt, replacing tmux
  key injection.
- A question registry keyed by id, source session and generation, shared by
  the Architect and the relay, so redraws and reconnects never re-ask.
- A conversation log and a delivery record kept apart: intent is not proof of
  delivery.

### 6.5 Acceptance cases
A1 a fresh executor discovers the right pane and socket. A2 a bounded capture
gives a faithful update with no duplicate speech. A3 an authorized reply is
submitted once and acknowledged. A4 a human draft, a running turn and an open
dialog are preserved. A5 a dim suggestion is told apart from typed text. A6 a
pane or socket change triggers rediscovery. A7 a long payload travels as a
shared file path. A8 an uncertain delivery never causes a second submission.
A9 a missing monitoring capability is reported plainly. A10 two inline
decisions are each asked once by voice without a modal, independent work
continues, and each answer resolves only its own id. A11 a direct user answer
or a withdrawn question is reconciled before the next voice prompt.
