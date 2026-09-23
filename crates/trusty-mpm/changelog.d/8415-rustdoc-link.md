Documentation

- The launchd doctor row's module doc now links `check_launchd_process_type_in`
  instead of the test-only `check_launchd_process_type`, fixing the broken
  intra-doc link the `Rustdoc intra-doc links` CI job reported after #8415
  renamed the production function ([#8415](https://github.com/bobmatnyc/trusty-tools/issues/8415))
