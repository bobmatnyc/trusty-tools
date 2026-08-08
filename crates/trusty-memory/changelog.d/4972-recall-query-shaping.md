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
  line (`original_tokens`, `sent_tokens`, `sent_tokens_max`, `budget_tokens`,
  `envelope_stripped`, `units_dropped`) plus a `tracing::warn!`. The budget is
  overridable with `TRUSTY_MEMORY_PROMPT_QUERY_TOKENS`; setting it well above the
  real window restores the previous behaviour.

  The token estimate charges ASCII-letter runs 1 token per 2 characters rather
  than per 3. The old divisor was calibrated on English and underestimated every
  compound-word language measured against the model's own tokenizer — Hungarian
  342 against a true 372, Finnish 362 against 392, Dutch 332 against 362 — which
  let those prompts pass through as fitting and be cut inside the embedder while
  the new metric reported no loss. The cost is paid by English: a prompt now
  delivers ~189 true tokens of the 512-token window rather than ~291.

  No divisor above 1 token per character can bound a run of ASCII letters, so
  `recall_query` also carries `sent_tokens_max`, a true ceiling on what was sent.
  `sent_tokens <= budget_tokens` is an estimate clearing a budget, not a proof;
  `sent_tokens_max > budget_tokens` marks the sends whose fit could not be
  proven, so the log stops reporting a clean pass on a query the embedder may
  still have cut.
