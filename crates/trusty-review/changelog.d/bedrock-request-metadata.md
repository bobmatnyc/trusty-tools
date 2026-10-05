Added

- Every Bedrock Converse request now carries `requestMetadata`
  `caller=trusty-review` and `crate_version=<version>`, so Bedrock model
  invocation logs attribute trusty-review's spend. Requests whose response
  schema identifies the call also carry `role` (`reviewer`, `verifier`,
  `synthesis` or `investigate`). OpenRouter and Fireworks requests are
  unchanged.
