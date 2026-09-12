Changed

- Session MCP scoping now trusts a project's declarations BY CONTENT (#7672). An
  untrusted project's `.mcp.json` entries and `[session] mcp_servers` opt-ins are
  classified rather than discarded: one whose executable spec already exists
  outside the repository — a trusty-* framework builtin, or an entry in the
  operator's own `tm mcp add` registry — loads exactly as it would for a trusted
  project. Equivalence is on the normalized spec (resolved command path, args,
  env keys and values for stdio; transport, URL and headers for remote); a
  matching name alone is never enough, a reserved builtin name matches only
  evidence under that same name, and every error or unmodelled field classifies
  as unknown. The `session-scoped MCP config degraded` warning now fires only
  when an unknown entry exists and names it, instead of reporting the whole file
  as ignored. `tm project trust` is unchanged: a trusted project still loads
  everything.
