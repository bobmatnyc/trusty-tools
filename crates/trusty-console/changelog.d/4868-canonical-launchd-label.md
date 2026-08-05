Fixed

- `trusty-console service` now names the unit launchd actually has loaded. The
  label was `com.trusty.trusty-console` while the live agent is
  `com.trusty.console`, so `service status` queried a label that does not exist
  and `service install` would have bootstrapped a second dashboard daemon beside
  the running one. The value comes from `trusty_common::launchd_labels::CONSOLE`
  and the old name is recorded as a legacy alias so an upgrade evicts it (#4868)
