Added

- The daemon socket serves `mpm.mcp.dispatch`, which takes the MCP JSON-RPC
  envelope `POST /rpc` takes and returns the same response, through the same
  body (#6288).
- The daemon socket serves `mpm.delegation.list`, `mpm.delegation.repair`,
  `mpm.delegation.repair_by_id`, `mpm.sessions.context` and
  `mpm.sessions.chat`, each through the same body as its HTTP route. A repair's
  caller session, an HTTP header, is the `caller_session` param on the socket
  (#6288).
- The socket route table maps the SESSCTL control, L2 proxy, delegation,
  coordinator and `/rpc` routes onto their socket methods. No client call site
  changed (#6288).
