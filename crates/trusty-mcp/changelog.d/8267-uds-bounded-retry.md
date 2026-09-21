Fixed
- The stdio bridge's first forwarded request dials under the longer startup retry bound, so a bridge launched before its daemon has bound the socket reaches it once the daemon comes up instead of failing for the whole session. Later requests keep the fast per-request bound (#8267).
