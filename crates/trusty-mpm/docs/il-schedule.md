# The Architect IL schedule — `local/il-schedule.json`

The IL schedule tells the Architect's poller which sessions it may clear and
at which context thresholds (SPEC-IL-01, section 7), and which cadence jobs
`il-cron.py` runs. `tm fleet init` seeds it from a preset embedded in `tm`.

`tm il config` is the only writer of the file (ADR-0077, D3):

| Command | Effect |
|---|---|
| `tm il config show` | Print the schedule; the built-in preset when the file is absent. |
| `tm il config enroll <session> --project <dir> [--code <c>]` | Add a session entry. |
| `tm il config set thresholds.<key> <n>` | Set a global threshold. |
| `tm il config set --session <name> <key> <value>` | Set a session field: `project`, `code`, `signals`, `state_path`, `thresholds.<key>`. |
| `tm il config remove <session>` | Drop a session entry. |
| `tm il config reset [--force]` | Restore the preset; a file that differs needs `--force`. |

Every verb takes `--dir <architect-dir>`; the default is the recorded
Architect. A refused edit or a failed write leaves the file unchanged and
exits non-zero.

## Reference

The section below is generated from the preset. Regenerate it with
`UPDATE_DOCS=1 cargo test -p trusty-mpm --test integration generated_docs::`.

<!-- BEGIN GENERATED: il-schedule -->
Generated from the preset embedded in `tm` (`crates/trusty-mpm/src/assets/architect/templates/local/il-schedule.json`).

### Top-level keys

| Key | Meaning |
|---|---|
| `jobs` | Cadence jobs `il-cron.py` runs: `{name, due, every, command}`. Not edited by `tm il config`. |
| `sessions` | Enrolled sessions, one entry each (see Session fields). Edited by `enroll`, `set --session` and `remove`. |
| `thresholds` | Global clear thresholds (see Thresholds). Edited by `set thresholds.<key>`. |
| `do_not_clear` | Projects whose sessions are never cleared (SPEC-IL-01 section 8, gate 8). |
| `paused_sessions` | Sessions on the pause list; the cycle skips them (SPEC-IL-01 section 8, gate 8). |
| `session_example` | The documented session example. No reader acts on it; copy its shape with `enroll` and `set`. |

### Session fields

| Field | Meaning |
|---|---|
| `name` | Required. The tmux session name; `enroll <session>` sets it. |
| `project` | Required. The project directory holding `.trusty-mpm/datg/`; stored as an absolute path. |
| `code` | The IL id code, 2-8 letters or digits. |
| `signals` | The clear signals evaluated, a subset of S1-S5; absent means all five. `set` takes a comma list. |
| `thresholds` | Per-session overrides of the global thresholds; `set --session <name> thresholds.<key> <n>`. |
| `state_path` | An absolute path to the IL `state.json`; absent means `<project>/.trusty-mpm/datg/[<name>/]state.json`. |

### Thresholds

| Key | Preset value | Meaning |
|---|---|---|
| `floor_ctx` | 150000 | Tokens. S1, S3 and S4 fire only at or above this context size. |
| `stale_pct` | 70 | Percent, 0-100. S1 fires when the self-reported stale share reaches it. |
| `bar_ctx` | 250000 | Tokens. S2 clears at a boundary at or above this context size. |
| `approach_ctx` | 220000 | Tokens. At or above it S2 asks the PM to reach a boundary. |
| `guard_ctx` | 400000 | Tokens. S5, the compaction guard. |
| `fixed_base` | 83000 | Tokens. The context a clear cannot recover; used for the stale proxy. |
| `s2_refire_s` | not set (reader default) | Seconds between two S2 firings for one session. |
| `s3_min_idle_s` | not set (reader default) | Seconds. S3 needs at least this much idle time. |
| `s3_max_idle_s` | not set (reader default) | Seconds. S3 never fires after this much idle time. |
| `s4_idle_s` | not set (reader default) | Seconds. S4 needs more idle time than this. |
| `boundary_slack_s` | not set (reader default) | Seconds a last turn may trail `boundary_at` and still be at the boundary. |
| `il_max_age_s` | not set (reader default) | Seconds. An IL older than this is stale. |
| `rate_per_hour` | not set (reader default) | Automatic clears allowed per session per hour. |
| `rate_per_day` | not set (reader default) | Automatic clears allowed per session per day. |
| `s2_after_clear_s` | not set (reader default) | Seconds after a clear in which S2 does not clear the session again. |
| `architect_bar_ctx` | not set (reader default) | Tokens. The Architect session's own context bar; absent means the PM bar. |

### Session example

The preset's `session_example`:

```json
{
  "name": "tm-my-project",
  "project": "/srv/projects/my-project",
  "code": "mp",
  "signals": [
    "S1",
    "S2",
    "S5"
  ],
  "thresholds": {
    "floor_ctx": 180000
  }
}
```

The commands that enroll the same session:

```bash
tm il config enroll tm-my-project --project /srv/projects/my-project --code mp
tm il config set --session tm-my-project signals S1,S2,S5
tm il config set --session tm-my-project thresholds.floor_ctx 180000
```
<!-- END GENERATED: il-schedule -->
