Changed

- An untrusted project's own `.mcp.json` entries are now classified by CONTENT
  rather than discarded wholesale (#7672). An entry loads without
  `tm project trust` only when its normalized executable spec equals a trusty-*
  framework builtin, or a registry server the operator has explicitly shared
  with projects — `tm mcp add --share-with-projects <name>`, `tm mcp share
  <name>`, `tm mcp unshare <name>`, recorded in
  `~/.trusty-tools/trusty-mpm/mcp-shared.json`. Registering a server with
  `tm mcp add` does NOT share it: sharing is off by default, and an unshared
  registry entry never matches even on an exact spec match, because a
  credential-bearing server whose secret arrives from the ambient environment
  has a fully public spec any repository could reproduce. A share is granted to
  the server's CONTENT, not to its name: the grant records the digest of the
  registry entry as it stood, `tm mcp share` refuses a name with no registered
  server, `tm mcp remove` drops the grant with the server, and a `tm mcp add`
  that replaces an entry drops the grant unless `--share-with-projects` renews
  it — so a reused name can never inherit an unrelated server's grant. A grant
  whose digest no longer matches is reported as stale, with the
  `tm mcp share <name>` that renews it. Equivalence covers the
  resolved command path, args, and env keys and values for stdio; transport, URL
  and headers for remote. A matching name is never sufficient, a framework
  builtin name matches only evidence under that same name, and every error or
  unmodelled field classifies as unknown. `[session] mcp_servers` opt-ins and
  `[session] plugins` are unchanged and stay fully gated on `tm project trust` —
  an opt-in is a bare name the repository supplies with no content to judge. The
  `session-scoped MCP config degraded` warning now fires only when something was
  actually ignored, names it, and adds a `tm mcp share <name>` hint when an
  entry matched an unshared registry server.
