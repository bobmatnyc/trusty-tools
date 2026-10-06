Added
- A review that fails to parse now records its reply shape (stop reason, output
  tokens, text length, and a short masked head and tail) in the warning log and
  in the `review not parsed` error. The Bedrock provider logs the content-block
  kinds of a structured reply that carried no tool call (#9310).
- When a reply's ```` ```json ```` or ```` ```jsonc ```` fence holds no valid
  review object, the fail-safe reason now says so. The review stays fail-safe
  UNKNOWN, and no other object in the reply is trusted (#9310).
- Opt-in raw capture of Bedrock reviewer replies: with
  `TRUSTY_REVIEW_CAPTURE_DIR=<dir>` set, every reviewer call (unified and
  map-reduce chunks, parsed or not) writes one JSON file (mode 0600, in a dir
  created 0700) holding the raw reply, the tool-use input, model, stop reason,
  token counts and a timestamp. Off by default. A failed write is logged and
  never changes the review. The model-eval report names each row's capture
  files (#9310).
