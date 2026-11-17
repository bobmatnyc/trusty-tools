# tctl restart on Linux restarts the daemons on the newly installed binaries

Plan document for the epic that gives the trusty daemons a Linux lifecycle and
makes `tctl restart` use it. The research sections come first. The
`## Epic plan` section is the input to `tm issue epic create --from`; its
headings are that command's schema. Research date: 2026-11-11.

## Problem

Observed 2026-11-11 on a Linux host, after `tctl upgrade`. `tctl restart`
printed `trusty-mpm ok`. It then reported FAILED for trusty-console,
trusty-events, trusty-memory and trusty-search, each with
"launchd control of `<x>` is only supported on macOS".

A live check on the same host found:

| Daemon | Installed | Running | `/proc/<pid>/exe` | Verdict |
|---|---|---|---|---|
| trusty-memory | 0.36.7 | 0.28.0 (since Oct 24) | `(deleted)` | stale |
| trusty-search | 0.63.1 | 0.61.1 (since Oct 24) | `(deleted)` | stale |
| trusty-mpm | 1.12.2 (`tm`) | 1.12.2 | current | current |
| trusty-console | 0.13.0 | not running | — | not running |

Two daemons ran binaries that the upgrade had already replaced on disk. No
command reported that.

## Cause

- `crates/trusty-installer/src/commands/stable_set.rs`, `manage_strategy_for`:
  only trusty-mpm gets `ManageStrategy::OwnVerb`. Every other daemon gets
  `Launchd`.
- `crates/trusty-installer/src/commands/lifecycle.rs`, `apply_to_member`: a
  `Launchd` member goes to `launchd_control`. Off macOS, `launchd_control` is a
  `cfg` stub that bails with the message above.
- `crates/trusty-installer/src/commands/upgrade.rs`, `restart_plan`: maps a
  `Launchd` member off macOS to a manual-restart note. `tctl restart` has no
  such guard.
