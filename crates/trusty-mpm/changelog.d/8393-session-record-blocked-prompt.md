Breaking
- `SessionRecord` gains a public `blocked_prompt` field, the free-text prompt a confident `blocked_on_permission` verdict surfaced (#8393). Code that builds a `SessionRecord` with a struct literal must set it (`None` keeps the previous behaviour).
