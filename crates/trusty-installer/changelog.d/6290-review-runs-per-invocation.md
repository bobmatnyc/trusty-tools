Changed

- `tctl` no longer treats trusty-review as a daemon (#6290): it never shells out
  to `trusty-review service install`, and boots out the retired
  `com.trusty.review` unit (and its `com.trusty.trusty-review` alias) during
  `tctl install`'s service-bootstrap pass instead.
- The member is probed by presence — binary on PATH plus `--version` — rather
  than by dialling a socket nothing binds. A presence probe can never report
  confirmed-down, so `launchctl kickstart -k` can no longer fire at a label that
  does not exist.
- The eviction now actually runs. `plans_service_bootstrap` gated on
  `manage == Launchd`, and a retired member is `ManageStrategy::None`, so the
  install loop skipped it and `com.trusty.review` was never booted out on any
  host. It is now visited when the member has a retired service.
- A retired unit that will not go down fails the install instead of being
  reported as a skip, so the exit code shows it.
- `tctl upgrade` evicts a retired member's unit too, through the same
  mechanism. `restart_plan` read the member as "not a daemon" and skipped it, so
  an upgrade left `com.trusty.review` loaded and respawning; only `tctl install`
  cleared it. A unit that will not go down now fails that member's upgrade.
