# T99 result: trusty in the cloud (headless container)

**Status:** Result record
**Subsystem:** `docker/cloudtest/` reference build; context in [DOC-90](./DOC-90-cloud-sled-network-boundary.md) and [ADR-0076](../adr/0076-cloud-sled-network-boundary.md)
**Issue:** [#11054](https://github.com/bobmatnyc/trusty-tools/issues/11054) (epic, closed)
**Last-updated:** 2026-11-12

This page records the T99 result for running the headless trusty stack in a container. It is a result record, not a spec: it states what was measured and what is open. Sources are the #11054 comments of 2026-11-11 and 2026-11-12. The harness is `docker/cloudtest/` (PR #11062).

## 1. Headless container status

A headless container ran on the Linux build host from the `docker/cloudtest/` image (ubuntu:26.04). It runs trusty-mpm `tm`, trusty-memory and trusty-search as released binaries. All three daemons listen on unix sockets only, with no TCP and no auth ([ADR-0032](../adr/0032-no-service-owns-http-console-is-the-only-http-surface.md)). Remote use today means SSH stdio or `docker exec`. A cloud MCP endpoint needs a new HTTP gateway with TLS and auth (DOC-90, ADR-0076).

Finding: the headless Claude Code login needs a human to copy and paste a code from a browser.

## 2. Rung 1

Result: PASS, with released binaries tm 1.12.2, trusty-memory 0.36.7 and trusty-search 0.63.1. Daemons came up, `tm doctor` exited 0, one MCP call per server succeeded, project register worked, and a memory and search round trip worked. State survived `docker restart` once the #11060 lock workaround was in place.

Image provenance: the source comment (6805119153) was written against the draft PR #11062 and does not say the run used the post-#11062 image. Rerun on post-#11062 image pending.

## 3. Rung 2

Pending owner login (tracked on #11054).

## 4. macOS-dependency list

Closure condition 5: macOS, launchd, tmux, Keychain and logged-in-user assumptions (rung 1 bar). Source: #11054 comment 6832593577, verified at `origin/main` `59631ba6e5`. Scope is the crates the container runs (trusty-mpm `tm`, trusty-memory, trusty-search, trusty-common). trusty-installer and trusty-secrets are listed where `tm` or the daemons reach them.

Key. Handling: **workaround** (Dockerfile or entrypoint), **not needed**, **open**. Blocks cloud: yes / no / partial.

No read of `USER`, `LOGNAME`, `whoami`, `getlogin` or `getpwuid` login lookups was found in the `src/` of tm, memory, search or common, except the `getpwuid` home fallback in row U3.

### 4.1 macOS-only paths and APIs (11 rows)

| # | Assumption | Where | Container handling | Blocks |
|---|---|---|---|---|
| M1 | Data dir defaults to `~/Library/Application Support`; Linux resolves `$XDG_DATA_HOME` | `trusty-common/src/data_dir.rs:129` | workaround: `HOME`, `XDG_DATA_HOME` on the `/data` volume (`Dockerfile:80-81`) | no |
| M2 | trusty-search HOME fallback builds a macOS path under `cfg(macos)`; Linux gets `~/.local/share` | `trusty-search/src/service/data_dir.rs:64-70` | not needed: Linux branch, `HOME` set | no |
| M3 | trusty-search refuses to index under `/tmp/`, `/private/tmp`, `/var/folders`, `/Library/Application Support` | `trusty-search/src/allowlist/mod.rs:118-127` | workaround: demo repo under `/data/work` (`check.sh:17`) | partial: project checkouts in the cloud must not live under `/tmp` |
| M4 | RAM detection uses `sysctl hw.memsize` on macOS; Linux reads `/proc/meminfo` and cgroup limits | `trusty-common/src/machine_tier/detect.rs:28-32`, `:58-69` | not needed: Linux branch | no |
| M5 | RAM minimum is now an advisory only; the Dockerfile still says the guard "would refuse" | `trusty-search/src/commands/start/daemon.rs:728-733` vs `Dockerfile:79,83` | not needed: `TRUSTY_SKIP_RAM_CHECK=1` only silences the advisory. Search side only; memory side unverified | no |
| M6 | CoreML execution provider and its cache `~/Library/Caches/trusty-embedder/coreml` are Apple-Silicon only | `trusty-common/src/embedder/fast_embedder.rs:479`, `:520-522` | not needed: CPU path | no |
| M7 | Embedder model files come from a fastembed cache dir; a cold cache needs a model host | `trusty-common/src/embedder/fast_embedder.rs:439` | workaround: host cache mounted read-only (`Dockerfile:82`, `README.md:26-27`). Which search mode ran is in `/data/work/search-mode` (`check.sh:129,136`); the mode of the PASS run is unverified | partial: egress allowlist has no model host, so the cache must ship in the image or volume |
| M8 | `tm tui` Focus drives iTerm2 through `osascript` | `trusty-mpm/src/tui/iterm2.rs:51`, `:233`, `:238` | not needed: headless | no |
| M9 | Fleet dashboard login agent needs launchd; off macOS `render_plist` bails | `trusty-mpm/src/bin/tm/commands/fleet/dashboard_agent.rs:436-438` | not needed: not run | no |
| M10 | Installer signs binaries (`codesign`) on macOS | `trusty-installer/src/commands/macos_signing/mod.rs:391` | not needed: binaries come from release tarballs (`Dockerfile:12-22`) | no |
| M11 | Binary lookup adds `/opt/homebrew/bin` and `/usr/local/bin` to `PATH` | `trusty-common/src/bin_resolve.rs:44` | not needed: binaries install to `/usr/local/bin` (`Dockerfile:72`) | no |

### 4.2 launchd (11 rows)

| # | Assumption | Where | Container handling | Blocks |
|---|---|---|---|---|
| L1 | The `launchd` module exists only on macOS | `trusty-common/src/lib.rs:259-260` | not needed | no |
| L2 | Supervision query shells `launchctl list` on macOS; off macOS it answers NotSupervised | `trusty-common/src/supervision.rs:112-125` | not needed: `tm daemon` reports unsupervised | no |
| L3 | `tm daemon` start, stop and the MCP bridge read `~/Library/LaunchAgents` to decide who owns the daemon; a missing dir reads as no unit | `trusty-mpm/src/bin/tm/commands/launchd_probe.rs:162-165`, `:275-277`; `daemon_run.rs:317-345` | not needed: no dir, so NotRegistered | no |
| L4 | Ownership probe runs `launchctl list` and `launchctl print gui/<uid>/<label>`; a missing `launchctl` reads as Unavailable | `trusty-mpm/src/bin/tm/commands/launchd_probe.rs:484-503`, `:517` | not needed | no |
| L5 | `trusty-memory service` and `trusty-search service` install LaunchAgents; elsewhere they bail and point to systemd | `trusty-memory/src/commands/service.rs:93-99`; `trusty-search/src/commands/service.rs:90-97` | workaround: `entrypoint.sh:41-43` runs the daemons in the foreground | partial: no Linux unit file in the repo; the container stop-on-first-exit (`entrypoint.sh:47-51`) stands in |
| L6 | `tm` guided autostart has a launchd target only on macOS | `trusty-mpm/src/bin/tm/commands/guided_autostart.rs:219-238` | not needed: returns None | no |
| L7 | `tm doctor` rows read `~/Library/LaunchAgents` (plist secrets, process type); the secrets row reads an absent dir as clean | `trusty-mpm/src/daemon/doctor.rs:687`, `:806-810`; `doctor_launchd_secrets.rs:138-141` | not needed: `tm doctor` exit 0 in rung 1. The process-type row on a host with no LaunchAgents is unverified | no |
| L8 | `trusty-memory doctor` launchd check is skipped off macOS and prints a WARN | `trusty-memory/src/commands/doctor/mod.rs:308-314` | open: `check.sh` runs `tm doctor` only (`:95`), not `trusty-memory doctor` | no |
| L9 | `tctl` start, stop and restart go through `launchctl`; a Linux backend exists but the daemons are not moved onto it | `trusty-installer/src/commands/lifecycle.rs:341`; `trusty-common/src/linux_lifecycle/mod.rs:1-32` (#11084 PHASE_1; #11098 open) | not needed: installer unused; daemons restart by container restart | partial: no in-place restart on Linux today |
| L10 | After a self-upgrade the daemon restarts only if launchd supervises it; otherwise it prints a hint | `trusty-common/src/update/upgrade.rs:507-509` | not needed: no `cargo` in the image. Whether the daemons attempt an update check is unverified (#11075) | no |
| L11 | Daemons start at login because launchd starts them (#10597 open) | issue #10597 | workaround: the container entrypoint starts all three (`entrypoint.sh:41-43`) | no |

### 4.3 tmux (5 rows)

| # | Assumption | Where | Container handling | Blocks |
|---|---|---|---|---|
| T1 | Managed Claude sessions use a tmux backend; `stream-json` is the other backend | `trusty-mpm/src/bin/tm/commands/sessctl.rs:88` | tmux installed (`Dockerfile:55`). No tm-managed session was run: rung 2 calls `claude -p` directly (`rung2.sh:53,71`) | partial: tmux sessions untested |
| T2 | tmux binary resolved through `bin_resolve` (PATH plus the Homebrew dirs) | `trusty-mpm/src/core/tmux.rs:102-110` | not needed: `/usr/bin/tmux` on PATH | no |
| T3 | tmux socket dir is `<TMUX_TMPDIR or /tmp>/tmux-<uid>`; the server is keyed to the OS user | `trusty-mpm/src/core/tmux_server.rs:64-70` | not needed: one uid (`cloudtest`) | no |
| T4 | `tm doctor` checks tmux options and tmux process priority; the priority row answers "not applicable" off macOS | `trusty-mpm/src/daemon/doctor.rs:803`, `:811`; `doctor_tmux_priority.rs:214-219` | not needed: `tm doctor` exit 0. The options row with no tmux server running is unverified | no |
| T5 | The Architect fleet poller starts its tmux session from a shell script | `trusty-mpm/src/assets/architect/scripts/start-fleet-poll.sh:21` | open: not run in the container | no (Architect is outside rung 1) |

### 4.4 Keychain (7 rows)

| # | Assumption | Where | Container handling | Blocks |
|---|---|---|---|---|
| K1 | The trusty-secrets Keychain backend exists only on macOS; elsewhere `get` returns `no_os_backend` | `trusty-secrets/src/store/keychain.rs:310-313` | open: `tm secrets` not run; no secret is stored in the image | partial: no secret store on Linux |
| K2 | Credential resolver tries env, `.env.local`, then the OS keyring, with a `0600` file store as the headless fallback | `trusty-common/src/credentials/keyring_store.rs:1-10`; `credentials/file_store.rs:1-9` | not needed: no credential reaches the daemons | no |
| K3 | `tm doctor` credential-reach row walks `.env.local` and the Keychain | `trusty-mpm/src/daemon/doctor_credential_reach.rs:141-145` | not needed: rows report ABSENT; `tm doctor` exit 0 | no |
| K4 | `gh` token write and read use `/usr/bin/security` on macOS and `secret-tool` on Linux | `trusty-mpm/src/core/gh_keyring_token.rs:199-238` | open: `secret-tool` and a Secret Service are not in the image; no GitHub flow was run | partial: GitHub-using flows need a token source |
| K5 | Worktree reclaim polls `gh`, which on macOS calls `security find-generic-password`, unless a `github:` binding is set | `trusty-mpm/src/session_manager/worktree_reclaim_gh.rs:207-215` | not needed: not run | no |
| K6 | `tm presence local` uses Touch ID or the login password and keeps its grant key in the login Keychain (macOS dependencies only) | `trusty-mpm/Cargo.toml:587-594` | open: not run; the Linux path is unverified | no |
| K7 | Claude Code keeps its login in the Keychain on macOS; on Linux it is `~/.claude/.credentials.json` | `docker/cloudtest/README.md:108-111` | workaround: state volume plus an interactive owner login (`README.md:94-106`); `rung2.sh:46` tests the file with `test -s` | partial: login needs a human to paste a code; token-file path exports the token to child processes (`claude-run:22`) |

### 4.5 Logged-in user, uid, HOME (6 rows)

| # | Assumption | Where | Container handling | Blocks |
|---|---|---|---|---|
| U1 | The launchd domain is `gui/<uid>`, which needs a logged-in GUI session | `trusty-common/src/launchd.rs:350`, `:397`, `:456`; `trusty-memory/src/commands/service.rs:280` | not needed: no launchd | no |
| U2 | Daemon sockets accept a peer only with the daemon's own uid (`SO_PEERCRED`) | `trusty-common/src/uds/peer.rs:246-248`; `trusty-common/src/uds/server/mod.rs:421` | workaround: non-root `cloudtest` user (`Dockerfile:58-70`); exec as that user (`README.md:35-43`); no `docker run --user` | partial: any remote path must run as the daemon uid; root exec is refused |
| U3 | Home dir comes from `$HOME`, then the passwd entry (`getpwuid`) | `trusty-search/src/service/data_dir.rs:96-123`, `:137-143` | workaround: `HOME=/data/home` and the passwd home match (`Dockerfile:68,80`) | no |
| U4 | State volume must be owned by the daemon uid, so uid and gid are build args | `Dockerfile:58-70`; `README.md:12-18` | workaround: build with the host uid and gid | partial: a cloud volume needs a matching owner |
| U5 | Claude Code needs an interactive owner login; the container has no browser | `docker/cloudtest/README.md:94-106` | open: manual copy and paste of the code; rung 2 steps 1-2 still await it | yes for unattended use |
| U6 | tm assumes a user session starts the daemon at login (#10597) | issue #10597 | see L11 | no |

### 4.6 Other, found while checking (4 rows)

| # | Assumption | Where | Container handling | Blocks |
|---|---|---|---|---|
| O1 | A socket file left by an unclean stop is not removed on bind; callers must unlink it | `trusty-common/src/uds/mod.rs:530-538` | workaround: entrypoint removes both socket files (`entrypoint.sh:9-10`) | partial |
| O2 | trusty-search `daemon.lock` holds a pid; after a container restart that pid belongs to a different process (#11060 open) | `trusty-search/src/service/daemon.rs:54-58`; `trusty-search/src/commands/start/daemon.rs:62` | workaround: entrypoint deletes the lock (`entrypoint.sh:12-17`) | partial: restart fails without the workaround |
| O3 | All three daemons listen on unix sockets only, no TCP, no auth (ADR-0032) | `trusty-mpm/src/daemon/doctor.rs:812-813`; issue comment 2026-11-11 | open: reach is `docker exec` or SSH stdio | yes |
| O4 | Update, model and telemetry calls at runtime (#11075) | issue #11075 | workaround: egress allowlist and disable flags (`Dockerfile:84-92`) | no |

### 4.7 Row count

macOS-only paths and APIs 11. launchd 11. tmux 5. Keychain 7. Logged-in user, uid, HOME 6. Other 4. Total 44.

### 4.8 Open

Assumptions with no Linux workaround yet:

- L5: no Linux service unit for trusty-memory or trusty-search. Fix: ship a documented systemd unit, or move both onto `linux_lifecycle` (#11083 PHASE_2).
- L9: no in-place restart on Linux. Fix: finish #11098 and move the daemons onto the Linux lifecycle backend.
- L8: `trusty-memory doctor` is not run in the container and warns off macOS. Fix: add it to `check.sh` and decide whether the skip stays a WARN.
- T1: no tm-managed tmux session was run. Fix: add one session spawn and attach to rung 1.
- T5: Architect fleet poller not run. Fix: out of rung 1; list it for a later rung.
- K1: no Linux secret store behind `tm secrets`. Fix: decide the Linux backend (file store or Secret Service) and record it.
- K4: no `secret-tool` or Secret Service for the `gh` token. Fix: supply the token by env binding (`github:` binding), or add a Secret Service to the image.
- K6: Linux path of `tm presence local` is unverified. Fix: run it once in the container and record the result.
- K7 and U5: Claude login needs a human paste. Fix: owner supplies a scoped token file, after the `claude-run:22` child-inheritance finding is settled.
- M7: no model host in the egress allowlist. Fix: bake the fastembed cache into the image or volume, or allow the model host.
- O1 and O2: stale socket and stale lock are cleared only by the entrypoint. Fix: #11060 for the lock; the socket needs its own owner (callers unlink before bind per `uds/mod.rs:537`).
- O3: no network endpoint. Fix: a gateway with TLS and auth, tracked with #11073 and #11074.
- L7 (process-type row) and T4 (options row): Linux outcome unverified. Fix: read the `tm doctor` output rows in the container and record them.


