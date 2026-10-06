Fixed

- A reviewer finding with no `title`, or a blank one, no longer turns the
  whole review into UNKNOWN. Its title is the first sentence of its `body`,
  capped at 120 characters, or "Untitled finding" when the body is blank too.
  `body` and `verdict` stay required, and the count of derived titles is
  logged once per parsed reply.
- When the review object fails to deserialize, the fail-safe reason now names
  the serde cause and its line and column, for example ``missing field
  `verdict` at line 1 column 52``. It never quotes the reply.
- The `review_output` schema declares `source_citation` as a nullable field,
  so strict-mode providers (OpenRouter, Fireworks) can send the citation the
  prompt asks for.
