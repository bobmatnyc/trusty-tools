Changed

- The `trusty-review` binary AUDIT invokes is resolved by a pure function over the `TRUSTY_REVIEW_BIN` value, and `run_review_report` reads the environment exactly once at its entry point. Behaviour is unchanged — an override wins unless it is empty, otherwise `trusty-review` is looked up on PATH — but the resolution rule and the spawn path are now testable without mutating the process environment, which `std::env::set_var` cannot do soundly under a parallel test harness.
