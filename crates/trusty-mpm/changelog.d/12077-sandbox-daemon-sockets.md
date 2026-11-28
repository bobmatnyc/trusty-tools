Fixed
- A sandboxed PM or agent can again reach trusty-memory, trusty-search, trusty-analyze, trusty-code and trusty-agents from Bash: the sandbox now allows each daemon's own socket by exact path, as it already did for `trusty-mpm.sock`. trusty-secrets, trusty-events, trusty-console, trusty-embedderd and sccache stay blocked (#12077).
