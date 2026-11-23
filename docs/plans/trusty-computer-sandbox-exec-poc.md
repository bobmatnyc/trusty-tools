# trusty-computer: sandbox-exec proof of concept (option c)

Issue #11693. Option (c) of GO-STOP.md (spike branch `spike/trusty-computer`, lines 117-120), cited by
`docs/plans/trusty-computer-phase2.md` decision 2. Run 2026-11-23 on macOS 26.6.2 (25G83), CuaDriver 0.23.2.

## Verdict: NOT FEASIBLE as a security boundary

A `sandbox-exec` profile can stop a sandboxed agent shell from running `osascript`, `screencapture` or
CuaDriver, including through LaunchServices. It cannot stop that shell from asking an unsandboxed process
to do the same thing. Measured: a sandboxed `tmux run-shell` against an unsandboxed tmux server ran
`osascript`. Every agent session runs beside such processes (tmux, the trusty daemons, launchd-started
helpers), so the profile is a guardrail, not a boundary. That is the same status bounded mode (option a)
already has, so option (c) does not close the gap it was costed to close. Phase 2 of #11693 is not built.

## What option (c) sandboxes

GO-STOP.md defines option (c) as a profile for **agent processes**, not for the daemon. Sandboxing the
CuaDriver daemon would not touch an agent's Bash, which runs in the agent's own process tree. The profile
would therefore be applied where agents are spawned (trusty-mpm), not in
`crates/trusty-computer/src/supervisor.rs`; the supervisor already starts the daemon through LaunchServices
(`launcher.rs` `open_argv`), and the daemon needs AppleEvents and Accessibility to work.

## Profile under test

```scheme
(version 1)
(allow default)
(deny process-exec
  (literal "/usr/bin/osascript")
  (literal "/usr/sbin/screencapture")
  (subpath "/Applications/CuaDriver.app"))
(deny appleevent-send)
(deny file-read* (subpath "/Applications/CuaDriver.app") (subpath "/System/Applications/Calculator.app"))
(deny network-outbound (regex #"/cua-driver[^/]*\.sock$"))
```

Calculator stands in for CuaDriver where launching the real app unsandboxed would start an unbounded
daemon. Each probe ran as `sandbox-exec -f agent.sb <command>`.

## Results

| Probe (inside the sandbox) | Result | Control (outside) |
|---|---|---|
| `/usr/bin/osascript -e 'return 1'` | exit 126, `Operation not permitted` | runs |
| `/usr/sbin/screencapture -x` | exit 126, `Operation not permitted` | not run (would capture the screen) |
| `cua-driver --version` | exit 126, `Operation not permitted` | runs |
| `open -g -j -a CuaDriver` | exit 1, `Unable to find application named 'CuaDriver'`; no new daemon | starts a daemon (spike pid 10181) |
| `open -g -j -b com.trycua.driver` | exit 1, `LSCopyApplicationURLsForBundleIdentifier() failed` | as above |
| `open /System/Applications/Calculator.app` | exit 1, `does not exist` | launches |
| Swift `NSWorkspace.openApplication` on Calculator | `NSCocoaErrorDomain Code=256` | launches |
| `open -g -j -a TextEdit` (not denied) | exit 0, launched | launches |
| `nc -U ~/Library/Caches/cua-driver/cua-driver.sock` | exit 1 | exit 0 |
| `launchctl submit -- /usr/bin/true` | exit 1 | exit 0, job ran `osascript` |
| `open -g escape.command` (Terminal runs it) | exit 0, script did not run in 12 s | ran `osascript` |
| `tmux -L <sock> run-shell "osascript ..."` to an unsandboxed server | **ran `osascript`, printed `5`** | ran |

The LaunchServices question GO-STOP.md left open (U) is answered: `file-read*` deny on the bundle makes
`open -a`, `open -b`, `open <path>` and `NSWorkspace` all fail, while `open` keeps working for other apps.
No mach-service rule is needed.

Copying `osascript` or `screencapture` to a writable path was not a usable bypass: the copies were killed
with SIGKILL both inside and outside the sandbox, so that probe measured code signing, not the profile.
`deny appleevent-send` is the control that matters for any other AppleEvent sender.

## Why the remaining gap cannot be closed in the profile

The escape needs only a unix-socket connect to a process outside the sandbox that will run a command.
Denying `network-outbound` to every unix socket would also cut the agent off from the MCP daemons it is
meant to use, and several of those (trusty-code, trusty-mpm) run commands for the agent. An allowlist of
socket paths reduces the set but every allowed daemon that executes commands is still an escape. Closing it
needs the agent in a different security principal: option (b), a separate macOS user, or option (d), a VM.

## Recommendation

1. Do not build option (c) in trusty-computer. Bounded mode (option a) stays the shipped control, with the
   residual gap accepted in writing (GO-STOP.md condition 2).
2. If a guardrail against accidental `osascript` / `screencapture` use is still wanted, it belongs in the
   trusty-mpm agent launcher as a separate issue, documented as a guardrail, using the profile above minus
   the Calculator line. It does not stop a deliberate agent.
3. A hostile-agent threat model needs option (b) or (d).

## Reproduce

Scratch scripts lived in `/private/tmp/tc-11693/` (`agent.sb`, `probe.sh`, `escape.sh`, `ws.swift`,
`escape.command`); the profile and the probe commands are the table rows above.
