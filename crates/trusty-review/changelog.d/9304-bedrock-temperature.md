Fixed

- The Bedrock provider no longer sends `temperature` to Claude Opus 5.5 or
  Claude Sonnet 5.5, which reject it with `ValidationException`, so reviews on
  either model no longer fail on every call. Every id shape is covered: bare,
  any region prefix, and inference-profile or foundation-model ARNs. Every
  other model, and an application-inference-profile ARN, still sends the
  configured temperature ([#9304](https://github.com/bobmatnyc/trusty-tools/issues/9304)).
