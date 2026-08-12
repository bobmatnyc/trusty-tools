Fixed

- Injected agent docs (`tm-tool-usage-guide.md`, `tm-circuit-breaker.md` CB#7, and the non-overridable "Trusty Tool Priority" rules) described `search_health` as a bare liveness check and never mentioned that a successful call no longer means the daemon is up, once PR #5534 lands. Each site now says to branch on the response's `healthy` field, not on the call succeeding, so an agent doesn't read a dead daemon as a false all-clear
