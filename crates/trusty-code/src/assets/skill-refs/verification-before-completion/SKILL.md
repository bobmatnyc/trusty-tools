---
name: verification-before-completion
description: "Run verification commands and confirm output before claiming success"
user-invocable: false
version: "1.0.0"
category: agent-reference
effort: high
---

# Verification Before Completion

## Overview

Claiming work is complete without verification is dishonesty, not efficiency.

**Core principle:** Evidence before claims, always.

**Violating the letter of this rule is violating the spirit of this rule.**

This skill enforces mandatory verification before ANY completion claim, preventing false positives, broken builds, and trust violations.

## When to Use This Skill

Activate ALWAYS before claiming:
- Success, completion, or satisfaction ("Done!", "Fixed!", "Great!")
- Tests pass, linter clean, build succeeds
- Committing, pushing, creating PRs
- Marking tasks complete or delegating to agents

**Use this ESPECIALLY when:**
- Under time pressure or tired
- "Quick fix" seems obvious or confidence is high
- Agent reports success or tests "should" pass

## The Iron Law

```
NO COMPLETION CLAIMS WITHOUT CURRENT VERIFICATION EVIDENCE
```

Evidence must cover the current relevant source, exact command, features and
environment. Reuse a recorded matching run; rerun when any relevant input
changes, provenance is missing, or the project requires an independent gate.

## Core Principles

1. **Evidence Required**: Every claim needs supporting evidence
2. **Current Evidence**: Match source, command, features and environment
3. **Scoped Verification**: Follow the project risk/stage test ladder
4. **Honest Reporting**: Report actual state, not hoped-for state

## Quick Start

The five-step gate function:

1. **IDENTIFY**: What command proves this claim?
2. **RUN OR REUSE**: Execute the required gate unless matching evidence exists
3. **READ**: Check terminal exit status and the relevant raw summary
4. **VERIFY**: Does output confirm the claim?
   - If NO: State actual status with evidence
   - If YES: State claim WITH evidence
5. **ONLY THEN**: Make the claim

Skip any step = lying, not verifying.

## Reading an Exit Code

<!-- #7561: a `kill <pid>` stop of a dev server was reported four times as
     "failed with exit code 143". -->

Step 3 says "check exit code". A non-zero exit is not automatically a failure.
A process stopped by a signal reaches the shell as `128 + N`, so a kill you
issued yourself lands in the same numeric range as a real crash:

| Exit | Meaning |
|---|---|
| `0` | Success |
| `1`–`125` | The command's own failure status |
| `130` (128+2) | `SIGINT` — Ctrl-C |
| `137` (128+9) | `SIGKILL` — `kill -9`, or the OOM killer |
| `143` (128+15) | `SIGTERM` — the ordinary `kill <pid>` |

When you or the harness stopped the process on purpose — `kill <pid>` on a dev
server you started, a monitor tearing down a background task — a `128 + N` exit
is **terminated by signal N**, which is the outcome you asked for. Report it
that way and never as "failed with exit code 143". Do not file a bug or start
debugging a failure that did not happen.

The one exception is `137` on a build or test run nobody killed: that is the
OOM killer, a real failure, and it is diagnosed rather than read as a clean
stop.

## Key Patterns

**Correct Pattern:**
```
✅ [Run pytest] [Output: 34/34 passed] "All tests pass"
```

**Incorrect Patterns:**
```
❌ "Should pass now"
❌ "Looks correct"
❌ "Tests were passing"
❌ "I'm confident it works"
```

## Red Flags - STOP Immediately

STOP when:
- Using "should", "probably", "seems to"
- Expressing satisfaction before verification
- About to commit/push/PR without verification
- Trusting agent success claims without matching raw evidence
- Relying on partial verification

**Resolve the evidence gap before claiming completion.**

## Why This Matters

**Statistics from real-world failures:**
- Verification cost: 2 minutes
- Recovery cost: 120+ minutes (60x more expensive)
- 40% of unverified "complete" claims required rework

**Core violation:** "Lying leads to replacement"

## Navigation

For detailed information:
- **[Gate Function](references/gate-function.md)**: Complete five-step verification process with decision trees
- **[Verification Patterns](references/verification-patterns.md)**: Correct verification patterns for tests, builds, deployments, and more
- **[Red Flags and Failures](references/red-flags-and-failures.md)**: Common failure modes, red flags, and real-world examples with time/cost data
- **[Integration and Workflows](references/integration-and-workflows.md)**: Integration with other skills, CI/CD patterns, and agent delegation workflows

## Direct Observation of Success (trusty-mpm, issue #7723)

Use the project's risk/stage test ladder (see #8021):

1. Run the required scoped gates. Documentation changes do not automatically
   owe builds or application tests; cross-package and release changes widen
   coverage according to the project policy.
2. Reuse exact matching raw evidence across engineer/QA handoffs. Record the
   relevant source revision or diff, command, features, environment, exit status
   and log path. QA independently checks coverage and provenance; required
   independent high-risk gates and live deployment checks still run.
3. Preserve build caches. Rebuild cleanly only for a specific invalidation,
   reproducibility or project requirement. A task-cache hit is reused evidence,
   not a fresh test run; require matching inputs and trustworthy provenance,
   otherwise force that affected task rather than clearing every cache.
4. Account for zero tests and skipped tests against the required coverage.
   Verify binary startup, UI or service health when the claim requires it.

```
WRONG:   "All 68 tests pass."
CORRECT: cargo test → "test result: ok. 68 passed; 0 failed; 0 ignored"
```

## Empty-Output Protocol

The harness can intermittently drop a command's stdout: exit 0, empty or
partial output. An empty result is NOT a real result — never fabricate output
you did not see, never report a pass/fail you could not observe.

