Fixed
- A `run-task` report and `session.get_transcript` label each delegated turn with the agent the PM actually delegated to, such as `rust-engineer`, instead of always `python-engineer`; `TCODE_DEBUG_TRANSCRIPT` capture uses the same label (#8158).
