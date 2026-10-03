Fixed
- A tool method called over the socket against a palace that has no `palace.json` now answers JSON-RPC `-32004` (not found) instead of `-32603` (internal error). Any other tool failure keeps `-32603`. Callers such as the catch-up digest can now tell "this project has no palace yet" from a daemon fault (#9026).
