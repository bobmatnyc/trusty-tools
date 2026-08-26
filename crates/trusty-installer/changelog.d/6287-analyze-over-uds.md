Changed

- `tctl`'s health probe reaches trusty-analyze over its Unix socket rather than
  port 7879 (#6287, ADR-0032). A probe left on the retired port reads `Refused`,
  which `is_confirmed_down` accepts and `verify_tail` turns into
  `launchctl kickstart -k` against a daemon that was working — the #4246 class.
- `uds_health_method` generalises the hardcoded `review.health` constant to one
  method per binary, so a second UDS member cannot inherit the first one's name.
