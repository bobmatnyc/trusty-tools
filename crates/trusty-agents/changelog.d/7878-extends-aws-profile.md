Fixed
- An agent that `extends` another can now declare its own `[llm].aws_profile` and `[llm].aws_region`; both were previously inherited wholesale from the base and silently dropped, so a Bedrock-pinned overlay dispatched against whatever `AWS_PROFILE` the process happened to carry.
