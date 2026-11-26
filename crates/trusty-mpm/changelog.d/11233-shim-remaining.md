Fixed

- The pinned gh shim no longer calls a GraphQL refusal the primary rate limit while the hourly quota still has points left. Its one `gh api rate_limit` lookup now reads `remaining` as well as `reset`; with points left the refusal line names a short-window or secondary limit, states the remaining count and carries no reset, so `tm wait` and the daemon's PR watch back off briefly instead of sleeping to the hourly reset (#11233).
- Both refusal lines now end with gh's own rate-limit line, verbatim, so GitHub's limit text is no longer dropped (#11233).
