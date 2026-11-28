Fixed
- With the sandbox opted in (`TM_CLAUDE_SANDBOX=1`), a sandboxed PM or agent can again reach trusty-memory, trusty-search, trusty-analyze and trusty-agents from Bash: the sandbox now allows each daemon's own socket by exact path, as it already did for `trusty-mpm.sock`. trusty-code, trusty-secrets, trusty-events, trusty-console, trusty-embedderd and sccache stay blocked (#12077).
