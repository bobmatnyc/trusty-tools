# Multi-runtime launcher — trusty-mpm sessions beyond Claude Code {#PRD-RUNTIMES-01}

**ID:** PRD-RUNTIMES-01  
**Status:** Draft — proposed; the epic is filed after this document and its spec merge.  
**Owner:** Product / Bob Matsuoka  
**Version:** v1  
**Last-updated:** 2026-10-05  
**Related specs/ADRs:** [DOC-81](../specs/DOC-81-multi-runtime-launcher.md) (the HOW), [ADR-0059](../adr/0059-canonical-agent-behavior-has-generated-host-adapters.md) (content adapters), [DOC-78](../specs/DOC-78-claude-code-mods-integration.md)  
**Epic:** not yet filed  
**Milestone:** `trusty-mpm 2.3.0 · runtimes`, due 2026-12-18 (proposed; does not exist yet)

---

## Problem & Context

trusty-mpm launches and drives one agent runtime: Claude Code. The `tm` CLI,
the daemon and the Architect assume it at every layer.

- The tmux pane command is a `claude` invocation built in
  [`core/model_inject.rs:326`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/model_inject.rs#L326).
- The pane is read with Claude Code's `❯` input-box glyph
  ([`core/input_box.rs:18`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/core/input_box.rs#L18)) and Claude Code's
  trust-dialog strings
  ([`session_manager/task_inject.rs:99`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/session_manager/task_inject.rs#L99)).
- Hooks, status line, resume and transcripts all use Claude Code's file formats
  and directories.

A trait already exists for this seam: `RuntimeAdapter`
([`runtime/mod.rs:101`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/mod.rs#L101)) with a two-variant
`RuntimeKind` ([`runtime/mod.rs:191`](https://github.com/bobmatnyc/trusty-tools/blob/de478389efe611699a05abdad4b9a0362de3cd9e/crates/trusty-mpm/src/runtime/mod.rs#L191)): `ClaudeCode`
and `Tcode`. The trait covers spawn, resume and a name. It does not cover
permission modes, hooks, idle detection, transcripts or config isolation. A new
runtime cannot be added by implementing it.

ADR-0059 already makes the host content layouts (`.claude/`, `.codex/`, tcode)
generated adapters. That covers what is deployed into a project. No ADR or spec
covers how a runtime is launched or driven.

The cost is concrete. An operator who wants Codex for one task launches it by
hand, outside `tm sessions ls`, resume, idle detection and the Architect's
view. [#7343](https://github.com/bobmatnyc/trusty-tools/issues/7343) asked for
this for Codex and is still open. The earlier harness-agnostic design,
[#1320](https://github.com/bobmatnyc/trusty-tools/issues/1320), is closed.

## Target Users / Personas

| User | Need |
|---|---|
| The operator (Bob) | Start a session on Codex, OpenCode or Gemini CLI from the same `tm` command, see it in `tm sessions ls`, resume it after a pane dies. |
| Fleet sessions | Run under the runtime that suits the task, without the daemon treating a non-Claude pane as broken. |
| The Architect | Poll, classify and inject into panes of any supported runtime, and know when a pane lacks a guard or an idle signal. |

## Goals & Non-Goals

### Goals

1. Launch, list, resume and inject into sessions of Codex, OpenCode and Gemini
   CLI through the existing `tm` surface.
2. Put every Claude Code control the launcher uses today behind one adapter
   interface, so the Claude path is one implementation of it.
3. Keep Claude Code the default runtime, byte-for-byte unchanged in behavior.
4. State, per runtime, what it cannot do and what the launcher does instead.
5. Replace guesses about pane appearance with captured evidence before an
   adapter relies on it.

### Non-Goals

- Cursor. It stays on milestone
  [cross-harness (#99)](https://github.com/bobmatnyc/trusty-tools/milestone/99)
  and epic [#7342](https://github.com/bobmatnyc/trusty-tools/issues/7342).
- Changing the default runtime. Claude Code stays the default.
- Generating agent, skill and instruction content for other hosts. That is
  ADR-0059 and the content epics
  ([#5418](https://github.com/bobmatnyc/trusty-tools/issues/5418),
  [#5421](https://github.com/bobmatnyc/trusty-tools/issues/5421),
  [#8619](https://github.com/bobmatnyc/trusty-tools/issues/8619)).
- Runtimes without tmux, such as `tagent` sessions
  ([#8208](https://github.com/bobmatnyc/trusty-tools/issues/8208)).
- Making a non-Claude runtime honor every Claude-only control. Where a runtime
  lacks one, the spec defines a degradation and the launcher reports it.

## Runtime ranking

Order of work: **Codex, OpenCode, Gemini CLI.** Both metrics were read on
2026-10-05.

| Runtime | GitHub stars | Star rank | npm downloads, week 2026-09-28 to 2026-10-04 | npm rank |
|---|---|---|---|---|
| OpenCode | [211,839](https://api.github.com/repos/sst/opencode) | 1 | [3,318,599](https://api.npmjs.org/downloads/point/last-week/opencode-ai) (`opencode-ai`) | 2 |
| Codex CLI | [127,918](https://api.github.com/repos/openai/codex) | 2 | [25,613,321](https://api.npmjs.org/downloads/point/last-week/@openai/codex) (`@openai/codex`) | 1 |
| Gemini CLI | [107,230](https://api.github.com/repos/google-gemini/gemini-cli) | 3 | [453,215](https://api.npmjs.org/downloads/point/last-week/@google/gemini-cli) (`@google/gemini-cli`) | 3 |
| Claude Code (reference) | [149,483](https://api.github.com/repos/anthropics/claude-code) | n/a | [14,786,283](https://api.npmjs.org/downloads/point/last-week/@anthropic-ai/claude-code) (`@anthropic-ai/claude-code`) | n/a |

Access date for every figure: 2026-10-05.

- Stars alone favor OpenCode.
- Averaging the two ranks ties Codex and OpenCode at 1.5.
- Two facts break the tie for Codex. Its npm volume is about 7.7 times
  OpenCode's. Tracker work already exists for it: #7343 (launch Codex beside
  Claude Code), #5418 (Codex custom agents) and ADR-0059's Codex layout.
- OpenCode's npm count understates its installs, because it also ships through
  brew and curl. The gap is large enough that the order holds.
- Gemini CLI is last on both metrics.

## Requirements

1. `tm session new --runtime <codex|opencode|gemini>` starts the runtime in a
   tmux pane, in the managed session's working directory.
2. The session appears in `tm sessions ls` with its runtime name and live state.
3. Resume restores the same conversation, or fails with an error that names
   why. It never resumes a different conversation.
4. Idle detection and prompt injection work for each runtime, from captured pane
   evidence.
5. An unknown runtime name, or a runtime whose binary probe fails, is an error.
   It never becomes `claude-code`.
6. Omitting the runtime still selects Claude Code.

The behavior contract is [DOC-81](../specs/DOC-81-multi-runtime-launcher.md).

## Success Metrics

Each is observable on a developer machine with the runtime installed.

| # | Measure | Observation |
|---|---|---|
| 1 | One live session per runtime | `tm sessions ls` shows a Codex, an OpenCode and a Gemini session, each live, each labeled with its runtime. |
| 2 | Resume | Kill the pane of each, resume it, and the runtime reports the prior conversation id. |
| 3 | Idle detection | A task injected into an idle pane of each runtime is submitted once. A busy pane is not injected into. |
| 4 | No Claude Code regression | The Claude Code command lines, env and settings written before and after the change are byte-identical in the golden tests, and the existing `trusty-mpm` suites pass unmodified. |
| 5 | No silent default | `--runtime nonesuch`, and a runtime whose binary is missing, exit non-zero with a message. |
| 6 | Evidence behind the cues | Every pane cue an adapter uses has a captured fixture and the runtime version it came from. |

## Scope & Out-of-Scope

In scope: the tmux launcher, daemon session lifecycle, resume, pane cues, the
`--runtime` flag and HTTP field, config isolation per launch.

Out of scope: Cursor, content generation for hosts, non-tmux sessions, making
`pm-guard` run on runtimes that have no equivalent hook (the spec records the
gap and requires the launcher to report it).

## Risks & Assumptions

- **Pane cues are unconfirmed.** For all three runtimes, no official source
  describes the idle prompt or input box. A dedicated capture task comes first.
- **Guard coverage differs.** `pm-guard` uses Claude Code's hook protocol.
  A runtime without a matching `PreToolUse` hook runs unguarded. The launcher
  must say so.
- **Runtimes change fast.** Versions on 2026-10-05: Codex rust-v0.160.0,
  OpenCode v1.18.34, Gemini CLI v0.62.0. Adapters record the version probed.
- **Persisted records.** Adding `RuntimeKind` variants means an older `tm`
  binary cannot read a newer record. The spec covers the rollback case.

## Placement

New milestone `trusty-mpm 2.3.0 · runtimes`, due 2026-12-18, one week after
`trusty-mpm 2.2.0` ([#125](https://github.com/bobmatnyc/trusty-tools/milestone/125),
due 2026-12-11). The milestone is created when the epic is filed.
`docs/roadmap/trusty-tools.md` is generated and picks it up from there.

## Linked Specs

- [DOC-81](../specs/DOC-81-multi-runtime-launcher.md): adapter interface,
  per-runtime gaps, migration, tests, sub-issue breakdown.
- [ADR-0059](../adr/0059-canonical-agent-behavior-has-generated-host-adapters.md):
  content adapters. DOC-81 covers the runtime-control adapter beside it.

## Related Issues

- [#7342](https://github.com/bobmatnyc/trusty-tools/issues/7342): cross-harness deploy epic (Claude, Codex, Cursor). Open; Cursor stays here.
- [#7343](https://github.com/bobmatnyc/trusty-tools/issues/7343): launch Codex beside Claude Code. The Codex sub-issue absorbs it.
- [#5418](https://github.com/bobmatnyc/trusty-tools/issues/5418): deploy agents as Codex custom agents.
- [#5421](https://github.com/bobmatnyc/trusty-tools/issues/5421): Agent Skills standard.
- [#7338](https://github.com/bobmatnyc/trusty-tools/issues/7338): Codex never called the MCP search tools.
- [#8619](https://github.com/bobmatnyc/trusty-tools/issues/8619): `tm generate agents-md`.
- [#1320](https://github.com/bobmatnyc/trusty-tools/issues/1320): closed; "Make MPM harness-agnostic: Codex, opencode, tcode". Moved to #7342 and #99.
- [#7941](https://github.com/bobmatnyc/trusty-tools/issues/7941): closed; tcode config compatibility with Claude, Codex, pi and opencode.
- No existing issue covers Gemini CLI.
