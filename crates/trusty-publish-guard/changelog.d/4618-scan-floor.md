Fixed

- `publish-guard` now asserts a non-zero scan floor before reporting OK. The exit code depended only on `drifted == 0 && unverifiable == 0`, so a run that discovered no crates printed `checked 0 publishable crate(s) — 0 drifted, 0 unverifiable.` and exited 0 — a version-parity gate that could not fail. Fewer than 10 checked crates is now a failure naming the floor, and the summary line prints the floor alongside the count ([#4618](https://github.com/bobmatnyc/trusty-tools/issues/4618)).
