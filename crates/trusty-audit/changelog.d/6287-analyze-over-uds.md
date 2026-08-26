Changed

- `grounding`'s analyze guard and hotspot fetch dial trusty-analyze's Unix
  socket instead of port 7879 (#6287, ADR-0032). As in `tga`, the health verdict
  is explicit — only `status: "ok"` passes — because a JSON-RPC health call
  answers with a result frame whether or not trusty-search is reachable, and a
  degraded daemon serves an empty hotspot list that reads as "nothing complex".
- `TRUSTY_ANALYZE_URL` becomes `TRUSTY_ANALYZE_SOCKET`. The value it carries is
  a filesystem path now, and a variable still saying URL would leave an operator
  setting `http://…` and getting a dial failure naming a socket they never
  configured.
