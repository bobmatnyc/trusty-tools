---
name: architect-tmux-ops
description: Framework skill. Load before any tmux capture or send, before classifying an input box, before any send-keys or dialog pick, and before any zsh command that takes a tmux or process target. Covers exact targets, the two-step send, input-box classes, Claude Code input-box traps, Monitor pipe flushing, PID waits and zsh `=` quoting.
---

# Architect tmux operations (framework)

Kind: framework. Index line in architect.md: load this before any tmux capture or send.

## Observe and send

<!-- rule:r-089 -->
**Exact targets only.** Address a pane as `=<session>:` (e.g. `-t '=tm-<name>:'`), never a bare `-t tm-<name>`. tmux falls back to a prefix match when no exact session exists, so a bare target can hit `a sibling session with a longer name`. On 2026-09-23 15:39Z a tm resume of tm-<name> replaced a sibling session with a longer name' PM this way (trusty-tools bug, reported by the owner).

Quote exact targets in zsh: `tmux has-session -t '=tm-x'`. Unquoted, zsh expands `=tm-x` as a command path and fails with "tm-x not found" (2026-09-27: a live poller looked dead).

| Action | Command |
| --- | --- |
| Find the fleet socket | `tmux display-message -p '#{socket_path}'` from inside any fleet pane. Default: `/tmp/tmux-<uid>/default` (macOS resolves `/tmp` to `/private/tmp`). |
| List sessions | `tmux list-sessions` |
| List panes with paths | `tmux list-panes -a -F '#{session_name} #{pane_id} #{window_id} #{pane_current_path} #{pane_current_command}'` |
| Capture a pane | `tmux capture-pane -t <pane> -p -S -40` (last 40 lines plus scrollback) |
| Classify the input box | `scripts/input-state.py <pane>...` → `empty`, `suggestion` or `typed` |
| Type literal text | `tmux send-keys -t <pane> -l "<text>"` (no Enter) |
| Submit | `tmux send-keys -t <pane> Enter`, as a separate call |
| Pick a dialog option | `tmux send-keys -t <pane> Down` (or `Up`), capture to confirm the `❯` sits on the intended option, then `Enter` |

Pane ids (`%N`) are unique only within one tmux server. Re-discover them after any restart; never reuse a remembered id without checking.

<!-- rule:r-090 -->
### Optional MCP tmux server

An MCP tmux server such as `tmux-mcp-rs` can replace the CLI calls above. It has returned an empty session list while the fleet was live, with no error. Cross-check its `list-sessions` against `tmux list-sessions` every pass, and use the CLI for the pass when it under-reports.

<!-- rule:r-111 -->
## Status line fields

The trusty-mpm status line in each pane reads: `TM <ver> ● | <project> ⎇ <branch> | @<gh> | ✻<account> | <model> | ctx N% | $<session cost> | ⏳<5-hour plan %> 📅<weekly plan %> | 💸<latest>%/<mean>%`. `💸` is tool-output compression savings (share of tokens saved), not spend. `●` means the daemon is reachable.

The agent rows under the status line (`◯ <agent> <activity> <elapsed> · ↓ <tokens>`) can freeze: a completed subagent's row may stay with a fixed elapsed time for hours (2026-09-27: a local-ops row stuck at 35m 21s from 15:16Z, harness showed it COMPLETED). An elapsed time that does not advance across captures means ask the PM for its harness agent list, not "stuck"; never act on the row alone.

## Claude Code input box

<!-- rule:r-151 -->
**Gate the send on the classifier in the same command** (`case`/`&&`), never `;`. On 2026-09-23 13:20Z a `;` chain typed a relay into an open AskUserQuestion dialog that `input-state.py` had reported as `typed`; the dialog ignored it, but it could have picked an option.

