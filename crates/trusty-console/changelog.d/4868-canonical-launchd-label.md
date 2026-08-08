Fixed

- `trusty-console service` now names the unit launchd actually has loaded. The
  label was `com.trusty.trusty-console` while the live agent is
  `com.trusty.console`, so `service status` queried a label that does not exist
  and `service install` would have bootstrapped a second dashboard daemon beside
  the running one. The value comes from `trusty_common::launchd_labels::CONSOLE`
  and the old name is recorded as a legacy alias so an upgrade evicts it (#4868)
- `service install` now evicts the old label instead of adding a second unit
  beside it. Console is one of only two services whose label value actually
  changes, so a host that ran the pre-fix installer would otherwise keep
  `com.trusty.trusty-console` loaded AND gain `com.trusty.console` — two console
  daemons on one port, the #2938 condition this issue exists to close (#4868)
