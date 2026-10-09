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
| `templates/` | What `tm fleet init` seeds into the Architect project: `CLAUDE.md` (the fleet specifics), `gitignore` (written as `.gitignore`), and `records/`. |
| `scripts/fleet-poll.py` | Deterministic fleet poller, no LLM. Classifies each `tm-*` PM pane, reads push-hook events, checks host load, disk, swap and the Architect's own context, appends new alerts to `inbox/alerts.jsonl`, and wakes the Architect's pane with a one-line pointer. `--once`, `--interval N`, `--dry-run`. |
| `scripts/fleet-classify.py` | The poller's pure half: pane parsing and alert evaluation, with no tmux call or file I/O. `fleet-poll.py` loads it by path, so it ships beside it. |
| `scripts/input-state.py` | Classifies a pane's input box as `empty`, `suggestion` (Claude Code's dim auto-suggestion) or `typed`. The poller never types over a real draft. |
| `scripts/self-ctx.py` | Measures the Architect's own context from its newest interactive Claude Code transcript. |
| `scripts/quiet-sessions.py` | Reads the quiet-session list (idle alerts dropped) and matches throwaway test-fixture session names (`exited` alerts dropped). |
| `scripts/start-fleet-poll.sh` | Starts the poller in its own tmux session unless it is already running. Arguments pass through. |
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
| `SELF_CTX_WINDOW`, `SELF_CTX_THRESHOLD` | `1000000`, `40` | Context window in tokens, and the alert threshold in percent. |
| `LOAD_FACTOR` | `1.5` | Load alert when the 5-minute average exceeds this many times the core count. |
| `TMUX_SOCKET` | tmux's own default | Selects the tmux server. |

## Tests

```sh
cd python/trusty-architect && python3 -m pytest
```

The tests need pytest and nothing else. CI runs them, plus the drift check's
selftest, only when this directory or the check changes.

## Deployment

trusty-mpm ships a byte-identical copy of every file under `scripts/`,
`skills/` and `templates/`, and the drift check fails when one differs.
`tm fleet init` writes them into the Architect project: `scripts/`,
`.claude/skills/<name>/SKILL.md`, `CLAUDE.md`, `.gitignore` and `records/`.
It writes a file only when it is absent and reports an edited one as skipped.
With a launch it starts the poller through `scripts/start-fleet-poll.sh`, and
a start that fails fails the command.

## Not here yet

The relay tooling, the question collector, the review, drift-check and
provision skills, and `fleet-restart` stay in the prototype for now (#8536).

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
- Never copy another installation's `CLAUDE.md`, `records/` or `inbox/`. Write
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
mkdir -p "$dir/scripts" "$dir/records/projects" \
  "$dir/.claude/skills/tm-fleet-check" "$dir/.claude/skills/tm-context-refresh"
cp -n "$src/templates/CLAUDE.md"        "$dir/CLAUDE.md"
cp -n "$src/templates/gitignore"        "$dir/.gitignore"
cp -n "$src/templates/records/state.md" "$src/templates/records/actions.md" "$dir/records/"
cp -n "$src"/scripts/*.py "$src"/scripts/*.sh "$dir/scripts/"
cp -n "$src/skills/tm-fleet-check.md"     "$dir/.claude/skills/tm-fleet-check/SKILL.md"
cp -n "$src/skills/tm-context-refresh.md" "$dir/.claude/skills/tm-context-refresh/SKILL.md"
chmod 755 "$dir/scripts/fleet-poll.py" "$dir/scripts/input-state.py" \
  "$dir/scripts/start-fleet-poll.sh"
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

`records/state.md` and `records/actions.md` stay as seeded. The Architect
fills them. Commit the project:

```sh
git -C "$dir" add -- CLAUDE.md .gitignore .trusty-mpm.toml records scripts .claude
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
  `records/actions.md`.

When a step fails, report it to the owner. Do not start a second Architect.
