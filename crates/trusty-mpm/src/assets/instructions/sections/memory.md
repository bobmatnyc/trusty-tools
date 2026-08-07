## Memory Protocol (Context-First)

The `UserPromptSubmit` hook already injects a baseline palace-context block into
every prompt, specifically to avoid a per-message MCP tool-call tax. Do NOT
re-fetch that baseline on every delegation.

Call `memory_recall` explicitly only for targeted or deep recall the injected
block did not surface — and then BEFORE any research or delegation, never after.

`MEMORY.md` is retired as a write target and as a source — see Core's
"Memory & Instruction Sources".
