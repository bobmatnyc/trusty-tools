Changed

- The published crates.io package no longer ships `CHANGELOG.md` or the integration tests under `tests/`, which brings the `.crate` under the crates.io 10 MiB upload limit that rejected 1.9.0 (refs [#10373](https://github.com/bobmatnyc/trusty-tools/pull/10373)). The tests still run from the repository.
