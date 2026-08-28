Added
- The daemon serves its registry-B project, deliverable/milestone, L3 manager, peer-bus, pairing and delegation-query routes as JSON-RPC methods on the Unix socket, alongside the unchanged HTTP surface (33 methods, `mpm.projects.*` through `mpm.delegation.*`).
- The peer bus's SSE `subscribe` leg is deliberately not among them — it needs the streaming seam a later slice adds, and its HTTP handler is untouched.
