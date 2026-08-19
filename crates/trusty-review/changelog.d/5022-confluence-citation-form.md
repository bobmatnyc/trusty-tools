Fixed

- `[confluence: "Page Title" — "excerpt"]` is now a recognised inline citation form. `BRACKET_CITATION_RE` matched four bracket forms and the two system prompts mandated the same four, but `duettoresearch/code-intelligence` emits a fifth — so a Confluence excerpt survived the pre-scan strip and was read as an ungrounded free-text code quote, tripping the fabrication check on prose the model had cited correctly. The regex and both prompt grammars now list all five. See #5022.
