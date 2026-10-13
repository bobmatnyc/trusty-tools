Fixed
- A deny (`/deny`, `tm session answer <id> no`) to a session held at its front gate no longer launches the withheld task. The decision closes, the session is decommissioned, and a repeat answer is a 409 that names the decision denied. Answer text that is neither an approve nor a deny is refused with a 422 and the decision stays pending (#9843).
- A front-gate approve whose runtime never came up now answers 500 and records the answer as failed, instead of reporting it delivered. A launch whose state write fails is no longer attempted (#9843).
- A session whose front-gate decision is pending or denied is no longer resumable, and a daemon restart keeps such a session at its front gate, where the decision can still be answered (#9844).
