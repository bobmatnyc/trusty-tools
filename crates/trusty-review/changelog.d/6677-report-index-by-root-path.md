Fixed

- The report pipeline finds a checkout's trusty-search index when it is
  registered under an id the path does not derive to. `--analyze` and the trace
  pass resolved by `derive_checkout_index_id` and an exact id match only, so a
  ready index registered at the same `root_path` under another id — the main
  checkout served as `trusty-tools-checkout` against a derived
  `trusty-tools-4e2cf878` — could never be reached: analyze degraded to scan,
  every trace lookup returned `IndexAbsent`, and the run exited 0. Both call
  sites now resolve through `report::index_registry::resolve_report_index`,
  which keeps the derived id when the daemon holds it, otherwise substitutes the
  index whose canonicalised `root_path` IS the checkout (logging the
  substitution), and otherwise names the derived id and says nothing registered
  covers the path (#6677).
- `HttpAnalyzeMetricsSource::with_search_base_url` points the trusty-search
  registry read at an explicit address. Without it the read resolved the
  machine's advertised daemon whatever socket the source was built with, so a
  caller holding the source over a stub analyze socket still issued a live HTTP
  GET to whatever trusty-search was running and resolved against what that
  daemon held (#6677).
- The report's unresolved-index warning says which cause it hit —
  `registry=empty` for a daemon that answered nothing, `registry=populated` for
  one holding indexes none of which is rooted at this checkout — so the remedy
  reads off one line instead of two (#6677).
