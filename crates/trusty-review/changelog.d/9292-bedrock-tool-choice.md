Fixed
- Structured Bedrock calls to Claude Sonnet 5.5 and Opus 5.5, including the
  default reviewer, no longer fail with `ValidationException: tool_choice:
  type "tool" and "any" are not supported for this model`. For these models
  the request sets `toolChoice` to `auto`, keeps the output tool, and adds one
  system-prompt line asking the model to answer through that tool. This holds
  behind any inference-profile region prefix, including `apac.` and `au.`
  inference-profile ARNs. An application-inference-profile ARN, which does not
  name its model, gets the same `auto` request. Every other model sends the
  same forced request as before. A prose reply is parsed as JSON text or fails
  closed, as before.
