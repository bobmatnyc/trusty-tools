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
| `skills/tm-architect-setup.md` | Canonical copy of the bundled setup skill. trusty-mpm ships a byte-identical copy in `content/skills/`. |
| `skills/tm-*.md`, `skills/architect-*/SKILL.md` | The Architect's skills: the pass (`tm-fleet-check`), the context refresh (`tm-context-refresh`), restarts (`tm-fleet-restart`), and the 16 `architect-*` procedures the operating manual points at. Architect only: tm deploys them into the Architect project's `.claude/skills/` at `tm fleet init` and at every launch; no other project receives them. |
| `agents/` | The Architect's agents, installed as `.claude/agents/`. |
| `templates/` | What `tm fleet init` seeds: `CLAUDE.md` (a short pointer), `AGENTS.md` (the instance layer, a marked placeholder), `gitignore` (written as `.gitignore`), the `local/` starting files and the dashboard page. The whole directory is also seeded as `templates/`. |
| `scripts/` | The poller and its helpers, hooks, installers, the dashboard, `init.sh` (the manual path) and `doctor.sh`. See `scripts/README.md`. `scripts/test_*.py` and `scripts/conftest.py` are tests and do not ship. |
| `docs/` | The voice guide, the history and the specs, seeded as `docs/`. |
| `tests/` | pytest suite for the poller scripts. |

The operating manual is not here: it is `content/instructions/architect/architect.md`
in trusty-tools, and tm composes it into the Architect's instructions.

## Configuration

Every setting is an environment variable with a default; no operator path,
account or session name is built in.

| Variable | Default | Used for |
| --- | --- | --- |
| `ARCHITECT_SESSION` | `tm-architect` | The Architect's tmux session: the only pane the poller sends keys to. |
| `ARCHITECT_POLL_SESSION` | `<ARCHITECT_SESSION>-poll` | The poller's own tmux session. |
| `ARCHITECT_INBOX_DIR` | `<this dir>/inbox` | `events.jsonl`, `alerts.jsonl`, `poll-state.json`, `poll.log`. |
| `ARCHITECT_QUIET_SESSIONS_FILE` | `<this dir>/local/quiet-sessions.txt` | One session name per line; `#` starts a comment. |
| `ARCHITECT_PROJECT_DIR` | `<this dir>` | The Architect's project, used to find its transcripts. |
| `CLAUDE_CONFIG_DIR` | `~/.trusty-tools/trusty-mpm/claude-config` | The Claude Code config dir trusty-mpm launches under. |
| `SELF_CTX_DIR` | `<CLAUDE_CONFIG_DIR>/projects/<slug of ARCHITECT_PROJECT_DIR>` | Transcript directory override. |
| `SELF_CTX_WINDOW`, `SELF_CTX_THRESHOLD` | `1000000`, `25` | Context window in tokens, and the alert threshold in percent. |
| `LOAD_FACTOR` | `1.5` | Load alert when the 5-minute average exceeds this many times the core count. |
| `TMUX_SOCKET` | tmux's own default | Selects the tmux server. |

## Tests

```sh
cd python/trusty-architect && python3 -m pytest
```

The tests need pytest and nothing else. CI runs them, plus the drift check's
selftest, only when this directory or the check changes.

## Deployment

trusty-mpm ships a byte-identical copy of every file under `agents/`, `docs/`,
`scripts/`, `skills/` and `templates/` (tests excepted), and the drift check
fails when one differs or a new file has no copy. `tm fleet init` writes them
into the Architect project: `scripts/`, `templates/`, `docs/`,
`.claude/agents/`, `.claude/skills/<name>/SKILL.md`, `CLAUDE.md`, `AGENTS.md`,
`.gitignore` and the `local/` skeleton. It writes a file only when it is absent
and reports an edited one as skipped. With a launch it starts the poller through
`scripts/start-fleet-poll.sh`, and a start that fails fails the command.
`--guard-lift` sets `TRUSTY_MPM_PM_UNRESTRICTED=1` in `.claude/settings.json`;
without it the PM guard stays on.

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
- Never set `TRUSTY_MPM_PM_UNRESTRICTED` unless the owner asks. It is off by
  default; `scripts/init.sh --guard-lift` sets it.
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

Copy the shipped files only where the target is absent, then let `init.sh`
create `local/`. Never overwrite an edited file.

```sh
src=<src>
mkdir -p "$dir/.claude/agents" "$dir/.claude/skills"
cp -Rn "$src/scripts" "$src/templates" "$src/docs" "$dir/"
cp -n "$src"/agents/*.md "$dir/.claude/agents/"
for f in "$src"/skills/tm-fleet-check.md "$src"/skills/tm-context-refresh.md "$src"/skills/tm-fleet-restart.md; do
  n=$(basename "$f" .md); mkdir -p "$dir/.claude/skills/$n"; cp -n "$f" "$dir/.claude/skills/$n/SKILL.md"
done
for d in "$src"/skills/architect-*/; do
  n=$(basename "$d"); mkdir -p "$dir/.claude/skills/$n"; cp -n "$d/SKILL.md" "$dir/.claude/skills/$n/SKILL.md"
done
cp -n "$src/templates/CLAUDE.md" "$dir/CLAUDE.md"
cp -n "$src/templates/AGENTS.md" "$dir/AGENTS.md"
cp -n "$src/templates/gitignore" "$dir/.gitignore"
rm -f "$dir"/scripts/test_*.py "$dir/scripts/conftest.py"
"$dir/scripts/init.sh"
```

This is the same tree `tm fleet init` writes. The setup skill
`skills/tm-architect-setup.md` is not copied here. It is a PM skill that tm
ships to every project.

### 4. Write the instance layer

Edit `$dir/AGENTS.md` for this user. Do not copy it from elsewhere.

- Replace `<Name>` with the user's name for the relay marker.
- Fill the watch set in `local/state.md`: one line per PM session, with its
  tmux session name and project.
- Set the cadence only if the owner wants a value other than the default.

`local/state.md` and `local/actions.md` stay as seeded. The Architect
fills them. Commit the project:

```sh
git -C "$dir" add -- CLAUDE.md AGENTS.md .gitignore .trusty-mpm.toml local scripts templates docs .claude
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
tm architect input-state %<pane>          # prints `<pane> <empty|suggestion|typed|no-prompt> | <text>`
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
