Fixed

- `tga collect` now exits non-zero when a collection stage's write or fetch path failed, instead of printing the failure as a warning and exiting 0 over a partially persisted database (#5655). `tga analyze` reports the same failures after its report stage, so the reports are still written.
- Every fault a provider records now carries a severity. A stage that never persisted its data (`StageFailed`) reaches the exit code; a single skipped record (`ItemSkipped`) is reported and never fails a long sweep. The split applies to ADO, Linear, GitHub, and Bitbucket alike.
