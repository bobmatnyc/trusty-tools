Changed

- `tctl` no longer treats trusty-review as a daemon (#6290): it never shells out
  to `trusty-review service install`, and boots out the retired
  `com.trusty.review` unit (and its `com.trusty.trusty-review` alias) during the
  install pass instead.
- The member is probed by presence — binary on PATH plus `--version` — rather
  than by dialling a socket nothing binds. A presence probe can never report
  confirmed-down, so `launchctl kickstart -k` can no longer fire at a label that
  does not exist.
