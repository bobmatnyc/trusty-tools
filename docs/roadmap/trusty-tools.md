<!-- The block between BEGIN/END GENERATED is rebuilt from GitHub milestones by scripts/roadmap/generate.mjs. Everything outside it is hand-written and kept. -->

# trusty-tools roadmap

This is the one roadmap for every crate in trusty-tools. The release plan
sets the order of the next releases, which come from trusty-mpm and
trusty-secrets. "Milestones by crate", further down, lists every open GitHub
milestone under the crate it belongs to.

## Release plan

trusty-mpm runs your coding sessions and keeps them going. Three releases
come before 2.0.0: one that makes the base lean and dependable, one for
secrets, and one that records every event on one event bus. 2.0.0 then adds
an architect that looks after all of your projects for you. A dashboard and
Claude Code mods support follow it.

trusty-mpm 1.8.0 shipped on 2026-10-10. Five releases remain, in this order.

| # | Release | What it delivers |
|---|---|---|
| 1 | trusty-mpm 1.8.0 (shipped 2026-10-10) | The base before the architect: content fully extracted, Unix sockets everywhere except the console, worktrees, a smaller starting context, and a pm-guard with only the checks it needs |
| 2 | trusty-secrets 0.1.0 | Secrets as a standalone, public crate with its own releases; trusty-mpm and the console use it |
| 3 | trusty-mpm 1.9.0 | Every event captured on one event bus, in the internal `trusty-events` crate, run as its own supervised daemon over a Unix socket; the console reads from it |
| 4 | trusty-mpm 2.0.0 | The architect |
| 5 | trusty-mpm 2.1.0 | The mpm dashboard, with list and tree views of events |
| 6 | trusty-mpm 2.2.0 | Claude Code mods support |

**1. trusty-mpm 1.8.0 — the base before the architect.** Agents, skills and
PM instructions leave the binaries and ship from `content/` with their own
versions. Every trusty service talks over a Unix socket; only the console
listens on the network. Worktrees are tracked in one machine-wide registry and
reclaimed once their work merges. A new session starts with much less context.
pm-guard keeps the checks that stop real harm, and the Claude Code sandbox
takes over the rest.

**2. trusty-secrets 0.1.0 — secrets, as their own crate.** `trusty-secrets`
works on its own, like trusty-memory: it has its own releases and is published
on crates.io. You keep project and owner secrets in it, starting with the macOS
Keychain, and manage them on the console's secrets page. Values stay out of
agent transcripts. trusty-mpm and the console use the crate; they do not
contain it.

**3. trusty-mpm 1.9.0 — every event on one bus.** trusty-mpm, trusty-code,
trusty-agents and trusty-analyze record what they do as typed events on one
event bus. The bus moves out of the console into `trusty-events`, a crate for
trusty components only, run as its own launchd-supervised daemon over a Unix
socket ([ADR-0065](../adr/0065-trusty-events-process-placement.md)). The
console reads from it. `trusty-events` is published to crates.io because it
ships its own binary, and ADR-0043 allows installs from the registry only. It
makes no stability promise to outside users, and no other crate links it.

