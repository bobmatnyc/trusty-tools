Added
- Each compression-opportunity row now carries a `skip_reason` naming why the result was not compressed (`chained`, `no_filter`, `source_read`, `wrapper_prefix`, `not_executable`, `denied`, …) or that it was (`compressed`); rows written earlier read as `unknown` (#10973).
- `fold_real_opportunities` tallies, per `skip_reason`, only the rows compression would actually have shrunk (#10973).
