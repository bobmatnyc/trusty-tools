# trusty-architect

Content for the Architect, the one fleet session per user that
trusty-mpm 2.0 sets up (trusty-tools #8436). The Architect watches the PM
sessions of the projects a user names, relays between them and the user, and
does not implement.

This directory is self-contained. It is not a Cargo workspace member, holds no
`Cargo.toml`, and imports nothing from the rest of the monorepo. Its Python is
standard library only. The repo-root drift check enforces this and also fails
when the skill copy shipped in trusty-mpm differs from `skills/`.

## Contents

| Path | Purpose |
| --- | --- |
| `skills/tm-architect-setup.md` | Canonical copy of the bundled setup skill. trusty-mpm ships a byte-identical copy. |
| `skills/tm-fleet-check.md`, `skills/tm-context-refresh.md` | The Architect's operations skills: one supervision pass, and a watched session's context refresh. Architect only: `tm fleet init` writes them into the Architect project's `.claude/skills/`, and no other project receives them. |
| `skills/architect-*.md` | The eight skills the operating manual indexes (tmux ops, operating rules, workflow, search and memory, GitHub, strategy, the completion standard, the owner model). Seeded the same way. |
| `docs/voice.md` | The voice guide, seeded as `docs/architect/voice.md`. |
| `templates/` | What `tm fleet init` seeds into the Architect project: `CLAUDE.md` (the fleet specifics), `gitignore` (written as `.gitignore`), and `local/`: the record templates (state, actions, goals, user, instance facts, `projects/_template.md`, the owner model in `owner/` and the mandates schema in `mandates/`), and the `architect.json` config seed with no owner values. |
| `scripts/fleet-poll.py` | Deterministic fleet poller, no LLM. Classifies each `tm-*` PM pane, reads push-hook events, checks host load, disk, swap and the Architect's own context, appends new alerts to `inbox/alerts.jsonl`, and wakes the Architect's pane with a one-line pointer. `--once`, `--interval N`, `--dry-run`. |
| `scripts/fleet-classify.py` | The poller's pure half: pane parsing and alert evaluation, with no tmux call or file I/O. `fleet-poll.py` loads it by path, so it ships beside it. |
| `scripts/input-state.py` | Classifies a pane's input box as `empty`, `suggestion` (Claude Code's dim auto-suggestion) or `typed`. The poller never types over a real draft. |
| `scripts/self-ctx.py` | Measures the Architect's own context from its newest interactive Claude Code transcript. |
| `scripts/alert_triage.py` | Sorts poller alerts into wake and log by fixed rules and the holds in `local/triage-rules.json`; the rest go to one batched Haiku call that fails open to wake. `--replay N` re-runs the gate over recent alerts. The shipped `fleet-poll.py` does not load it yet (#10161, P1b). |
| `scripts/architect-watchdog.py` | Classifies the Architect session as dead, frozen, silent or ok and decides on a debounced relaunch. Pure logic, no I/O. The shipped `fleet-poll.py` does not load it yet (#10161, P1b). |
| `scripts/pm_intake.py` | PM-message intake: `notify` appends one line to `inbox/pm-messages.jsonl`, and the pure classifier marks each message wake or hold (decision, blocker, head and done wake; ack, progress and fyi wait for the digest) and drops a repeat within 30 minutes. Its cursor verbs (`unread`, `ack`, `ack-through`, `push-due`) need `pm_cursor.py`, which lands in P4a; until then they fail with `ModuleNotFoundError`. The shipped `fleet-poll.py` does not load it yet (#10161, P1b). |
| `scripts/pm-digest.py` | Prints what the PMs sent since the last wake in at most 20 lines, and builds the one-line wake digest. Loads `pm_intake.py` by path, so it ships beside it. |
| `scripts/pm-notify.sh` | The command a PM runs to queue a message for the Architect: `pm-notify.sh --kind K --session S "text"`. Runs `pm_intake.py notify`. |
| `scripts/notify-supervisor.sh` | Claude Code `Notification` hook receiver: appends one line to `inbox/events.jsonl` and always exits 0. A fallback only: `tm hook` already writes the same events, so `tm fleet init` seeds it and wires no hook. |
| `scripts/quiet-sessions.py` | Reads the quiet-session list (idle alerts dropped) and matches throwaway test-fixture session names (`exited` alerts dropped). |
| `scripts/start-fleet-poll.sh` | Starts the poller in its own tmux session unless it is already running. Arguments pass through. |
| `scripts/relay.sh` | The two-step relay send: refuses unless the box is empty or a suggestion, types the text, and presses Enter only when the box starts with the Architect marker. |
| `scripts/posted.sh`, `scripts/relay-marker.py` | The double-post guard for a shared pane: lists relays sent since a time, or prints `NONE`. |
| `scripts/architect_config.py` | Loads the instance settings in `local/architect.json` (see Configuration). Imported by sibling scripts; no command line. |
| `scripts/datg.py`, `scripts/datg_store.py`, `scripts/datg_ops.py`, `scripts/datg_log.py`, `scripts/datg_read.py`, `scripts/datg-hook.sh` | The Intent Ledger store (`docs/specs/SPEC-IL-01.md`): a PM's goals, decisions, asks, tasks and session record in `<project>/.trusty-mpm/datg/`, with the resume digest, `fresh` and the Stop-hook `stamp`. `datg.py` is the CLI and imports the four `datg_*` modules from its own directory, so the five ship together. `datg-hook.sh` is the SessionStart, Stop and PreToolUse hook wrapper and always exits 0. The shipped poller does not load them yet (#10161, P1b). |
| `scripts/ctx-watch.py` | Evaluates the clear signals S1 to S5 per session from `local/il-schedule.json`, the IL session record and the session transcript, and prints the clear candidates. Sends no keys. Loads `self-ctx.py` by path. The shipped poller does not load it yet (#10161, P1b). |
| `scripts/il-cron.py` | Runs the Intent Ledger cadence jobs in `local/il-schedule.json` when due, retries a failed job up to 3 times, then raises one alert. Loads `ctx-watch.py` by path. Its `prune` job runs `il-archive.py`, which lands in P4a; until then that job fails and alerts. The shipped poller does not load it yet (#10161, P1b). |
| `scripts/self-clear.sh` | The Architect's gated self-refresh: refuses unless `datg.py fresh` passes and the IL session block is recorded, then, with `--live` only, waits for idle and sends `/clear` and a resume prompt. A dry run by default. Its idle test needs `turn-active.py` (P1b). The shipped poller does not load it yet (#10161, P1b). |
| `scripts/state-archive.py` | Moves old handoff sections out of `local/state.md` into `local/state-archive.md`, keeping the newest 3 and the `NEXT IDS` line. `--dry-run` writes nothing. The shipped poller does not load it yet (#10161, P1b). |
| `scripts/architect-idle.py` | Exits 0 when the Architect's newest transcript ends on a finished assistant turn: the transcript half of `self-clear.sh`'s idle test. Loads `self-ctx.py` by path. The shipped poller does not load it yet (#10161, P1b). |
| `docs/specs/SPEC-IL-01.md` | The Intent Ledger spec the scripts above cite. A design record: trusty-mpm does not ship it. |
| `tests/` | pytest suite for the scripts above. |

## Configuration

Every setting is an environment variable with a default; no operator path,
account or session name is built in.

| Variable | Default | Used for |
| --- | --- | --- |
| `ARCHITECT_SESSION` | `tm-architect` | The Architect's tmux session: the only pane the poller sends keys to. |
| `ARCHITECT_POLL_SESSION` | `<ARCHITECT_SESSION>-poll` | The poller's own tmux session. |
| `ARCHITECT_INBOX_DIR` | `<this dir>/inbox` | `events.jsonl`, `alerts.jsonl`, `poll-state.json`, `poll.log`. |
| `ARCHITECT_QUIET_SESSIONS_FILE` | `<this dir>/quiet-sessions.txt` | One session name per line; `#` starts a comment. |
| `ARCHITECT_PROJECT_DIR` | `~/trusty-mpm-projects/architect` | The Architect's project, used to find its transcripts. |
| `CLAUDE_CONFIG_DIR` | `~/.trusty-tools/trusty-mpm/claude-config` | The Claude Code config dir trusty-mpm launches under. |
| `SELF_CTX_DIR` | `<CLAUDE_CONFIG_DIR>/projects/<slug of ARCHITECT_PROJECT_DIR>` | Transcript directory override. |
| `CONTEXT_BAR_TOKENS` | `250000` | The context bar for every watched session and the Architect itself (#9997). |
| `CONTEXT_WINDOW_TOKENS` | `1000000` | The window a pane's `ctx N%` is read against; keep it above the bar. |
| `LOAD_FACTOR` | `1.5` | Load alert when the 5-minute average exceeds this many times the core count. |
| `TMUX_SOCKET` | tmux's own default | Selects the tmux server. |

### Instance settings: `local/architect.json`

Settings that name the owner or the owner's repos live in the Architect
project's own `local/architect.json`, never in a script (#10161).
`tm fleet init` seeds it with empty values; the Architect fills it in.
`scripts/architect_config.py` reads it: `architect_config.load()` returns the
resolved settings. A variable beats the file, the file beats the default, and
an empty variable counts as unset.

| Key | Type | Default | Variable |
| --- | --- | --- | --- |
| `owner_name` | string | `""`: say "the owner" | `ARCHITECT_OWNER_NAME` |
| `memory_palace` | string | `""`: derive it from the session or project | `ARCHITECT_MEMORY_PALACE` |
| `dashboard_port` | integer, 1 to 65535 | `7890` | `DASHBOARD_PORT` |
| `repos` | `{name: {slug, gh_config_dir, checkout}}`, `slug` required | `{}` | none |
| `caps` | `{name: number}`; `builder_slots` an integer of 1 or more, `load_factor` above 0 | `{}`: each script's own constant | `LOAD_FACTOR` sets `caps.load_factor` |

`ARCHITECT_CONFIG` names another file. A missing or blank file gives the
defaults. Malformed JSON, a wrong type or an out-of-range value raises
`ConfigError` naming the file and the key, never the value; nothing falls back
to the defaults. An unknown key is ignored and listed in `Config.unknown`.
Session names stay variables only.

## Tests

```sh
cd python/trusty-architect && python3 -m pytest
```

The tests need pytest and nothing else. CI runs them, plus the drift check's
selftest, only when this directory or the check changes.

## Export and scrub (maintainers)

Files reach this directory from the Architect checkout through the repository
root's `scripts/architect_export.py`, never by hand copy (#9171). It copies only
the paths in `scripts/architect_export_manifest.txt`, refuses instance paths
(`local/`, `owner/`, `records/`, `inbox/`, `docs/briefs/`, `.env*`,
`.mcp.json`) and symlinks even when listed, applies an out-of-tree rewrite
table, and exits non-zero on any leak residue. It requires an out-of-tree deny
file and writes nothing unless the result is clean.

```sh
python3 scripts/architect_export.py --source <architect checkout> \
  --staging <empty dir outside this repo> --deny-file <deny file> \
  --rewrite-file <rewrite file> --dry-run
```

Checklist for a PR that brings exported files in:

- [ ] Staged with `scripts/architect_export.py`, gate clean, then copied in.
- [ ] Ran `ARCHITECT_LEAK_DENY_FILE=<out-of-tree file> bash scripts/check_architect_subproject.sh` on the final tree.
- [ ] Ran the deny list over `git log -p origin/main..HEAD`.
- [ ] Opened the diff of every `*.json` and `*.jsonl` fixture.

## Deployment

trusty-mpm ships a byte-identical copy of every file under `scripts/`,
`skills/`, `templates/` and `docs/` except `docs/specs/`, and the drift check
fails when one differs. `tm fleet init` writes them into the Architect project: `scripts/`,
`.claude/skills/<name>/SKILL.md`, `CLAUDE.md`, `.gitignore`, `local/` and
`docs/architect/voice.md`.
It writes a file only when it is absent and reports an edited one as skipped.
With a launch it starts the poller through `scripts/start-fleet-poll.sh`, and
a start that fails fails the command.

## Not here yet

The question collector, the host-admin, review, drift-check, improvements,
provision and dashboard skills, and `fleet-restart` stay in the prototype for
now (#8536).

## Provision by hand

For a PM whose `tm` lacks `tm fleet init`, or whose `tm fleet init` fails.
Try `tm fleet init` first (`tm fleet --help` lists it). When it works, stop
here: it does every step below. When it fails, read its error and fix that
cause first. The hand procedure below makes the same project, but it skips
the preflight checks `init` makes. Do not run it beside a working Architect.

Rules:

- One Architect per user. Run `tmux has-session -t =tm-architect` first. If it
  succeeds, an Architect exists. Stop.
- Never add a git remote to the Architect's repo.
- Never set `TRUSTY_MPM_PM_UNRESTRICTED`. The `supervisor` profile replaces it.
- Never copy another installation's `CLAUDE.md`, `local/` or `inbox/`. Write
  them for this user.
- Launch the session only when the owner says so.

### 1. Check the prerequisites

| Need | Check |
| --- | --- |
| `tm`, `claude`, `tmux`, `gh`, `python3` on `PATH` | `command -v tm claude tmux gh python3` |
| trusty-memory and trusty-search healthy | `tm doctor`: the `memory` and `search` rows |
| A copy of this directory | A trusty-tools checkout. Below, `<src>` is its `python/trusty-architect/`. |

The scripts use the Python standard library only.

### 2. Make the project

```sh
dir=~/architect     # any path; use it in every step, and set ARCHITECT_PROJECT_DIR="$dir" if it is not the default
mkdir -p "$dir" && git -C "$dir" init   # local repo, no remote
printf 'profile = "supervisor"\n' > "$dir/.trusty-mpm.toml"
```

`git -C "$dir" remote` must print nothing.

### 3. Place the files

Copy each file only if the target is absent. Never overwrite an edited one.

```sh
src=<src>
mkdir -p "$dir/scripts" "$dir/local/projects" "$dir/local/users" "$dir/local/briefs" \
  "$dir/docs/architect"
cp -n "$src/templates/CLAUDE.md"        "$dir/CLAUDE.md"
cp -n "$src/templates/gitignore"        "$dir/.gitignore"
cp -Rn "$src/templates/local/."         "$dir/local/"
cp -n "$src"/scripts/*.py "$src"/scripts/*.sh "$dir/scripts/"
cp -n "$src/docs/voice.md"             "$dir/docs/architect/voice.md"
for f in "$src"/skills/tm-fleet-check.md "$src"/skills/tm-context-refresh.md "$src"/skills/architect-*.md; do
  stem="$(basename "$f" .md)"
  mkdir -p "$dir/.claude/skills/$stem"
  cp -n "$f" "$dir/.claude/skills/$stem/SKILL.md"
done
chmod 755 "$dir/scripts/fleet-poll.py" "$dir/scripts/input-state.py" \
  "$dir/scripts/start-fleet-poll.sh" "$dir/scripts/relay.sh" "$dir/scripts/posted.sh"
```

This is the same set `tm fleet init` writes. The setup skill
`skills/tm-architect-setup.md` is not copied here. It is a PM skill that tm
ships to every project.

### 4. Write the instance layer

Edit `$dir/CLAUDE.md` for this user. Do not copy it from elsewhere.

- Replace `<Name>` with the user's name for the relay marker.
- Fill the watch set: one line per PM session, with its tmux session name,
  project path and goal.
- Set the cadence only if the owner wants a value other than the default.

The files under `local/` stay as seeded. The Architect fills them. Commit
the project:

```sh
git -C "$dir" add -- CLAUDE.md .gitignore .trusty-mpm.toml local scripts .claude
git -C "$dir" commit -m "chore: seed the Architect project"
```

### 5. Grant the profile

Add the absolute path of `$dir` to the user-level allowlist,
`~/.trusty-mpm/config.toml`. Extend an existing `[supervisor]` table; do not
add a second one:

```toml
[supervisor]
projects = ["/absolute/path/to/architect"]
```

Without this entry a launch runs the PM profile, not `supervisor`.

Do not grant direct action on other panes (a pm-guard lift) or twin mode
(`[supervisor.twin]`). Each is the owner's decision. Ask. `tm fleet init`
sets neither.

Give the Architect its own memory palace and search index. Both are named
`architect`. Do not name them `supervisor`: that is the profile, not the project.

```sh
trusty-memory link --path "$dir" --slug architect   # pins the palace in .trusty-tools/trusty-memory.yaml
trusty-search index "$dir" --name architect         # registers and indexes the project
```

```sh
git -C "$dir" add -- .trusty-tools
git -C "$dir" commit -m "chore: pin the Architect palace"
```

### 6. Check before launch

From `$dir`:

```sh
tm doctor | grep session_profile    # says a launch here runs the `supervisor` profile
tm fleet status --dir "$dir"        # only on a tm that has `tm fleet`; reports the pre-launch checks
python3 scripts/fleet-poll.py --dry-run   # one cycle: classifies panes, writes nothing, sends no keys
python3 scripts/input-state.py %<pane>    # prints `<pane> <empty|suggestion|typed|no-prompt> | <text>`
```

Pick `%<pane>` from `tmux list-panes -a -F '#{pane_id} #{session_name}'`.
Before the launch, `tm fleet status` shows `allowlist` and `profile` as `ok`.
It shows `session`, `launch_stamp`, `binding` and `this_session` as failing
until the launch, and exits 1. That is expected.

### 7. Launch (the owner's instruction only)

The Architect runs on Opus in tmux session `tm-architect`, or the name in
`ARCHITECT_SESSION`. Set `ARCHITECT_SESSION` in the poller's environment if
you choose another name (step 8 shows how).

```sh
tmux new-session -d -s tm-architect -c "$dir"
tmux send-keys -t =tm-architect "tm launch '$dir'" Enter
```

`tm launch` reads the allowlist entry and runs the `supervisor` profile on
its model. After the launch, `/model` in the pane must show Opus. If it shows
the PM profile, return to step 5.

### 8. Start the poller

```sh
"$dir/scripts/start-fleet-poll.sh"   # prints `started: tm-architect-poll` or `running: ...`
```

The poller runs in tmux session `<ARCHITECT_SESSION>-poll`. With a custom
name, run `ARCHITECT_SESSION=<name> "$dir/scripts/start-fleet-poll.sh"`.
The script is idempotent. Pass `--interval N` to change the cycle length.

### 9. Verify after launch

- `ls "$dir/inbox/"` shows `poll.log` and `poll-state.json`. The poller
  appends `alerts.jsonl` when it raises its first alert.
- Run from `$dir`:
  `python3 -c "import importlib.util as u; s=u.spec_from_file_location('c','scripts/self-ctx.py'); m=u.module_from_spec(s); s.loader.exec_module(m); print(m.self_ctx_tokens())"`
  prints a token count once the Architect has a transcript. It prints `None`
  before that.
- `tm fleet status --dir "$dir"` exits 0 and prints `complete`.
- The Architect's first supervision pass appends an entry to
  `local/actions.md`.

When a step fails, report it to the owner. Do not start a second Architect.
