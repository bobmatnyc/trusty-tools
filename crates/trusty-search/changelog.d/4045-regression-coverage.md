Added

- **New regression tests guard the #4045 re-regressed fixes.** `tests_4045` (`commands::start`) asserts single-registration-per-corpus against the registry warm boot populates, driven through the colocated discovery scan that #3929 introduced — the path the #2305 / #2336 call-site guards do not sit on. No source behavior change; the crate's version was bumped only to keep main's version-parity gate green (`check-pr-version-bump.sh`).
