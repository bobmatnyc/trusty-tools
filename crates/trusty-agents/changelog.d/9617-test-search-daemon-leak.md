Fixed

- Test runs no longer leave a `trusty-search` daemon behind (#9617). The integration-test harness stamps every `tagent` it spawns with the parent-death marker, and the `init_global_is_idempotent` unit test stamps its own process, so a daemon started through the plugin spawn path exits when the test runner dies, including on SIGKILL. Test code only; no production behaviour changed.
