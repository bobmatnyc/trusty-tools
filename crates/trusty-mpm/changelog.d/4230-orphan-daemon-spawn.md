Fixed

- `tm start` / `tm restart` no longer spawn an unsupervised daemon that can seize the port from launchd (closes [#4230](https://github.com/bobmatnyc/trusty-tools/issues/4230))
  - `commands::daemon::start` was the one client-side daemon-spawn path with no
    launchd awareness at all — the MCP stdio bridge has refused to auto-spawn
    since [#2486](https://github.com/bobmatnyc/trusty-tools/issues/2486) and
    `guided_autostart` has nudged launchd since
    [#1900](https://github.com/bobmatnyc/trusty-tools/issues/1900), while
    `start` spawned a detached bare `tm daemon` unconditionally. That is how the
    #4230 orphan was created: PID 98606, `PPID 1`, `cwd=$HOME`, holding
    `127.0.0.1:7880` for two days and answering `/health` 200 with a stale 1.0.2
    image while launchd's `com.trusty.mpm` reported `state = not running`. A
    fresh signed install and its behavioural verification both passed against
    the binary the install had just replaced.
  - Both `start` and `restart` now refuse when a DAEMON launchd unit is
    registered, naming the unit that actually exists on that host, its
    `launchctl kickstart` recipe, and the `tm daemon --force` opt-in. The check
    runs BEFORE `restart`'s `pkill`, so a refusal never tears the supervised
    daemon down and then declines to bring it back. The decision is keyed on
    plist presence alone — no supervision heuristic — because the callee-side
    [#4397](https://github.com/bobmatnyc/trusty-tools/issues/4397) guard folds in
    `is_launchd_supervised`, whose `XPC_SERVICE_NAME` and `getppid() == 1` prongs
    can BOTH report `true` for a non-launchd child.
  - `com.trusty.mpm.supervisor.plist` no longer counts as "launchd owns the
    daemon". That plist runs `tm supervisor`, a passive fleet observer that never
    starts a daemon, so counting it made every consumer wrong on a `tctl install`
    host whose only plist is the supervisor's: the refusal fired with a false
    claim and prescribed a `com.trusty.mpm` unit that does not exist there. This
    also removes a latent false refusal in the #4397 child guard and a latent
    false `no_spawn` in the #2486 bridge guard on those hosts. A drifted daemon
    label still resolves, matching `install-trusty-mpm-signed.sh`'s existing
    `resolve_mpm_plist` rules.
  - `tm stop` is not refused — stopping the daemon is a legitimate request — but
    now warns that launchd will NOT respawn it
    (`KeepAlive.SuccessfulExit=false`) and names the command that will. That
    SIGTERM is how the incident's `com.trusty.mpm` came to report `not running`
    for four days before anything else went wrong.
  - Every remediation that prescribed `tm restart` now resolves the verb for the
    calling host — `tm doctor`'s staleness line (which printed "run `tm restart`"
    two lines above the new orphan check), the three `tm manager` upgrade hints,
    the session-picker stale-numbering warning, and the bundled
    `tm-cli-operations` skill that agents read.
  - The #2486 bridge guard did NOT regress. The orphan's own
    `{"supervised":false}` proves a plist was registered when it started
    (`supervised` is `false` only in that case), which means the bridge's
    `no_spawn` was necessarily set and it could not have been the spawner.
- new `tm doctor` check `daemon_orphan` catches a daemon that answers `/health` but is not the one launchd runs ([#4230](https://github.com/bobmatnyc/trusty-tools/issues/4230))
  - It compares WHO ANSWERED (`/health`'s new `pid`) against WHO LAUNCHD RUNS
    (`launchctl`), two independent sources, so neither the daemon nor launchd
    alone can make the check pass. The daemon's own `supervised` flag is a
    FALLBACK only, used when the responding daemon is too old to report `pid`:
    its `getppid() == 1` prong reads `true` for a genuine orphan, so a `true`
    there yields `Unknown`, never a pass.
  - The launchd probe answers in three states, not two. "launchd has this unit
    and its job is DOWN" is the #4230 incident and yields a hard `Fail` with a
    `kill -TERM` remediation; "launchd could not be asked" — `launchctl` missing
    from `PATH`, sandboxed, or the unit invisible in the caller's bootstrap
    domain — yields `Unknown` and prescribes nothing. Collapsing the two would
    make `tm doctor` order the operator to kill a correctly supervised daemon,
    which `KeepAlive {SuccessfulExit: false}` would then decline to restart. A
    `launchctl list` that exits 113 is retried against an explicit `gui/<uid>`
    domain before the probe gives up.
  - The daemon launchd label is resolved per host rather than hardcoded, and a
    drifted label must have `ProgramArguments` that actually run `daemon` — so
    neither the supervisor plist nor a `logrotate` sibling can be mistaken for
    the daemon unit, and every remediation names a unit that exists on this host.
