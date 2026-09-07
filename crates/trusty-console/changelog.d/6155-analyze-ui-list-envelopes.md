Fixed
- The analyze dashboard's Dashboard, Complexity, Smells, Refactors, Clusters and
  Facts views render their rows. Five `analyze.*` methods answer an object
  wrapping their list — `complexity_hotspots` sends `{index_id, top_n,
  hotspots}`, `smells` a pagination envelope around `chunks`,
  `refactor_suggestions` `{index_id, count, min_severity, suggestions}`,
  `clusters` a `ClusterResponse`, and `facts_list` `{facts, count}` — while
  every view iterates a flat array, so the landing view threw `hotspots.slice is
  not a function` and the other four `not iterable`. `api.js` unwraps each list,
  passing a bare array through unchanged, the same shape of fix
  [#7083](https://github.com/bobmatnyc/trusty-tools/issues/7083) made for the
  memory UI's palace roster. The envelope predates the console bridge: the
  retired HTTP router served these same handler functions, so these views have
  never rendered
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
- The Smells view groups by the smell the detector actually found. Its rows
  carry `CodeSmell`, which serde renders externally tagged
  (`{"LongFunction": {"lines": 80}}`, or a bare string for a variant with no
  fields), and the view read `category`/`name` off each one, so every row landed
  in a single `unknown` bucket
  ([#6155](https://github.com/bobmatnyc/trusty-tools/issues/6155)).
