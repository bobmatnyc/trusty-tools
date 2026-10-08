Security
- A trusty-search URL with credentials no longer reaches `result.error`, `review_body` (what `review_pr` posts) or stderr when search is unreachable: its userinfo password and credential query values (for example `access_token`) print as `[redacted]`, on the CLI and over MCP. The host, path and error stay. (#9431)