**4. trusty-mpm 2.0.0 — the architect.** Anticipated ship date 2026-11-27, the
due date of the GitHub milestone "trusty-mpm 2.0.0" (#97). See "What 2.0.0
brings" below.

**5. trusty-mpm 2.1.0 — the mpm dashboard.** The console shows the event
stream as a scrolling list and as a call tree whose nodes appear while each
action runs. Any event opens the object it links to, and each view has a
screensaver mode.

**6. trusty-mpm 2.2.0 — Claude Code mods.** A trusty-mpm mod watches sessions
from inside Claude Code instead of reading the tmux screen, sends what it sees
to the event bus, shows status inside the session, and adds a second,
fail-closed layer to pm-guard.

## What 2.0.0 brings

**An Architect you create with one request.** Ask for an Architect and
trusty-mpm sets one up: a small project of its own, kept as a local git
repository, watching the projects you choose. You can add or remove a
project later. It runs on the latest Opus model.

**Instructions made for supervising.** A normal trusty-mpm session is a
project manager that hands coding work to specialist agents. An Architect
has a different job: it watches sessions, acts directly, and reports what it
saw and what it checked. It gets its own instructions and its own writing
style, so it carries none of the project-manager rules it never uses.

**Chat channels for your projects, starting with Slack.** A project can be
given a channel. When the Architect needs your decision, it sends you the
question there, and your reply goes straight back to it. You no longer have
to be at the terminal to unblock it.

**Careful about who can talk to it.** Messages follow a routing table that
refuses anything not explicitly allowed. At first the only allowed sender and
recipient is the owner. An incoming message can answer a question the
Architect asked, and nothing else: it cannot start work or run commands. If
two answers arrive, the first one counts.

**One Architect for everything.** A single Architect serves every project,
so the same question never reaches you twice.

## How we get there

trusty-mpm 1.8.0 shipped on 2026-10-10. Next come trusty-secrets 0.1.0 and
then trusty-mpm 1.9.0. The order of the releases from here is in the
"Release plan" section above.

## Milestones by crate

Every open milestone in the repository, grouped by crate. Crates in the
release plan come first, in release order; the others follow by name, and
milestones that belong to no single crate come last. A milestone with a
published summary shows it with its stage: Now, Next or Later. Other
milestones are listed as links. GitHub supplies the progress counts each time
this section is generated.

trusty-git-analytics and trusty-audit moved to their own repository (bobmatnyc/trusty-git-analytics).

Ship dates are anticipated targets taken from GitHub milestone due dates, not
commitments. TBD means a versioned milestone has no due date yet. Backlog
milestones carry no ship date.

<!-- BEGIN GENERATED: roadmap -->

### trusty-mpm

#### 1.8.0 · Now

trusty-mpm 1.8.0 is the base before the architect. Agents, skills and PM instructions leave the binaries and ship from content/ with their own versions. Every trusty service talks over a Unix socket; only the console listens on the network. Worktrees are tracked in one machine-wide registry and reclaimed once their work merges. A new session starts with much less context. pm-guard keeps the checks that stop real harm, and the Claude Code sandbox takes over the rest.

Anticipated ship date: 2026-11-13 · 61 of 117 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/111)

#### 1.9.0 · Next

trusty-mpm 1.9.0 records every event on one event bus. trusty-mpm, trusty-code, trusty-agents and trusty-analyze record what they do as typed events. The bus moves out of the console into trusty-events, a crate for trusty components only, served over a Unix socket. The console reads from it.

Anticipated ship date: 2026-11-20 · 1 of 6 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/124)

#### 2.0.0 · Next

trusty-mpm 2.0.0 adds an architect: one session that watches all of your projects, handles what it can, and asks you about the rest. You create it with a single request, and it comes with its own instructions and writing style, made for watching and reporting instead of for running coding work. Each project can be given its own chat channel, starting with Slack, so the architect can send you a question and take your answer there. Incoming messages go through a routing table that refuses anything not explicitly allowed, and a message can only answer a question the architect actually asked. One architect serves every project, so the same question never reaches you twice.

Anticipated ship date: 2026-11-27 · 8 of 22 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/97)

#### 2.1.0 · Later

trusty-mpm 2.1.0 is the mpm dashboard. The console shows the event stream as a scrolling list and as a call tree whose nodes appear while each action runs. Any event opens the object it links to, and each view has a screensaver mode.

Anticipated ship date: 2026-12-04 · 48 of 57 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/72)

#### 2.2.0 · Later

trusty-mpm 2.2.0 adds Claude Code mods support. A trusty-mpm mod watches sessions from inside Claude Code instead of reading the tmux screen, sends what it sees to the event bus, shows status inside the session, and adds a second, fail-closed layer to pm-guard.

Anticipated ship date: 2026-12-11 · 0 of 5 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/125)

#### cross-harness · Later

Work so trusty-mpm can drive coding harnesses other than Claude Code — Codex and Cursor — each independently usable behind one orchestration layer, without breaking any harness already deployed.

Anticipated ship date: 2026-11-27 · 1 of 10 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/99)

Other open milestones:

