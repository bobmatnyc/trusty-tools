Fixed

- **`tcode serve --http` now routes through the shared origin guard, so a web
  page cannot drive or read the daemon cross-origin
  ([#6003](https://github.com/bobmatnyc/trusty-tools/issues/6003)).**
  `serve::http::build_axum_router` called
  `trusty_common::server::with_standard_middleware` — permissive CORS
  (`allow_origin(Any)`) and no origin guard — while every sibling daemon used
  the guarded stack. The daemon defaults to a fixed loopback port and `tcode
  tui` auto-spawns it, so any page open in the operator's browser could POST
  cross-origin to `/rpc`, `/tasks`, `/sessions`,
  `/sessions/{id}/messages`, `/agents` and the workstream write routes, and read
  `GET /sessions/{id}/transcript` plus both SSE streams — conversation content.
  The router now calls `with_guarded_middleware_same_origin_cors` with
  `SelfOrigins::default()`: a write carrying a foreign `Origin` gets `403`
  before any handler runs, and the CORS policy reflects only same-machine
  origins so a foreign page cannot read the `GET` surface either. Callers that
  send no `Origin` — `tcode tui`'s own client, the console reverse proxy,
  `curl` — are unaffected, and a loopback `Origin` still passes. This is CSRF
  and read-disclosure defence, not caller authentication;
  [#5439](https://github.com/bobmatnyc/trusty-tools/issues/5439) stays open for
  that.
