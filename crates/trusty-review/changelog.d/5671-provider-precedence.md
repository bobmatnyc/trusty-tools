Fixed

- The built-in default provider is now resolved from the credentials that are
  actually available instead of being hardcoded to Bedrock (#5671). Precedence:
  an explicit `--provider` / `TRUSTY_REVIEW_PROVIDER` / config-file `provider`
  always wins; otherwise a non-blank `OPENROUTER_API_KEY` selects OpenRouter
  (with OpenRouter model defaults, not Bedrock inference-profile ids);
  otherwise detectable AWS credentials select Bedrock; otherwise the run fails
  with a message naming both options rather than defaulting into an obscure
  failure. A blank or whitespace-only `OPENROUTER_API_KEY` does not count.
  Behaviour change: a machine with BOTH an OpenRouter key and AWS credentials
  and no explicit override now uses OpenRouter — set
  `TRUSTY_REVIEW_PROVIDER=bedrock` to keep Bedrock. A machine whose only AWS
  credential source is EC2/ECS instance metadata is not detected; set
  `TRUSTY_REVIEW_PROVIDER=bedrock` there.
