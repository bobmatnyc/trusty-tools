Changed
- Architect skills: in `architect-operating-rules`, owner "asks" become "actions" and D/A/Q are defined. `architect-workflow` and `tm-context-refresh` name the Intent Ledger digest as the Architect's handoff, through `scripts/self-clear.sh --live`. `architect-strategy` adds the factory issue-tracking rule (#10161).
- Architect core: the verified checkpoint for a session with an Intent Ledger is a fresh ledger digest. For any other session it is a verified `/tm-session-pause` snapshot (#10161).
