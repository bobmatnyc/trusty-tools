Added
- A review that fails to parse now records its reply shape (stop reason, output
  tokens, text length, and a short masked head and tail) in the warning log and
  in the `review not parsed` error. The Bedrock provider logs the content-block
  kinds of a structured reply that carried no tool call (#9310).
- When a reply's ```` ```json ```` or ```` ```jsonc ```` fence holds no valid
  review object, the fail-safe reason now says so. The review stays fail-safe
  UNKNOWN, and no other object in the reply is trusted (#9310).
