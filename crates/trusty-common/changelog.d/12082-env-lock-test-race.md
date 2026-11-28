Fixed

- trusty-common tests that mutate process environment variables now serialise on `ENV_LOCK`, removing a test-only race that made them fail intermittently (#12082)
