# Console secrets service — secrets stay out of agent transcripts {#PRD-SECRETS-01}

**ID:** PRD-SECRETS-01  
**Status:** Draft — scheduled for 2.0.0. Implementation starts after the search dashboard work.  
**Owner:** Product / Bob Matsuoka  
**Version:** v1  
**Last-updated:** 2026-10-01  
**Related specs/ADRs:** [DOC-74](../specs/DOC-74-secrets-integration.md) (the HOW), [DOC-45](../specs/DOC-45-credential-authority-model.md), [ADR-0018](../adr/0018-loopback-only-doctrine.md), [ADR-0032](../adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)  
**Epic:** [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517)

---

## Problem & Context

Developers hand secrets to agents through the same channel they use for
instructions: the chat. A value typed or pasted into a session lands in the
transcript and travels to the model API. A value read by an agent's shell
command lands in tool output, which is also transcript.

The repo already fights this one leak at a time:

- A Bash/Read/Grep guard refuses secret-shaped file reads
  ([#7266](https://github.com/bobmatnyc/trusty-tools/issues/7266)).
  [#8879](https://github.com/bobmatnyc/trusty-tools/issues/8879) shows a key
  still escaping through a script the agent wrote and then ran.
- `!tm secrets add` (DOC-74 §9, 2026-09-11) put the value after a `!` prefix.
  That value lands in the transcript too, which is why the 2026-09-17 ruling
  moved the value path to the clipboard.

Each fix closes one path. The owner asked for a fix to the class (2026-10-01,
verbatim):

> "Secrets should be its own service in dashboard. Devs should be able to add
> secrets to any project or owner using the console interface, solving the
> issue of secrets in transcripts once and for all."

The answer is to move value entry out of the session entirely. A developer
types a value into a trusty-console page. An agent only ever names the key.
Only code that spawns a process or answers an exec-granted client ever holds
the value.

---

## Target Users / Personas

| Persona | Need | Context |
|---------|------|---------|
| **Developer** | Store a project or owner secret without pasting it into a session | Sets up a repo, rotates a token, onboards an integration |
| **Agent** (PM or specialist) | Run a command that needs a credential, knowing only the key name | `gh`, deploy CLIs, test suites, app code under `tm secrets exec` |
| **App code** (Rust, Python, JS/TS) | Read a secret at runtime without a `.env` full of plaintext | Local dev servers and scripts started under `tm secrets exec` |
| **Remote developer** | Manage secrets from another tailnet machine | The console's Tailscale listener |

---

## Goals & Non-Goals

### Goals

- **No value in any transcript.** A developer never pastes a value into a
  session. An agent never receives one in a tool result.
- **Console-first entry.** Secrets are their own service page in the
  trusty-console dashboard, scoped to a project or an owner.
- **Lightweight and fast.** A library crate that serves its own on-demand UDS
  socket. No new daemon, no startup work, no background task. Daemon here
  means a resident, supervised process started at login or by launchd. An
  on-demand trusty-secrets socket that spawns on the first call, holds no
  background task and exits when idle is not a daemon (owner ruling 28,
  2026-10-02; ADR-0034 pattern).
- **Many stores, one surface.** Keychain, 1Password, Keeper, Vercel, GitHub
  Actions and more behind one trait.
- **Usable from any language.** App code reads secrets through env injection,
  a dotenv loader, or a thin client in Rust, Python or JS.

### Non-Goals

- A new daemon (as defined above) or TCP listener for secrets (owner ruling 2026-10-01).
- A machine-wide secret scope. Scope is project or owner only.
- Showing a stored value. The console shows length and last-updated only.
- Rotating or generating secrets.
- Renegotiating DOC-45's principal/grant/audit model.
- An MCP tool or console route that returns a value.

---

## User Stories / Jobs-to-be-Done

1. **As a developer**, I open `/tools/secrets/`, pick `bobmatnyc/trusty-tools`,
   and save `OPENAI_API_KEY`. The page confirms with the first 8 characters and
   the length, once. No agent sees any of it.
2. **As a developer**, I save `NPM_TOKEN` at owner scope `bobmatnyc`. Every
   repo under that owner resolves it, unless a repo sets its own.
3. **As an agent**, I run `tm secrets exec --env GH_TOKEN=GH_TOKEN -- gh pr list`.
   I know the key name. I never see the token.
4. **As a developer**, I commit a `.env` that holds `secret://` references, and
   run my server with `tm secrets exec --dotenv .env -- npm run dev`.
5. **As a developer**, I tick "agents may use" on one key. Only that key can be
   injected into a process that an agent started.
6. **As a developer**, I push a project secret to Vercel's production and
   preview targets from the same page.
7. **As a developer on my laptop**, I manage secrets on my desktop's console
   over Tailscale. A different tailnet user is refused.

---

## Requirements

### Functional Requirements

#### Console service page {#PRD-SECRETS-02}
**ID:** PRD-SECRETS-02  
**Status:** Draft  
**Priority:** Must  

trusty-console serves a secrets page at `/tools/secrets/`. It lists scopes and
key names, and sets, deletes and copies keys. The list shows each key's length
and last-updated time. The first 8 characters appear once, in the set
confirmation. A value of 8 characters or fewer shows only its length. The
console never reads a store and never receives a value back.

**Why:** Value entry outside any session is what removes the transcript path.

---

#### Project and owner scopes {#PRD-SECRETS-03}
**ID:** PRD-SECRETS-03  
**Status:** Draft  
**Priority:** Must  

A secret belongs to a project (`<owner>/<repo>`) or an owner (the GitHub owner
from the git remote). There is no machine scope. A project key wins over an
owner key with the same name.

**Why:** Owners share tokens across their repos; repos still override.

---

#### Library crate over UDS {#PRD-SECRETS-04}
**ID:** PRD-SECRETS-04  
**Status:** Draft  
**Priority:** Must  

The service is a library crate, `trusty-secrets`. It owns and serves its own
on-demand socket (owner ruling 24, 2026-10-02); the tm daemon and the console
are clients of it. The socket path and the spawn contract are pending owner decision (socket path, spawn contract).
The crate does no work at startup.

**Why:** Owner rulings: "Secrets doesn't need to be a daemon. A crate accessed
over UDS" and "Should be lightweight and fast."

---

#### Store and sync integrations {#PRD-SECRETS-05}
**ID:** PRD-SECRETS-05  
**Status:** Draft  
**Priority:** Must (Keychain), Should (each further integration)  

One backend trait covers stores and sync targets. Keychain ships first. Then
one PR per integration: 1Password, Vercel, GitHub Actions, Keeper. Doppler and
AWS Secrets Manager fit the same trait. Vercel's development target is opt-in
per key, because Vercel development values are readable.

**Why:** Owner ruling: "It should have integrations with keychain, 1P, keeper,
Vercel etc."

---

#### Agent use flag {#PRD-SECRETS-06}
**ID:** PRD-SECRETS-06  
**Status:** Draft  
**Priority:** Must  

Each key carries an "agents may use" flag, set in the console, default OFF. A
key without the flag is never injected into a process whose parent chain
includes Claude Code.

**Why:** A developer decides per key what an agent-started process may hold.
The default keeps new keys away from agents.

---

#### Programming-language integrations {#PRD-SECRETS-07}
**ID:** PRD-SECRETS-07  
**Status:** Draft  
**Priority:** Must  

App code reaches secrets three ways, all in scope:

1. `tm secrets exec -- <cmd>` injects values as env vars.
2. `tm secrets exec --dotenv .env -- <cmd>` resolves `secret://` references in a
   `.env` file and injects the values.
3. Thin clients — a Rust `client` feature, a zero-dependency Python package, a
   zero-dependency npm package — resolve a key over UDS, only inside a process
   that `exec` granted.

**Why:** Owner ruling: "It should also have integrations with programming
languages." Tiers 1 and 2 need no per-language code.

---

#### Remote access over Tailscale {#PRD-SECRETS-08}
**ID:** PRD-SECRETS-08  
**Status:** Draft  
**Priority:** Must  

Secrets routes serve on loopback and on the console's Tailscale listener. A
tailnet request must come from the same Tailscale login as this machine.
Until that gate ships, secrets routes refuse tailnet requests.

**Why:** Owner answer on remote access: "Local or Tailscale."

---

#### CLI parity {#PRD-SECRETS-09}
**ID:** PRD-SECRETS-09  
**Status:** Draft  
**Priority:** Should  

`tm secrets` keeps a CLI path over the same crate. `set` is the one write verb
(upsert) and reads the value from the clipboard by default. The
`!tm secrets add` bang command is retired.

**Why:** Rulings of 2026-09-17: "let's just use 'set', remove 'add'" and "make
--paste the default".

---

### Non-Functional Requirements

#### Value confinement {#PRD-SECRETS-10}
**ID:** PRD-SECRETS-10  
**Status:** Draft  
**Priority:** Must  

No method returns a value to the console. No MCP tool returns a value. Error
text is fixed and never echoes request content. Values never appear in argv,
logs, hook payloads, or `Debug` output.

---

#### Performance {#PRD-SECRETS-11}
**ID:** PRD-SECRETS-11  
**Status:** Draft  
**Priority:** Must  

No preload at session start. No background task. Values resolve lazily, on
use. Only CLI-backed sources (1Password, Keeper, Doppler, AWS) cache, to avoid
repeat unlock prompts. Tool detection runs only on `doctor`.

---

#### Browser hardening {#PRD-SECRETS-12}
**ID:** PRD-SECRETS-12  
**Status:** Draft  
**Priority:** Must  

Secrets routes require same-origin CORS, an exact Origin match, a Host check,
and a per-launch anti-CSRF header. These ship before any secret goes through
the console.

---

## Acceptance Criteria

**PRD-SECRETS-02 (Console service page)**
- [ ] `/tools/secrets/` lists scopes and key names with length and last-updated.
- [ ] Set confirmation shows the first 8 characters and length once; a value of ≤8 characters shows only its length.
- [ ] No console response body contains a stored value.

**PRD-SECRETS-03 (Scopes)**
- [ ] `secret://KEY` resolves the project key when both project and owner hold `KEY`.
- [ ] No API or UI offers a machine scope.

**PRD-SECRETS-04 (Library crate over UDS)**
- [ ] `trusty-secrets` serves `secrets.*` on its own on-demand socket; the tm daemon and the console are clients and host no `secrets.*` method (socket path, spawn contract: pending owner decision).
- [ ] Daemon start does no secrets work.

**PRD-SECRETS-05 (Integrations)**
- [ ] Keychain works end to end through the console page before any other integration merges.
- [ ] Each further integration lands as its own PR.
- [ ] A Vercel sync writes production and preview by default; development only when ticked per key.

**PRD-SECRETS-06 (Agent use flag)**
- [ ] A new key's flag is OFF.
- [ ] `tm secrets exec` started under Claude Code refuses a key without the flag.

**PRD-SECRETS-07 (Language integrations)**
- [ ] Tier 1 and tier 2 work for any language that reads env vars.
- [ ] A tier 3 client resolves a key only with a valid grant token, a granted key, and a caller inside the granted process tree.
- [ ] Rust, Python and npm clients add no third-party dependency.

**PRD-SECRETS-08 (Tailscale)**
- [ ] A tailnet request from a different Tailscale login gets 403.
- [ ] A request from a tagged node gets 403.

**PRD-SECRETS-10 (Value confinement)**
- [ ] A malformed `secrets.set` request gets fixed error text that contains none of the request.

---

## Success Metrics / KPIs

| Metric | Baseline | Target | How measured |
|--------|----------|--------|--------------|
| Secret values seen in session transcripts | Recurring ([#8879](https://github.com/bobmatnyc/trusty-tools/issues/8879) class) | 0 from console-entered keys | Transcript scrub audit against the key index |
| Value-entry paths that pass through a session | `!tm secrets add` (DOC-74 §9 as drafted 2026-09-11) | 0 | Console and clipboard are the only value paths |
| Added latency per `secrets.*` call | N/A (new) | Milliseconds, excluding the backend's own unlock | Method benchmark |
| Integrations shipped | None merged ([#7521](https://github.com/bobmatnyc/trusty-tools/issues/7521) open) | Keychain, 1Password, Vercel, GitHub Actions, Keeper | Merged PRs on [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517) |

---

## Scope & Out-of-Scope

### In Scope

- The `trusty-secrets` crate, its on-demand socket, and the tm daemon's client.
- The console page, bridge, and browser hardening.
- The tailnet gate for secrets routes.
- Keychain, then 1Password, Vercel, GitHub Actions and Keeper.
- All three language tiers, including the Rust, Python and npm clients and
  their publish workflows.
- `tm secrets` CLI over the crate; agent and skill text updated to `set`.

### Out of Scope

- A Go client (later).
- Machine-scope secrets.
- Showing, exporting or revealing a stored value.
- `copy` across projects (ruling 2026-09-23: `copy` stays in-project).

---

## Scope Rulings

All rulings are the owner's, dated, and recorded on
[#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517).

| Date | Ruling |
|------|--------|
| 2026-09-11 | Default store is the machine's encrypted store; per-project store choice; auto-detect local tools; a source `.env` is never deleted after import. |
| 2026-09-12 | Headless: accept a service-account token from an env var for 1Password/Keeper; otherwise fail closed. |
| 2026-09-17 | `set` replaces `add` (upsert); clipboard is the default value source; confirm with first 8 characters and length. |
| 2026-09-23 | Scheduled for 2.0.0; cross-repo sharing only via explicit `secrets.vault`; `copy` stays in-project. |
| 2026-10-01 | Secrets start after the search dashboard. A value of 8 characters or fewer shows only its length. |
| 2026-10-01 | "Secrets should be its own service in dashboard. Devs should be able to add secrets to any project or owner using the console interface, solving the issue of secrets in transcripts once and for all." |
| 2026-10-01 | "Secrets doesn't need to be a daemon. A crate accessed over UDS" / "Should be lightweight and fast." |
| 2026-10-01 | "It should have integrations with keychain, 1P, keeper, Vercel etc." / "It should also have integrations with programming languages." |
| 2026-10-01 | List shows length and updated_at only; first 8 characters once on set. Owner scope = GitHub owner from the git remote; no machine scope; project > owner. Remote: "Local or Tailscale." Order: Keychain through the console page first, then one PR per integration. Default taken: Vercel development target opt-in per key. |
| 2026-10-01 | Per-key "agents may use" flag, default OFF. All three language tiers are in scope. |

---

## Phasing

Implementation starts after the search dashboard backend items
([#9028](https://github.com/bobmatnyc/trusty-tools/issues/9028),
[#9029](https://github.com/bobmatnyc/trusty-tools/issues/9029),
[#9030](https://github.com/bobmatnyc/trusty-tools/issues/9030)) finish.
DOC-74 §15.9 holds the slice table with test-ladder rungs.

1. **Keychain through the console** — S0 docs, S1 crate, S2 secrets socket and tm client,
   S3a console bridge and hardening, S3b tailnet gate, S4 UI (with the
   "agents may use" toggle), S5 agent/skill text.
2. **Integrations** — S6+, one PR each: 1Password, Vercel, GitHub Actions,
   Keeper.
3. **Language integrations** — S7 `exec --dotenv`, S8 `secrets.resolve` with
   grants and the ancestry check, S9 Rust client, S10 Python and npm clients
   with publish workflows.

---

## Risks & Assumptions

| Risk | Mitigation | Status |
|------|-----------|--------|
| An agent with shell access and an exec grant writes a value to a file and reads it back | The "agents may use" flag keeps unflagged keys out of agent-started processes; tier 3 must not widen what tier 1 already allows | Accepted limit; [#8879](https://github.com/bobmatnyc/trusty-tools/issues/8879) is one instance |
| The console's tailnet listener serves every route with no peer authentication | S3b tailnet gate; secrets routes answer 403 on the tailnet until it merges | Open — [#9035](https://github.com/bobmatnyc/trusty-tools/issues/9035) |
| A browser page on another origin reads or writes secrets routes | S3a: same-origin CORS, exact Origin, Host check, anti-CSRF header | Planned (S3a) |
| The UDS router echoes request content in decode errors | Fixed error text for `secrets.*` | Planned (S3a) |
| Vercel development values are readable | Development target opt-in per key | Default taken 2026-10-01 |
| The repo has no PyPI or npm publish workflow | S10 adds one publish child per registry | Planned (S10) |

| Assumption | Validation plan |
|-----------|-----------------|
| The git remote names the owner for every project | Scope resolution tests in S1 |
| A process-ancestry walk identifies a Claude Code parent | S8 ancestry tests on macOS and Linux |

---

## Open Questions

1. **Doppler and AWS Secrets Manager timing.** The trait covers both as CLI
   sources. The owner's integration order names 1Password, Vercel, GitHub
   Actions and Keeper. Do Doppler and AWS follow in that queue, or wait for
   demand?
2. **Go client timing.** Go is out of scope for this PRD. Which release picks
   it up?

---

## Linked Specs

| PRD Requirement | Spec | Status | Notes |
|-----------------|------|--------|-------|
| PRD-SECRETS-02 (Console page) | [DOC-74](../specs/DOC-74-secrets-integration.md) §15.6 | Draft | Bridge, routes, masking |
| PRD-SECRETS-03 (Scopes) | DOC-74 §15.3 | Draft | Vault names, `secret://` grammar |
| PRD-SECRETS-04 (Crate over UDS) | DOC-74 §15.2 | Draft | Features, methods, host |
| PRD-SECRETS-05 (Integrations) | DOC-74 §15.4 | Draft | `SecretBackend` caps |
| PRD-SECRETS-06 (Agent flag) | DOC-74 §15.8 | Draft | Exec grant check |
| PRD-SECRETS-07 (Languages) | DOC-74 §15.8 | Draft | Tiers, grants, packaging |
| PRD-SECRETS-08 (Tailscale) | DOC-74 §15.7 | Draft | S3b gate |
| PRD-SECRETS-09 (CLI) | DOC-74 §9 | Draft | `set`, clipboard |
| PRD-SECRETS-10–12 (Non-functional) | DOC-74 §15.5, §15.6 | Draft | Lazy resolution, hardening |

---

## References

- Epic [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517) and the
  [2026-10-01 design and owner answers](https://github.com/bobmatnyc/trusty-tools/issues/7517#issuecomment-5941463665).
- [DOC-74](../specs/DOC-74-secrets-integration.md) — the design.
- [DOC-45](../specs/DOC-45-credential-authority-model.md) — the credential
  authority this service sits behind.
- [ADR-0032](../adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)
  — the console is the only HTTP surface.

---

## Context & Related Docs

- Children re-scoped by this design:
  [#7519](https://github.com/bobmatnyc/trusty-tools/issues/7519),
  [#7520](https://github.com/bobmatnyc/trusty-tools/issues/7520),
  [#7521](https://github.com/bobmatnyc/trusty-tools/issues/7521),
  [#7522](https://github.com/bobmatnyc/trusty-tools/issues/7522),
  [#7524](https://github.com/bobmatnyc/trusty-tools/issues/7524),
  [#7525](https://github.com/bobmatnyc/trusty-tools/issues/7525). DOC-74 §15.10
  lists each new scope.
- Bundled `secrets-manager` agent and `tm-secrets` skill
  ([#7526](https://github.com/bobmatnyc/trusty-tools/issues/7526),
  [#7527](https://github.com/bobmatnyc/trusty-tools/issues/7527)) move to the
  `set` grammar in S5.

---

**Document history:**
- **v1 (2026-10-01):** Initial draft from the owner's console-secrets rulings
  and answers of 2026-10-01.