1. Retry the exact command up to 2 more times — it usually succeeds.
2. Still empty → redirect to a file under your scratchpad directory named for
   this task and step (`<command> > <scratchpad>/out-<issue>-<step>.txt 2>&1`,
   never a fixed `/tmp` name and never `$$` — the scratchpad is shared across
   concurrent agents) and open that exact path with the Read tool, not `cat`
   (which goes back through the same capture path).
3. Still unobservable → report "Could not verify — command output unavailable"
   and hand back.

This applies especially to test runs, `git`/`gh` reads and writes, and build
output. An unobservable result is never a passing result.

## Never Directly Monitor a Declarative Process

A declarative process — test suite, build, lint, CI status check, install,
migration — is one where you issue a command and want its verdict: pass/fail
plus what broke. You never need the play-by-play. Watching one directly is
the defect: an agent told to "rerun the suite until green" spent 415k tokens
because `cargo test` prints a line per test; a sibling spent 546k on
`gh pr checks --watch` streaming a 15-17 minute CI job.

1. **Run it into a file, never a pipe.** Don't watch, tail, or poll a live
   stream. A pipe eats the verdict. Redirect into your scratchpad directory,
   naming the file for this task and step (never a fixed `/tmp` name, never
   `$$`, never a glob to find it again) — the scratchpad is shared across
   concurrently dispatched agents, and a name you cannot reproduce exactly
   lets one agent read a sibling's build output as its own result:

   ```bash
   <command> > <scratchpad>/gate-<crate>-<step>.txt 2>&1; echo "EXIT=$?"
   ```

   Run in the foreground, the `EXIT=$?` above prints straight to your own tool
   output — read it there. Backgrounded this command instead? `echo` then
   writes to the tool's stdout, not into the redirected file, so
   `tm wait --for file --contains "EXIT="` on that file never matches. Either
   append the sentinel into the file too, or wait on the process itself with
   `tm wait --for run --pid <pid>` instead.
2. **`EXIT=0` → capture the required summary, then stop.** Do not stream
   passing-test detail or rerun merely for another agent to see the same result.
3. **Non-zero → trim the file, then read it.** Trim reads FROM the file,
   never from the live command: `--quiet` on the command, `grep`/`tail` over
   the file, or this repo's Unix filter:

   ```bash
   tm compress --tool "cargo test" < <scratchpad>/gate-<crate>-<step>.txt
   ```

   `--tool` is free-form, substring-matched (`"cargo test"`, `"git diff"`).
   Known gap: its structured-format guard can misread a leading
   `key: value`-shaped line — such as a `warning: <path>: …` build warning
   ahead of the test output — as YAML and silently skip compression.
   `--quiet`/`grep` is the reliable default; `tm compress` is an addition on
   top, not a replacement, until that gap is fixed.
4. **Still long → have Haiku summarize it** before you read it.

On failure, re-run only the failing case with full output. That is the only
place per-test detail carries information.

This does NOT weaken the evidence rule. Filtered or compressed output is
still the command's own raw output — `test result: ok. 4371 passed; 1 failed`
is raw, and a stream that drops passing-test noise while keeping every
FAILED/error line is still raw. What is forbidden is YOU summarizing results
in your own words. Raw output stays mandatory for failures, flakes, and
performance claims.

## Waiting on a Background Command (#8264)

Wait on the PROCESS, never on log text. Start it with `<cmd> > scratch.txt
2>&1 & pid=$!`, then `wait $pid` (or poll `kill -0 $pid`). Read the exit code
after the process ends, not before.

Never loop on `pgrep -f`/`ps | grep` for a pattern your own loop's command line
also contains — it matches itself and never exits; three such loops ran for as
long as 32 minutes in one day. Never loop on output text like `OK`/`FAIL`/
`error` either — a `timeout` kill prints `EXIT=124` and matches none of those
strings, so that loop waited five minutes past a command that had already died.

Every wait has a bound. At the bound, stop and report the stage instead of
waiting longer.

## Never End a Gate Chain in a Pipe (#7440)

A pipeline's exit status is the LAST command's, so `cargo test … | tail` exits
0 on a failing suite. Redirect and read `EXIT=$?` — the rule above.

Where a chain must pipe, know what each mechanism actually gives you.
`set -o pipefail` carries the LAST non-zero status, not the first:
`set -o pipefail; false | bash -c "exit 3"` exits `3`. Per-stage codes are
`${PIPESTATUS[0]}` in bash and `${pipestatus[1]}` in zsh, which indexes from 1;
neither exists in `sh`, and reaching for the wrong one FAILS OPEN. This
harness's Bash tool is zsh 5.9, where `${PIPESTATUS[0]}` expands to nothing,
`[ -ne 0 ]` aborts with `unknown condition: -ne`, and the `if` takes its else
branch — a green verdict over a stage that exited 3. Redirect-then-read stays
the default for exactly that reason.

Under Claude Code worktree isolation a grouped `( … )` command is refused
before it runs, so give each gate its own plain command, its own redirect, and
its own `echo "EXIT=$?"` (#6937). Backgrounded the chain? The `echo` writes to
the tool's stdout, not the file — append the sentinel into the file, or wait on
the process itself (above).

## Stack-Specific Gate Traps

- When a fresh execution is required, `pnpm test -- --force` can silently
  drop `--force`. Use `pnpm exec turbo run test --force` and confirm
  `Cached: 0 cached`; otherwise retain valid cached evidence (#7560).
- Stop a dev server with `lsof -ti tcp:<port> | xargs kill` FIRST — `pkill -f
  <path>` misses a bundled server whose argv does not carry the path (#7562).

## The Bottom Line

**No shortcuts for verification.**

Run the command. Read the output. THEN claim the result.

This is non-negotiable.
