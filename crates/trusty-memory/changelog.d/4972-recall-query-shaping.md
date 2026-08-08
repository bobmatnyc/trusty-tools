Fixed

- **The `prompt-context` recall query is shaped to the embedder's window instead
  of being cut inside it (closes [#4972](https://github.com/bobmatnyc/trusty-tools/issues/4972)).**
  The raw user prompt went to `/recall` verbatim and `all-MiniLM-L6-v2` truncated
  it at 512 tokens with no warning, no metric and no signal to the caller, so the
  vector represented a prefix. Over the logged corpus 52.0% of prompts exceed
  that window, and 65.3% arrive wrapped in a `<task-notification>` envelope whose
  task ids and absolute paths spend a median 253 of the 512 tokens before any
  payload begins. The hook now strips that envelope to its `<summary>` and
  `<result>` — which alone raises the share of prompts that fit whole from 46.1%
  to 55.3% — and, when the remainder is still over budget, keeps whole leading
  lines (falling back to whole words) rather than letting the cut land mid-word.
  Every reduction is recorded: a `recall_query` object on the enriched-prompt log
  line (`original_tokens`, `sent_tokens`, `budget_tokens`, `envelope_stripped`,
  `units_dropped`) plus a `tracing::warn!`. The budget is overridable with
  `TRUSTY_MEMORY_PROMPT_QUERY_TOKENS`; setting it well above the real window
  restores the previous behaviour.