<!-- rule:r-152 -->
**Prompt suggestions are not drafts.** Claude Code shows an auto-generated next prompt as dim text (SGR 2). A plain `capture-pane -p` renders it like typed text. `scripts/input-state.py` checks the escape codes. Enter on a suggestion submits nothing. A suggestion is also not a user ruling: in a plain capture the input box sits above the agent list, so its `❯ text` line looks like the last submitted turn. Before recording any `❯` line as the user's ruling, run `input-state.py`; a submitted turn is followed by the PM's response, not by the footer (2026-09-24: six suggestions misread as rulings).

<!-- rule:r-153 -->
**Long sends truncate.** A single `send-keys -l` of ~1,700 characters was treated as a paste and arrived with its start cut off. Keep one send under ~600 characters. For longer content, write `local/briefs/<date>-<slug>.md` and send a pointer to its absolute path (older pointers into `inbox/briefs/` still resolve through the symlink). After submitting, check that the echoed turn starts with your prefix.

<!-- rule:r-154 -->
**An open question blocks the heartbeat.** While an AskUserQuestion prompt in the Architect's session waits for an answer, the session is not idle, so the cron heartbeat and inbox monitor events are not processed. On 2026-09-23 one unanswered question stalled supervision for about five hours. Keep questions batched and answered quickly, and on the first pass after a long gap run a full poll of every pane.

<!-- rule:r-155 -->
**Messages queue.** Text submitted while a PM turn runs shows "Press up to edit queued messages" and is delivered when the turn ends. That counts as delivered; do not resend. Exception: a hold or stop that must bind before the running turn acts. Deliver it at once with `tmux send-keys -t <pane> C-x C-s` ("ctrl+x ctrl+s to send now"), then capture again to confirm the PM acknowledged it.

<!-- rule:r-156 -->
**Clearing the Architect's own box: verify empty before typing.** In a narrow pane (47 columns on 2026-09-28 11:15Z) a wrapped poller wake spans several lines and one C-u clears only the current line; the rest was submitted together with the typed /clear as one message, so the clear never ran. Clear, run scripts/input-state.py, repeat until `empty`, then type.

<!-- rule:r-157 -->
**Monitors survive the Architect's own /clear.** Background Monitors belong to the process, not the conversation. After a self-refresh, TaskStop the pre-clear Monitor ids recorded in `local/state.md` before arming new ones, or every event arrives twice (2026-09-27 17:14Z).

<!-- rule:r-158 -->
**Monitor pipes must flush per line.** `tail -F file | cut -c1-300` delivers nothing: `cut` block-buffers when writing to a pipe, so a 30-minute Monitor expired silent over 8 events (2026-09-27 17:14-17:44Z). Truncate with `awk '{print substr($0,1,300); fflush()}'` or use `grep --line-buffered`.

## Process tracking

<!-- rule:r-149 -->
**Wait on a PID, never on a pattern.** `while kill -0 <pid> 2>/dev/null; do sleep 15; done`. A background `while pgrep -f <name>` loop matches its own shell's command line and never ends (2026-09-30, daily_sonic cutover).

<!-- rule:r-150 -->
**Process identity by name breaks under exec.** cargo execs external subcommands (cargo-nextest, cargo-llvm-cov, cargo-miri), rustup proxies exec cargo, and sh -c / env -S exec too, so a recorded process name need not match the live image. Do not add a name match to a liveness check; pid + start time survives exec (#8736 fold critic, 2026-09-28: a supervisor-added name match reopened the #8261 overcommit).

## zsh quirks

<!-- rule:r-179 -->
zsh does not word-split an unquoted variable: `A="--profile <name> --region us-east-1"; aws ... $A` passes one argument and aws rejects it (2026-09-28 23:38Z). Write AWS flags out, or use an array (`A=(--profile <name> --region us-east-1); aws ... "${A[@]}"`).

<!-- rule:r-180 -->
**zsh expands a leading `=` in an unquoted word.** `tmux kill-session -t =tm-supervisor-poll` failed with "tm-supervisor-poll not found": zsh treats `=name` as the path of command `name`. Quote exact-match targets: `tmux kill-session -t '=tm-supervisor-poll'` (2026-09-29 13:51Z).
