---
name: architect-tmux-ops
description: Architect skill. Load before any tmux capture or send, before classifying an input box, before any send-keys or dialog pick, and before any zsh command that takes a tmux or process target. Covers exact targets, the two-step send through scripts/relay.sh, input-box classes, Claude Code input-box traps, Monitor pipe flushing, PID waits and zsh `=` quoting. Architect only.
user-invocable: true
metadata:
  version: "0.1.0"
category: monitoring
tags: [architect, tmux, send, input-box]
effort: high
---

# architect-tmux-ops — Observe and Send Safely

Load this before any tmux capture or send. Every path below is relative to
this project's root, the directory that holds its `CLAUDE.md`.

## Observe and send

<!-- rule:r-089 -->
**Exact targets only.** Address a pane as `=<session>:` (e.g.
`-t '=tm-<name>:'`), never a bare `-t tm-<name>`. When no session has exactly
that name, tmux falls back to a prefix match, so a bare target can reach a
sibling session with a longer name. A resume aimed at one session has
replaced a sibling's PM this way.

Quote exact targets in zsh: `tmux has-session -t '=tm-x'`. Unquoted, zsh
expands `=tm-x` as a command path and fails with "tm-x not found", which makes
a live session look dead.

| Action | Command |
| --- | --- |
| Find the fleet socket | `tmux display-message -p '#{socket_path}'` from inside any fleet pane. Default: `/tmp/tmux-<uid>/default`. |
| List sessions | `tmux list-sessions` |
| List panes with paths | `tmux list-panes -a -F '#{session_name} #{pane_id} #{window_id} #{pane_current_path} #{pane_current_command}'` |
| Capture a pane | `tmux capture-pane -t '=<session>:' -p -S -40` (last 40 lines plus scrollback) |
| Classify the input box | `tm architect input-state '=<session>:'` or `python3 scripts/input-state.py '=<session>:'`: `empty`, `suggestion`, `typed`, `no-prompt` or `missing` |
| Send a relay | `scripts/relay.sh <session> '<text>'` (the two-step send below, refusing on doubt) |
| Type literal text by hand | `tmux send-keys -t '=<session>:' -l '<text>'` (no Enter) |
| Submit by hand | `tmux send-keys -t '=<session>:' Enter`, as a separate call |
| Pick a dialog option | `tmux send-keys -t '=<session>:' Down` (or `Up`), capture to confirm the `❯` sits on the intended option, then `Enter` |

Pane ids (`%N`) are unique only within one tmux server. Re-discover them after
any restart; never reuse a remembered id without checking.

## The two-step send

`scripts/relay.sh <session> '<text>'` does steps 1 to 3 in one call:

1. It refuses (`NO-MARKER`) unless the text starts with your relay marker,
   `[<Name> Architect HH:MMZ]`, and refuses (`SKIP`) unless the box is
   `empty` or `suggestion`.
2. It types the text literally, with no Enter, then classifies the box again.
   Unless the box is `typed` and starts with your marker, it stops
   (`NOT-OURS`) and sends no Enter.
3. Only then it sends Enter as a separate call and prints `sent HH:MM:SSZ`.

Step 4 is yours: capture the pane and confirm a submitted turn and an
acknowledgement. Exit 1 means nothing was submitted; record it and look
again next pass. When `NOT-OURS` left your text in the box, it is still your
text: clear it on the next pass only after you capture and confirm that.

<!-- rule:r-090 -->
### Optional MCP tmux server

An MCP tmux server can replace the CLI calls above. One has returned an empty
session list while the fleet was live, with no error. Cross-check its
`list-sessions` against `tmux list-sessions` every pass, and use the CLI for
the pass when it under-reports.

<!-- rule:r-111 -->
## Status line fields

The trusty-mpm status line in each pane reads: `TM <ver> ● | <project> ⎇
<branch> | @<gh> | ✻<account> | <model> | ctx N% | $<session cost> | ⏳<5-hour
plan %> 📅<weekly plan %> | 💸<latest>%/<mean>%`. `ctx N%` is the share of
that session's own model window. `💸` is tool-output compression savings, not
spend. `●` means the daemon is reachable.

