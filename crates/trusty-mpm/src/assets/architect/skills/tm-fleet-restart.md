---
name: tm-fleet-restart
description: Restart every trusty-mpm session in the fleet, including the Architect's own, with a verified checkpoint first — after a Claude account change (new /login) or to replace a tmux server at background priority. Also relaunch, resume, new PM sessions, Remote Control restore and Architect hang recovery. Architect only.
version: 1.0.0
category: monitoring
when_to_use: when the user has logged into a different Claude account and asks to exit and reload the sessions, or when the tmux server runs at priority 4 (trusty-tools #8415)
author: Claude MPM Team
license: Elastic-2.0
effort: high
tags:
  - supervisor
  - architect
  - trusty-mpm
  - restart
  - account
  - remote-control
  - tmux
requires_tools:
  - bash
---

Kind: framework (the Architect's own skill; portable with the framework core).

# tm-fleet-restart — Restart the Fleet

A running Claude Code session keeps the login it started with. After the user
changes account, every session must exit and start again to use the new login.
Remote Control also disconnects on the switch ("signed-in claude.ai account or
organization changed"), so each restarted session needs `/remote-control`.

Two ways to restart:
- **Per session (default, account change):** `scripts/session-relaunch.sh`.
  `/exit` drops the pane to its shell, which prints "run `tm` to relaunch this
  session"; `tm` there starts a fresh Claude Code on the current login. Other
  sessions and their agents keep running. Verified 2026-09-23 19:53–19:56Z on
  nine PMs.
- **Whole server:** `scripts/fleet-restart.sh --rc`. Only to replace a tmux
  server at background priority. `kill-server` ends every session on the
  socket, this one included.

## 1. Decide the resume list

- List live sessions with `tmux list-sessions`. Every one is either resumed or
  deliberately exited. Ask the user when the choice is unclear.
- A session the user says is not being worked on still gets a checkpoint,
  then stays exited.

## 2. Clear open dialogs

- Capture each pane with `-S -60`; a dialog's options scroll off the visible
  area. An open AskUserQuestion is the user's decision: bring it to the user,
  relay the answer, and confirm the `→ <answer>` line before the pause.

## 3. Checkpoint every PM

- Classify inputs (`tm architect input-state '=<session>:'`). Send
  `/tm-session-pause <reason>` with the two-step send to every `empty` or
  `suggestion` pane. Never type over `typed` input.
- Background agents die with the server. The snapshot records them; the
  resumed PM re-dispatches from it.
- Verify each snapshot: a new `.trusty-mpm/sessions/<id>/session-*.md` in the
  project, newer than the send time, with next steps. A pane that reports no
  snapshot is not restarted until it has one.

## 4. Checkpoint the Architect

- Rewrite the handoff snapshot in `local/state.md`: what is in flight per PM,
  the resume list, the exited sessions, and the monitor status. Append the
  `local/actions.md` entry and commit `local/` if it is a repository.

## 5. Run the restart

- A session already on the new account (status line `✻<account>`, the user
  says so) needs only `/remote-control` with the two-step send, not a restart.
  2026-09-23: a PM and the Architect's own session.
- Per session, in parallel, logging to one file:
  ```sh
  for s in tm-a tm-b; do scripts/session-relaunch.sh $s & sleep 1; done; wait
  cat inbox/relaunch.log
  ```
- `STOPPED: background work running` means Claude asked whether to stop
  running agents or monitors. Read the list. Rerun with `--stop-tasks` when
  the snapshot covers the work (the resumed PM re-dispatches it) or when the
  item is a monitor or cron the resume re-creates; "Move to background" would
  leave a duplicate running on the old login. Otherwise ask the user.
- Whole-server path: `nohup scripts/fleet-restart.sh --rc [--force] tm-a ... the Architect's session &`
  (`--force` kills unlisted sessions; only after their checkpoints). Log:
  `inbox/restart.log`.

## 6. After the restart

- After a whole-server restart (new Architect session): with `architect.md`
  and `AGENTS.md` in your instructions, read `local/state.md` → the `local/actions.md` tail, then `inbox/restart.log`.
- A resumed PM may re-ask a question the user already answered; answer it
  from the recorded ruling. A new question goes to the user.
- For each resumed session, verify from a capture: the status line shows the
  new account (`✻<account>`), Remote Control is connected (no `/rc failed`),
  and `/tm-session-resume` restored the snapshot. Send `/remote-control` again
  where it failed.
- Re-arm one inbox Monitor only if `local/state.md` says monitoring is active
  and no other session runs one.
- Report to the user: sessions resumed, sessions exited, any failures. The
  first report after a restart opens with the Architect's introduction
  (`architect.md`, "Voice").

## Relaunch, Remote Control and new sessions (moved from AGENTS.md and the operating manual, restructure 2026-10-07)

Kind: framework. Index line in architect.md: load this before any relaunch, resume, new PM session, Remote Control restore, or Architect hang recovery.

### trusty-mpm session commands

<!-- rule:r-091 -->

| Command | Use / caveat |
| --- | --- |
| `tm ls --no-prune` | Managed sessions without the pruning side effect of plain `tm ls`. Cross-check against tmux. |
| `tm status` / `tm health` | Daemon and fleet health. |
| `tm sessions resume <name>` | Start a stopped managed session detached. `tm launch` attaches to the current terminal, so the Architect cannot use it. After a resume, send `/tm-session-resume` into the new pane, then run `scripts/rc-restore.sh <name>` (/rename + /remote-control) at once, the same pass. owner, 2026-10-06 15:2xZ: "You should know to run /rc when the session is up" (a PM resumed 15:15Z without RC; the owner could not see it). |
| `tm sessions catchup` | CLI checkpoint digest. Unbounded output; prefer the MCP `session_context_catchup` tool inside the target session. |
| `/tm-session-pause [summary]` | In a PM pane: checkpoint the session. Verify the snapshot file before clearing. |
| `/clear` | In a PM pane: reset the conversation. Does not restore anything by itself. |
| `/tm-session-resume` | In a PM pane: restore the latest checkpoint and reconcile live work. |

Do not start `tm supervisor`, restart daemons, redeploy assets or reclaim worktrees just to learn how a command behaves.

### Relaunch

<!-- rule:r-093 -->
- **Fleet server restart: sister session (owner, 2026-10-06 07:1xZ).** "Next time, we should create a new server and launch a sister session in it, so you can control the process." the owner starts a second tmux server from iTerm2 on a new socket, and a sister Architect session runs in it. The Architect, still in the old server, drives the move: it relaunches each PM into the new server and verifies it, then hands over to the sister session. the owner no longer relaunches the Architect by hand. 2026-10-06: `tm` run inside a tmux session already named `the Architect's session` failed with "failed to create tmux session the Architect's session in <dir>", so the owner launched the Architect by hand. The same pattern worked for a PM (`tmux new-session -d -s <name> -c <dir>`, then `scripts/session-relaunch.sh <name>`). Gatekeeper check after the move: a fresh copy of a binary starts in under 1 s (0.10-0.22 s on 2026-10-06; before the move it did not start within 20 s).
<!-- rule:r-094 -->
- **Relaunch by record (2026-10-06).** Inside a tmux session whose name matches a daemon record (`tm ls --no-prune`), bare `tm` replaces the shell with Claude, with full setup. Where no record has that name, bare `tm` opens the dashboard, and `tm launch` creates a separate session named after the folder (`tm-trusty-tools`). Several PMs that share one folder (memory, search, secrets and review all use trusty-tools) therefore need `tm sessions new --name-hint <name> <dir>` (owner, 07:2xZ). the owner, 07:2xZ: `tm <owner>/<repo> --user <gh login>` should launch the project and clone the repo if it is missing. This is the intended behaviour; whether it is implemented is not verified. Test it before relying on it. A gap is a trusty-tools improvement.
<!-- rule:r-101 -->
- After a relaunch the tmux window id changes, so resolving by window finds nothing; pass the session id explicitly (trusty-tools #8408).

### Remote Control and window names

<!-- rule:r-098 -->
- Account change drops Remote Control (status line `/rc failed`): `fleet-poll.py` runs `scripts/rc-restore.sh <session>` on its own (`/rename <session>`, `/remote-control <session>`, confirm a new claude.ai URL), once per session per 10 min, and alerts `rc_restored` or `rc_failed`. Run it by hand the same way for a session the poller skipped (busy or typed input). `session-relaunch.sh` and `fleet-restart.sh --rc` also `/rename` before `/remote-control` (owner, 2026-10-02 17:27Z).
- **The Architect renames itself after every login (owner, 2026-10-07 18:1xZ).** the owner's words: "every time we do a login into a different account, it should automatically rename itself to tm architect." After any `/login` in the Architect pane, run `scripts/rc-restore.sh the Architect's session` at the first idle boundary (box `empty`, no spinner), which sends `/rename the Architect's session` then `/remote-control the Architect's session`. It is a self-targeted key script: re-check idle immediately before each keystroke group. Then run it for every other tm-* session (b6 procedure). Until the poller does this on its own, the Architect starts it as a background job in the same turn as the login.
<!-- rule:r-099 -->
- Remote Control name (owner, 2026-10-02 17:3xZ): `/rename` names only the local session; the app's RC name is the argument of `/remote-control <session>`, else a generated name like "<host>-<rc-name>". If RC is active, `/remote-control` opens a dialog ("Disconnect this session", "Show QR code", "❯ Continue"): Up until `❯ Disconnect this session` (max 3), Enter, wait for "Remote Control disconnected.", retype `/remote-control <s>`. All three scripts share `scripts/rc-lib.sh`.
<!-- rule:r-100 -->
- Window names (owner, 2026-10-02 17:3xZ): automatic-rename shows the Claude Code version ("2.1.287") as the window name; `fleet-poll.py` renames every tm-* window to its session each cycle, and the three scripts above run `tmux rename-window -t "=$s:" "$s"` after `/rename` (quote the target: zsh expands an unquoted `=word` to a command path). By hand: `tmux rename-window -t "=<s>:<idx>" <s>`.

### New PM session

<!-- rule:r-104 -->
`tm sessions new --account <gh login> --name-hint <bare name> --task "..." <abs project path>`. The daemon prefixes "tm-" and appends "-01" (a bare name became tm-tm-<name>-01). Do NOT `tmux rename-session` (owner AQ-bc 2026-10-05: a renamed session shows as stopped in `tm ls`, trusty-tools #9238; keep the daemon's name until #9238 ships a rename-aware record or `tm sessions rename`). Submit the task line and run `scripts/rc-restore.sh <auto name>` (/rename + window name + /remote-control <name>) (owner 2026-10-02 18:3xZ: "remember to RC after connecting"). Then ask the user whether the new session is a factory session (owner 2026-10-07; architect.md "Factory sessions") and record the answer in `local/state.md` before giving it work.

### Architect hang recovery

<!-- rule:r-079 -->
An Architect hang has symptoms: spinner timer frozen, no input echo, transcript mtime stale. Recovery: `sample <pid> 1 > evidence`, keep the transcript, SIGTERM then SIGKILL that pid only, `claude --resume <session-id>` in the same pane, verify Remote Control, re-arm one inbox Monitor.
