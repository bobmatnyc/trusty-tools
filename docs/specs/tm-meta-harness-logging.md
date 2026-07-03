# tm Meta-Harness Logging — Per-Delegation Observability, Verbosity CLI, and Log Pruning

**DOC-33** | Status: `Draft` | Date: 2026-07-03

**Status:** Draft
**Version:** v1
**Subsystem:** METALOG
**Owner:** Engineering / Observability (trusty-mpm)
**Last-updated:** 2026-07-03
**Related:** [DOC-30-project-manager-vision.md](./DOC-30-project-manager-vision.md), [tool-output-interception-seam.md](./tool-output-interception-seam.md)

> **⚠️ This is a design doc only. It has not been approved for implementation.**
> No code changes accompany this document. See [§ Decision & Next Steps](#decision--next-steps)
> for the approval gate and [§ Follow-up Implementation Issues](#follow-up-implementation-issues)
> for what would be filed once approved.

## Summary

Issue #1965 asks for **meta-harness logging** in `tm` (trusty-mpm) — observability
into `tm`'s own orchestration layer, explicitly *not* a replication of Claude Code's
internal logging. Four requirements:

1. Per-delegation INFO-level log fields: agent, model, tokens in/out, response time,
   errors.
2. A first-class `--logging`/`-v`/`-d` CLI flag design for the `tm` binary.
3. A design for propagating `tm`'s log-level setting into the native Claude Code
   sessions it spawns and manages.
4. A default 30-day log-pruning policy.

**Prior state (this investigation confirmed by reading the code, not assuming):**
`tm` today has zero per-delegation logging — the `agent_delegate` MCP tool is a
circuit-breaker tracking gate, not a log emission point (already corrected in
#1942/#1948). Log files rotate daily via `tracing-appender` but are never pruned;
they accumulate in `~/.trusty-mpm/logs/` forever. There is no top-level verbosity
flag on the `tm` CLI — tracing is only initialized for long-running daemon/supervisor
subcommands, and short-lived CLI invocations skip subscriber init entirely. No
mechanism exists anywhere in the codebase to propagate a harness's log-level setting
into a spawned Claude Code subprocess's verbosity — requirement 3 is genuinely
net-new design work, flagged as speculative where appropriate below.

## Contents

- [Problem](#problem)
- [Current State (as read from code)](#current-state-as-read-from-code)
- [Design 1: Per-Delegation INFO Log Fields](#design-1-per-delegation-info-log-fields-spec-metalog-01draft)
- [Design 2: `--logging` / `-v` / `-d` CLI Flag](#design-2---logging---v---d-cli-flag-spec-metalog-02draft)
- [Design 3: Verbosity Propagation to Native Claude Code Sessions](#design-3-verbosity-propagation-to-native-claude-code-sessions-spec-metalog-03draft)
- [Design 4: 30-Day Log Pruning](#design-4-30-day-log-pruning-spec-metalog-04draft)
- [Decision & Next Steps](#decision--next-steps)
- [Follow-up Implementation Issues](#follow-up-implementation-issues)
- [References](#references)

## Problem

`tm` orchestrates delegations to sub-agents and spawns/manages native Claude Code
sessions in tmux panes, but an operator debugging a stuck or misbehaving session has
no structured record of: which agent ran, which model served the call, how many
tokens went in/out, how long it took, or whether it errored. The only existing
structured event sink (`crates/trusty-common/src/error_capture/`) captures *errors*
only, as a JSONL ring tied to `tracing::error!` events — there is no equivalent for
successful delegation completions, which are the common case an operator needs to
audit for cost/latency/model-selection questions, not just failures.

Separately, `tm`'s CLI has no verbosity knob at all: daemon-mode tracing is
hand-initialized in `main.rs` with a fixed layer stack, and non-daemon subcommands
(`tm session start`, `tm launch`, etc.) never call any tracing-init function, so
`tracing::info!`/`debug!` calls anywhere in those code paths are silently dropped.
This makes any of the requirement-1 log fields unobservable outside daemon mode
until requirement 2 is designed alongside it.

## Current State (as read from code)

This section is the load-bearing factual basis for the designs below — every
subsequent recommendation cites back to one of these findings.

| # | Finding | Location |
|---|---------|----------|
| 1 | Shared tracing-init helpers (`init_tracing`, `init_tracing_with_buffer`, `init_tracing_with_buffer_and_capture`) live in `trusty-common`, not `tm`-specific. Verbosity ladder: `0=warn, 1=info, 2=debug, 3+=trace`; `RUST_LOG` env override always wins. | `crates/trusty-common/src/tracing_init.rs` |
| 2 | `tm`'s actual daemon-mode init is a separate, hand-built 3-layer `tracing_subscriber::registry()` (stderr fmt + daily-rotating file fmt + bug-capture layer) — it does **not** reuse the `trusty-common` helpers directly. Only runs for `Command::Daemon`/`Command::Supervisor`; all other subcommands skip init entirely. | `crates/trusty-mpm/src/bin/tm/main.rs` (~lines 140-190) |
| 3 | `tm`'s clap `Cli` struct has exactly one `global = true` arg today (`url`). No verbosity flag exists. | `crates/trusty-mpm/src/bin/tm/cli.rs` (`Cli` struct, ~line 45) |
| 4 | The MCP `agent_delegate` tool is a circuit-breaker tracking gate only — confirmed by #1942/#1948 — it never calls an LLM and is not a logging seam. | `crates/trusty-mpm/src/daemon/mcp_backend.rs:113` `agent_delegate()`; dispatch at `crates/trusty-mpm/src/mcp/mod.rs:394` |
| 5 | The **real** delegation-completion path with usable stats is `SessionManagerAgent::decompose()`, which calls `resolved.provider.complete(req).await` and gets back an `LlmResponse` already carrying `model`, `input_tokens`, `output_tokens`, `latency_ms`. | `crates/trusty-mpm/src/core/sm/agent/delegate/mod.rs` (lines 294-345, `.complete()` call ~330-334); `LlmResponse` struct at `crates/trusty-mpm/src/core/sm/providers/mod.rs:177` |
| 6 | Log files land at `~/.trusty-mpm/logs/trusty-mpm.log.YYYY-MM-DD` via `tracing_appender::rolling::daily`. No pruning exists anywhere in the codebase today. | `crates/trusty-mpm/src/bin/tm/main.rs` (~lines 148-155) |
| 7 | An existing structured JSONL sink pattern already exists for errors: `ErrorStore::open(app_name, capacity)` resolves `<data_dir>/<app_name>/errors.jsonl`, appends one JSON object per line via `ErrorStore::append`. `CapturedError` fields: `timestamp_secs, crate_target, crate_version, message, fields, file, line, os, arch` (+ fingerprint). | `crates/trusty-common/src/error_capture/{types.rs,store.rs,layer.rs}`; `ERRORS_FILENAME = "errors.jsonl"` at `store.rs:37` |
| 8 | The single source of truth for the `claude` CLI invocation string used by tmux-backed sessions is `build_claude_command`. It always appends `--setting-sources project,local` and `--dangerously-skip-permissions`. The string is sent into the tmux pane via `TmuxBackend::new()` → `driver.send_line()`. | `crates/trusty-mpm/src/core/model_inject.rs:99`; `crates/trusty-mpm/src/control/backend/tmux.rs` (~lines 112-120); `crates/trusty-mpm/src/daemon/tmux.rs` |
| 9 | A **separate, non-tmux** backend already spawns `claude -p --output-format stream-json [--append-system-prompt-file <f>] --workdir <dir>` directly via `std::process::Command`, proving the flag works end-to-end for programmatic/API-driven sessions. This is a different code path from the interactive tmux one. | `crates/trusty-mpm/src/control/backend/stream_json.rs` (`build_claude_command`, ~line 63) |
| 10 | Two independent config surfaces exist: (a) `~/.trusty-mpm/config.toml` via `MpmConfig::load`/`load_default`, falls back to defaults silently; (b) `~/.trusty-tools/trusty-mpm/config.yaml`, MCP-exposed via `config_read`/`config_write`, currently holding only `workspace_root_template`, `auto_resume`, `default_model`. | `crates/trusty-mpm/src/core/config.rs` (struct ~line 291, loader ~line 369); `crates/trusty-mpm/src/daemon/mcp_console.rs` (`config_read` ~line 180, `config_write` ~line 194); wired in `crates/trusty-mpm/src/mcp/mod.rs` (~lines 264, 274, 460-466) |

## Design 1: Per-Delegation INFO Log Fields {#SPEC-METALOG-01~draft}

**ID:** SPEC-METALOG-01~draft
**Status:** Draft

### Behavior Contract (WHAT)

- **Inputs:** A completed (successful or failed) call to `resolved.provider.complete(req)`
  inside `SessionManagerAgent::decompose()` (Finding 5).
- **Outputs:** Exactly one structured log event per delegation attempt, emitted at
  `tracing::info!` level on success and `tracing::warn!` (or `error!`, see Open
  Question below) on failure, carrying these fields:

  | Field | Type | Source |
  |-------|------|--------|
  | `agent` | string | The SM agent identity/role performing the delegation (available in `decompose()`'s calling context — the SM-8 loop's current agent, not the MCP `agent_delegate` gate's tracking record) |
  | `model` | string | `LlmResponse.model` |
  | `tokens_in` | u32 | `LlmResponse.input_tokens` |
  | `tokens_out` | u32 | `LlmResponse.output_tokens` |
  | `response_time_ms` | u64 | `LlmResponse.latency_ms` |
  | `error` | `Option<String>` | `None` on success; the `DelegationError::Degraded` message (or its source) on failure |
  | `session_id` | string | Whatever session/delegation identifier `decompose()` already has in scope, so log lines can be correlated back to a `tm session status` entry |

- **Preconditions:** A tracing subscriber must be initialized in the process that
  runs `decompose()` — today this is the daemon process, which already initializes
  tracing (Finding 2), so no new init call is needed for this design in isolation.
  Design 2 (CLI verbosity) is what makes these events visible in non-daemon
  invocations, and is a *separate* concern from emitting them in the first place.
- **Postconditions:** Every delegation attempt produces exactly one log line,
  regardless of success/failure, so `grep`-ing `~/.trusty-mpm/logs/` for a
  `session_id` yields a complete audit trail of that session's delegation history.
- **Error conditions:** Emitting the log event itself must be infallible — it is a
  `tracing::info!`/`warn!` macro call, not a fallible I/O operation, so there is no
  new failure mode introduced by this design. If `LlmResponse` is unavailable
  because `.complete()` itself returned `Err` before producing a response, the log
  event still fires (in the `.map_err` arm) with `tokens_in`/`tokens_out`/
  `response_time_ms` as `None`/`0` and `error` populated.

### Emission Point (WHERE)

Instrument `SessionManagerAgent::decompose()` at two points (Finding 5, lines
~330-334 and its `.map_err` arm):

```rust
// crates/trusty-mpm/src/core/sm/agent/delegate/mod.rs (illustrative — not to be
// implemented by this design doc; see Decision & Next Steps)
match resolved.provider.complete(req).await {
    Ok(resp) => {
        tracing::info!(
            target: "tm::delegation",
            agent = %current_agent_id,
            model = %resp.model,
            tokens_in = resp.input_tokens,
            tokens_out = resp.output_tokens,
            response_time_ms = resp.latency_ms,
            session_id = %session_id,
            error = tracing::field::Empty,
            "delegation completed"
        );
        // ... existing success path
    }
    Err(e) => {
        tracing::warn!(
            target: "tm::delegation",
            agent = %current_agent_id,
            model = tracing::field::Empty,
            tokens_in = tracing::field::Empty,
            tokens_out = tracing::field::Empty,
            response_time_ms = tracing::field::Empty,
            session_id = %session_id,
            error = %e,
            "delegation failed"
        );
        // ... existing error path (DelegationError::Degraded)
    }
}
```

Using a dedicated `target: "tm::delegation"` lets an `EnvFilter` directive
(`RUST_LOG=tm::delegation=info,warn=warn`) or a dedicated `tracing_subscriber::Layer`
select these events independently of general application logs — the same pattern
`BugCaptureLayer` already uses for `errors.jsonl` (Finding 7), so a follow-up
`DelegationLogLayer` could tap `target: "tm::delegation"` events into a parallel
`delegations.jsonl` file without inventing a new mechanism.

### Rationale (WHY)

Reuses the `LlmResponse` struct that already carries every field the issue asks
for — no new data plumbing between the provider call and the log site, only a
`tracing::info!`/`warn!` call at the point where the data is already in scope.
Explicitly *not* instrumenting the `agent_delegate` MCP gate (Finding 4) avoids
repeating the #1942 confusion between "tracking gate" and "delegation path."

### Open Question — flag for reviewer

Should a failed delegation log at `warn!` or `error!`? `error!` would route it
through the existing `BugCaptureLayer`/`errors.jsonl` sink automatically (Finding 7),
which may be desirable (single place to look for delegation failures) or may be
noise (delegation failures may be routine/retried, not bug-worthy). This doc takes
no position — flag for review before implementation.

## Design 2: `--logging` / `-v` / `-d` CLI Flag {#SPEC-METALOG-02~draft}

**ID:** SPEC-METALOG-02~draft
**Status:** Draft

### Behavior Contract (WHAT)

- **Inputs:** Zero or more of `-v` (repeatable), `-d` (shorthand for debug), or
  `--logging <LEVEL>` (explicit level: `off|error|warn|info|debug|trace`), passed
  to any `tm` subcommand — not just `Command::Daemon`/`Command::Supervisor`.
- **Outputs:** A `tracing_subscriber::EnvFilter`-backed subscriber is initialized
  for **every** `tm` invocation, not only daemon/supervisor mode, before any
  subcommand body executes.
- **Preconditions:** None beyond normal CLI parsing.
- **Postconditions:** `tracing::info!`/`debug!` calls anywhere in `tm`'s code
  (including Design 1's delegation events) are observable from a plain
  `tm session start` or `tm launch` invocation when the flag is passed, not only
  from the daemon's log file.
- **Error conditions:** Conflicting flags (e.g. `-v -v --logging warn`) resolve by
  explicit-flag-wins: `--logging <LEVEL>` takes precedence over `-v`/`-d` count if
  both are given, matching the general CLI convention that explicit values beat
  repeated-flag counting. `RUST_LOG` continues to win over both (Finding 1) —
  unchanged, since operators already rely on `RUST_LOG` for ad-hoc debugging.

### CLI Surface

Add to `Cli` (Finding 3, `crates/trusty-mpm/src/bin/tm/cli.rs`) as a new
`global = true` field group, mirroring the existing `url` global arg:

```rust
// illustrative — not to be implemented by this design doc
#[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count, global = true)]
pub verbose: u8,

#[arg(short = 'd', long = "debug", global = true, conflicts_with = "logging")]
pub debug: bool,

#[arg(long = "logging", global = true, value_enum)]
pub logging: Option<LogLevel>,
```

Where `LogLevel` is a `clap::ValueEnum` mirroring `trusty-common`'s existing
`0=warn,1=info,2=debug,3+=trace` ladder (Finding 1) plus an explicit `off`/`error`
at the bottom, so `--logging` can express strictly more than the repeated-`-v`
count can. `-d` is sugar for `--logging debug`.

### Wiring

`main.rs`'s tracing-init block (Finding 2) currently only runs inside the
daemon/supervisor branch. This design proposes moving subscriber initialization
to the top of `main()`, before the `match cli.command` dispatch, reusing
`trusty-common::tracing_init::init_tracing`-family helpers (Finding 1) for
non-daemon invocations (stderr-only, no file layer — short-lived CLI commands
don't need a rotating log file) and keeping the existing 3-layer daemon-mode
init for `Command::Daemon`/`Command::Supervisor` (which additionally need the
file + bug-capture layers). The resolved verbosity level (from `-v`/`-d`/
`--logging`, defaulting to the existing `warn` baseline if none given) is passed
into whichever init path is selected, so both paths share one source of truth
for "how verbose is this run."

### Rationale (WHY)

`trusty-common::tracing_init` already implements exactly this verbosity ladder
(Finding 1) — this design is "wire the existing helper up to a CLI flag and make
sure it runs for every subcommand," not "invent a new logging framework."

## Design 3: Verbosity Propagation to Native Claude Code Sessions {#SPEC-METALOG-03~draft}

**ID:** SPEC-METALOG-03~draft
**Status:** Draft — **speculative; needs live validation before implementation**

### Problem Restated

Requirement 3 asks that `tm`'s INFO/DEBUG log-level setting "trigger the same for
claude code as well." Claude Code itself has no public verbosity flag equivalent to
`RUST_LOG` — the closest lever available to `tm` is `--output-format stream-json`,
which changes Claude Code's *output shape* (structured JSON events on stdout instead
of the interactive TUI) rather than a true log-verbosity toggle. This is confirmed
by Finding 9: a non-tmux backend (`stream_json.rs`) already constructs exactly this
invocation for programmatic sessions, proving the flag is wired and working
end-to-end for that code path today — it is not a new capability, only a new
*condition* under which the tmux-backed path (Finding 8) would also use it.

### Proposed Design

- **Inputs:** `tm`'s resolved log level (from Design 2) at the point where
  `build_claude_command` (Finding 8, `model_inject.rs:99`) constructs the launch
  string for a tmux-backed native session.
- **Outputs:** When `tm`'s resolved level is `debug` or higher, `build_claude_command`
  conditionally appends `--output-format stream-json` to the constructed command,
  mirroring what `stream_json.rs`'s backend already does unconditionally for its
  own sessions (Finding 9). At `info` or below, the command is unchanged from
  today's behavior (interactive TUI, no `stream-json`).
- **Preconditions:** `tm` must already be resolving a log level per-invocation
  (Design 2) before this conditional can exist.
- **Postconditions:** In DEBUG mode, the native Claude Code session emits
  structured JSON events on stdout, which the tmux driver (Finding 8,
  `crates/trusty-mpm/src/daemon/tmux.rs`) is *already reading* pane output from —
  this doubles as a data source for Design 1's `model`/`tokens_in`/`tokens_out`
  fields if/when `tm` wants to correlate its own delegation stats against the
  underlying Claude Code session's own token usage, not just the SM's internal
  provider calls.
- **Error conditions:** If Claude Code's `stream-json` output changes the
  interactive experience in a way an operator watching the tmux pane live
  finds disruptive (this is the "live validation" caveat below), this design
  should gate the behavior behind an explicit opt-in (e.g. only at `trace` level,
  not `debug`) rather than `debug`, or behind a separate flag entirely
  (`--propagate-verbosity`) so DEBUG logging and stream-json output are
  independently controllable. **This doc does not resolve that tradeoff — it
  requires a live spike against a real tmux-backed session, watching what the
  operator-facing pane looks like with `stream-json` on, before committing to
  "DEBUG implies stream-json" as the default.**

### Explicitly Flagged as Speculative

Per the issue's own framing ("Flag clearly if this is speculative/needs live
validation"): everything in this section beyond Finding 9 (which is empirically
confirmed code, not speculation) is a proposal, not a decision. The core open
questions are:

1. Does injecting `--output-format stream-json` into the **tmux-backed** launch
   path (as opposed to the already-working non-tmux `stream_json.rs` path)
   change any other behavior an interactive operator relies on (e.g. does
   `stream-json` disable some of Claude Code's own interactive affordances that
   `tm session tui` or `tm coordinator` depend on scraping from the pane)?
2. Is `--output-format stream-json` actually a verbosity control, or purely an
   output-shape control? If Claude Code's own internal debug logging is a
   separate, independent concept with no CLI flag, requirement 3 may only ever
   be achievable in the "richer structured data available for `tm` to parse"
   sense (which is still valuable, and is what this design delivers), not in a
   literal "Claude Code's own verbosity increases" sense. This doc takes the
   position that the former (richer structured data) is the achievable
   interpretation of requirement 3, and that `tm` should not claim to control
   Claude Code's actual internal log verbosity, which it does not have a
   documented lever for.

## Design 4: 30-Day Log Pruning {#SPEC-METALOG-04~draft}

**ID:** SPEC-METALOG-04~draft
**Status:** Draft

### Behavior Contract (WHAT)

- **Inputs:** The daemon process starting up (Finding 6 — this is where the log
  directory and file appender are already constructed).
- **Outputs:** Any file matching `~/.trusty-mpm/logs/*.log.*` (the
  `tracing-appender` daily-rotation naming convention already in use) whose
  mtime is older than 30 days is deleted. Runs once per daemon startup, not on a
  background timer — matching the "automatic sweep on daemon startup" framing in
  the issue, and avoiding a second always-running timer thread for a task that
  only needs to run roughly once a day (daemon restarts are frequent enough in
  practice, per the existing connection-safe-restart convention in CLAUDE.md, that
  a startup-time sweep is sufficient).
- **Preconditions:** `~/.trusty-mpm/logs/` exists (already guaranteed by Finding 6's
  `create_dir_all` call, which runs immediately before this sweep would).
- **Postconditions:** Disk usage under `~/.trusty-mpm/logs/` is bounded to
  approximately 30 days of daily-rotated files, regardless of how long the daemon
  has been running historically.
- **Error conditions:** Pruning must be fail-open and non-fatal — a permission
  error or I/O error deleting one stale file must not prevent daemon startup or
  block deletion of other stale files. Log (at `warn!`) and continue on a
  per-file basis; never `panic!`/`unwrap()` (per CLAUDE.md's no-`unwrap()`-in-library-code
  rule, and this is arguably binary code but the same discipline applies to
  startup-path robustness).

### Single Canonical Retention Window — Resolving the 30-day-vs-48-hour Ambiguity

The issue's prior-research section flags that the sibling `claude-mpm` (Python)
project has **two conflicting retention concepts in the same codebase**: a
`DEFAULT_ARCHIVED_MAX_AGE_DAYS = 30` constant and a separate, newer 48-hour
retention system. This design explicitly does **not** import that ambiguity:
`tm` has exactly one log-file retention window (30 days), applying uniformly to
every `~/.trusty-mpm/logs/*.log.*` file regardless of its rotation reason. If a
future need arises for a *shorter* retention tier (e.g. a 48-hour "hot" window
for a different artifact class, such as the error-capture JSONL ring or session
transcripts), it must be a **separate, explicitly named** constant and directory
scope — never silently overloading the same 30-day constant or the same `logs/`
directory glob with two different meanings. This doc takes the position that
`tm`'s rotated `trusty-mpm.log.*` files and any future "hot" artifact class are
different retention domains and should never share one ambiguous constant.

### Sketch (illustrative — not to be implemented by this design doc)

```rust
// crates/trusty-mpm/src/bin/tm/main.rs, near the existing log_dir setup
// (Finding 6)
const LOG_RETENTION_DAYS: u64 = 30;

fn prune_stale_logs(log_dir: &Path, retention_days: u64) {
    let cutoff = SystemTime::now() - Duration::from_secs(retention_days * 86_400);
    let Ok(entries) = std::fs::read_dir(log_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_rotated_log = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.contains(".log."));
        if !is_rotated_log {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff {
                    if let Err(e) = std::fs::remove_file(&path) {
                        tracing::warn!(path = %path.display(), error = %e, "failed to prune stale log file");
                    }
                }
            }
        }
    }
}
```

### Rationale (WHY)

Matches the issue's explicit ask ("Default log pruning after 30 days") and the
"automatic sweep of `~/.trusty-mpm/logs/*.log.*` on daemon startup" framing in
the deliverable description, while deliberately avoiding the claude-mpm
ambiguity by keeping exactly one constant, one glob scope, and one enforcement
point.

## Decision & Next Steps

**No implementation work is authorized by this document.** This doc is a design
proposal responding to issue #1965's investigation-and-design-doc deliverable. It
requires explicit human approval (Bob) before any of the four designs above are
turned into implementation tickets or code changes. Design 3 in particular carries
an open empirical question (does `stream-json` change the interactive tmux
experience in an undesirable way?) that should be spiked/validated live before
committing to the "DEBUG implies stream-json" default proposed above.

## Follow-up Implementation Issues

**Not filed by this investigation** — to be filed only after the design above is
reviewed and approved:

1. **Design 1**: Instrument `SessionManagerAgent::decompose()`
   (`crates/trusty-mpm/src/core/sm/agent/delegate/mod.rs`) with the
   `tm::delegation`-targeted `tracing::info!`/`warn!` events described above.
   Resolve the "Open Question" (warn vs. error on failure) during ticket triage.
2. **Design 1 (optional extension)**: A `DelegationLogLayer` mirroring
   `BugCaptureLayer`'s JSONL-sink pattern (Finding 7), tapping `target:
   "tm::delegation"` events into a parallel `delegations.jsonl`, if plain
   `tracing::info!` text lines prove insufficient for downstream tooling
   (`tm session status`, dashboards) to consume.
3. **Design 2**: Add `-v`/`-d`/`--logging` to `Cli`
   (`crates/trusty-mpm/src/bin/tm/cli.rs`), move subscriber init to the top of
   `main()` for all subcommands, reusing `trusty-common::tracing_init` helpers
   for the non-daemon case.
4. **Design 3 (spike first)**: Live-validate whether appending
   `--output-format stream-json` to the tmux-backed `build_claude_command`
   output (`crates/trusty-mpm/src/core/model_inject.rs:99`) changes the
   operator-facing tmux pane experience in a disruptive way, before deciding
   whether it gates on `debug`, `trace`, or a dedicated
   `--propagate-verbosity` flag. Only file the actual wiring ticket once this
   spike resolves the open question in [§ Design 3](#design-3-verbosity-propagation-to-native-claude-code-sessions-spec-metalog-03draft).
5. **Design 4**: Implement `prune_stale_logs` (or equivalent) in
   `crates/trusty-mpm/src/bin/tm/main.rs`'s daemon-startup path, with the
   30-day constant named distinctly from any future shorter-retention artifact
   class (see [§ Single Canonical Retention Window](#single-canonical-retention-window--resolving-the-30-day-vs-48-hour-ambiguity)).
6. **Config surface extension (supports Design 2)**: Decide whether the
   resolved log level should also be persistable via the existing
   `~/.trusty-tools/trusty-mpm/config.yaml` (`config_read`/`config_write`,
   Finding 10) as a `log_level` field, so an operator can set a durable default
   without passing `-v`/`-d` on every invocation, versus keeping it purely a
   per-invocation CLI flag. This doc takes no position; flag for review.
7. **Docs**: Update `docs/reference/environment-variables.md` and
   `docs/reference/common-pitfalls.md` once Design 2 ships, documenting the new
   flag alongside the existing `RUST_LOG` precedence note (Finding 1).

## References

- Issue #1965 — this investigation's source ticket.
- Issue #1942 / PR #1948 — established that `agent_delegate` is a tracking gate,
  not the delegation path (Finding 4); this doc's Design 1 deliberately
  instruments the actual delegation-completion path instead.
- [`tool-output-interception-seam.md`](./tool-output-interception-seam.md) (DOC-32) —
  structural precedent for this doc's format (Behavior Contract WHAT/WHY,
  Options/Designs with `SPEC-*~draft` IDs, Decision section, Follow-up
  Implementation Issues section).
- `crates/trusty-common/src/tracing_init.rs` — existing verbosity-ladder helpers
  (Finding 1).
- `crates/trusty-mpm/src/bin/tm/main.rs` — daemon-mode tracing init and log
  rotation setup (Findings 2, 6).
- `crates/trusty-mpm/src/bin/tm/cli.rs` — `Cli` struct, current `global` args
  (Finding 3).
- `crates/trusty-mpm/src/core/sm/agent/delegate/mod.rs` — `SessionManagerAgent::decompose()`,
  the real delegation-completion path (Finding 5).
- `crates/trusty-mpm/src/core/sm/providers/mod.rs` — `LlmResponse` struct
  (Finding 5).
- `crates/trusty-common/src/error_capture/{types.rs,store.rs,layer.rs}` —
  existing JSONL structured-sink pattern to mirror (Finding 7).
- `crates/trusty-mpm/src/core/model_inject.rs`,
  `crates/trusty-mpm/src/control/backend/tmux.rs`,
  `crates/trusty-mpm/src/daemon/tmux.rs` — tmux-backed native session launch
  path (Finding 8).
- `crates/trusty-mpm/src/control/backend/stream_json.rs` — existing non-tmux
  `--output-format stream-json` usage, proving the flag works (Finding 9).
- `crates/trusty-mpm/src/core/config.rs`,
  `crates/trusty-mpm/src/daemon/mcp_console.rs` — existing config surfaces
  (Finding 10).
