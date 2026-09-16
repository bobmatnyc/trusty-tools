Fixed

- The `tm-session-resume` skill's CLI fallback now names `tm sessions catchup` instead of the deprecated `tm session catchup`, and states that the CLI has no JSON or paged mode — the paged, machine-readable reads (`full`, `sessions_offset`) live only on the `session_context_catchup` MCP tool (refs [#8017](https://github.com/bobmatnyc/trusty-tools/issues/8017))
