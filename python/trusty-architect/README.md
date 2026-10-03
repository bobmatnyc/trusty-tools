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
