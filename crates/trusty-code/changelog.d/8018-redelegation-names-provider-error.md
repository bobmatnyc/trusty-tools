Fixed

- A delegated run whose retries were exhausted by an LLM/HTTP provider failure now names that error — its type and message — in the terminal error and in the `warn` log at both the per-delegation and run-wide exhaustion sites. `turn_cap_exceeded` is reported only when the turn cap was the actual reason (refs [#8018](https://github.com/bobmatnyc/trusty-tools/issues/8018))
