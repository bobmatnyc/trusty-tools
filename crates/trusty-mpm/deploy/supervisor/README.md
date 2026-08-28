# trusty-mpm unattended supervisor (#1206)

The **supervisor** is a lightweight, always-on process that keeps a managed-session
fleet running 24/7 with **no live caller attached**. It is a passive **observer +
auto-resumer**, *not* a decision-maker:

- It **auto-resumes** enduring (`stopped`) sessions so a rebooted host or an exited
  runtime does not leave work parked. (Gated by `TRUSTY_MPM_AUTO_RESUME`.)
- It **observes** session health and classifies idle `active` sessions via the
  activity monitor (optional; needs `OPENROUTER_API_KEY`, otherwise `unknown`).
- It **surfaces** `pending_decision`s for a human or a higher-level fleet manager
  — it **never auto-answers** a decision.
- It **survives reboots** under launchd (macOS) or systemd (Linux).
- It **publishes** fleet state to `~/.trusty-mpm/supervisor-metrics.json` after
  every sweep. It binds **no network port** (#6288): the trusty-mpm daemon reads
  that file, and the console renders it through `supervisor_status` /
  `console_metrics`.

## Run it by hand

```bash
# Observe-only (no resume):
tm supervisor

# Auto-resume stopped sessions, poll every 30s:
TRUSTY_MPM_AUTO_RESUME=1 tm supervisor --interval 30

# Or pass --auto-resume instead of the env var:
tm supervisor --auto-resume
```

Then query fleet state:

```bash
jq . ~/.trusty-mpm/supervisor-metrics.json
```

The file carries the fleet counts, the surfaced `pending_decision`s, the
supervisor's cumulative `run_stats`, and the `written_at` instant it was
published. A snapshot older than 300 seconds (ten default sweeps) is reported as
**stale** rather than current — that, or an absent file, is how the console says
"the supervisor is not running" instead of showing a confident zero.

## Configuration (env vars)

| Variable | Default | Meaning |
|---|---|---|
| `TRUSTY_MPM_AUTO_RESUME` | `0` (off) | `1`/`true` enables auto-resume of `stopped` sessions. **This is the master switch for resume.** |
| `TRUSTY_MPM_SUPERVISOR_INTERVAL` | `30` | Poll interval, seconds. |
| `TRUSTY_MPM_SUPERVISOR_CLASSIFY` | `1` (on) | `0`/`false` disables idle classification (no LLM spend). |
| `TRUSTY_LLM_MODEL` | `openai/gpt-4o-mini` | OpenRouter model used for idle-session activity classification. |
| `OPENROUTER_API_KEY` | — | Required for real activity classification; absent ⇒ `unknown`. |

CLI flags (`--interval`, `--auto-resume`, `--no-classify`) override the env vars.
`TRUSTY_MPM_SUPERVISOR_ADDR` and `--addr` were removed by #6288 along with the
listener they configured; a leftover value in an environment is ignored.

## Enabling auto-resume

Auto-resume is **opt-in** for safety — by default the supervisor runs observe-only
and only surfaces fleet state. The launchd plist and systemd unit below ship with
`TRUSTY_MPM_AUTO_RESUME` **commented out**, so an out-of-the-box install never
resumes sessions until you opt in. To enable it, do **one** of:

- **uncomment / set `TRUSTY_MPM_AUTO_RESUME=1`** in the supervisor's environment
  (uncomment the `TRUSTY_MPM_AUTO_RESUME` block in the plist, or the
  `Environment=TRUSTY_MPM_AUTO_RESUME=1` line in the systemd unit), **or**
- pass `tm supervisor --auto-resume`.

## Persistence — macOS (launchd)

```bash
# 1. Find your tm binary and home directory:
which tm          # e.g. /Users/you/.cargo/bin/tm
echo "$HOME"      # e.g. /Users/you  (use the REAL $HOME, not an assumed /Users/<user>)

# 2. Copy the plist and substitute the placeholders. __HOME__ expands to the
#    real $HOME so non-default home prefixes (not just /Users/<user>) work:
mkdir -p ~/Library/LaunchAgents ~/.trusty-mpm/logs
sed -e "s|__HOME__|$HOME|g" \
    -e "s|__TM_BINARY_PATH__|$(which tm)|g" \
    crates/trusty-mpm/deploy/supervisor/com.trusty.mpm.supervisor.plist \
    > ~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist

# 3. Load it (and restart it after any `cargo install` of tm):
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist

# Restart / reload:
launchctl bootout   gui/$(id -u) ~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.trusty.mpm.supervisor.plist
```

`RunAtLoad` + `KeepAlive` make it survive reboots and restart on crash.

The plist's `EnvironmentVariables` block sets a full `PATH`
(`/opt/homebrew/bin:/usr/local/bin:$HOME/.local/bin:$HOME/.cargo/bin:` + system
dirs, with `$HOME` substituted in for the `__HOME__` placeholder by the `sed`
recipe above — launchd does NOT expand `~`, so the value must be the literal
absolute home path). launchd otherwise
relaunches the agent with a minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`)
that omits Homebrew (`tmux`) and `~/.local/bin` (`claude`), which 500s
managed-session spawns after every restart (#1298). The daemon also resolves
`tmux`/`claude` via well-known dirs at runtime as a belt-and-braces fallback,
so spawns survive even a minimal inherited `PATH`.

## Persistence — Linux (systemd user service)

```bash
mkdir -p ~/.config/systemd/user
sed "s|__TM_BINARY_PATH__|$(which tm)|g" \
    crates/trusty-mpm/deploy/supervisor/trusty-mpm-supervisor.service \
    > ~/.config/systemd/user/trusty-mpm-supervisor.service

systemctl --user daemon-reload
systemctl --user enable --now trusty-mpm-supervisor.service
loginctl enable-linger "$USER"     # run without an active login session
```

## Logs

- macOS: `~/.trusty-mpm/logs/supervisor.{out,err}.log` plus the daily-rotated
  `~/.trusty-mpm/logs/trusty-mpm.log*`.
- Linux: `journalctl --user -u trusty-mpm-supervisor -f`.

A publish failure is logged at `error` and the sweep loop keeps going — losing
observability must not stop auto-resume. The daemon then reports the snapshot as
stale or unavailable, so a persistent failure shows up in the console rather than
being swallowed.
