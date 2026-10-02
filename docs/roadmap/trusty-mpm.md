<!-- The block between BEGIN/END GENERATED is rebuilt from GitHub milestones by scripts/roadmap/generate.mjs. Everything outside it is hand-written and kept. -->

# trusty-mpm roadmap

trusty-mpm runs your coding sessions and keeps them going. The next two
releases make that dependable. The release after them, 2.0.0, adds a
supervisor that looks after all of your projects for you.

## What 2.0.0 brings

**A supervisor you create with one request.** Ask for a supervisor and
trusty-mpm sets one up: a small project of its own, kept as a local git
repository, watching the projects you choose. You can add or remove a
project later. It runs on the latest Opus model.

**Instructions made for supervising.** A normal trusty-mpm session is a
project manager that hands coding work to specialist agents. A supervisor
has a different job: it watches sessions, acts directly, and reports what it
saw and what it checked. It gets its own instructions and its own writing
style, so it carries none of the project-manager rules it never uses.

**Chat channels for your projects, starting with Slack.** A project can be
given a channel. When the supervisor needs your decision, it sends you the
question there, and your reply goes straight back to it. You no longer have
to be at the terminal to unblock it.

**Careful about who can talk to it.** Messages follow a routing table that
refuses anything not explicitly allowed. At first the only allowed sender and
recipient is the owner. An incoming message can answer a question the
supervisor asked, and nothing else: it cannot start work or run commands. If
two answers arrive, the first one counts.

**One supervisor for everything.** A single supervisor serves every project,
so the same question never reaches you twice.

## How we get there

1.7.1 finishes the reliability work already under way. 1.7.2 is bug fixes
only. 2.0.0 follows.

<!-- BEGIN GENERATED: roadmap -->

## Now

### 1.7.1

1.7.1 is a reliability release focused on session and account safety. tm now always acts on the exact tmux session it means, so it never touches another session with a similar name; worktree removal only happens once your work has actually landed; and `--user`/account pinning no longer falls back silently to your global account. It also fixes the supervisor's background launch behavior, so the LaunchAgent no longer runs tmux at background priority and your `tmux.alternate_screen` setting reaches Claude Code on every launch path.

6 of 50 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/92)

## Next

### 1.7.2

trusty-mpm 1.7.2 — bug fixes after 1.7.1. The release line is 1.7.1 (in flight) → 1.7.2 (bug fixes) → 2.0.0 (supervisor and channels platform).

0 of 59 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/96)

### 2.0.0

trusty-mpm 2.0.0 adds a supervisor: one session that watches all of your projects, handles what it can, and asks you about the rest. You create it with a single request, and it comes with its own instructions and writing style, made for watching and reporting instead of for running coding work. Each project can be given its own chat channel, starting with Slack, so the supervisor can send you a question and take your answer there. Incoming messages go through a routing table that refuses anything not explicitly allowed, and a message can only answer a question the supervisor actually asked. One supervisor serves every project, so the same question never reaches you twice.

0 of 8 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/97)

## Later

### cross-harness

Work so trusty-mpm can drive coding harnesses other than Claude Code — Codex and Cursor — each independently usable behind one orchestration layer, without breaking any harness already deployed.

0 of 7 items done · [follow on GitHub](https://github.com/bobmatnyc/trusty-tools/milestone/99)

<!-- END GENERATED: roadmap -->

## Also on the roadmap

These initiatives are not part of a trusty-mpm release. They are tracked
here because they change what trusty-mpm and the console do. States use the
issue-lifecycle vocabulary: not started, in-progress, merged.

### Console secrets service

Epic [#7517](https://github.com/bobmatnyc/trusty-tools/issues/7517) puts
secrets behind `tm secrets` and the console, with macOS Keychain first and
1Password, Keeper and others added one per PR. Specs:
[PRD-SECRETS-01](../prd/PRD-SECRETS-01-console-secrets.md) and
[DOC-74](../specs/DOC-74-secrets-integration.md).

Implementation starts after the search dashboard work: #9027, #9028, #9029
and the #9030 backend routes (owner ruling).

| Slice | Scope | State |
|---|---|---|
| S0 | PRD-SECRETS-01 and the DOC-74 amendment | in-progress |
| S1 | `trusty-secrets` crate with the Keychain backend | not started |
| S2 | tm daemon `secrets.*` UDS methods | not started |
| S3a | Console bridge and hardening | not started |
| S3b | Tailnet identity gate; also fixes [#9035](https://github.com/bobmatnyc/trusty-tools/issues/9035) | not started |
| S4 | Console `/tools/secrets` UI with the per-key "agents may use" flag | not started |
| S5 | Agent and skill text | not started |
| S6+ | Integrations, one per PR: 1Password, Keeper, Vercel push, GitHub Actions push, Doppler, AWS Secrets Manager | not started |
| S7 | `tm secrets exec --dotenv` | not started |
| S8 | Exec-granted `secrets.resolve` | not started |
| S9 | Rust client | not started |
| S10 | Python (PyPI) and npm clients, with publish workflows | not started |

### Instructional content outside the binary

Epic [#8378](https://github.com/bobmatnyc/trusty-tools/issues/8378) tracks
agents, skills, PM instructions and output styles with their own versions
and releases, apart from any binary release. The content moves to the
workspace-root `content/` tree
([ADR-0064](../adr/0064-instructional-content-tracked-separately-from-code.md)).

| Order | Phase | Issue | State |
|---|---|---|---|
| 1 | PR-A: content-release workflow and seed `content-v0.1.0` | [#8800](https://github.com/bobmatnyc/trusty-tools/pull/8800) | merged |
| 2 | PHASE_3: content resolver, `tm content` commands | [#8389](https://github.com/bobmatnyc/trusty-tools/issues/8389) | in-progress; PR-C [#8982](https://github.com/bobmatnyc/trusty-tools/pull/8982) in progress |
| 3 | PHASE_2: manifest, content version, schema major | [#8388](https://github.com/bobmatnyc/trusty-tools/issues/8388) | not started |
| 4 | PHASE_1: per-crate move-and-drop, five PRs | [#8387](https://github.com/bobmatnyc/trusty-tools/issues/8387) | not started |
| 5 | Republish wave | tracked under #8387 | not started |
| 6 | PHASE_4: trusty-code's own asset tree | [#8390](https://github.com/bobmatnyc/trusty-tools/issues/8390) | not started |

### Claude Code mods (after 2.0.0)

Investigation and design, after the architect release (2.0.0). Claude Code
2.1.287 adds mods: plugin code that runs inside the session, sees each tool
call and turn as it happens, and can draw a band or pane. trusty-mpm would use
a mod to watch sessions without reading the tmux screen, to show build slots
and architect messages inside the session, and to add a second, fail-closed
layer to pm-guard. Proposal: [DOC-78](../specs/DOC-78-claude-code-mods-integration.md)
(draft).

| Phase | Scope | State |
|---|---|---|
| 0–1 | Version gate at 2.1.287; observe-only event mod, replacing capture-pane scraping | not started |
| 2–3 | In-session band, toasts and `/tm` commands; additive pm-guard layer | not started |
