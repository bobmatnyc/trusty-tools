Removed

- BREAKING (public API): deleted the `symgraph::server` module (`AppState`, `router`, `serve`, `DEFAULT_PORT`, `ApiError`) and the `symgraph-server` cargo feature. The module bound `0.0.0.0` and had no caller in the workspace; only the console may bind TCP (ADR-0032). Refs [#8926](https://github.com/bobmatnyc/trusty-tools/issues/8926)
