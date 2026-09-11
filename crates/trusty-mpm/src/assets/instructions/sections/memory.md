## Memory Protocol (Context-First)

The `UserPromptSubmit` hook already injects a baseline palace-context block into
every prompt — do NOT re-fetch it per delegation. Call `memory_recall` only for
targeted or deep recall that block did not surface, and then BEFORE any research
or delegation, never after.
