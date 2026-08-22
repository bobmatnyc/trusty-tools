Added

- A grounding leg measures the crate topology of any audited repository whose
  root `Cargo.toml` declares a `[workspace]`, and writes it into that
  repository's `manifest.toml` as a `crate_topology` table: member count, each
  member's direct internal dependencies and how many members depend on it, the
  total edge count, and any dependency cycle over those edges. It runs
  `cargo metadata --no-deps --format-version 1` — no dependency resolution, no
  network — and reads three keys out of the result, so it adds no dependency to
  the crate. `trusty-review` renders those numbers as a deterministic table in
  Code Quality & Architecture and states them to the architecture paragraph, so
  that paragraph comments on the workspace's real shape instead of inferring one
  from complexity buckets and a language list.
- Dev-dependencies are deliberately not counted as architecture edges: cargo
  permits a dev-dependency cycle, so counting them would report a routine
  test-only arrangement as a cycle. A cycle over the edges that remain is one
  cargo itself rejects, which the report states as such.
- A repository that is not a Cargo workspace is a declared skip, not a gap — it
  has no crate topology to miss, and its report is unchanged. A repository that
  IS a workspace whose metadata could not be read is a named gap, naming the
  repository and what its report therefore will not carry.
