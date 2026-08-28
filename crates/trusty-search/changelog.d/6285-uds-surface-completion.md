Added
- The Unix-socket RPC surface serves the last four routes with a named consumer:
  `search.index.config.set`, `search.config.set`, `search.logs.tail` and
  `search.registry.orphans`. Each runs the same core its HTTP route runs, so a
  caller moving onto the socket gets the same body and the same refusals (#6285).

Changed
- The socket accepts frames up to 64 MiB instead of the shared 8 MiB
  control-plane default, matching the `DefaultBodyLimit` that
  `POST /indexes/{id}/graph` already carried. `POST /indexes/{id}/graph` now
  names that constant rather than restating the literal. A client reads its own
  responses under its own budget, so a consumer dialling these names must use
  `trusty_common::uds::send_framed_request_capped` with the same figure (#6285).
