Changed
- The Unix socket accepts frames up to 64 MiB instead of the shared 8 MiB
  control-plane default, matching the `DefaultBodyLimit` that
  `POST /indexes/{id}/graph` already carried — which now names that constant
  rather than restating the literal. A client reads its own responses under its
  own budget, so a consumer dialling these names must use
  `trusty_common::uds::send_framed_request_capped` with the same figure (#6285).
