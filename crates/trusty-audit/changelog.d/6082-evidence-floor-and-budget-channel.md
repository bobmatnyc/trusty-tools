Fixed

- Evidence discovery scores hits against a share of their own query's best hit
  instead of an absolute 0.15 floor. `POST /indexes/{id}/search` fuses its lanes
  with Reciprocal Rank Fusion, so a hit's score is `Σ weight / (60 + rank)` and
  can never exceed `1/61 ≈ 0.0164` — an order of magnitude below the floor. Every
  hit of every query was therefore dropped on every run, and the discovery leg
  produced nothing whatever the index held: the 2026-08-22 dogfood audit searched
  an 85 840-chunk index and wrote zero search-derived evidence rows. The relative
  floor drops the same filler beside a strong hit and can no longer empty a
  result set, because the best hit always clears it.
- A search leg that matched nothing now states its gap even when the repository
  has a build manifest. The deterministic `Cargo.toml` enumeration ran before the
  gap check read the dimension list, so it created a dimension on a run where
  search answered nothing at all — and the manifest then declared
  `attributed_only = true`, telling `trusty-review` to decline its path-name
  top-up over a sample with no search evidence in it. Both the gap line and
  `attributed_only` are now decided by the search result alone.
- The investigation budget reaches the renderer through the `tga audit` child's
  environment as well as through the manifest. `tga audit` renders from the
  manifest in the same process that wrote it, and this crate's grounding pass
  edits that file only after the child exits — so the recorded budget reached a
  re-render and never the sweep's own report. The 2026-08-22 run shipped a
  manifest declaring `investigate_max_files = 240` over an investigation that
  recorded `max_files: 40`. An explicit flag and a declared manifest key both
  still win.
- A ranking written into a manifest that has already been rendered from now says
  so on the console, and names `taudit render` as the recovery. That degradation
  previously had no line anywhere: the same run shipped 37 ranked paths beside an
  investigation whose every examined file read "path-name heuristic".
