Changed
- The verifier now defaults to a different model family than the reviewer: Amazon Nova Pro (`us.amazon.nova-pro-v1:0`) on Bedrock and Gemini 2.5 Flash on OpenRouter, replacing Claude Haiku 4.5. `--verifier-model`, `TRUSTY_REVIEW_VERIFIER_MODEL` and `[models.verifier]` still override it.
- A warning is logged when the reviewer and verifier resolve to the same model id.
