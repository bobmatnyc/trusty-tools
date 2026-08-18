Fixed

- An unset or malformed `RUST_LOG` now filters at `info` instead of dropping everything below `error`, so the `warn!` decision points the logging convention requires are visible on a default developer machine; `DEFAULT_LOG_LEVEL` is the real fallback rather than advisory documentation ([#5928](https://github.com/bobmatnyc/trusty-tools/pull/5928))
