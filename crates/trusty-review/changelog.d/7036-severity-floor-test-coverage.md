Fixed

- A `test-coverage` finding on its own no longer drives REQUEST_CHANGES or BLOCK. `FindingCategory::TestCoverage` has been documented as advisory since #1418, but the severity floor partitioned only `method-conformance` out of the correctness bucket, so a high-effort coverage gap floored exactly like a correctness bug. `TestCoverage` now reports `is_informational`, joining `Style` under the advisory ceiling (#7036).
- A coverage-gap finding's inline PR comment now carries a coverage-specific "does not block" label instead of the style/preference one (#7036).
