Fixed

- **`prompt-context` no longer injects drawers that did not match the prompt
  ([#5037](https://github.com/bobmatnyc/trusty-tools/issues/5037)).** The hook asked for `top_k` drawers and rendered whatever came
  back. A probe of "what is the capital of France" against the live palace
  returned five drawers all scoring exactly `0.15` — the L1 no-similarity
  penalty — formatted identically to a genuine `0.56` hit, so the reader could
  not tell noise from signal. Four changes:

  - **Relevance floor.** `RecalledDrawer` now parses the `score` the recall
    endpoint has always sent, and drawers below the floor are dropped via
    `trusty-common`'s `apply_relevance_floor` (default `0.35`; set
    `TRUSTY_MEMORY_PROMPT_MIN_SCORE=0` to restore the old behaviour). It runs
    after the deny-tag filter so a tag-excluded drawer is never also counted as
    withheld.
  - **Withheld notice.** When the floor drops drawers the injection says so and
    points at `memory_recall`, with distinct wording for a partial drop and for
    a recall that kept nothing. Zero candidates still renders nothing — an empty
    palace has nothing to announce. Without this the floor would have swapped a
    visible wrong answer for an invisible one.
  - **Larger budget.** `DEFAULT_TOP_K` 5 → 12 (override ceiling 20 → 30) and
    `INJECTION_BYTE_CAP` 4 KB → 8 KB. Both were sized when nothing distinguished
    a good recall from a bad one, so extra room only bought more noise; with the
    floor active the extra slots can only fill with candidates that cleared it.
    Measured over 80 real logged prompts, `K=12` renders a drawer section of at
    most ~2.7 KB.
  - **Whole-input query, pinned.** The recall query was already the entire user
    prompt; `recall_query_is_the_whole_prompt` now pins that against the
    truncating `hook_prompt_excerpt` helper sitting next to it. Truncation at
    the embedder's 512-token window is a separate layer and remains
    [#4972](https://github.com/bobmatnyc/trusty-tools/issues/4972).
