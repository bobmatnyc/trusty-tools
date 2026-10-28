# trusty-mpm unattended supervisor (#1206)

The **supervisor** is a lightweight, always-on process that keeps a managed-session
fleet running 24/7 with **no live caller attached**. It is a passive
**observer**, *not* a decision-maker. Since #10356 it does **not** auto-resume
sessions: the trusty-mpm daemon does, because only the daemon installs the
runtime relauncher that puts a runtime back in a recreated pane.

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
tm supervisor --interval 30
```

Then query fleet state:

```bash
jq . ~/.trusty-mpm/supervisor-metrics.json
```

The file carries the fleet counts, the surfaced `pending_decision`s, the
supervisor's cumulative `run_stats`, the `written_at` instant it was published,
and the `interval_secs` the supervisor is configured at.

A snapshot is reported as **stale** rather than current once it is older than
three sweeps at that interval, with a 300-second floor — so the default 30s
cadence tolerates 300 seconds, and a `TRUSTY_MPM_SUPERVISOR_INTERVAL=900`
overnight supervisor tolerates 2700. Sizing the window from the cadence is what
keeps a slow overnight supervisor from reading as dead for most of every cycle;
the floor keeps a fast cadence from flapping on ordinary scheduling jitter. A
stale or absent file is how the console says "the supervisor is not running"
instead of showing a confident zero.

## Configuration (env vars)

| Variable | Default | Meaning |
|---|---|---|
| `TRUSTY_MPM_AUTO_RESUME` | — | Ignored here since #10356; set it on the **daemon** (see below). |
| `TRUSTY_MPM_SUPERVISOR_INTERVAL` | `30` | Poll interval, seconds. |
| `TRUSTY_MPM_SUPERVISOR_CLASSIFY` | `1` (on) | `0`/`false` disables idle classification (no LLM spend). |
| `TRUSTY_LLM_MODEL` | `openai/gpt-4o-mini` | OpenRouter model used for idle-session activity classification. |
| `OPENROUTER_API_KEY` | — | Required for real activity classification; absent ⇒ `unknown`. |

CLI flags (`--interval`, `--no-classify`) override the env vars. `--auto-resume`
is still accepted, and ignored with a warning.
`TRUSTY_MPM_SUPERVISOR_ADDR` and `--addr` were removed by #6288 along with the
listener they configured; a leftover value in an environment is ignored.

## Enabling auto-resume

Auto-resume is **opt-in** and runs in the **daemon** (#10356). Its sweep runs at
daemon start and then every `TRUSTY_MPM_SUPERVISOR_INTERVAL` seconds. To enable
it, do **one** of:

- turn on the console's auto-resume toggle (it writes `~/.trusty-mpm/auto_resume`;
  this file wins over the env var), **or**
- set `TRUSTY_MPM_AUTO_RESUME=1` in the **daemon's** environment.

For `TRUSTY_MPM_RESUME_BOOT_GRACE_SECS` after the daemon starts (default 600), a
runtime exit does not count toward the flap breaker. When the window ends, the
daemon logs one `warn` line (target `trusty_mpm::resume_park`) naming every
session still parked.

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
