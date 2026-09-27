Fixed

- a turn that ends with no tool calls and no text now gets one chance to produce its closing message ([#8238](https://github.com/bobmatnyc/trusty-tools/issues/8238))
  - WHY the model produces such a turn is not diagnosed. This is a mitigation for the one shape #8238's transcripts show: the loop read "no tool calls" as "the model is done" and accepted the empty turn, so the user saw nothing after the last tool result. A turn that ends silent for any other reason is not covered
  - that shape now costs one extra round-trip in which the model is asked for the closing message; the ask is one-shot, so a second silent turn still ends the run
  - a turn carrying neither text nor tool calls is no longer recorded in the transcript at all. It contributed no content block on Bedrock Converse, which merged the turns around it into two consecutive user messages and rejected the request — so on a `bedrock/*` model the ask itself failed and the run ended `Failed`
  - the ask is skipped on the last budgeted turn, where it could not be answered and only changed a run that returned `Ok` into one reporting `TurnCapExceeded`