- [1.7.10 · bugfix](https://github.com/bobmatnyc/trusty-tools/milestone/122) · 2026-10-16 · 94 of 113 items done
- [Session, worktree & daemon lifecycle · mpm/core](https://github.com/bobmatnyc/trusty-tools/milestone/64) · 2026-11-13 · 71 of 83 items done
- [Backlog · mpm/core](https://github.com/bobmatnyc/trusty-tools/milestone/55) · Backlog · 737 of 774 items done

### trusty-secrets

#### 0.1.0 · Next

trusty-secrets 0.1.0 is secrets as a standalone, public crate with its own releases, like trusty-memory. You keep project and owner secrets in it, starting with the macOS Keychain, and manage them on the console's secrets page. Values stay out of agent transcripts. trusty-mpm and the console use the crate; they do not contain it.

Anticipated ship date: 2026-11-27 · 3 of 23 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/123)

### trusty-agents

Open milestones:

- [trusty agents mvp](https://github.com/bobmatnyc/trusty-tools/milestone/86) · 2026-10-23 · 22 of 29 items done
- [1.0 — assistant platform](https://github.com/bobmatnyc/trusty-tools/milestone/83) · 2026-10-30 · 36 of 40 items done
- [Backlog · agents](https://github.com/bobmatnyc/trusty-tools/milestone/73) · Backlog · 102 of 113 items done

### trusty-code

Open milestones:

- [0.7.1 · bugfix](https://github.com/bobmatnyc/trusty-tools/milestone/102) · 2026-10-23 · 2 of 5 items done
- [R1 · Reliable independent core](https://github.com/bobmatnyc/trusty-tools/milestone/54) · 2026-11-06 · 19 of 22 items done
- [R2 · Shared instructions, agents & skills](https://github.com/bobmatnyc/trusty-tools/milestone/59) · 2026-11-06 · 14 of 16 items done
- [v0.7.0 · Claude Code TUI parity + PM-delegated coding tasks](https://github.com/bobmatnyc/trusty-tools/milestone/87) · 2026-11-06 · 29 of 44 items done
- [Backlog · code](https://github.com/bobmatnyc/trusty-tools/milestone/76) · Backlog · 15 of 20 items done

### trusty-common

Open milestones:

- [0.53.4 · bugfix](https://github.com/bobmatnyc/trusty-tools/milestone/126) · 2026-10-16 · 0 of 3 items done

### trusty-console

Open milestones:

- [0.12.1 · bugfix](https://github.com/bobmatnyc/trusty-tools/milestone/104) · 2026-10-09 · 1 of 2 items done
- [Backlog · console](https://github.com/bobmatnyc/trusty-tools/milestone/77) · Backlog · 21 of 24 items done

### trusty-mcp

Open milestones:

- [Backlog · mcp](https://github.com/bobmatnyc/trusty-tools/milestone/80) · Backlog · 4 of 7 items done

### trusty-memory

Open milestones:

- [0.28.4 · bugfix](https://github.com/bobmatnyc/trusty-tools/milestone/127) · 2026-10-16 · 1 of 6 items done
- [Backlog · memory (triaged)](https://github.com/bobmatnyc/trusty-tools/milestone/81) · Backlog · 20 of 32 items done

### trusty-review

Open milestones:

- [0.38.0 · feature](https://github.com/bobmatnyc/trusty-tools/milestone/128) · 2026-10-16 · 0 of 2 items done

### trusty-search

Open milestones:

- [0.54.8 · bugfix](https://github.com/bobmatnyc/trusty-tools/milestone/129) · 2026-10-16 · 6 of 7 items done
- [0.54.9 · feature](https://github.com/bobmatnyc/trusty-tools/milestone/130) · 2026-10-16 · 3 of 4 items done
- [Backlog · search](https://github.com/bobmatnyc/trusty-tools/milestone/58) · Backlog · 72 of 77 items done

### Across the workspace

Open milestones:

- [Issue management](https://github.com/bobmatnyc/trusty-tools/milestone/94) · 2026-11-06 · 11 of 14 items done
- [Backlog · analyze/review](https://github.com/bobmatnyc/trusty-tools/milestone/74) · Backlog · 15 of 24 items done
- [Backlog · optimize](https://github.com/bobmatnyc/trusty-tools/milestone/121) · Backlog · 18 of 32 items done

<!-- END GENERATED: roadmap -->

## Also on the roadmap

These initiatives each belong to a release in the plan above. They are
tracked here in more detail because they change what trusty-mpm and the
console do. States use the issue-lifecycle vocabulary: not started,
in-progress, merged.

### Instructional content outside the binary (release 1)

Epic [#8378](https://github.com/bobmatnyc/trusty-tools/issues/8378) tracks
agents, skills, PM instructions and output styles with their own versions
and releases, apart from any binary release. The content moves to the
workspace-root `content/` tree
([ADR-0064](../adr/0064-instructional-content-tracked-separately-from-code.md)).
"Content fully extracted" is part of trusty-mpm 1.8.0: every phase below is
done before that release.

| Order | Phase | Issue | State |
|---|---|---|---|
| 1 | PR-A: content-release workflow and seed `content-v0.1.0` | [#8800](https://github.com/bobmatnyc/trusty-tools/pull/8800) | merged |
| 2 | PHASE_3: content resolver, `tm content` commands | [#8389](https://github.com/bobmatnyc/trusty-tools/issues/8389) | in-progress; PR-C [#8982](https://github.com/bobmatnyc/trusty-tools/pull/8982) merged |
| 3 | PHASE_2: manifest, content version, schema major | [#8388](https://github.com/bobmatnyc/trusty-tools/issues/8388) | merged |
| 4 | PHASE_1: per-crate move-and-drop, five PRs | [#8387](https://github.com/bobmatnyc/trusty-tools/issues/8387) | in-progress; first PR [#9011](https://github.com/bobmatnyc/trusty-tools/issues/9011) (trusty-agents-common) in progress |
| 5 | Republish wave | tracked under #8387 | not started |
| 6 | PHASE_4: trusty-code's own asset tree | [#8390](https://github.com/bobmatnyc/trusty-tools/issues/8390) | not started |

### trusty-secrets (release 2)

Epic [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517) becomes
`trusty-secrets` 0.1.0: a standalone crate like trusty-memory, with its own
releases, published on crates.io as a public crate. trusty-mpm (`tm secrets`)
and the console secrets page use it; neither contains it. macOS Keychain comes
first; 1Password, Keeper and others are added one per PR. Specs:
[PRD-SECRETS-01](../prd/PRD-SECRETS-01-console-secrets.md) and
[DOC-74](../specs/DOC-74-secrets-integration.md); both still describe the
service as hosted by the tm daemon and will be updated for the standalone
crate.

Implementation starts after the search dashboard work: #9027, #9028, #9029
and the #9030 backend routes (owner ruling). Those are now merged.

| Slice | Scope | State |
|---|---|---|
| S0 | PRD-SECRETS-01 and the DOC-74 amendment | in-progress |
| S1 | `trusty-secrets` crate (standalone, published) with the Keychain backend | not started |
| S2 | trusty-secrets on-demand socket serving `secrets.*`; tm daemon as client | not started |
| S3a | Console bridge and hardening | not started |
| S3b | Tailnet identity gate; also fixes [#9035](https://github.com/bobmatnyc/trusty-tools/issues/9035) | not started |
| S4 | Console `/tools/secrets` UI with the per-key "agents may use" flag | not started |
| S5 | Agent and skill text | not started |
| S6+ | Integrations, one per PR: 1Password, Keeper, Vercel push, GitHub Actions push, Doppler, AWS Secrets Manager | not started |
| S7 | `tm secrets exec --dotenv` | not started |
| S8 | Exec-granted `secrets.resolve` | not started |
| S9 | Rust client | not started |
| S10 | Python (PyPI) and npm clients, with publish workflows | not started |

### Claude Code mods (release 6, trusty-mpm 2.2.0)

The last release in the plan, after the architect (2.0.0) and the dashboard
(2.1.0). Claude Code 2.1.287 adds mods: plugin code that runs inside the
session, sees each tool call and turn as it happens, and can draw a band or
pane. trusty-mpm would use a mod to watch sessions without reading the tmux
screen, to send what it sees to the event bus, to show build slots and
architect messages inside the session, and to add a second, fail-closed layer
to pm-guard. Proposal: [DOC-78](../specs/DOC-78-claude-code-mods-integration.md)
(draft). The open pm-guard bypass by user-installed mods is
[#9057](https://github.com/bobmatnyc/trusty-tools/issues/9057).

| Phase | Scope | State |
|---|---|---|
| 0–1 | Version gate at 2.1.287; observe-only event mod, replacing capture-pane scraping | not started |
| 2–3 | In-session band, toasts and `/tm` commands; additive pm-guard layer | not started |
