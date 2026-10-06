Fixed
- A review reply that carries one complete review object outside a ```` ```json ````
  fence now parses: after or before prose, in a bare, `JSON` or `jsonc` fence,
  or as a tool input wrapped once in `review_output` or `input`. Bedrock 5.5
  models on `toolChoice` auto answer this way. Two different review objects in
  one reply, an object in a non-JSON code fence, and a keyword-only reply stay
  fail-safe UNKNOWN (#9310).
- A review that fails to parse now records its reply shape (stop reason, output
  tokens, text length, and a short masked head and tail) in the warning log and
  in the `review not parsed` error. The Bedrock provider logs the content-block
  kinds of a structured reply that carried no tool call (#9310).
