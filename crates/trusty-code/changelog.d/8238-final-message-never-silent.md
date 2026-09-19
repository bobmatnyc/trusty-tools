Fixed

- a turn no longer ends with no assistant message at all ([#8238](https://github.com/bobmatnyc/trusty-tools/issues/8238))
  - the agent loop treated an assistant turn with no tool calls as "the model is done", so a turn that also carried no text ended the run in silence — no report, no question, no refusal
  - such a turn now costs one extra round-trip in which the model is asked for the closing message; the ask is one-shot, so a second silent turn still ends the run
  - a turn carrying neither text nor tool calls is no longer recorded in the transcript at all. It contributed no content block on Bedrock Converse, which merged the turns around it into two consecutive user messages and rejected the request — so on a `bedrock/*` model the ask itself failed and the run ended `Failed`
  - the ask is skipped on the last budgeted turn, where it could not be answered and only changed a run that returned `Ok` into one reporting `TurnCapExceeded`
