Added
- `tm hook` applies an IL block to the session's ledger when a PM receives an agent handback (`PostToolUse`), an inbound session or Architect message arrives, or the Architect submits a prompt (`UserPromptSubmit`). A rejected or failed block writes nothing, is never reported as applied, and is recorded where `list_recent_errors` reads it as `il-auto-apply:<rule>` (G0b).