- Linux restart was never implemented. No commit removed it.
- `lifecycle.rs`, `own_verb_control`: runs `<binary> <verb>` by bare name,
  resolved through `which`/PATH. It does not use the path that upgrade
  installed to. The code carries a
  [#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964) "Phase 3" note
  about this.
- No code compares the version a daemon runs with the version of the installed
  binary.

## Findings

### Version reporting

- Every daemon reports `env!("CARGO_PKG_VERSION")` in a `version` field:
  `mpm.health`, `memory.health` and `search.health` over UDS, and console
  `GET /health` over HTTP (default `127.0.0.1:7788`).
- `probe_member_health` (`crates/trusty-installer/src/commands/probe.rs`)
  already returns the running version. Every caller drops it.

### Install directory

- `canonical_bin_dir()` (`crates/trusty-common/src/bin_resolve.rs`) is
  `$CARGO_HOME/bin`, or `~/.cargo/bin`. The tctl helper is
  `download::install_dir_or_fallback()`.
- The install path is not persisted anywhere.
- `~/.cargo/.crates.toml` is stale: it records trusty-mpm 1.6.3. It must never
  be read as the installed version.

### trusty-mpm

- `tm` is the daemon binary. `trusty-mpm` is a std-only shim that execs the
  `tm` in its own directory (`crates/trusty-mpm/src/bin/trusty-mpm.rs`).
- The stable-set member's binary is the shim. A version comparison must run
  `tm --version`.
- `tm restart` (`crates/trusty-mpm/src/bin/tm/commands/daemon.rs`) runs
  `pkill -f`, waits 3 s for health to fail, never sends SIGKILL, and then
  starts unconditionally.
- `tm start` returns Ok when the daemon is unhealthy.

### trusty-memory

- `stop` on Linux scans process argv (`with_cmd`). The scan works, but it does
  not see data dirs. It sends SIGTERM, polls 5 s, sends SIGKILL, and fails on a
  survivor.
- `start` spawns `current_exe() serve --foreground` detached and waits up to
  30 s for socket readiness.
- The daemon exits 0 on SIGTERM. It has no `restart` verb.
- There is no pidfile. `SO_PEERCRED` peer-pid support exists
  (`commands/stop_sandbox.rs`, `trusty_common::uds::peer`).

### trusty-search

- `stop` signals only the pid in `daemon.lock`, with no identity check. Its
  argv scan never requests argv, so it finds nothing.
- `stop` exits 0 while a process survives SIGKILL.
- `start` returns right after the spawn. It does not wait for readiness.
- `stop` reads `TRUSTY_DATA_DIR`; `start` takes `--data-dir`.
- The daemon holds the `daemon.lock` flock for its whole run, through the
  shutdown flush. A free lock is a reliable "stopped" signal.
- `deploy/systemd/trusty-search.service` exists. Nothing installs it.

### trusty-console

- Its verbs are `serve`, `port`, `config` and `service`. `service` is
  macOS-only. It has no `start`, `stop` or `restart`.
- It binds a UDS socket in-process at `daemon_socket_path("trusty-console")`.
  A bind failure is only a warning. The fallback identity is the `http_addr`
  file plus a `/proc` lookup.
- SIGTERM shutdown is graceful.
- Its only single-instance guard is the TCP bind.

### Socket locks

`bind_singleton` (`crates/trusty-common/src/uds/singleton.rs`) holds
`<socket>.lock` only during the bind. The lock is not a liveness signal for
memory, console or tm. For those three, "stopped" means the pid has exited AND
the socket refuses connections.

### Stdio bridges

The memory and search MCP bridges run their daemon guard once, at startup. An
old bridge cannot respawn an old daemon. A new bridge that starts in the gap
between stop and start spawns the new binary.

### Exit codes

Both existing `stop` commands exit 1 when nothing is running.

## Design

### Linux-only module in trusty-common

The module is gated with `cfg(target_os = "linux")`.

- Each daemon passes a descriptor: binary name, data-dir resolution, socket
  path, spawn args, stop grace, and the "stopped" signal.
- Identity is the `SO_PEERCRED` peer pid of the daemon's UDS socket, plus a
  process-name check.
- `stop` returns `Stopped`, `NotRunning` or `Err(Survivor)`. It sends SIGTERM,
  waits, then sends SIGKILL.
- `start` spawns `<install dir>/<binary> …` detached, then waits until the
  socket serves.
- `restart` is `stop` (`NotRunning` is accepted), then `start`.

### Daemon verbs

memory, search, console and tm use the module on Linux for `start`, `stop` and
`restart`. Console gains the three verbs. macOS code is not changed. "Stopped"
means:

- search: the `daemon.lock` flock is free;
- memory, console, tm: the pid has exited and the socket refuses connections.

### tctl on Linux

- tctl runs `<install dir>/<binary> restart`, not the PATH copy.
- After each member, tctl compares the running version (`probe_member_health`)
  with `<install dir>/<binary> --version`. For trusty-mpm the binary is `tm`.
  A mismatch is FAILED, and the line shows both versions.
- A member without Linux verbs keeps the manual-restart note.

## Open questions

These are PM recommendations. The owner has not ratified them. The plan uses
each as a proposed default and names it where it applies.

| # | Proposed default | Effect if rejected |
|---|---|---|
| P-a | Move `tm` onto the shared Linux module too | Phase 2 drops `tm` from its scope; O2 then does not hold for `tm restart` |
| P-b | A daemon that is not running counts as stopped, so `restart` starts it. On the observed host, `tctl restart` then starts trusty-console | `restart` on a stopped daemon fails or does nothing; O1 then needs trusty-console started by another route |

One more question has no proposed default:

- Q1. `tctl upgrade` has its own restart step (`upgrade.rs`, `restart_plan`),
  which prints the manual-restart note on Linux. This plan changes
  `tctl restart` only. Decide whether the upgrade step must take the same
  Linux route.

## Epic plan

On Linux, `tctl restart` fails for every daemon except trusty-mpm, because
tctl routes them to launchd. Daemons then keep running binaries that
`tctl upgrade` replaced. This epic adds a Linux lifecycle backend, gives
memory, search, console and tm Linux `start`/`stop`/`restart` verbs on it, and
makes `tctl restart` run the installed binary and check the running version.

### Outcomes

- **O1** On Linux, after `tctl upgrade`, `tctl restart` reports `ok` for
  trusty-mpm, trusty-memory, trusty-search and trusty-console, and each
  daemon's health reports the installed version.
- **O2** Each daemon's `restart` meets D1. A daemon that survives `stop` fails
  the restart and is not started over.
- **O3** `tctl restart` reports FAILED, with both versions, when the running
  version differs from the installed binary.
- **O4** macOS lifecycle behaviour is unchanged.

### Ratified decisions

| # | Decision |
|---|---|
| D1 | `stop` stops a daemon. `start` starts it. `restart` = stop, make sure it stopped, and only if it did, start (owner, 2026-11-11) |
| D2 | macOS stop/start/restart behaviour, and tctl's launchd path, stay exactly as they are. Linux gets a separate, abstracted lifecycle backend (owner, 2026-11-11) |
| D3 | trusty-events is out of scope (owner, 2026-11-11) |
| D4 | When tctl meets an older binary that has no `restart` verb, reporting that member FAILED is acceptable (owner, 2026-11-11) |

### Ordering

Phase 1 builds the lifecycle module with tests that run against real child
processes. Phase 2 puts four daemons on that module. A defect in stop or
identity logic, found after four crates depend on it, costs a change in five
crates. Phase 2 therefore starts only when Phase 1 is merged and its
real-process tests are green on Linux.

Phase 3 makes tctl run `<install dir>/<binary> restart`. That verb exists only
in binaries built from Phase 2. Against older binaries tctl reports FAILED for
every member (D4), so Phase 3 cannot be checked live until Phase 2 is released
and installed on a Linux host. That install is the gate for Phase 3.

The epic closes on a live check of O1 to O4 on a Linux host, after Phase 3 is
released and installed there.

### Phase: Linux daemon lifecycle module in trusty-common

Gate: None. Decisions D1 to D4 are ratified, and no earlier phase exists.

Adds the shared Linux backend that D2 requires. It runs first because every
later phase calls it. No daemon uses it at the end of this phase.

#### Scope

- A new module in `crates/trusty-common`, gated with
  `cfg(target_os = "linux")`.
- A daemon descriptor: binary name, data-dir resolution, socket path, spawn
  args, stop grace, and the "stopped" signal. Two "stopped" signals exist:
  "pid exited and socket refuses connections", and "lock file flock is free".
- Identity: the `SO_PEERCRED` peer pid of the daemon's UDS socket
  (`trusty_common::uds::peer`), plus a process-name check against the
  descriptor's binary name.
- `stop` returns `Stopped`, `NotRunning` or `Err(Survivor)`: SIGTERM, wait for
  the stop grace, SIGKILL, wait again.
- `start` spawns `<install dir>/<binary>` with the descriptor's args, detached,
  then waits until the socket serves. The install dir comes from
  `canonical_bin_dir()`.
- `restart` is `stop`, then `start`. `NotRunning` from `stop` is accepted
  (proposed default P-b).
- Tests that spawn real child processes. Test ladder rung 5: process lifecycle
  in a shared library, with `--include-ignored` coverage, failure-path tests
  and a `code-critic` round.
- A changelog fragment in `crates/trusty-common/changelog.d/`.

#### Acceptance criteria

- **AC1** The shortcut reuses `tm restart`'s wait for health to fail. A test
  child closes its socket on SIGTERM and stays alive past that wait. `stop`
  does not return `Stopped` until the pid has exited.
- **AC2** A test child ignores SIGTERM. `stop` sends SIGKILL after the stop
  grace and returns `Stopped` only after the pid has exited.
- **AC3** The shortcut trusts that a sent signal worked. When the process still
  exists after the SIGKILL wait, `stop` returns `Err(Survivor)` with the pid.
  `restart` then returns that error, and the test asserts that no spawn
  happened.
- **AC4** The shortcut signals a pid with no identity check. When the socket's
  peer pid belongs to a process whose name is not the descriptor's binary,
  `stop` returns an error and sends no signal. The test asserts that the other
  process is still alive.
- **AC5** With no socket, or a socket that refuses connections, `stop` returns
  `NotRunning` and sends no signal. `restart` then runs `start`.
- **AC6** The shortcut spawns by bare name or with `current_exe()`. The test
  puts a decoy binary of the same name first on PATH and runs the module from
  a binary outside the install dir. The spawned process's executable path is
  `<install dir>/<binary>`.
- **AC7** The shortcut returns right after the spawn. `start` returns an error
  when the socket does not serve inside the readiness limit, and the test uses
  a child that never binds.
- **AC8** For the flock "stopped" signal, a child that has closed its socket
  but still holds the lock is not `Stopped`. `stop` returns `Stopped` only when
  the flock can be taken.
- **AC9** The started child survives the exit of the process that started it.
- **AC10** The diff changes no line of trusty-common outside the new module,
  except the `mod` declaration and its `cfg` attribute.

#### Non-goals

No daemon calls the module in this phase. No macOS code changes (D2). No
systemd backend. No pidfile.

#### Risk

Signals sent to the wrong process are the worst failure: pid reuse, or a
socket that another program serves. AC4 covers it. Timing-based tests can
flake on a loaded host; the tests must wait on conditions (pid exit, flock
state), not on fixed sleeps.

### Phase: Linux start, stop and restart verbs for memory, search, console and tm

Gate: Phase 1 merged, with its real-process tests green on Linux.

Puts each daemon's Linux `start`, `stop` and `restart` on the Phase 1 module.
It follows Phase 1 because it only supplies descriptors and CLI wiring. It
precedes Phase 3 because tctl calls these verbs.

#### Scope

- trusty-memory: `stop` and `start` use the module on Linux. A new `restart`
  verb. The argv scan is no longer the Linux stop path.
- trusty-search: `stop` and `start` use the module on Linux, with the
  `daemon.lock` flock as the "stopped" signal. A new `restart` verb. `stop`
  and `start` resolve the data dir the same way. When the
  `trusty-search.service` systemd unit is active, `restart` refuses and prints
  the `systemctl` command to run.
- trusty-console: new `start`, `stop` and `restart` verbs on Linux. Identity
  is the UDS socket's peer pid. When the socket bind failed, identity falls
  back to the `http_addr` file plus a `/proc` lookup.
- trusty-mpm: `tm start`, `tm stop` and `tm restart` use the module on Linux
  (proposed default P-a).
- "Stopped" for memory, console and tm: the pid has exited and the socket
  refuses connections.
- A daemon that is not running counts as stopped, so `restart` starts it
  (proposed default P-b). `stop` keeps exit 1 when nothing is running.
- Test ladder rung 5 for each crate: `--include-ignored` coverage,
  failure-path tests and a `code-critic` round.
- A changelog fragment for each crate whose `src/` changes.
- Live checks use only `scripts/sandbox_memory_daemon.sh`,
  `scripts/sandbox_search_daemon.sh` and `scripts/sandbox_daemon.sh`.

#### Acceptance criteria

- **AC1** For each of the four daemons, in a sandbox: start the daemon from
  binary A at the install path, replace that file with binary B of a different
  version, and run `restart`. The health reply then reports B's version, and
  exactly one daemon process exists for that data dir.
- **AC2** The shortcut trusts `stop`'s exit 0. For each daemon, when the
  process survives SIGKILL (simulated through the module's test seam),
  `restart` exits nonzero, names the surviving pid, and starts nothing.
- **AC3** trusty-search `stop` exits nonzero while a process survives. The
  current code exits 0.
- **AC4** The shortcut keeps `tm restart`'s 3 s health wait. `tm restart` on
  Linux does not spawn until the old pid has exited, tested with a daemon that
  takes longer than 3 s to exit.
- **AC5** `tm start` and trusty-search `start` exit nonzero when the daemon
  does not serve inside the readiness limit. Both return success today.
- **AC6** A trusty-search daemon started with `--data-dir X` is stopped by
  `stop --data-dir X` while `TRUSTY_DATA_DIR` is unset.
- **AC7** The shortcut keeps the argv scan, which does not see data dirs. Two
  trusty-memory sandbox daemons run with different data dirs. `stop` for one
  leaves the other running and serving.
- **AC8** The shortcut signals the `daemon.lock` pid unchecked. When
  `daemon.lock` names a pid that is not a trusty-search process, `stop` sends
  no signal to it.
- **AC9** With the `trusty-search.service` unit active, `restart` sends no
  signal, exits nonzero, and prints the `systemctl` restart command.
- **AC10** trusty-console `stop` finds the daemon when its UDS socket bind
  failed. The test makes the socket path unbindable and asserts the fallback
  identity is used.
- **AC11** `restart` on a daemon that is not running starts it and exits 0.
  `stop` on the same daemon exits 1.
- **AC12** The diff changes no code that compiles on macOS for these verbs.
  The existing macOS lifecycle tests are unmodified.

#### Non-goals

No macOS change (D2). No trusty-events work (D3). No systemd backend; the
systemd refusal in AC9 is the whole of the systemd handling. No change to
`default-services.yaml`'s `stop && start` restart command for search. No fix
for old MCP sessions that keep a stale tool list.

#### Risk

This phase changes how four daemons stop, in four crates, in one release. A
stop that targets the wrong process, or a start that reports success early,
reaches every Linux host. The console fallback identity is the weakest path:
console has no single-instance guard except the TCP bind. If P-a is rejected,
`tm` leaves this phase and O2 does not cover it.

### Phase: tctl restart on Linux runs the install-dir binary and checks the running version

Gate: Phase 2 released and installed on a Linux host.

Routes `tctl restart` on Linux to each member's own `restart` verb, and adds
the running-versus-installed version check. It is last because the verbs it
calls exist only in Phase 2 binaries.

#### Scope

- `crates/trusty-installer/src/commands/lifecycle.rs`: on Linux, a daemon
  member with Linux verbs is restarted with `<install dir>/<binary> restart`.
  The install dir comes from `download::install_dir_or_fallback()`.
- The macOS `Launchd` arm and `launchd_control` are not changed (D2).
- After each member, tctl compares the running version from
  `probe_member_health` with the output of `<install dir>/<binary> --version`.
  For trusty-mpm the binary is `tm`.
- A mismatch is FAILED, and the line shows both versions.
- A member without Linux verbs (trusty-events, D3) gets the manual-restart
  note that `restart_plan` already prints, not the launchd error.
- A member whose installed binary has no `restart` verb is FAILED (D4). The
  other members still run.
- Test ladder rung 5, with `--include-ignored` coverage, failure-path tests
  and a `code-critic` round.
- A changelog fragment in `crates/trusty-installer/changelog.d/`.
- Live checks use the repository's sandbox scripts only.

#### Acceptance criteria

- **AC1** The shortcut runs the PATH copy. The test puts a decoy binary of the
  same name first on PATH. The command tctl runs is the absolute
  `<install dir>/<binary>` path, and the decoy is never executed.
- **AC2** The shortcut reads `~/.cargo/.crates.toml`. The test supplies a
  `.crates.toml` that records a different version. The installed version tctl
  reports equals the `--version` output of the install-dir binary.
- **AC3** The shortcut runs `trusty-mpm --version` on the shim. For the
  trusty-mpm member, the installed version comes from
  `<install dir>/tm --version`.
- **AC4** The shortcut trusts the verb's exit 0. When `restart` exits 0 and the
  daemon's health still reports another version, the member is FAILED. The
  line contains both the running and the installed version, and tctl exits
  nonzero.
- **AC5** When `restart` exits 0 and the health probe is refused, the member is
  FAILED.
- **AC6** On Linux, no member's output contains "only supported on macOS". A
  member without Linux verbs prints the manual-restart note.
- **AC7** A member whose binary rejects `restart` is FAILED, and the members
  after it are still restarted.
- **AC8** On macOS, `manage_strategy_for` returns the same strategy for every
  member as before this phase. The existing launchd tests are unmodified.
- **AC9** Live check on a Linux host that runs older daemon versions than the
  installed binaries: `tctl restart` reports `ok` for trusty-mpm,
  trusty-memory, trusty-search and trusty-console. Each health reply reports
  the installed version, and no `/proc/<pid>/exe` reads `(deleted)`.

#### Non-goals

No change to `tctl upgrade`'s own restart step (open question Q1 in the plan
document). No persisted install path. No macOS change (D2). No container
support ([#11060](https://github.com/bobmatnyc/trusty-tools/issues/11060)).

#### Risk

tctl now reports FAILED where it reported nothing, on any host with a daemon
older than Phase 2 (D4 accepts this). A wrong version parse reports FAILED on
a correct restart, so the parse needs a test for each binary's real
`--version` output. AC9 depends on proposed default P-b: without it, tctl
cannot report `ok` for a trusty-console that was not running.

### Deferred

<!-- deferred:start -->
| Item | Why deferred | Where it went |
|------|--------------|---------------|
| A systemd lifecycle backend | The Linux backend manages processes directly; search only refuses when its unit is active | unscheduled |
| trusty-events restart on Linux | D3 | unscheduled |
| `default-services.yaml`'s `stop && start` restart command for search | Outside the tctl path | unscheduled |
| Old MCP sessions keep a stale tool list after a daemon restart | Client-side behaviour, not daemon lifecycle | unscheduled |
| Daemon lifecycle in containers | Separate work | [#11060](https://github.com/bobmatnyc/trusty-tools/issues/11060) |
<!-- deferred:end -->

## Related issues

- [#4964](https://github.com/bobmatnyc/trusty-tools/issues/4964)
- [#10607](https://github.com/bobmatnyc/trusty-tools/issues/10607)
- [#10975](https://github.com/bobmatnyc/trusty-tools/issues/10975)
- [#11060](https://github.com/bobmatnyc/trusty-tools/issues/11060)
- [#11054](https://github.com/bobmatnyc/trusty-tools/issues/11054)

## Follow-up note

One finding is outside this epic's scope. `tm issue epic create` renders the
tracker's `followups` block empty, so the row is added to the tracker by hand
after creation:

| Finding | Severity | Where it went |
|---------|----------|---------------|
| trusty-memory's health reply reports `update_available: 0.35.0` while 0.36.7 is installed | not assessed | this row |

## Maintenance

The sections above are authored once. Issue numbers for this epic are not
written back here. This document carries no phases table: the tracker's
`phases` block is generated from each phase's `Gate:` line and from child
issue state.
