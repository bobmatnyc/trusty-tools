Changed

- The MCP `review_pr` / `review_diff` tools fail a `reviewer_model` override
  they cannot honour. #1357 made this case detectable — it ran the review on the
  startup provider and reported a `reviewer_model_fallback` string — but that is
  still a verdict from a model the caller did not ask for. The tool call now
  returns an error naming the requested model, and the `reviewer_model_fallback`
  envelope and payload field is removed, since nothing can set it any more.
