Changed

- Version skipped from 4.0.1 to 4.0.2 with no code change, and tga's row in
  `scripts/semver-checks-crate-exclusions.tsv` is removed. The row claimed no
  workspace package links tga as a library; #6294 made tga a dev-dependency of
  trusty-analyze, so the gate refused the skip and `check_semver.sh --crate tga`
  exited 3 with no verdict on every tga branch. tga is compared against the
  registry like any other crate now. The `tga-v4.0.1` tag is pinned to the
  commit that predates this fix, and #6178 makes a release tag here immovable,
  so 4.0.1 is spent.
