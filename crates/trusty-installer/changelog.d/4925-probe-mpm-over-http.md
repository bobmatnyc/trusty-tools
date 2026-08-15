Fixed

- `probe_member_health` now probes trusty-mpm's `/health` over HTTP instead of
  reporting it `unknown`. Probeability is a property of the daemon's HTTP
  transport, not of its lifecycle-management strategy; the two axes diverged when
  #4246 moved the probe from `<binary> health --json` to HTTP, and the
  `ManageStrategy::OwnVerb` arm was still keyed off the lifecycle enum. mpm now
  reports `healthy` when serving and `down` when not, in `tctl status`,
  `tctl stack health`, `tctl stack doctor` and the `tctl install` verify tail —
  and `tctl up` no longer issues a redundant `start` against a daemon already
  known to be answering.
  - **Behaviour change.** trusty-mpm is `required: true`, so a stopped mpm now
    yields `tctl status` / `tctl stack health` / `tctl stack doctor` →
    `degraded`, exit 2, and `tctl install` → NOT VERIFIED. This is intended: mpm
    becomes consistent with its declared `required` flag rather than exempt from
    it, exactly as a stopped trusty-search already behaves. CI that gates on
    `tctl status` exit codes may start failing where it previously passed; start
    mpm (`tctl start trusty-mpm`) or drop it from the gated set.
  - `ManageStrategy` is unchanged and still governs `tctl start|stop|restart`
    dispatch and `needs_kickstart`, so a confirmed-down mpm still cannot be
    handed to `launchctl kickstart -k` against the nonexistent `com.trusty.mpm`
    label.
