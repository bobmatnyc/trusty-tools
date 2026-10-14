# The Architect: a trusty-mpm fleet supervisor

The Architect (named after the Architect in The Matrix Reloaded) is a Claude
Code session that watches your other [trusty-mpm](https://github.com/example-org/trusty-tools)
sessions and handles them for you. It reads every PM's tmux pane, answers the
routine prompts itself, and brings you everything else as one multiple-choice
question with a recommendation. You talk to one session instead of many.

It works because trusty-mpm runs every Claude Code session inside tmux. The
Architect can capture any pane's output and type into any pane's prompt. It
can also read each project's trusty-memory palace and trusty-search index, so
it checks a PM's claims against that project's own code and history.

## What it does

- **Watches.** Each watched project gets a Claude Code `Notification` hook. When
  a PM waits for input or asks permission, the hook appends one line to
  `inbox/events.jsonl`, and a monitor wakes the Architect. A five-minute
  heartbeat runs a pass as a fallback, and every fourth pass sweeps every pane.
- **Answers routine prompts.** It answers a PM only when the answer follows from
  a ruling you already made or an established project decision.
- **Escalates the rest.** Every other decision reaches you as a multiple-choice
  question, recommended option first, prefixed with the session name:
  `[tm-api]: …`.
- **Relays safely.** It never types over text at a prompt. It tells dim prompt
  suggestions from real drafts. It sends in two steps (type, verify, Enter,
  verify) and confirms the PM acknowledged the message.
- **Keeps evidence straight.** A PM's "merged" or "tests pass" is recorded as
  *reported* until the Architect confirms it with its own command.
- **Keeps PMs fresh.** When a PM nears 50% context, it checkpoints the session
  with `/tm-session-pause`, clears it, and resumes it with `/tm-session-resume`.
- **Writes everything down.** State, decisions and actions go to plain markdown
  in `local/`, so any session or account can pick up where the last one stopped.

It never makes a personnel, policy or production decision, never bypasses a
review gate, and never clears a session without a verified checkpoint.

The operating manual, `architect.md`, is composed into the Architect's instructions by tm;
it is not a file in this directory. `AGENTS.md` holds this installation's facts, and
tm composes it after the manual. The skills hold the procedures.

## Requirements

- macOS or Linux with `tmux`, `python3` and `git`. `jq` is optional.
- trusty-mpm (`tm`) with its daemon running, and your projects launched through
  `tm` so each runs as a tmux session on the same tmux server.
- Claude Code, logged in.
- Recommended: trusty-memory and trusty-search, so the Architect can read each
  project's memory and code index.

## Set up

```sh
tm fleet init            # the supported path: seeds this layout, grants the profile, starts the session
```

By hand, from a copy of the shipped files (`scripts/`, `templates/`, `.claude/`,
`AGENTS.md`, `CLAUDE.md`):

```sh
scripts/init.sh
```

`init.sh` creates `local/` from `templates/` (state, goals and actions, the improvement
and usage ledgers, `local/.gitignore`) and `inbox/`. In `.claude/settings.json` it sets
`statusLine` -> `scripts/statusline-tee.sh` only when absent; an existing different
`statusLine` is kept and the line it would set is printed. It lifts the PM guard
(`TRUSTY_MPM_PM_UNRESTRICTED=1`) only when you pass `--guard-lift` or set
`ARCHITECT_GUARD_LIFT=1`: the lift is the owner's choice and is off by default, as with
`tm fleet init --guard-lift`. It installs the push hook in each watch-set project it can
resolve, prints the `[supervisor]` grant line for `~/.trusty-mpm/config.toml` (it never
edits that file), and runs `scripts/doctor.sh`. Re-running changes nothing that exists.
`scripts/init.sh --start` also runs `scripts/architect-start.sh`.

Then:

1. **Fill in `AGENTS.md`.** Replace every placeholder with this installation's facts.
2. **List your watch set.** Edit `local/state.md` and add the tmux sessions the
   Architect should manage (`tmux ls` shows them).
3. **Install the push hook in each watched project:**
   ```sh
   scripts/install-notify-hook.sh ~/code/my-project
   ```
   The hook stores the absolute path of this directory. If you move it, run the
   installer again. `--remove` takes the hook out.
4. **Restart each watched PM** (or wait for its next launch) so Claude Code loads
   the new hook.

## Run it

Start the Architect as its own trusty-mpm session, from this directory:

```sh
cd ~/architect
tm launch
```

`tm launch` runs the session in this checkout (without `--worktree` it does not
make a managed clone) and attaches it to your terminal. Run it inside the same
tmux server as the fleet.

In the Architect session:

| You say | It does |
| --- | --- |
| `/tm-fleet-check` | One supervision pass. |
| "Resume monitoring" | Creates the five-minute heartbeat (a session cron running `/tm-fleet-check`) and the inbox monitor, and records their ids in `local/state.md`. |
| "Pause monitoring" | Deletes the heartbeat and monitor. |
| "Refresh `<session>`" | Runs `tm-context-refresh` on that PM. |
| Any instruction for a project | Relays it to that PM's pane and confirms receipt. |

The heartbeat and monitor belong to the Architect session. They stop when it
exits, and they fire only while the session is idle. After a restart, ask it to
resume monitoring again. It checks `local/state.md` first, so it never runs two.

## Post-launch

After the Architect session exists (`tm sessions resume tm-architect` or `tm fleet init`):

1. Shell part, from any shell: `scripts/architect-start.sh --rc`. It starts the poller
   `tm-architect-poll` at `--interval 600` (one poller only; a second run does nothing)
   and runs `scripts/rc-restore.sh tm-architect`.
2. In the Architect session (these die with the session, so repeat after every restart;
   `local/state.md` records the ids):
   - `/remote-control tm-architect` if step 1 skipped it (busy pane).
   - Create the daily cron (07:53 local: owner dreaming pass and goal brief).
   - Arm one inbox Monitor (`tail -F inbox/events.jsonl`); ask "Resume monitoring".
3. Create the memory palaces `architect` and `owner` (MCP `palace_create`). The owner
   model and each project's rulings and snapshots live in palaces, not in files.
4. Logins you complete yourself: Claude, `gh`, Full Disk Access for the `claude` binary.

`scripts/doctor.sh` checks the repo and host pieces and exits 1 on a miss.

## Run it from Codex

Not supported in this release. Codex reads `AGENTS.md` natively but cannot see the
instructions tm composes, so it would run without the operating manual.

## Remote control

The Architect is the one session you need to reach from your phone. Claude
Code's Remote Control connects a running local session to claude.ai and the
Claude mobile app. The session keeps running on your machine, and you read and
reply from anywhere.

1. In the Architect pane, run `/remote-control` (or start Claude Code with
   `claude --remote-control`). The pane prints
   `/remote-control is active · Continue here, on your phone, or at https://claude.ai/code/session_…`.
2. Open that link in a browser, or find the session in the Claude app.
3. Work from there:
   - The Architect's multiple-choice questions appear as choices you can tap.
   - Files it sends, such as a cover image to approve, show inline.
   - A message you send while it is mid-pass is read within the same pass.

Notes:

- **Enable it on the Architect only.** You can also turn Remote Control on in
  a PM pane, but then you have two conversations with that PM. If you do answer
  a PM directly, the Architect sees the answer on its next capture and records
  it.
- **Keep the machine awake.** The fleet, the Architect and its heartbeat all
  run locally. If the machine sleeps or the tmux server stops, remote access
  stops too.
- **trusty-mpm's own chat bots.** `tm telegram pair` / `tm telegram start` and
  `tm slack start` control the fleet through the trusty-mpm daemon. They
  complement the Architect; they do not replace it.

## Layout

| Path | Tracked | What it is |
| --- | --- | --- |
| `CLAUDE.md` | yes | A short pointer: who this project is and where its facts live. |
| `AGENTS.md` | yes | Instance layer: owner, host, fleet, instance nevers, project rules. tm composes it after the operating manual. |
| `.claude/agents/` | yes | The Architect's agents: `architect` (one pass), the runner, the event extractor, the curators. |
| `.claude/skills/` | yes | The Architect's skills (`tm-*`, `architect-*`). tm refreshes an untouched copy at every launch and keeps an edited one. |
| `scripts/` | yes | Poller, hooks, installers, input-box classifier, fleet restart. See [scripts/README.md](../scripts/README.md). |
| `templates/` | yes | Starting copies of the `local/` files and the dashboard page. |
| `docs/` | yes | The voice guide, the history and the specs. |
| `local/` | yes | Your fleet: `state.md` (snapshot), `goals.md`, `actions.md` (log), `briefs/`, `archive/`, `mandates/`, the improvement and usage ledgers. |
| `inbox/` | no | Transient queues and logs: push events, alerts, collector, poll and restart logs. |
| `.claude/settings.json` | no | Written by `tm` at launch; `init.sh` adds `statusLine`, and the guard lift only on request. |

`local/.gitignore` excludes bundles and temp files. Keep secret values out of `local/`:
record locations only. The repository has no remote; push only when you decide to.

## Known limits

- Context percentage is read from the trusty-mpm status line, which narrow panes
  truncate (trusty-tools #8394).
- `tm launch` attaches to the current terminal, so the Architect restarts a
  stopped session with `tm sessions resume <name>` instead (#8406).
- On macOS, a tmux server started by a background launchd job runs every pane at
  background priority. Check it with the command in the `architect-host-admin` skill ("tmux priority"); fix it with
  `scripts/fleet-restart.sh` (#8415).

Issue numbers refer to https://github.com/example-org/trusty-tools/issues.
