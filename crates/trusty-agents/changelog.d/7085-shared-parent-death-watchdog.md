Changed

- The `tagent --api` sidecar's parent-death watchdog now delegates to `trusty_common::parent_death` instead of carrying its own copy of the detection loop. Behavior is unchanged — the same reparent and pid-liveness signals, armed only when the GUI passes `--parent-pid` (#7085).