The agent rows under the status line (`◯ <agent> <activity> <elapsed> · ↓
<tokens>`) can freeze: a completed subagent's row can keep a fixed elapsed
time for hours while the harness shows it completed. An elapsed time that does
not advance across captures means ask the PM for its harness agent list, not
"stuck"; never act on the row alone.

## Claude Code input box

<!-- rule:r-151 -->
**Gate the send on the classifier in the same command** (`case`/`&&`, as
`scripts/relay.sh` does), never `;`. A `;` chain has typed a relay into an
open dialog that the classifier had reported as `typed`; the dialog ignored
it, but it could have picked an option.

<!-- rule:r-152 -->
**Prompt suggestions are not drafts.** Claude Code shows an auto-generated
next prompt as dim text (SGR 2). A plain `capture-pane -p` renders it like
typed text; the classifier checks the escape codes. Enter on a suggestion
submits nothing. A suggestion is also not a user ruling: in a plain capture
the input box sits above the agent list, so its `❯ text` line looks like the
last submitted turn. Before recording any `❯` line as the user's ruling,
classify the box; a submitted turn is followed by the PM's response, not by
the footer.

<!-- rule:r-153 -->
**Long sends truncate.** A single `send-keys -l` of about 1,700 characters
was treated as a paste and arrived with its start cut off. Keep one send
under about 600 characters. For longer content, write
`local/briefs/<date>-<slug>.md` and send a pointer to its absolute path.
After submitting, check that the echoed turn starts with your marker.

<!-- rule:r-154 -->
**An open question blocks the heartbeat.** While an AskUserQuestion dialog
in your own session waits for an answer, the session is not idle, so the
heartbeat and the poller's wake are not processed. One unanswered dialog has
stalled supervision for hours. Keep questions batched and answered quickly,
and on the first pass after a long gap run a full poll of every pane.

<!-- rule:r-155 -->
**Messages queue.** Text submitted while a PM turn runs shows "Press up to
edit queued messages" and is delivered when the turn ends. That counts as
delivered; do not resend. Exception: a hold or stop that must bind before the
running turn acts. Deliver it at once with
`tmux send-keys -t '=<session>:' C-x C-s` ("ctrl+x ctrl+s to send now"), then
capture again to confirm the PM acknowledged it.

<!-- rule:r-156 -->
**Clearing your own box: verify empty before typing.** In a narrow pane a
wrapped poller wake spans several lines and one C-u clears only the current
line; the rest has been submitted together with a typed `/clear` as one
message, so the clear never ran. Clear, classify, repeat until `empty`, then
type.

<!-- rule:r-157 -->
**Monitors survive your own /clear.** Background Monitors belong to the
process, not the conversation. After a self-refresh, stop the pre-clear
Monitor ids recorded in `local/state.md` before arming new ones, or every
event arrives twice.

<!-- rule:r-158 -->
**Monitor pipes must flush per line.** `tail -F file | cut -c1-300` delivers
nothing: `cut` block-buffers when writing to a pipe, so a Monitor can expire
silent over several events. Truncate with
`awk '{print substr($0,1,300); fflush()}'` or use `grep --line-buffered`.

## Process tracking

<!-- rule:r-149 -->
**Wait on a PID, never on a pattern.**
`while kill -0 <pid> 2>/dev/null; do sleep 15; done`. A background
`while pgrep -f <name>` loop matches its own shell's command line and never
ends.

<!-- rule:r-150 -->
**Process identity by name breaks under exec.** cargo execs external
subcommands (cargo-nextest, cargo-llvm-cov, cargo-miri), rustup proxies exec
cargo, and `sh -c` and `env -S` exec too, so a recorded process name need not
match the live image. Do not add a name match to a liveness check; pid plus
start time survives exec.

## zsh quirks

<!-- rule:r-179 -->
zsh does not word-split an unquoted variable: `A="--flag one --other two";
cmd $A` passes one argument. Write the flags out, or use an array
(`A=(--flag one --other two); cmd "${A[@]}"`).

<!-- rule:r-180 -->
**zsh expands a leading `=` in an unquoted word.**
`tmux kill-session -t =tm-x-poll` fails with "tm-x-poll not found": zsh
treats `=name` as the path of command `name`. Quote exact-match targets:
`tmux kill-session -t '=tm-x-poll'`.

## Related skills

- `architect-operating-rules`: what a relay says and when to send it.
- `tm-fleet-check`: the pass that captures and sends.
