Fixed

- `trusty-audit distribute` names the member that actually failed. A write that failed on `README.md`, `audit.sh` or `engagement.toml` was reported as "taudit failed" — pointing the operator at the one member that had already succeeded ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
- `Session::distribute` reads the packaging credential through an injected lookup rather than the live process environment, so `cargo test -p trusty-audit` can no longer write a developer's exported `OPENROUTER_API_KEY` into a zip on disk, and both credential sources are now asserted end to end ([#5825](https://github.com/bobmatnyc/trusty-tools/issues/5825))
