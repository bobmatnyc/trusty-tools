Changed

- Both contributor-profiling model passes — the per-period review and the
  narrative synthesis — now send their JSON Schema on
  `trusty_common::inference::ChatRequest.response_schema`, so a provider that
  supports structured output constrains the answer instead of being asked in
  prose to follow the schema. On a provider that cannot honour it (Bedrock's
  Converse API has no such parameter), the schema is still rendered into the
  system turn as before, so `tga profile --model bedrock/…` keeps working
  (#5588).
